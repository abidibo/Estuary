//! Fixed delay line and dry/wet crossfade (dsp M10.1). Allocate only in `new`.

/// Delays a signal by a fixed number of frames (0 allowed).
#[derive(Debug)]
pub struct DelayLine {
    buf: Box<[f32]>,
    pos: usize,
}

impl DelayLine {
    pub fn new(d: usize) -> Self {
        Self {
            buf: vec![0.0; d].into_boxed_slice(),
            pos: 0,
        }
    }

    pub fn process(&mut self, io: &mut [f32]) {
        if self.buf.is_empty() {
            return;
        }
        for x in io {
            std::mem::swap(&mut self.buf[self.pos], x);
            self.pos = (self.pos + 1) % self.buf.len();
        }
    }

    pub fn reset(&mut self) {
        self.buf.fill(0.0);
        self.pos = 0;
    }
}

/// Linear equal-gain fade between time-aligned (correlated) dry and wet signals.
#[derive(Debug)]
pub struct Crossfade {
    /// 0 = fully dry, `len` = fully wet.
    pos: usize,
    len: usize,
    to_wet: bool,
}

impl Crossfade {
    pub fn new(len: usize, wet: bool) -> Self {
        let len = len.max(1);
        Self {
            pos: if wet { len } else { 0 },
            len,
            to_wet: wet,
        }
    }

    /// Fades toward `wet` from wherever the current fade is (no jump).
    pub fn set_target(&mut self, wet: bool) {
        self.to_wet = wet;
    }

    pub fn mix(&mut self, dry: &[f32], wet: &[f32], out: &mut [f32]) {
        #[allow(clippy::cast_precision_loss)] // fade lengths are small
        let len = self.len as f32;
        for ((o, d), w) in out.iter_mut().zip(dry).zip(wet) {
            if self.to_wet && self.pos < self.len {
                self.pos += 1;
            } else if !self.to_wet && self.pos > 0 {
                self.pos -= 1;
            }
            #[allow(clippy::cast_precision_loss)]
            let g = self.pos as f32 / len;
            *o = d + (w - d) * g;
        }
    }

    /// Jumps to fully dry or wet (after a reset, when there is no continuity to keep).
    pub fn snap(&mut self, wet: bool) {
        self.to_wet = wet;
        self.pos = if wet { self.len } else { 0 };
    }

    pub fn is_fully_dry(&self) -> bool {
        !self.to_wet && self.pos == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delay_is_exact() {
        let mut d = DelayLine::new(3);
        let mut a = [1.0, 2.0, 3.0, 4.0, 5.0];
        d.process(&mut a);
        assert_eq!(a, [0.0, 0.0, 0.0, 1.0, 2.0]);
        let mut b = [6.0, 7.0];
        d.process(&mut b);
        assert_eq!(b, [3.0, 4.0], "continues across calls");
        d.reset();
        let mut c = [8.0; 4];
        d.process(&mut c);
        assert_eq!(c, [0.0, 0.0, 0.0, 8.0]);
        let mut zero = DelayLine::new(0);
        let mut x = [1.0, 2.0];
        zero.process(&mut x);
        assert_eq!(x, [1.0, 2.0]);
    }

    #[test]
    fn crossfade_is_continuous_and_reversible_mid_fade() {
        let mut x = Crossfade::new(4, false);
        let dry = [0.0; 8];
        let wet = [1.0; 8];
        let mut out = [0.0; 2];
        x.set_target(true);
        x.mix(&dry[..2], &wet[..2], &mut out);
        assert_eq!(out, [0.25, 0.5]);
        x.set_target(false); // reverse halfway: no jump
        x.mix(&dry[..2], &wet[..2], &mut out);
        assert_eq!(out, [0.25, 0.0]);
        assert!(x.is_fully_dry());
        x.set_target(true);
        let mut out = [0.0; 6];
        x.mix(&dry[..6], &wet[..6], &mut out);
        assert_eq!(out, [0.25, 0.5, 0.75, 1.0, 1.0, 1.0]);
        assert!(!x.is_fully_dry());
    }
}
