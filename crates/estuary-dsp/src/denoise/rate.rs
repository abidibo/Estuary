//! Resampling around the denoiser (dsp M10.2). Every pair is built off the RT thread (rubato
//! allocates FFT plans at construction); processing reuses fixed buffers and never allocates.

use rubato::audioadapter_buffers::direct::SequentialSlice;
use rubato::{Fft, FixedSync, Resampler, ResamplerConstructionError};

use crate::consts::{DENOISE_BLOCK, DENOISE_RATE};

/// Graph rates with a preconstructed path; anything else bypasses (F11a).
pub const RATE_TABLE: [u32; 11] = [
    8_000, 11_025, 16_000, 22_050, 24_000, 32_000, 44_100, 88_200, 96_000, 176_400, 192_000,
];

pub struct RatePath {
    pub rate: u32,
    /// rate → 48 kHz, always yields exactly one 480-frame block.
    down: Fft<f32>,
    /// 48 kHz → rate, always consumes exactly one 480-frame block.
    up: Fft<f32>,
    up_out: Box<[f32]>,
}

impl std::fmt::Debug for RatePath {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RatePath")
            .field("rate", &self.rate)
            .finish_non_exhaustive()
    }
}

impl RatePath {
    pub fn new(rate: u32) -> Result<Self, ResamplerConstructionError> {
        let (r, d) = (rate as usize, DENOISE_RATE as usize);
        let down = Fft::new(r, d, DENOISE_BLOCK, 1, FixedSync::Output)?;
        let up = Fft::new(d, r, DENOISE_BLOCK, 1, FixedSync::Input)?;
        let up_out = vec![0.0; up.output_frames_max()].into_boxed_slice();
        Ok(Self {
            rate,
            down,
            up,
            up_out,
        })
    }

    /// Frames at `rate` the next `downsample` consumes.
    pub fn input_needed(&self) -> usize {
        self.down.input_frames_next()
    }

    /// Consumes `input_needed()` frames of `src`, writes one 48 kHz block. `false` on error.
    pub fn downsample(&mut self, src: &[f32], dst: &mut [f32; DENOISE_BLOCK]) -> bool {
        let need = self.down.input_frames_next();
        let (Ok(input), Ok(mut output)) = (
            SequentialSlice::new(&src[..need.min(src.len())], 1, need),
            SequentialSlice::new_mut(&mut dst[..], 1, DENOISE_BLOCK),
        ) else {
            return false;
        };
        match self.down.process_into_buffer(&input, &mut output, None) {
            Ok((consumed, produced)) => {
                debug_assert_eq!((consumed, produced), (need, DENOISE_BLOCK));
                true
            }
            Err(_) => false,
        }
    }

    /// Consumes one 48 kHz block; the frames produced at `rate` (length varies). `None` on error.
    pub fn upsample(&mut self, src: &[f32; DENOISE_BLOCK]) -> Option<&[f32]> {
        let produce = self.up.output_frames_next();
        let input = SequentialSlice::new(&src[..], 1, DENOISE_BLOCK).ok()?;
        let mut output = SequentialSlice::new_mut(&mut self.up_out[..produce], 1, produce).ok()?;
        let (consumed, produced) = self
            .up
            .process_into_buffer(&input, &mut output, None)
            .ok()?;
        debug_assert_eq!((consumed, produced), (DENOISE_BLOCK, produce));
        Some(&self.up_out[..produced])
    }

    /// Output zeros to prime with so the output FIFO never runs dry.
    pub fn priming(&self) -> usize {
        self.down.input_frames_max() + self.up.output_frames_max()
    }

    /// Both resamplers' delay, in frames at `rate`.
    pub fn delay_frames_at_rate(&self) -> f64 {
        let down_at_48k = self.down.output_delay() as f64;
        down_at_48k * f64::from(self.rate) / f64::from(DENOISE_RATE) + self.up.output_delay() as f64
    }

    pub fn reset(&mut self) {
        self.down.reset();
        self.up.reset();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_table_rate_builds_and_round_trips_block_sizes() {
        for rate in RATE_TABLE {
            let mut p = RatePath::new(rate).unwrap();
            let src = vec![0.1f32; p.priming() + 8192];
            let mut blk = [0.0; DENOISE_BLOCK];
            let mut consumed = 0;
            let mut produced = 0;
            for _ in 0..50 {
                let need = p.input_needed();
                assert!(p.downsample(&src[..need], &mut blk), "{rate}");
                consumed += need;
                produced += p.upsample(&blk).unwrap().len();
            }
            // 50 blocks of 10 ms each way: about half a second at `rate`, both sides.
            let expected = 50 * rate as usize / 100;
            assert!(
                consumed.abs_diff(expected) <= p.input_needed(),
                "{rate}: in {consumed}"
            );
            assert!(
                produced.abs_diff(expected) <= p.priming(),
                "{rate}: out {produced}"
            );
        }
    }
}
