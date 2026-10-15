//! Edit model (UI M5.2): every user action becomes an `Edit`; `apply_edit` is the only place
//! the desired state (`ConfigFile`) changes, and it returns the engine commands to send.
//! Pure: unit-tested without GTK. Noise-suppression and preset edits join in M10–M11.

use estuary_engine::setup::{
    AppRule, CellDef, ChannelDef, ConfigFile, DeviceKind, KnownDevice, VirtualMicDef,
};
use estuary_engine::{
    AppKey, CellKey, ChannelId, Command, DeviceKey, GraphSnapshot, MatrixDest, MatrixSource,
    RouteTarget, VirtualMicId, Volume,
};

use super::volume::{clamp_db, set_volume_level};
use crate::i18n::{i18n, ni18n_f};

/// Longest channel or virtual mic name (UI M9.6).
pub const MAX_NAME_CHARS: usize = 64;

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
    /// Fast path while dragging; saved on `commit`.
    ChannelVolume {
        id: ChannelId,
        level: f64,
        muted: bool,
        commit: bool,
    },
    MicVolume {
        id: VirtualMicId,
        level: f64,
        muted: bool,
        commit: bool,
    },
    AddChannel {
        name: String,
    },
    RenameChannel {
        id: ChannelId,
        name: String,
    },
    /// Rules targeting it fall back to the default output (F6).
    DeleteChannel {
        id: ChannelId,
    },
    AddMic {
        name: String,
    },
    RenameMic {
        id: VirtualMicId,
        name: String,
    },
    DeleteMic {
        id: VirtualMicId,
    },
    /// Absent: create it on at 0 dB. Present: flip `enabled`.
    ToggleCell {
        key: CellKey,
    },
    /// `commit = false` while dragging (engine fast path only).
    SetCellGain {
        key: CellKey,
        gain_db: f32,
        commit: bool,
    },
    RemoveCell {
        key: CellKey,
    },
    /// Only while the device is absent (F10).
    ForgetDevice {
        key: DeviceKey,
    },
}

