//! Denoiser tests (dsp M10.5, M10.7).
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss
)]

use std::sync::Arc;
use std::sync::atomic::Ordering;

use super::*;
use crate::stats::NodeStats;

/// Deterministic pseudo-random numbers (xorshift), in [-1, 1).
struct Rng(u64);
impl Rng {
    fn next_u64(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn uniform(&mut self) -> f32 {
        (self.next_u64() >> 40) as f32 / (1u64 << 24) as f32 * 2.0 - 1.0
    }
}

fn white(seed: u64, n: usize, amp: f32) -> Vec<f32> {
    let mut r = Rng(seed);
    (0..n).map(|_| r.uniform() * amp).collect()
}

/// Band-limited noise that RNNoise keeps (so correlation finds the delay).
fn coloured(seed: u64, n: usize, amp: f32) -> Vec<f32> {
    let mut r = Rng(seed);
    let mut lp = 0.0f32;
    (0..n)
        .map(|_| {
            lp += 0.3 * (r.uniform() - lp);
            lp * amp
        })
        .collect()
}

fn rms(v: &[f32]) -> f64 {
    (v.iter().map(|x| f64::from(*x).powi(2)).sum::<f64>() / v.len().max(1) as f64).sqrt()
}

struct Rig {
    p: DenoiseProcessor,
    h: DenoiseHandle,
    stats: Arc<NodeStats>,
}

fn rig() -> Rig {
    let (mut p, h) = denoise_processor();
    let stats = Arc::new(NodeStats::default());
    p.attach_stats(Arc::clone(&stats));
    Rig { p, h, stats }
}

impl Rig {
    /// Feeds `input` (as both channels) in cycles of the given sizes; returns the output.
    /// Asserts both outputs are identical every cycle.
    fn run(&mut self, input: &[f32], rate: u32, quanta: &mut dyn FnMut() -> usize) -> Vec<f32> {
        let mut out = Vec::with_capacity(input.len());
        let (mut l, mut r) = (vec![0.0; MAX_QUANTUM], vec![0.0; MAX_QUANTUM]);
        let mut pos = 0;
        while pos < input.len() {
            let n = quanta().clamp(1, MAX_QUANTUM).min(input.len() - pos);
            let chunk = &input[pos..pos + n];
            self.p
                .process(&[chunk, chunk], &mut [&mut l[..n], &mut r[..n]], n, rate);
            assert_eq!(l[..n], r[..n], "stereo outputs identical");
            out.extend_from_slice(&l[..n]);
            pos += n;
        }
        out
    }

    fn run_fixed(&mut self, input: &[f32], rate: u32, q: usize) -> Vec<f32> {
        self.run(input, rate, &mut || q)
    }
}

/// Lag in 0..=max_lag maximizing the cross-correlation of `a` with `b` (b lags a).
fn best_lag(a: &[f32], b: &[f32], from: usize, max_lag: usize) -> usize {
    let end = a.len().min(b.len()).saturating_sub(max_lag);
    (0..=max_lag)
        .max_by(|x, y| {
            let c = |lag: usize| -> f64 {
                (from..end)
                    .map(|t| f64::from(a[t]) * f64::from(b[t + lag]))
                    .sum()
            };
            c(*x).total_cmp(&c(*y))
        })
        .unwrap_or(0)
}

#[test]
fn denoise_block_const() {
    assert_eq!(DENOISE_BLOCK, nnnoiseless::DenoiseState::FRAME_SIZE);
}

#[test]
fn nn_delay_is_pinned() {
    let mut st = nnnoiseless::DenoiseState::new();
    let input: Vec<f32> = coloured(7, 200 * DENOISE_BLOCK, 0.3)
        .into_iter()
        .map(|x| x * NN_SCALE)
        .collect();
    let mut output = vec![0.0; input.len()];
    for (i, o) in input
        .chunks_exact(DENOISE_BLOCK)
        .zip(output.chunks_exact_mut(DENOISE_BLOCK))
    {
        st.process_frame(o, i);
    }
    assert_eq!(
        best_lag(&input, &output, 20 * DENOISE_BLOCK, 1440),
        NN_DELAY
    );
}

#[test]
fn denoise_latency_per_rate() {
    for rate in [48_000, 44_100, 96_000, 16_000] {
        let lat = latency_frames(rate);
        let tol = usize::from(rate != DENOISE_RATE);
        // Bypass: an impulse comes out exactly `lat` frames later.
        let mut r = rig();
        r.h.set_enabled(false);
        let n = rate as usize / 2;
        let mut input = vec![0.0; n];
        let at = rate as usize / 10;
        input[at] = 1.0;
        let out = r.run_fixed(&input, rate, 256);
        let peak = out
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.abs().total_cmp(&b.1.abs()))
            .map(|(i, _)| i)
            .unwrap();
        assert!(
            peak.abs_diff(at + lat) <= tol,
            "{rate}: bypass impulse at {} frames, expected {lat}",
            peak - at
        );
        // Enabled: the same delay (signal RNNoise keeps, located by correlation).
        let mut r = rig();
        let input = coloured(11, rate as usize * 2, 0.3);
        let out = r.run_fixed(&input, rate, 256);
        let lag = best_lag(&input, &out, rate as usize / 2, lat + 200);
        assert!(
            lag.abs_diff(lat) <= 2,
            "{rate}: enabled latency {lag}, expected {lat}"
        );
        eprintln!(
            "{rate} Hz: {lat} frames = {:.1} ms",
            lat as f64 * 1000.0 / f64::from(rate)
        );
        assert_eq!(r.stats.underruns.load(Ordering::Relaxed), 0);
    }
}

