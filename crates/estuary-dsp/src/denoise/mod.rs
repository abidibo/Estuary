//! Noise suppression (dsp M10): RNNoise via nnnoiseless, at any table graph rate.
//!
//! Signal path at graph rate R: downmix (L+R)/2 → input FIFO → [R→48 kHz] → 480-frame block →
//! RNNoise (wet) and a delay-matched copy (dry) → crossfade → [48 kHz→R] → output FIFO, primed
//! with zeros so it never runs dry. The dry path has the same latency as the wet one, so
//! toggling suppression never shifts timing and the crossfade is click-free.

pub mod fifo;
pub mod gate;
pub mod rate;
pub mod xfade;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use self::fifo::Fifo;
use self::gate::VadGate;
use self::rate::{RATE_TABLE, RatePath};
use self::xfade::{Crossfade, DelayLine};
use crate::consts::{DENOISE_BLOCK, DENOISE_RATE, MAX_QUANTUM};
use crate::processor::{Processor, STEREO_IN, STEREO_OUT};
use crate::stats::NodeStats;
use crate::{DspError, FilterNode};

/// nnnoiseless's algorithmic delay at 48 kHz, measured (dsp M10.5, `nn_delay_is_pinned`).
pub const NN_DELAY: usize = 480;
/// RNNoise scale: samples in the 16-bit range.
const NN_SCALE: f32 = 32768.0;
const FIFO_CAPACITY: usize = 2 * MAX_QUANTUM + 4096;

/// Control from the engine thread (C11). Atomics only.
#[derive(Clone, Debug)]
pub struct DenoiseHandle {
    pub enabled: Arc<AtomicBool>,
    pub vad_gate: Arc<AtomicU32>,
}

impl DenoiseHandle {
    pub fn set_enabled(&self, on: bool) {
        self.enabled.store(on, Ordering::Relaxed);
    }

    /// Clamped to 0.0–0.95 (C6); 0 is off.
    pub fn set_vad_gate(&self, g: f32) {
        let g = if g.is_nan() { 0.0 } else { g.clamp(0.0, 0.95) };
        self.vad_gate.store(g.to_bits(), Ordering::Relaxed);
    }
}

pub struct DenoiseProcessor {
    enabled: Arc<AtomicBool>,
    vad_gate: Arc<AtomicU32>,
    stats: Option<Arc<NodeStats>>,
    nn: Box<nnnoiseless::DenoiseState<'static>>,
    paths: Box<[RatePath]>,
    cur_rate: u32,
    /// `None` at 48 kHz (blocks move directly between the FIFOs).
    cur_path: Option<usize>,
    unsupported: bool,
    in_fifo: Fifo,
    out_fifo: Fifo,
    mono: Box<[f32]>,
    /// Holds a wrapped FIFO read for the downsampler.
    peek_tmp: Box<[f32]>,
    blk48: [f32; DENOISE_BLOCK],
    wet48: [f32; DENOISE_BLOCK],
    dry48: [f32; DENOISE_BLOCK],
    scaled: [f32; DENOISE_BLOCK],
    nn_out: [f32; DENOISE_BLOCK],
    dry_delay: DelayLine,
    xfade: Crossfade,
    gate: VadGate,
    /// The first denoised block after (re)enabling has fade-in artifacts: it's replaced by dry.
    warm_blocks: u32,
    was_enabled: bool,
}

impl std::fmt::Debug for DenoiseProcessor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DenoiseProcessor")
            .field("cur_rate", &self.cur_rate)
            .field("unsupported", &self.unsupported)
            .finish_non_exhaustive()
    }
}