#[derive(Debug, Default)]
pub struct EditOutcome {
    pub commands: Vec<Command>,
    pub save: bool,
    /// Rows depending on the setup must re-render.
    pub structure_changed: bool,
    /// Short notices for the toast overlay (F6).
    pub toasts: Vec<String>,
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
        Edit::ChannelVolume {
            id,
            level,
            muted,
            commit,
        } => {
            if let Some(c) = cfg.setup.channels.iter_mut().find(|c| c.id == id) {
                c.volume = set_volume_level(&c.volume, level, muted);
                out.commands.push(Command::SetChannelVolume {
                    id,
                    volume: c.volume.clone(),
                });
                out.save = commit;
            }
        }
        Edit::MicVolume {
            id,
            level,
            muted,
            commit,
        } => {
            if let Some(m) = cfg.setup.virtual_mics.iter_mut().find(|m| m.id == id) {
                m.volume = set_volume_level(&m.volume, level, muted);
                out.commands.push(Command::SetVirtualMicVolume {
                    id,
                    volume: m.volume.clone(),
                });
                out.save = commit;
            }
        }
        Edit::AddChannel { name } => {
            let taken: Vec<&str> = cfg.setup.channels.iter().map(|c| c.name.as_str()).collect();
            let name = clean_name(&name).unwrap_or_else(|| default_name(&i18n("Channel"), &taken));
            cfg.setup.channels.push(ChannelDef {
                id: ChannelId::new_random(),
                name,
                volume: Volume::unity(2),
                position: None,
            });
            setup_changed(cfg, &mut out);
        }
        Edit::AddMic { name } => {
            let taken: Vec<&str> = cfg
                .setup
                .virtual_mics
                .iter()
                .map(|m| m.name.as_str())
                .collect();
            let name =
                clean_name(&name).unwrap_or_else(|| default_name(&i18n("Virtual mic"), &taken));
            cfg.setup.virtual_mics.push(VirtualMicDef {
                id: VirtualMicId::new_random(),
                name,
                volume: Volume::unity(2),
                position: None,
            });
            setup_changed(cfg, &mut out);
        }
        Edit::RenameChannel { id, name } => {
            if let (Some(c), Some(name)) = (
                cfg.setup.channels.iter_mut().find(|c| c.id == id),
                clean_name(&name),
            ) && c.name != name
            {
                c.name = name;
                setup_changed(cfg, &mut out);
            }
        }
        Edit::RenameMic { id, name } => {
            if let (Some(m), Some(name)) = (
                cfg.setup.virtual_mics.iter_mut().find(|m| m.id == id),
                clean_name(&name),
            ) && m.name != name
            {
                m.name = name;
                setup_changed(cfg, &mut out);
            }
        }
        Edit::DeleteChannel { id } => {
            let before = cfg.setup.channels.len();
            cfg.setup.channels.retain(|c| c.id != id);
            if cfg.setup.channels.len() != before {
                cfg.setup
                    .cells
                    .retain(|c| c.source != MatrixSource::Channel { id });
                let moved = retarget_rules(cfg, &RouteTarget::Channel { id });
                moved_toast(moved, &mut out);
                setup_changed(cfg, &mut out);
            }
        }
        Edit::DeleteMic { id } => {
            let before = cfg.setup.virtual_mics.len();
            cfg.setup.virtual_mics.retain(|m| m.id != id);
            if cfg.setup.virtual_mics.len() != before {
                cfg.setup
                    .cells
                    .retain(|c| c.dest != MatrixDest::VirtualMic { id });
                setup_changed(cfg, &mut out);
            }
        }
        Edit::ToggleCell { key } => {
            if let Some(c) = cfg.setup.cells.iter_mut().find(|c| c.key() == key) {
                c.enabled = !c.enabled;
            } else {
                match &key.source {
                    MatrixSource::Input { key } | MatrixSource::Denoised { key } => {
                        remember_device(cfg, snap, key, DeviceKind::Input);
                    }
                    MatrixSource::Channel { .. } => {}
                }
                if let MatrixDest::Output { key } = &key.dest {
                    remember_device(cfg, snap, key, DeviceKind::Output);
                }
                cfg.setup.cells.push(CellDef {
                    source: key.source,
                    dest: key.dest,
                    gain_db: 0.0,
                    enabled: true,
                });
            }
            setup_changed(cfg, &mut out);
        }
        Edit::SetCellGain {
            key,
            gain_db,
            commit,
        } => {
            if let Some(c) = cfg.setup.cells.iter_mut().find(|c| c.key() == key) {
                c.gain_db = clamp_db(gain_db);
                out.commands.push(Command::SetCellGain {
                    cell: key,
                    gain_db: c.gain_db,
                });
                out.save = commit;
            }
        }
        Edit::RemoveCell { key } => {
            let before = cfg.setup.cells.len();
            cfg.setup.cells.retain(|c| c.key() != key);
            if cfg.setup.cells.len() != before {
                setup_changed(cfg, &mut out);
            }
        }
        Edit::ForgetDevice { key } => {
            let present = snap
                .outputs
                .iter()
                .chain(&snap.inputs)
                .any(|d| d.key == key && d.present);
            if present {
                return out; // F10: only an absent device can be forgotten
            }
            let before = cfg.setup.known_devices.len();
            cfg.setup.known_devices.retain(|d| d.key != key);
            let refers = |c: &CellDef| {
                matches!(&c.source, MatrixSource::Input { key: k } | MatrixSource::Denoised { key: k } if *k == key)
                    || matches!(&c.dest, MatrixDest::Output { key: k } if *k == key)
            };
            let cells_before = cfg.setup.cells.len();
            cfg.setup.cells.retain(|c| !refers(c));
            let denoisers_before = cfg.setup.denoisers.len();
            cfg.setup.denoisers.retain(|d| d.input != key);
            let moved = retarget_rules(cfg, &RouteTarget::Device { key });
            let changed = before != cfg.setup.known_devices.len()
                || cells_before != cfg.setup.cells.len()
                || denoisers_before != cfg.setup.denoisers.len()
                || moved > 0;
            if changed {
                moved_toast(moved, &mut out);
                setup_changed(cfg, &mut out);
            }
        }
    }
    out
}

/// Trimmed, non-empty, at most [`MAX_NAME_CHARS`] characters (longer names are cut).
pub fn clean_name(name: &str) -> Option<String> {
    let t = name.trim();
    (!t.is_empty()).then(|| t.chars().take(MAX_NAME_CHARS).collect())
}

/// "Channel 1", "Channel 2", …: the first number not already taken.
pub fn default_name(base: &str, taken: &[&str]) -> String {
    (1..)
        .map(|n| format!("{base} {n}"))
        .find(|candidate| !taken.contains(&candidate.as_str()))
        .unwrap_or_else(|| base.to_owned())
}

/// Rules pointing at `target` fall back to the default output (F6); returns how many moved.
fn retarget_rules(cfg: &mut ConfigFile, target: &RouteTarget) -> u32 {
    let mut n = 0;
    for r in cfg
        .setup
        .app_rules
        .iter_mut()
        .filter(|r| &r.target == target)
    {
        r.target = RouteTarget::Default;
        n += 1;
    }
    n
}

