//! Flush subnormal floats to zero on the RT thread (dsp M7.4). RNN recursions can produce
//! subnormals, which are 10–100× slower on x86. Idempotent; about one instruction per cycle.
#![allow(unsafe_code)]

#[inline(always)]
pub(crate) fn flush_denormals() {
    #[cfg(target_arch = "x86_64")]
    // SAFETY: reads and writes MXCSR of the current thread only, setting FTZ (bit 15) and DAZ
    // (bit 6). The thread is Estuary's own RT data thread, so nothing else depends on it.
    unsafe {
        let mut csr: u32 = 0;
        core::arch::asm!("stmxcsr [{}]", in(reg) &raw mut csr, options(nostack));
        csr |= 0x8040;
        core::arch::asm!("ldmxcsr [{}]", in(reg) &raw const csr, options(nostack, readonly));
    }
    #[cfg(target_arch = "aarch64")]
    // SAFETY: sets FZ (bit 24) in FPCR of the current thread only.
    unsafe {
        let mut fpcr: u64;
        core::arch::asm!("mrs {}, fpcr", out(reg) fpcr, options(nomem, nostack));
        fpcr |= 1 << 24;
        core::arch::asm!("msr fpcr, {}", in(reg) fpcr, options(nomem, nostack));
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn subnormals_flush_to_zero_after_call() {
        super::flush_denormals();
        let tiny = std::hint::black_box(f32::MIN_POSITIVE);
        let half = std::hint::black_box(tiny / 4.0);
        if cfg!(any(target_arch = "x86_64", target_arch = "aarch64")) {
            assert_eq!(half, 0.0, "subnormal result flushed to zero");
        }
    }
}