fn odd_quanta(cycles: usize) {
    const SIZES: [usize; 11] = [1, 7, 128, 333, 479, 480, 481, 1024, 2048, 4097, 8192];
    for rate in [48_000, 44_100, 96_000, 16_000, 192_000, 8_000] {
        let mut r = rig();
        r.h.set_enabled(false); // timing is the same; skips RNNoise to keep this fast
        let mut rng = Rng(rate.into());
        let (mut l, mut rr) = (vec![0.0; MAX_QUANTUM], vec![0.0; MAX_QUANTUM]);
        let input = vec![0.01f32; MAX_QUANTUM];
        for i in 0..cycles {
            let n = if i % 2 == 0 {
                SIZES[(rng.next_u64() % SIZES.len() as u64) as usize]
            } else {
                1 + (rng.next_u64() % MAX_QUANTUM as u64) as usize
            };
            r.p.process(
                &[&input[..n], &input[..n]],
                &mut [&mut l[..n], &mut rr[..n]],
                n,
                rate,
            );
            assert!(
                r.p.in_fifo.len() < r.p.in_fifo.capacity(),
                "{rate}: input FIFO bounded"
            );
            assert!(
                r.p.out_fifo.len() < r.p.out_fifo.capacity(),
                "{rate}: output FIFO bounded"
            );
        }
        assert_eq!(r.stats.underruns.load(Ordering::Relaxed), 0, "{rate}");
    }
}

#[test]
fn denoise_odd_quanta_no_underrun() {
    odd_quanta(2_000);
}

/// The spec's full count (dsp M10.7): `cargo test --release -- --ignored`.
#[test]
#[ignore = "slow: 10^6 cycles per rate"]
fn denoise_odd_quanta_no_underrun_full() {
    odd_quanta(1_000_000);
}

#[test]
fn denoise_bypass_bit_exact_after_delay() {
    let mut r = rig();
    r.h.set_enabled(false);
    let input = white(3, 48_000, 0.5);
    let mut q = Rng(5);
    let out = r.run(&input, 48_000, &mut || 1 + (q.next_u64() % 2000) as usize);
    let lat = latency_frames(48_000);
    assert_eq!(lat, 960);
    assert!(out[..lat].iter().all(|x| *x == 0.0));
    assert_eq!(out[lat..], input[..input.len() - lat], "bit-exact");
}

#[test]
fn denoise_toggle_click_free() {
    let rate = 48_000.0f32;
    let amp = 0.5;
    let f = 300.0;
    let input: Vec<f32> = (0..96_000)
        .map(|i| amp * (2.0 * std::f32::consts::PI * f * i as f32 / rate).sin())
        .collect();
    let max_slope = amp * 2.0 * std::f32::consts::PI * f / rate;
    let mut r = rig();
    let mut out = Vec::new();
    for (k, chunk) in input.chunks(2400).enumerate() {
        r.h.set_enabled(k % 2 == 0); // toggles every 50 ms
        out.extend(r.run_fixed(chunk, 48_000, 240));
    }
    let worst = out
        .windows(2)
        .skip(latency_frames(48_000) + DENOISE_BLOCK)
        .map(|w| (w[1] - w[0]).abs())
        .fold(0.0f32, f32::max);
    assert!(
        worst <= 1.5 * max_slope,
        "max step {worst:.5} vs sine slope {max_slope:.5}"
    );
}