fn moved_toast(n: u32, out: &mut EditOutcome) {
    if n > 0 {
        out.toasts.push(ni18n_f(
            "{} app moved to the default output",
            "{} apps moved to the default output",
            n,
            &[&n.to_string()],
        ));
    }
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

    fn music_setup() -> (ConfigFile, ChannelId, VirtualMicId) {
        let mut cfg = ConfigFile::default();
        apply_edit(
            &mut cfg,
            &snap(),
            Edit::AddChannel {
                name: "  Music ".into(),
            },
        );
        apply_edit(
            &mut cfg,
            &snap(),
            Edit::AddMic {
                name: String::new(),
            },
        );
        let ch = cfg.setup.channels[0].id;
        let mic = cfg.setup.virtual_mics[0].id;
        (cfg, ch, mic)
    }

    fn to_hp(ch: ChannelId) -> CellKey {
        CellKey {
            source: MatrixSource::Channel { id: ch },
            dest: MatrixDest::Output {
                key: DeviceKey("hp".into()),
            },
        }
    }

    #[test]
    fn add_names_are_trimmed_or_defaulted() {
        let (mut cfg, _, _) = music_setup();
        assert_eq!(cfg.setup.channels[0].name, "Music");
        assert_eq!(cfg.setup.virtual_mics[0].name, "Virtual mic 1");
        apply_edit(&mut cfg, &snap(), Edit::AddMic { name: "   ".into() });
        assert_eq!(cfg.setup.virtual_mics[1].name, "Virtual mic 2");
        let long = "x".repeat(100);
        let out = apply_edit(&mut cfg, &snap(), Edit::AddChannel { name: long });
        assert_eq!(cfg.setup.channels[1].name.chars().count(), MAX_NAME_CHARS);
        assert!(out.save && out.structure_changed);
        assert_eq!(cfg.setup.channels[1].volume, Volume::unity(2));
    }

    #[test]
    fn default_name_takes_the_first_free_number() {
        assert_eq!(default_name("Channel", &[]), "Channel 1");
        assert_eq!(
            default_name("Channel", &["Channel 1", "Channel 3"]),
            "Channel 2"
        );
    }

    #[test]
    fn rename_ignores_empty_and_unchanged_names() {
        let (mut cfg, ch, mic) = music_setup();
        let out = apply_edit(
            &mut cfg,
            &snap(),
            Edit::RenameChannel {
                id: ch,
                name: " ".into(),
            },
        );
        assert!(out.commands.is_empty());
        let out = apply_edit(
            &mut cfg,
            &snap(),
            Edit::RenameChannel {
                id: ch,
                name: "Music".into(),
            },
        );
        assert!(out.commands.is_empty(), "unchanged");
        let out = apply_edit(
            &mut cfg,
            &snap(),
            Edit::RenameChannel {
                id: ch,
                name: "Tunes ".into(),
            },
        );
        assert!(out.save && out.structure_changed);
        assert_eq!(cfg.setup.channels[0].name, "Tunes");
        apply_edit(
            &mut cfg,
            &snap(),
            Edit::RenameMic {
                id: mic,
                name: "Calls".into(),
            },
        );
        assert_eq!(cfg.setup.virtual_mics[0].name, "Calls");
    }

    #[test]
    fn deleting_a_channel_drops_its_sends_and_moves_its_apps() {
        let (mut cfg, ch, mic) = music_setup();
        apply_edit(&mut cfg, &snap(), Edit::ToggleCell { key: to_hp(ch) });
        apply_edit(
            &mut cfg,
            &snap(),
            Edit::ToggleCell {
                key: CellKey {
                    source: MatrixSource::Channel { id: ch },
                    dest: MatrixDest::VirtualMic { id: mic },
                },
            },
        );
        apply_edit(
            &mut cfg,
            &snap(),
            Edit::AssignApp {
                app: spotify(),
                display_name: "Spotify".into(),
                target: RouteTarget::Channel { id: ch },
            },
        );
        let out = apply_edit(&mut cfg, &snap(), Edit::DeleteChannel { id: ch });
        assert!(cfg.setup.channels.is_empty() && cfg.setup.cells.is_empty());
        assert_eq!(cfg.setup.app_rules[0].target, RouteTarget::Default);
        assert_eq!(
            out.toasts,
            vec!["1 app moved to the default output".to_owned()]
        );
        assert!(matches!(&out.commands[..], [Command::SetSetup(_)]));
    }

    #[test]
    fn deleting_a_mic_drops_its_sends() {
        let (mut cfg, ch, mic) = music_setup();
        let key = CellKey {
            source: MatrixSource::Channel { id: ch },
            dest: MatrixDest::VirtualMic { id: mic },
        };
        apply_edit(&mut cfg, &snap(), Edit::ToggleCell { key });
        apply_edit(&mut cfg, &snap(), Edit::ToggleCell { key: to_hp(ch) });
        let out = apply_edit(&mut cfg, &snap(), Edit::DeleteMic { id: mic });
        assert!(out.toasts.is_empty() && out.save);
        assert_eq!(cfg.setup.cells.len(), 1, "the send to the headphones stays");
    }

    #[test]
    fn toggle_creates_then_flips_and_remembers_the_device() {
        let (mut cfg, ch, _) = music_setup();
        apply_edit(&mut cfg, &snap(), Edit::ToggleCell { key: to_hp(ch) });
        assert_eq!(cfg.setup.cells.len(), 1);
        assert!(cfg.setup.cells[0].enabled && cfg.setup.cells[0].gain_db == 0.0);
        assert_eq!(cfg.setup.known_devices[0].kind, DeviceKind::Output);
        let out = apply_edit(&mut cfg, &snap(), Edit::ToggleCell { key: to_hp(ch) });
        assert!(!cfg.setup.cells[0].enabled && out.structure_changed);
        assert_eq!(cfg.setup.cells.len(), 1, "turning off keeps the send");
        let out = apply_edit(&mut cfg, &snap(), Edit::RemoveCell { key: to_hp(ch) });
        assert!(cfg.setup.cells.is_empty() && out.save);
        let out = apply_edit(&mut cfg, &snap(), Edit::RemoveCell { key: to_hp(ch) });
        assert!(out.commands.is_empty(), "removing twice is a no-op");
    }

    #[test]
    fn cell_gain_is_a_fast_path_saved_on_commit() {
        let (mut cfg, ch, _) = music_setup();
        let out = apply_edit(
            &mut cfg,
            &snap(),
            Edit::SetCellGain {
                key: to_hp(ch),
                gain_db: -6.0,
                commit: true,
            },
        );
        assert!(out.commands.is_empty(), "no send, nothing to adjust");
        apply_edit(&mut cfg, &snap(), Edit::ToggleCell { key: to_hp(ch) });
        let out = apply_edit(
            &mut cfg,
            &snap(),
            Edit::SetCellGain {
                key: to_hp(ch),
                gain_db: -99.0,
                commit: false,
            },
        );
        assert!(!out.save && !out.structure_changed);
        assert!(
            matches!(&out.commands[..], [Command::SetCellGain { gain_db, .. }] if *gain_db == -60.0)
        );
        let out = apply_edit(
            &mut cfg,
            &snap(),
            Edit::SetCellGain {
                key: to_hp(ch),
                gain_db: -18.04,
                commit: true,
            },
        );
        assert!(out.save);
        assert_eq!(cfg.setup.cells[0].gain_db, -18.0);
    }

    #[test]
    fn channel_and_mic_volume_save_only_on_commit() {
        let (mut cfg, ch, mic) = music_setup();
        let out = apply_edit(
            &mut cfg,
            &snap(),
            Edit::ChannelVolume {
                id: ch,
                level: 0.5,
                muted: false,
                commit: false,
            },
        );
        assert!(!out.save);
        assert!(
            matches!(&out.commands[..], [Command::SetChannelVolume { volume, .. }] if volume.channels == vec![0.125, 0.125])
        );
        assert_eq!(cfg.setup.channels[0].volume.channels, vec![0.125, 0.125]);
        let out = apply_edit(
            &mut cfg,
            &snap(),
            Edit::MicVolume {
                id: mic,
                level: 1.0,
                muted: true,
                commit: true,
            },
        );
        assert!(out.save && cfg.setup.virtual_mics[0].volume.muted);
    }

    #[test]
    fn only_absent_devices_can_be_forgotten() {
        let (mut cfg, ch, _) = music_setup();
        apply_edit(&mut cfg, &snap(), Edit::ToggleCell { key: to_hp(ch) });
        apply_edit(
            &mut cfg,
            &snap(),
            Edit::AssignApp {
                app: spotify(),
                display_name: "Spotify".into(),
                target: hp(),
            },
        );
        let out = apply_edit(
            &mut cfg,
            &snap(),
            Edit::ForgetDevice {
                key: DeviceKey("hp".into()),
            },
        );
        assert!(out.commands.is_empty(), "present: refused");
        let mut gone = snap();
        gone.outputs[0].present = false;
        let out = apply_edit(
            &mut cfg,
            &gone,
            Edit::ForgetDevice {
                key: DeviceKey("hp".into()),
            },
        );
        assert!(cfg.setup.known_devices.is_empty() && cfg.setup.cells.is_empty());
        assert_eq!(cfg.setup.app_rules[0].target, RouteTarget::Default);
        assert_eq!(out.toasts.len(), 1);
    }
}
