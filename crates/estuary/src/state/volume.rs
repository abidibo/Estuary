//! Volume math (UI M5.1). Sliders use a cubic mapping like pavucontrol: slider position
//! `s` ∈ [0, 1] ↔ linear PipeWire volume `s³`. Volumes in the engine stay linear (C4).

use estuary_engine::Volume;

pub fn slider_to_linear(s: f64) -> f32 {
    #[allow(clippy::cast_possible_truncation)] // 0..=1
    let v = s.clamp(0.0, 1.0).powi(3) as f32;
    v
}

pub fn linear_to_slider(v: f32) -> f64 {
    f64::from(v.max(0.0)).cbrt().min(1.0)
}

/// Slider level of a volume: its loudest channel.
pub fn volume_level(vol: &Volume) -> f64 {
    linear_to_slider(vol.channels.iter().copied().fold(0.0, f32::max))
}

/// Apply a slider level, keeping the channel balance: scale so the loudest channel == level.
pub fn set_volume_level(vol: &Volume, s: f64, muted: bool) -> Volume {
    let target = slider_to_linear(s);
    let max = vol.channels.iter().copied().fold(0.0, f32::max);
    let channels = if vol.channels.is_empty() {
        vec![target; 2]
    } else if max <= 1e-6 {
        vec![target; vol.channels.len()]
    } else {
        vol.channels.iter().map(|c| c / max * target).collect()
    };
    Volume { channels, muted }
}

/// Perceptual percentage shown to the user (100 % = unity gain).
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // finite, clamped to 0..=1000
pub fn percent(linear: f32) -> u32 {
    let p = (f64::from(linear.max(0.0)).cbrt() * 100.0).round();
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

    #[test]
    fn slider_round_trip() {
        for s in [0.0, 0.1, 0.5, 0.73, 1.0] {
            assert!((linear_to_slider(slider_to_linear(s)) - s).abs() < 1e-6);
        }
        assert_eq!(slider_to_linear(2.0), 1.0, "no over-amplification in v1");
    }

    #[test]
    fn level_keeps_balance() {
        let v = Volume {
            channels: vec![0.5, 0.25],
            muted: false,
        };
        let w = set_volume_level(&v, 1.0, true);
        assert_eq!(w.channels, vec![1.0, 0.5]);
        assert!(w.muted);
        assert!((volume_level(&w) - 1.0).abs() < 1e-6);
        let silent = Volume {
            channels: vec![0.0, 0.0],
            muted: false,
        };
        assert_eq!(
            set_volume_level(&silent, 0.5, false).channels,
            vec![0.125, 0.125]
        );
    }
}