/// 2 in / 2 out (C11). Enabled, gate off. Allocates everything: call off the RT thread.
pub fn denoise_processor() -> (DenoiseProcessor, DenoiseHandle) {
    let enabled = Arc::new(AtomicBool::new(true));
    let vad_gate = Arc::new(AtomicU32::new(0.0f32.to_bits()));
    let paths: Box<[RatePath]> = RATE_TABLE
        .iter()
        .filter_map(|r| match RatePath::new(*r) {
            Ok(p) => Some(p),
            Err(e) => {
                tracing::warn!(rate = r, "no resampler for this rate: {e}");
                None
            }
        })
        .collect();
    let peek = paths
        .iter()
        .map(RatePath::priming)
        .max()
        .unwrap_or(0)
        .max(DENOISE_BLOCK);
    let p = DenoiseProcessor {
        enabled: Arc::clone(&enabled),
        vad_gate: Arc::clone(&vad_gate),
        stats: None,
        nn: nnnoiseless::DenoiseState::new(),
        paths,
        cur_rate: 0,
        cur_path: None,
        unsupported: false,
        in_fifo: Fifo::with_capacity(FIFO_CAPACITY),
        out_fifo: Fifo::with_capacity(FIFO_CAPACITY),
        mono: vec![0.0; MAX_QUANTUM].into_boxed_slice(),
        peek_tmp: vec![0.0; peek].into_boxed_slice(),
        blk48: [0.0; DENOISE_BLOCK],
        wet48: [0.0; DENOISE_BLOCK],
        dry48: [0.0; DENOISE_BLOCK],
        scaled: [0.0; DENOISE_BLOCK],
        nn_out: [0.0; DENOISE_BLOCK],
        dry_delay: DelayLine::new(NN_DELAY),
        xfade: Crossfade::new(DENOISE_BLOCK, false),
        gate: VadGate::default(),
        warm_blocks: 0,
        was_enabled: true,
    };
    (p, DenoiseHandle { enabled, vad_gate })
}

/// A connected denoiser node with C5 stereo ports.
pub fn new_denoiser_node(
    core: &pipewire::core::Core,
    name: &str,
    props: pipewire::properties::PropertiesBox,
) -> Result<(FilterNode, DenoiseHandle), DspError> {
    let (p, h) = denoise_processor();
    let latency = crate::filter::process_latency_param(latency_ns_at_48k());
    let node =
        FilterNode::new_with_params(core, name, props, &STEREO_IN, &STEREO_OUT, p, &[latency])?;
    Ok((node, h))
}

/// Reported through `ProcessLatency`: the 48 kHz figure (resampled rates add a few ms, v2).
fn latency_ns_at_48k() -> i64 {
    #[allow(clippy::cast_possible_wrap)] // a few thousand frames
    let frames = (DENOISE_BLOCK + NN_DELAY) as i64;
    frames * 1_000_000_000 / i64::from(DENOISE_RATE)
}

/// Denoiser latency in frames at graph `rate` (the same in bypass). 0 for unsupported rates.
pub fn latency_frames(rate: u32) -> usize {
    if rate == DENOISE_RATE {
        return DENOISE_BLOCK + NN_DELAY;
    }
    if !RATE_TABLE.contains(&rate) {
        return 0;
    }
    let Ok(p) = RatePath::new(rate) else {
        return 0;
    };
    let nn_at_rate = NN_DELAY as f64 * f64::from(rate) / f64::from(DENOISE_RATE);
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // small, positive
    let frames = (p.priming() as f64 + p.delay_frames_at_rate() + nn_at_rate).round() as usize;
    frames
}

impl DenoiseProcessor {
    fn reconfigure(&mut self, rate: u32) {
        self.cur_rate = rate;
        self.unsupported = false;
        self.cur_path = None;
        if rate != DENOISE_RATE {
            match self.paths.iter().position(|p| p.rate == rate) {
                Some(i) => self.cur_path = Some(i),
                None => self.mark_unsupported(),
            }
        }
        self.in_fifo.clear();
        self.out_fifo.clear();
        for p in &mut self.paths {
            p.reset();
        }
        self.dry_delay.reset();
        self.gate.reset();
        // Restart from dry; if enabled, the next warm-up block fades the denoiser back in.
        self.xfade.snap(false);
        self.warm_blocks = 0;
        let priming = match self.cur_path {
            Some(i) => self.paths[i].priming(),
            None => DENOISE_BLOCK,
        };
        self.out_fifo.push_zeros(priming);
    }

    fn mark_unsupported(&mut self) {
        self.unsupported = true;
        if let Some(s) = &self.stats {
            s.unsupported_rate.store(self.cur_rate, Ordering::Relaxed);
        }
    }