/// RNNoise was trained on real recordings; on synthetic stationary noise its result depends
/// on the noise realization (1.6–59 dB measured across seeds, raw nnnoiseless alike). This
/// pins one realization (white, seed 9, starting at t=0) as a regression fixture. Even the
/// start offset changes the outcome (1.4–15.9 dB), so live tests can't assert attenuation.
#[test]
fn denoise_attenuates_noise() {
    let amp = 10f32.powf(-30.0 / 20.0) * 3f32.sqrt(); // uniform noise with −30 dBFS RMS
    let input = white(9, 5 * 48_000, amp);
    let mut r = rig();
    let out = r.run_fixed(&input, 48_000, 512);
    let tail = 48_000..out.len();
    let reduction = 20.0 * (rms(&input[tail.clone()]) / rms(&out[tail])).log10();
    eprintln!("white noise at −30 dBFS reduced by {reduction:.1} dB");
    assert!(reduction >= 10.0, "only {reduction:.1} dB");
}

#[test]
fn vad_gate_closes_on_noise() {
    let input = white(13, 2 * 48_000, 0.03);
    let mut r = rig();
    r.h.set_vad_gate(0.9);
    let out = r.run_fixed(&input, 48_000, 480);
    // Warm-up, hold (200 ms), one ramp block and the latency: well within 400 ms.
    assert!(out[19_200..].iter().all(|x| *x == 0.0), "gate closed");
}

#[test]
fn rate_change_resets_cleanly() {
    let mut r = rig();
    let input = coloured(17, 48_000, 0.3);
    for rate in [48_000, 44_100, 48_000] {
        let out = r.run_fixed(&input, rate, 333);
        assert_eq!(out.len(), input.len());
        assert!(out.iter().all(|x| x.is_finite()));
    }
    assert_eq!(r.stats.underruns.load(Ordering::Relaxed), 0);
    assert_eq!(r.stats.unsupported_rate.load(Ordering::Relaxed), 0);
}

#[test]
fn unsupported_rate_bypasses() {
    let mut r = rig();
    let input = white(19, 10_000, 0.5);
    let out = r.run_fixed(&input, 50_000, 256);
    assert_eq!(out, input, "downmix passthrough, no latency");
    assert_eq!(r.stats.unsupported_rate.load(Ordering::Relaxed), 50_000);
}

#[test]
fn handle_clamps_the_gate() {
    let r = rig();
    r.h.set_vad_gate(2.0);
    assert_eq!(f32::from_bits(r.h.vad_gate.load(Ordering::Relaxed)), 0.95);
    r.h.set_vad_gate(f32::NAN);
    assert_eq!(f32::from_bits(r.h.vad_gate.load(Ordering::Relaxed)), 0.0);
}

/// `cargo test -p estuary-dsp --release -- --ignored bench_cpu --nocapture`
#[test]
#[ignore = "benchmark: run in release"]
fn bench_cpu() {
    let mut r = rig();
    let input = white(23, 48_000 * 10, 0.1);
    let t = std::time::Instant::now();
    r.run_fixed(&input, 48_000, 480);
    let per_block = t.elapsed().as_secs_f64() * 1000.0 / (input.len() / DENOISE_BLOCK) as f64;
    eprintln!("denoise: {per_block:.3} ms per 480-frame block (budget 1 ms)");
    assert!(per_block < 1.0);
}

#[test]
#[ignore = "measurement"]
fn rate_table_construction() {
    let t = std::time::Instant::now();
    let paths: Vec<_> = RATE_TABLE
        .iter()
        .map(|r| RatePath::new(*r).unwrap())
        .collect();
    eprintln!(
        "{} rate paths built in {:.1} ms",
        paths.len(),
        t.elapsed().as_secs_f64() * 1000.0
    );
}
