//! Fixed-capacity single-thread ring buffer (dsp M10.1). Allocates only in `with_capacity`.

#[derive(Debug)]
pub struct Fifo {
    buf: Box<[f32]>,
    head: usize,
    len: usize,
}

impl Fifo {
    pub fn with_capacity(cap: usize) -> Self {
        Self {
            buf: vec![0.0; cap.max(1)].into_boxed_slice(),
            head: 0,
            len: 0,
        }
    }

    pub fn len(&self) -> usize {
        self.len
    }

    #[cfg(test)]
    pub fn capacity(&self) -> usize {
        self.buf.len()
    }

    pub fn free(&self) -> usize {
        self.buf.len() - self.len
    }

    fn tail(&self) -> usize {
        (self.head + self.len) % self.buf.len()
    }

    /// Returns the frames pushed (`src.len()` unless full).
    pub fn push(&mut self, src: &[f32]) -> usize {
        let n = src.len().min(self.free());
        let tail = self.tail();
        let first = n.min(self.buf.len() - tail);
        self.buf[tail..tail + first].copy_from_slice(&src[..first]);
        self.buf[..n - first].copy_from_slice(&src[first..n]);
        self.len += n;
        n
    }

    pub fn push_zeros(&mut self, n: usize) -> usize {
        let n = n.min(self.free());
        let tail = self.tail();
        let first = n.min(self.buf.len() - tail);
        self.buf[tail..tail + first].fill(0.0);
        self.buf[..n - first].fill(0.0);
        self.len += n;
        n
    }

    /// Returns the frames popped.
    pub fn pop_into(&mut self, dst: &mut [f32]) -> usize {
        let n = dst.len().min(self.len);
        let first = n.min(self.buf.len() - self.head);
        dst[..first].copy_from_slice(&self.buf[self.head..self.head + first]);
        dst[first..n].copy_from_slice(&self.buf[..n - first]);
        self.discard(n);
        n
    }

    /// The oldest `n` frames as one slice; copied into `tmp` only when they wrap.
    pub fn peek_contiguous<'a>(&'a self, n: usize, tmp: &'a mut [f32]) -> &'a [f32] {
        let n = n.min(self.len);
        if self.head + n <= self.buf.len() {
            return &self.buf[self.head..self.head + n];
        }
        let first = self.buf.len() - self.head;
        tmp[..first].copy_from_slice(&self.buf[self.head..]);
        tmp[first..n].copy_from_slice(&self.buf[..n - first]);
        &tmp[..n]
    }

    pub fn discard(&mut self, n: usize) {
        let n = n.min(self.len);
        self.head = (self.head + n) % self.buf.len();
        self.len -= n;
    }

    pub fn clear(&mut self) {
        self.head = 0;
        self.len = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wraps_around() {
        let mut f = Fifo::with_capacity(5);
        assert_eq!(f.push(&[1.0, 2.0, 3.0, 4.0]), 4);
        let mut out = [0.0; 3];
        assert_eq!(f.pop_into(&mut out), 3);
        assert_eq!(out, [1.0, 2.0, 3.0]);
        assert_eq!(f.push(&[5.0, 6.0, 7.0]), 3, "wraps past the end");
        let mut tmp = [0.0; 5];
        assert_eq!(f.peek_contiguous(4, &mut tmp), &[4.0, 5.0, 6.0, 7.0]);
        let mut out = [0.0; 4];
        assert_eq!(f.pop_into(&mut out), 4);
        assert_eq!(out, [4.0, 5.0, 6.0, 7.0]);
        assert_eq!(f.len(), 0);
    }

    #[test]
    fn full_and_empty() {
        let mut f = Fifo::with_capacity(3);
        assert_eq!(f.push(&[1.0, 2.0, 3.0, 4.0]), 3, "only what fits");
        assert_eq!(f.free(), 0);
        assert_eq!(f.push_zeros(2), 0);
        let mut out = [9.0; 5];
        assert_eq!(f.pop_into(&mut out), 3);
        assert_eq!(&out[..3], &[1.0, 2.0, 3.0]);
        assert_eq!(f.pop_into(&mut out), 0, "empty");
        f.push_zeros(2);
        f.discard(1);
        assert_eq!(f.len(), 1);
        f.clear();
        assert_eq!((f.len(), f.free()), (0, 3));
    }

    #[test]
    fn peek_without_wrap_borrows_in_place() {
        let mut f = Fifo::with_capacity(8);
        f.push(&[1.0, 2.0, 3.0]);
        let mut tmp = [0.0; 8];
        assert_eq!(f.peek_contiguous(2, &mut tmp), &[1.0, 2.0]);
        assert_eq!(tmp, [0.0; 8], "no copy needed");
    }
}
