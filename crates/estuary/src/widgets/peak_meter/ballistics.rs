//! Meter ballistics (UI M4.1, master C9): instant attack, 20 dB/s release, 1.5 s peak hold.

pub const FLOOR_DB: f32 = -60.0;
pub const RELEASE_DB_PER_S: f32 = 20.0;
pub const HOLD_S: f32 = 1.5;
/// No new reading for this long → decay to silence.
pub const STALE_S: f32 = 0.5;
pub const WARN_DB: f32 = -12.0;
pub const CLIP_DB: f32 = -3.0;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ChannelBallistics {
    pub level_db: f32,
    pub hold_db: f32,
    pub hold_left_s: f32,
}

impl Default for ChannelBallistics {
    fn default() -> Self {
        Self {
            level_db: FLOOR_DB,
            hold_db: FLOOR_DB,
            hold_left_s: 0.0,
        }
    }
}

pub fn lin_to_db(x: f32) -> f32 {
    if x <= 1e-6 || !x.is_finite() {
        FLOOR_DB
    } else {
        (20.0 * x.log10()).max(FLOOR_DB)
    }
}

/// Advance one frame. `input_db` is the newest peak (or `FLOOR_DB` when stale);
/// `dt` is seconds since the last frame, clamped to 0..=0.25.
pub fn step(b: &mut ChannelBallistics, input_db: f32, dt: f32) {
    let dt = dt.clamp(0.0, 0.25);
    b.level_db = if input_db >= b.level_db {
        input_db
    } else {
        (b.level_db - RELEASE_DB_PER_S * dt).max(input_db)
    };
    if input_db >= b.hold_db {
        b.hold_db = input_db;
        b.hold_left_s = HOLD_S;
    } else {
        b.hold_left_s -= dt;
        if b.hold_left_s <= 0.0 {
            b.hold_db = (b.hold_db - RELEASE_DB_PER_S * dt).max(b.level_db);
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Zone {
    Normal,
    Warn,
    Clip,
}

pub fn zone(db: f32, raw_peak: f32) -> Zone {
    if db > CLIP_DB || raw_peak >= 1.0 {
        Zone::Clip
    } else if db >= WARN_DB {
        Zone::Warn
    } else {
        Zone::Normal
    }
}

/// dB → 0..=1 bar fraction, with more resolution near 0 dBFS.
pub fn fraction(db: f32) -> f32 {
    ((db - FLOOR_DB) / -FLOOR_DB).clamp(0.0, 1.0).powf(1.5)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn instant_attack_then_release() {
        let mut b = ChannelBallistics::default();
        step(&mut b, -6.0, 0.016);
        assert_eq!(b.level_db, -6.0, "attack is instant");
        step(&mut b, FLOOR_DB, 0.1);
        assert!(
            (b.level_db - (-8.0)).abs() < 1e-4,
            "20 dB/s release: -6 - 2 = -8"
        );
    }

    #[test]
    fn hold_then_fall() {
        let mut b = ChannelBallistics::default();
        step(&mut b, -3.0, 0.016);
        for _ in 0..14 {
            step(&mut b, FLOOR_DB, 0.1); // 1.4 s: still holding
        }
        assert_eq!(b.hold_db, -3.0);
        step(&mut b, FLOOR_DB, 0.1);
        step(&mut b, FLOOR_DB, 0.1);
        assert!(b.hold_db < -3.0, "hold released after 1.5 s");
        assert!(b.hold_db >= b.level_db, "hold never below the level");
    }

    #[test]
    fn dt_is_clamped() {
        let mut b = ChannelBallistics::default();
        step(&mut b, 0.0, 0.0);
        step(&mut b, FLOOR_DB, 10.0); // a frozen frame clock must not jump to the floor
        assert!((b.level_db - (-5.0)).abs() < 1e-4);
    }

    #[test]
    fn zones_and_fraction() {
        assert_eq!(zone(-20.0, 0.1), Zone::Normal);
        assert_eq!(zone(-10.0, 0.3), Zone::Warn);
        assert_eq!(zone(-1.0, 0.9), Zone::Clip);
        assert_eq!(zone(-20.0, 1.0), Zone::Clip, "a full-scale sample clips");
        assert_eq!(fraction(FLOOR_DB), 0.0);
        assert_eq!(fraction(0.0), 1.0);
        assert!(fraction(-12.0) > 0.5 && fraction(-12.0) < 0.8);
        assert_eq!(lin_to_db(0.0), FLOOR_DB);
        assert!((lin_to_db(0.5) - (-6.0206)).abs() < 1e-3);
    }
}