    /// One 480-frame block from the input FIFO into `blk48`. `false` if not enough input.
    fn next_block(&mut self) -> bool {
        match self.cur_path {
            None => {
                if self.in_fifo.len() < DENOISE_BLOCK {
                    return false;
                }
                self.in_fifo.pop_into(&mut self.blk48);
                true
            }
            Some(i) => {
                let need = self.paths[i].input_needed();
                if self.in_fifo.len() < need {
                    return false;
                }
                let src = self.in_fifo.peek_contiguous(need, &mut self.peek_tmp);
                let ok = self.paths[i].downsample(src, &mut self.blk48);
                self.in_fifo.discard(need);
                if !ok {
                    self.mark_unsupported();
                }
                ok
            }
        }
    }

    fn denoise_block(&mut self) {
        self.dry48 = self.blk48;
        self.dry_delay.process(&mut self.dry48);
        let want = self.enabled.load(Ordering::Relaxed);
        if want != self.was_enabled {
            self.was_enabled = want;
            if want {
                self.warm_blocks = 0; // the fade to wet starts after the warm-up block
            } else {
                self.xfade.set_target(false);
            }
        }
        let mut start_fade = false;
        if want || !self.xfade.is_fully_dry() {
            for (s, x) in self.scaled.iter_mut().zip(&self.blk48) {
                *s = x * NN_SCALE;
            }
            let vad = self.nn.process_frame(&mut self.nn_out, &self.scaled);
            for (w, x) in self.wet48.iter_mut().zip(&self.nn_out) {
                *w = x / NN_SCALE;
            }
            if self.warm_blocks == 0 {
                // Discard RNNoise's fade-in. The fade to wet starts with the next block (after
                // this mix): fading here would complete on dry audio and jump to wet next.
                self.wet48 = self.dry48;
                self.warm_blocks = 1;
                start_fade = want;
            } else {
                let gate = f32::from_bits(self.vad_gate.load(Ordering::Relaxed)).clamp(0.0, 0.95);
                self.gate.apply(vad, gate, &mut self.wet48);
            }
        } else {
            self.wet48 = self.dry48; // fully bypassed: save the CPU
        }
        self.xfade.mix(&self.dry48, &self.wet48, &mut self.blk48);
        if start_fade {
            self.xfade.set_target(true);
        }
    }

    fn emit_block(&mut self) {
        match self.cur_path {
            None => {
                self.out_fifo.push(&self.blk48);
            }
            Some(i) => match self.paths[i].upsample(&self.blk48) {
                Some(out) => {
                    self.out_fifo.push(out);
                }
                None => self.mark_unsupported(),
            },
        }
    }
}

impl Processor for DenoiseProcessor {
    fn process(&mut self, inputs: &[&[f32]], outputs: &mut [&mut [f32]], n: usize, rate: u32) {
        let n = n.min(MAX_QUANTUM);
        if rate != self.cur_rate {
            self.reconfigure(rate);
        }
        let silence: &[f32] = &[];
        let l = inputs.first().copied().unwrap_or(silence);
        let r = inputs.get(1).copied().unwrap_or(l);
        for (i, m) in self.mono[..n].iter_mut().enumerate() {
            let (a, b) = (
                l.get(i).copied().unwrap_or(0.0),
                r.get(i).copied().unwrap_or(0.0),
            );
            *m = 0.5 * (a + b);
        }
        if self.unsupported {
            for out in outputs.iter_mut() {
                out[..n].copy_from_slice(&self.mono[..n]);
            }
            return;
        }
        self.in_fifo.push(&self.mono[..n]);
        while !self.unsupported && self.next_block() {
            self.denoise_block();
            self.emit_block();
        }
        if self.unsupported {
            // A resampler failed mid-cycle: pass through from now on (F11a / D13).
            for out in outputs.iter_mut() {
                out[..n].copy_from_slice(&self.mono[..n]);
            }
            return;
        }
        let Some((first, rest)) = outputs.split_first_mut() else {
            return;
        };
        let got = self.out_fifo.pop_into(&mut first[..n]);
        if got < n {
            first[got..n].fill(0.0);
            if let Some(s) = &self.stats {
                s.underruns.fetch_add(1, Ordering::Relaxed);
            }
        }
        for out in rest {
            out[..n].copy_from_slice(&first[..n]);
        }
    }

    fn attach_stats(&mut self, stats: Arc<NodeStats>) {
        self.stats = Some(stats);
    }
}

#[cfg(test)]
mod tests;
