//! Volume display math (UI M5.1, the part M3 needs). Sliders use a cubic mapping like
//! pavucontrol: slider position `s` ∈ [0, 1] ↔ linear volume `s³`.

/// Linear PipeWire volume → perceptual slider position.
pub fn linear_to_slider(linear: f32) -> f32 {
    linear.max(0.0).cbrt()
}

/// Perceptual percentage shown to the user (100 % = unity gain).
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // finite, clamped to 0..=1000
pub fn percent(linear: f32) -> u32 {
    let p = (linear_to_slider(linear) * 100.0).round();
    if p.is_finite() {
        p.clamp(0.0, 1000.0) as u32
    } else {
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cubic_percent() {
        assert_eq!(percent(1.0), 100);
        assert_eq!(percent(0.0), 0);
        assert_eq!(percent(0.125), 50);
        assert_eq!(percent(0.386_794_54), 73, "the dev machine's output volume");
        assert_eq!(percent(-1.0), 0);
        assert_eq!(percent(f32::NAN), 0);
    }
}
