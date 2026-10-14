//! Per-cell gain (dsp M7.6, master C11): stereo in → smoothed gain → stereo out.

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use pipewire::properties::PropertiesBox;

use crate::consts::GAIN_RAMP_SECS;
use crate::error::DspError;
use crate::filter::FilterNode;
use crate::processor::{Processor, STEREO_IN, STEREO_OUT};

/// The target gain, shared with the RT processor (f32 bits). Cheap to clone.
#[derive(Debug, Clone)]
pub struct GainHandle(Arc<AtomicU32>);

impl GainHandle {
    pub fn set_db(&self, db: f32) {
        self.set_linear(db_to_linear(db));
    }

    pub fn set_linear(&self, g: f32) {
        self.0.store(sanitize(g).to_bits(), Ordering::Relaxed);
    }

    pub fn linear(&self) -> f32 {
        f32::from_bits(self.0.load(Ordering::Relaxed))
    }
}

/// `10^(db/20)`; only `NEG_INFINITY` maps to 0.0 (C11).
pub fn db_to_linear(db: f32) -> f32 {
    if db == f32::NEG_INFINITY {
        0.0
    } else if !db.is_finite() {
        1.0
    } else {
        10f32.powf(db / 20.0)
    }
}

/// Non-finite or negative → silence; +12 dB hard ceiling (C6 clamps to +6 anyway).
fn sanitize(g: f32) -> f32 {
    if g.is_finite() && g >= 0.0 {
        g.min(4.0)
    } else {
        0.0
    }
}

#[derive(Debug)]
pub struct GainProcessor {
    target: Arc<AtomicU32>,
    current: f32,
    last_target: f32,
    step: f32,
    remaining: u32,
}

/// 2 in / 2 out, starting at `initial_db` with no ramp.
pub fn gain_processor(initial_db: f32) -> (GainProcessor, GainHandle) {
    let g = sanitize(db_to_linear(initial_db));
    let target = Arc::new(AtomicU32::new(g.to_bits()));
    let p = GainProcessor {
        target: Arc::clone(&target),
        current: g,
        last_target: g,
        step: 0.0,
        remaining: 0,
    };
    (p, GainHandle(target))
}

/// A connected gain cell node with C5 stereo ports.
pub fn new_gain_node(
    core: &pipewire::core::Core,
    name: &str,
    props: PropertiesBox,
    initial_db: f32,
) -> Result<(FilterNode, GainHandle), DspError> {
    let (p, h) = gain_processor(initial_db);
    let node = FilterNode::new(core, name, props, &STEREO_IN, &STEREO_OUT, p)?;
    Ok((node, h))
}

impl Processor for GainProcessor {
    fn process(&mut self, inputs: &[&[f32]], outputs: &mut [&mut [f32]], n: usize, rate: u32) {
        let t = f32::from_bits(self.target.load(Ordering::Relaxed));
        if t.to_bits() != self.last_target.to_bits() {
            self.last_target = t;
            #[allow(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                clippy::cast_precision_loss
            )]
            let len = ((GAIN_RAMP_SECS * rate as f32).round() as u32).max(1);
            #[allow(clippy::cast_precision_loss)]
            {
                self.step = (t - self.current) / len as f32;
            }
            self.remaining = len;
        }
        let silence: &[f32] = &[];
        // Every channel runs the same ramp from the same start; the state after the last
        // channel is the state for the next cycle.
        let mut end = (self.current, self.remaining);
        for (ch, out) in outputs.iter_mut().enumerate() {
            let input = inputs.get(ch).copied().unwrap_or(silence);
            let (mut current, mut remaining) = (self.current, self.remaining);
            for (i, o) in out[..n].iter_mut().enumerate() {
                if remaining > 0 {
                    current += self.step;
                    remaining -= 1;
                    if remaining == 0 {
                        current = t; // exact landing, no float drift
                    }
                }
                *o = input.get(i).copied().unwrap_or(0.0) * current;
            }
            end = (current, remaining);
        }
        (self.current, self.remaining) = end;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(p: &mut GainProcessor, input: &[f32], rate: u32) -> Vec<f32> {
        let mut l = vec![0.0; input.len()];
        let mut r = vec![0.0; input.len()];
        {
            let mut outs: [&mut [f32]; 2] = [&mut l, &mut r];
            p.process(&[input, input], &mut outs, input.len(), rate);
        }
        assert_eq!(l, r, "both channels get the same gain");
        l
    }

    #[test]
    fn ramp_shape_and_exact_landing() {
        let (mut p, h) = gain_processor(0.0);
        h.set_linear(0.125);
        let out = run(&mut p, &[1.0; 600], 48_000);
        for (k, v) in out.iter().enumerate().take(479) {
            let expect = 1.0 + (k as f32 + 1.0) * (0.125 - 1.0) / 480.0;
            assert!((v - expect).abs() < 1e-5, "k={k}: {v} vs {expect}");
        }
        assert_eq!(out[479], 0.125, "lands exactly");
        assert!(out[480..].iter().all(|v| *v == 0.125), "then constant");
    }

    #[test]
    fn ramp_across_quanta_is_identical() {
        let input: Vec<f32> = (0..768).map(|i| (i as f32 * 0.01).sin()).collect();
        let (mut a, ha) = gain_processor(0.0);
        let (mut b, hb) = gain_processor(0.0);
        ha.set_db(-18.0);
        hb.set_db(-18.0);
        let whole = run(&mut a, &input, 48_000);
        let mut parts = Vec::new();
        for chunk in input.chunks(256) {
            parts.extend(run(&mut b, chunk, 48_000));
        }
        assert_eq!(whole, parts);
    }

    #[test]
    fn retarget_mid_ramp_has_no_jump() {
        let (mut p, h) = gain_processor(0.0);
        h.set_linear(0.0);
        let first = run(&mut p, &[1.0; 200], 48_000);
        let before = *first.last().unwrap();
        h.set_linear(1.0);
        let second = run(&mut p, &[1.0; 480], 48_000);
        let step = (1.0 - before) / 480.0;
        assert!(
            (second[0] - before - step).abs() < 1e-5,
            "continues from where it was"
        );
        assert_eq!(second[479], 1.0);
    }

    #[test]
    fn ramp_length_follows_rate() {
        for (rate, len) in [(44_100, 441), (48_000, 480), (96_000, 960)] {
            let (mut p, h) = gain_processor(0.0);
            h.set_linear(0.0);
            let out = run(&mut p, &vec![1.0; len + 10], rate);
            assert!(
                out[len - 2] > 0.0 && out[len - 1] == 0.0,
                "rate {rate}: ramp of {len}"
            );
        }
    }

    #[test]
    fn neg_inf_and_nan() {
        let (mut p, h) = gain_processor(0.0);
        h.set_db(f32::NEG_INFINITY);
        assert!(
            run(&mut p, &[1.0; 600], 48_000)[480..]
                .iter()
                .all(|v| *v == 0.0)
        );
        h.set_linear(f32::NAN);
        assert_eq!(h.linear(), 0.0, "NaN is treated as silence");
        assert!((db_to_linear(-60.0) - 0.001).abs() < 1e-7);
    }

    #[test]
    fn no_ramp_at_creation() {
        let (mut p, _) = gain_processor(-6.0);
        let out = run(&mut p, &[1.0; 4], 48_000);
        assert!(out.iter().all(|v| (v - db_to_linear(-6.0)).abs() < 1e-7));
    }
}
