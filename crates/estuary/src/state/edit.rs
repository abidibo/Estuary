//! Edit model (UI M5.2): every user action becomes an `Edit`; `apply_edit` is the only place
//! the desired state (`ConfigFile`) changes, and it returns the engine commands to send.
//! Pure: unit-tested without GTK. Matrix, noise-suppression and preset edits join in M9–M11.

use estuary_engine::setup::{AppRule, ConfigFile, DeviceKind, KnownDevice};
use estuary_engine::{AppKey, Command, DeviceKey, GraphSnapshot, RouteTarget};

use super::volume::set_volume_level;

#[derive(Debug, Clone)]
pub enum Edit {
    /// The user picked where an app plays (R1, R3).
    AssignApp {
        app: AppKey,
        display_name: String,
        target: RouteTarget,
    },
    /// The app was moved from outside Estuary; adopt it as the new rule (C7).
    ExternalRetarget {
        app: AppKey,
        target: RouteTarget,
    },
    ForgetAppRule {
        app: AppKey,
    },
    /// Not persisted (C3): WirePlumber remembers stream volumes.
    AppVolume {
        app: AppKey,
        level: f64,
        muted: bool,
    },
    /// Not persisted: device volume lives in WirePlumber.
    DeviceVolume {
        key: DeviceKey,
        level: f64,
        muted: bool,
    },
}

#[derive(Debug, Default)]
pub struct EditOutcome {
    pub commands: Vec<Command>,
    pub save: bool,
    /// Rows depending on the setup must re-render.
    pub structure_changed: bool,
}

pub fn apply_edit(cfg: &mut ConfigFile, snap: &GraphSnapshot, edit: Edit) -> EditOutcome {
    let mut out = EditOutcome::default();
    match edit {
        Edit::AssignApp {
            app,
            display_name,
            target,
        } => {
            upsert_rule(cfg, snap, app, display_name, target);
            setup_changed(cfg, &mut out);
        }
        Edit::ExternalRetarget { app, target } => {
            let existing = cfg.setup.app_rules.iter().find(|r| r.app == app);
            if existing.is_some_and(|r| r.target == target) {
                return out; // C7 anti-ping-pong: nothing to adopt
            }
            let display_name = snap
                .apps
                .iter()
                .find(|g| g.key == app)
                .map(|g| g.display_name.clone())
                .or_else(|| existing.map(|r| r.display_name.clone()))
                .unwrap_or_else(|| app.0.clone());
            upsert_rule(cfg, snap, app, display_name, target);
            setup_changed(cfg, &mut out);
        }
        Edit::ForgetAppRule { app } => {
            let before = cfg.setup.app_rules.len();
            cfg.setup.app_rules.retain(|r| r.app != app);
            if cfg.setup.app_rules.len() != before {
                setup_changed(cfg, &mut out);
            }
        }
        Edit::AppVolume { app, level, muted } => {
            let current = snap
                .apps
                .iter()
                .find(|g| g.key == app)
                .map(|g| g.volume.clone());
            if let Some(current) = current {
                let volume = set_volume_level(&current, level, muted);
                out.commands.push(Command::SetAppVolume { app, volume });
            }
        }
        Edit::DeviceVolume { key, level, muted } => {
            let current = snap
                .outputs
                .iter()
                .chain(&snap.inputs)
                .find(|d| d.key == key)
                .and_then(|d| d.volume.clone());
            if let Some(current) = current {
                let volume = set_volume_level(&current, level, muted);
                out.commands.push(Command::SetDeviceVolume { key, volume });
            }
        }
    }
    out
}

fn setup_changed(cfg: &ConfigFile, out: &mut EditOutcome) {
    out.commands.push(Command::SetSetup(cfg.setup.clone()));
    out.save = true;
    out.structure_changed = true;
}

fn upsert_rule(
    cfg: &mut ConfigFile,
    snap: &GraphSnapshot,
    app: AppKey,
    display_name: String,
    target: RouteTarget,
) {
    // C6 invariant: every device a rule references is in known_devices.
    if let RouteTarget::Device { key } = &target {
        remember_device(cfg, snap, key, DeviceKind::Output);
    }
    match cfg.setup.app_rules.iter_mut().find(|r| r.app == app) {
        Some(r) => {
            r.target = target;
            r.display_name = display_name;
        }
        None => cfg.setup.app_rules.push(AppRule {
            app,
            target,
            display_name,
        }),
    }
}

