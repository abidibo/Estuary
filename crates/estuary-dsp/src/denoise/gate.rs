//! Voice-activity gate (dsp M10.1): mutes the denoised signal when RNNoise hears no voice.

use crate::consts::DENOISE_BLOCK;

#[derive(Debug)]
pub struct VadGate {
    gain: f32,
    hold_blocks_left: u32,
}

impl Default for VadGate {
    fn default() -> Self {
        Self {
            gain: 1.0,
            hold_blocks_left: 0,
        }
    }
}

impl VadGate {
    /// 200 ms at 10 ms per block: word tails aren't chopped.
    pub const HOLD_BLOCKS: u32 = 20;

    /// Gate 0 is always open. Voice (vad ≥ gate) restarts the hold; the gain then ramps
    /// linearly to its target across the block.
    pub fn apply(&mut self, vad: f32, gate: f32, block: &mut [f32; DENOISE_BLOCK]) {
        let target = if gate <= 0.0 || vad >= gate {
            self.hold_blocks_left = Self::HOLD_BLOCKS;
            1.0
        } else if self.hold_blocks_left > 0 {
            self.hold_blocks_left -= 1;
            1.0
        } else {
            0.0
        };
        let start = self.gain;
        if start != target {
            for (i, x) in block.iter_mut().enumerate() {
                // From the position, not accumulated steps: lands exactly on the target.
                #[allow(clippy::cast_precision_loss)]
                let t = (i + 1) as f32 / DENOISE_BLOCK as f32;
                *x *= start + (target - start) * t;
            }
        } else if target == 0.0 {
            block.fill(0.0);
        }
        self.gain = target;
    }

    pub fn reset(&mut self) {
        *self = Self::default();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_gate_passes_everything() {
        let mut g = VadGate::default();
        let mut b = [1.0; DENOISE_BLOCK];
        g.apply(0.0, 0.0, &mut b);
        assert!(b.iter().all(|x| *x == 1.0));
    }

    #[test]
    fn holds_then_ramps_closed() {
        let mut g = VadGate::default();
        let mut b = [1.0; DENOISE_BLOCK];
        g.apply(0.9, 0.5, &mut b); // voice: hold armed
        for _ in 0..VadGate::HOLD_BLOCKS {
            let mut b = [1.0; DENOISE_BLOCK];
            g.apply(0.1, 0.5, &mut b);
            assert!(b.iter().all(|x| *x == 1.0), "held open");
        }
        let mut b = [1.0; DENOISE_BLOCK];
        g.apply(0.1, 0.5, &mut b);
        assert!(b[0] < 1.0 && b[0] > 0.99, "ramp starts near 1");
        assert!(b[DENOISE_BLOCK - 1].abs() < 1e-6, "ramp ends at 0");
        let mut b = [1.0; DENOISE_BLOCK];
        g.apply(0.1, 0.5, &mut b);
        assert!(b.iter().all(|x| *x == 0.0), "closed");
        let mut b = [1.0; DENOISE_BLOCK];
        g.apply(0.6, 0.5, &mut b);
        assert!(
            b[0] < 0.01 && (b[DENOISE_BLOCK - 1] - 1.0).abs() < 1e-6,
            "reopens with a ramp"
        );
    }
}
