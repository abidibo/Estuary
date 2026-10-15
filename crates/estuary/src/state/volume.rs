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

/// Cell gain range (C6).
pub const CELL_DB_MIN: f32 = -60.0;
pub const CELL_DB_MAX: f32 = 6.0;

/// NaN → 0 dB, clamped to the cell range, rounded to 0.1 dB.
pub fn clamp_db(db: f32) -> f32 {
    if db.is_nan() {
        return 0.0;
    }
    let r = (db.clamp(CELL_DB_MIN, CELL_DB_MAX) * 10.0).round() / 10.0;
    if r == 0.0 { 0.0 } else { r } // no "−0 dB"
}

/// "0 dB", "−18 dB", "+3.5 dB": U+2212 minus, the locale's decimal separator.
pub fn format_db(db: f32) -> String {
    // Translators: the decimal separator in gain values such as "−3.5 dB"; "," in many languages
    format_db_with(db, &crate::i18n::pi18n("decimal separator", "."))
}

fn format_db_with(db: f32, separator: &str) -> String {
    let db = clamp_db(db);
    let sign = if db > 0.0 {
        "+"
    } else if db < 0.0 {
        "\u{2212}"
    } else {
        ""
    };
    let abs = db.abs();
    let number = if (abs - abs.round()).abs() < 0.05 {
        format!("{abs:.0}")
    } else {
        format!("{abs:.1}").replace('.', separator)
    };
    format!("{sign}{number} dB")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn db_clamp_and_format() {
        assert_eq!(clamp_db(f32::NAN), 0.0);
        assert_eq!(clamp_db(-100.0), -60.0);
        assert_eq!(clamp_db(12.0), 6.0);
        assert_eq!(clamp_db(-3.04), -3.0);
        assert_eq!(clamp_db(-0.04), 0.0);
        assert_eq!(format_db_with(0.0, "."), "0 dB");
        assert_eq!(format_db_with(-0.01, "."), "0 dB");
        assert_eq!(format_db_with(-18.0, "."), "\u{2212}18 dB");
        assert_eq!(format_db_with(3.5, "."), "+3.5 dB");
        assert_eq!(format_db_with(-3.5, ","), "\u{2212}3,5 dB");
        assert_eq!(format_db_with(f32::NAN, "."), "0 dB");
    }

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