fn remember_device(cfg: &mut ConfigFile, snap: &GraphSnapshot, key: &DeviceKey, kind: DeviceKind) {
    if cfg.setup.known_devices.iter().any(|d| &d.key == key) {
        return;
    }
    let description = snap
        .outputs
        .iter()
        .chain(&snap.inputs)
        .find(|d| &d.key == key)
        .map(|d| d.description.clone())
        .unwrap_or_default();
    cfg.setup.known_devices.push(KnownDevice {
        key: key.clone(),
        kind,
        description,
        position: None,
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use estuary_engine::{AppGroup, DeviceInfo, StreamInfo, Volume};

    fn snap() -> GraphSnapshot {
        GraphSnapshot {
            apps: vec![AppGroup {
                key: AppKey("bin:spotify".into()),
                display_name: "Spotify".into(),
                icon_name: None,
                streams: vec![StreamInfo {
                    node_id: 1,
                    serial: 1001,
                    media_name: None,
                }],
                current_target: RouteTarget::Default,
                movable: true,
                volume: Volume {
                    channels: vec![0.5, 0.25],
                    muted: false,
                },
            }],
            outputs: vec![DeviceInfo {
                key: DeviceKey("hp".into()),
                description: "Headphones".into(),
                present: true,
                channels: 2,
                volume: Some(Volume::unity(2)),
            }],
            ..Default::default()
        }
    }

    fn spotify() -> AppKey {
        AppKey("bin:spotify".into())
    }

    fn hp() -> RouteTarget {
        RouteTarget::Device {
            key: DeviceKey("hp".into()),
        }
    }

    #[test]
    fn assign_creates_rule_and_remembers_device() {
        let mut cfg = ConfigFile::default();
        let out = apply_edit(
            &mut cfg,
            &snap(),
            Edit::AssignApp {
                app: spotify(),
                display_name: "Spotify".into(),
                target: hp(),
            },
        );
        assert!(out.save && out.structure_changed);
        assert!(matches!(&out.commands[..], [Command::SetSetup(s)] if s.app_rules.len() == 1));
        assert_eq!(cfg.setup.known_devices[0].description, "Headphones");
        // Re-assigning updates the same rule.
        apply_edit(
            &mut cfg,
            &snap(),
            Edit::AssignApp {
                app: spotify(),
                display_name: "Spotify".into(),
                target: RouteTarget::Default,
            },
        );
        assert_eq!(cfg.setup.app_rules.len(), 1);
        assert_eq!(cfg.setup.app_rules[0].target, RouteTarget::Default);
        assert_eq!(cfg.setup.known_devices.len(), 1, "known devices are kept");
    }

    #[test]
    fn external_retarget_is_adopted_once() {
        let mut cfg = ConfigFile::default();
        let out = apply_edit(
            &mut cfg,
            &snap(),
            Edit::ExternalRetarget {
                app: spotify(),
                target: hp(),
            },
        );
        assert_eq!(out.commands.len(), 1);
        assert_eq!(cfg.setup.app_rules[0].display_name, "Spotify");
        let out = apply_edit(
            &mut cfg,
            &snap(),
            Edit::ExternalRetarget {
                app: spotify(),
                target: hp(),
            },
        );
        assert!(
            out.commands.is_empty() && !out.save,
            "same target again: no-op (no ping-pong)"
        );
    }

    #[test]
    fn forget_rule() {
        let mut cfg = ConfigFile::default();
        apply_edit(
            &mut cfg,
            &snap(),
            Edit::AssignApp {
                app: spotify(),
                display_name: "Spotify".into(),
                target: hp(),
            },
        );
        let out = apply_edit(&mut cfg, &snap(), Edit::ForgetAppRule { app: spotify() });
        assert!(cfg.setup.app_rules.is_empty() && out.save);
        let out = apply_edit(&mut cfg, &snap(), Edit::ForgetAppRule { app: spotify() });
        assert!(out.commands.is_empty(), "forgetting twice is a no-op");
    }

    #[test]
    fn volumes_are_fast_paths_not_saved() {
        let mut cfg = ConfigFile::default();
        let out = apply_edit(
            &mut cfg,
            &snap(),
            Edit::AppVolume {
                app: spotify(),
                level: 1.0,
                muted: false,
            },
        );
        assert!(!out.save);
        match &out.commands[..] {
            [Command::SetAppVolume { volume, .. }] => {
                assert_eq!(volume.channels, vec![1.0, 0.5], "balance kept");
            }
            other => panic!("unexpected {other:?}"),
        }
        let out = apply_edit(
            &mut cfg,
            &snap(),
            Edit::DeviceVolume {
                key: DeviceKey("hp".into()),
                level: 0.5,
                muted: true,
            },
        );
        assert!(
            matches!(&out.commands[..], [Command::SetDeviceVolume { volume, .. }] if volume.muted)
        );
        let out = apply_edit(
            &mut cfg,
            &snap(),
            Edit::DeviceVolume {
                key: DeviceKey("gone".into()),
                level: 0.5,
                muted: false,
            },
        );
        assert!(out.commands.is_empty(), "absent device: nothing to send");
    }
}
