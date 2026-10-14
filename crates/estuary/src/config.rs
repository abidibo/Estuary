//! Config file I/O (UI M6, master C12): load with recovery, debounced atomic saves.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Duration;

pub use estuary_engine::setup::{ConfigFile, SCHEMA_VERSION};
use estuary_engine::{SetupIssue, setup};
use relm4::gtk::glib;

const SAVE_DEBOUNCE: Duration = Duration::from_millis(500);

/// How loading went; surfaced once the window is shown (UI M6.3).
#[derive(Debug, Clone, PartialEq)]
pub enum LoadOutcome {
    /// No file yet: defaults, welcome pending.
    Fresh,
    Loaded,
    /// F14: unreadable; kept as `backup`, started fresh.
    Recovered {
        backup: PathBuf,
        error: String,
    },
    /// F16: written by a newer Estuary; loaded read-only.
    NewerSchema {
        found: u32,
    },
    /// C12: invalid entries dropped (not written back until the next real edit).
    Repaired {
        issues: Vec<SetupIssue>,
    },
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("{0}")]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Serialize(#[from] toml::ser::Error),
    #[error("read-only configuration")]
    ReadOnly,
}

/// C12: `$XDG_CONFIG_HOME/estuary/config.toml` (Flatpak maps it under ~/.var/app automatically).
pub fn config_file() -> PathBuf {
    glib::user_config_dir().join("estuary").join("config.toml")
}

pub struct ConfigStore {
    path: PathBuf,
    file: ConfigFile,
    read_only: bool,
    /// The scheduled save; cleared by the timer itself when it fires.
    pending: Rc<RefCell<Option<glib::SourceId>>>,
    on_save_due: Option<Rc<dyn Fn()>>,
    last_error: Option<String>,
}

impl std::fmt::Debug for ConfigStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConfigStore")
            .field("path", &self.path)
            .field("read_only", &self.read_only)
            .finish_non_exhaustive()
    }
}

impl ConfigStore {
    pub fn load(path: PathBuf) -> (ConfigStore, LoadOutcome) {
        let (file, read_only, outcome) = load_file(&path);
        let store = ConfigStore {
            path,
            file,
            read_only,
            pending: Rc::default(),
            on_save_due: None,
            last_error: None,
        };
        (store, outcome)
    }

    /// Enables debounced saves: `on_save_due` runs (on the main loop) when a save should happen;
    /// the owner then calls `flush_sync`.
    pub fn attach(&mut self, on_save_due: impl Fn() + 'static) {
        self.on_save_due = Some(Rc::new(on_save_due));
    }

    pub fn file(&self) -> &ConfigFile {
        &self.file
    }

    pub fn file_mut(&mut self) -> &mut ConfigFile {
        &mut self.file
    }

    #[allow(dead_code)] // read by the M11 banner priority (read-only wins over reconnect)
    pub fn is_read_only(&self) -> bool {
        self.read_only
    }

    /// Debounced save (500 ms after the last change); no-op when read-only or not attached.
    pub fn schedule_save(&mut self) {
        let Some(notify) = self.on_save_due.clone() else {
            return;
        };
        if self.read_only {
            return;
        }
        self.cancel_pending();
        let pending = self.pending.clone();
        let id = glib::timeout_add_local_once(SAVE_DEBOUNCE, move || {
            pending.borrow_mut().take();
            notify();
        });
        *self.pending.borrow_mut() = Some(id);
    }

    fn cancel_pending(&self) {
        if let Some(id) = self.pending.borrow_mut().take() {
            id.remove();
        }
    }

    /// Writes now, atomically (temp file + fsync + rename).
    pub fn flush_sync(&mut self) -> Result<(), ConfigError> {
        if self.read_only {
            return Err(ConfigError::ReadOnly);
        }
        self.cancel_pending();
        match write_atomic(&self.path, &self.file) {
            Ok(()) => {
                self.last_error = None;
                Ok(())
            }
            Err(e) => {
                self.last_error = Some(e.to_string());
                Err(e)
            }
        }
    }

    /// The error of the last failed save, cleared by a successful one (F15).
    #[cfg(test)]
    pub fn last_error(&self) -> Option<&str> {
        self.last_error.as_deref()
    }
}

fn load_file(path: &Path) -> (ConfigFile, bool, LoadOutcome) {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        // Missing (including an unusable parent directory): start fresh. Only a file that exists
        // but can't be read counts as corrupt.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound || !path.exists() => {
            return (ConfigFile::default(), false, LoadOutcome::Fresh);
        }
        Err(e) => {
            tracing::warn!("cannot read {}: {e}", path.display());
            return recover(path, &e.to_string());
        }
    };
    let version = toml::from_str::<toml::Table>(&text)
        .ok()
        .and_then(|t| t.get("schema_version").and_then(toml::Value::as_integer));
    let parsed = toml::from_str::<ConfigFile>(&text);
    match (version, parsed) {
        (Some(v), parsed) if v > i64::from(SCHEMA_VERSION) => {
            // F16: keep what we understand, never overwrite a newer schema.
            let file = parsed.unwrap_or_default();
            let found = u32::try_from(v).unwrap_or(u32::MAX);
            (file, true, LoadOutcome::NewerSchema { found })
        }
        (_, Ok(mut file)) => {
            let (setup, issues) = setup::sanitize(std::mem::take(&mut file.setup));
            file.setup = setup;
            if issues.is_empty() {
                (file, false, LoadOutcome::Loaded)
            } else {
                for i in &issues {
                    tracing::warn!("dropped from the saved setup: {}", i.message());
                }
                (file, false, LoadOutcome::Repaired { issues })
            }
        }
        (_, Err(e)) => recover(path, &e.to_string()),
    }
}

/// F14: move the unreadable file aside (never delete it) and start fresh.
fn recover(path: &Path, error: &str) -> (ConfigFile, bool, LoadOutcome) {
    let stamp = glib::DateTime::now_local()
        .and_then(|d| d.format("%Y-%m-%dT%H-%M-%S"))
        .map_or_else(|_| "unknown".to_owned(), |s| s.to_string());
    let base = format!("{}.broken-{stamp}", path.display());
    let mut backup = PathBuf::from(&base);
    let mut n = 1;
    while backup.exists() {
        backup = PathBuf::from(format!("{base}-{n}"));
        n += 1;
    }
    match std::fs::rename(path, &backup) {
        Ok(()) => (
            ConfigFile::default(),
            false,
            LoadOutcome::Recovered {
                backup,
                error: error.to_owned(),
            },
        ),
        Err(e) => {
            // Can't move it aside: run read-only so the original is never overwritten.
            tracing::error!("cannot back up {}: {e}", path.display());
            (
                ConfigFile::default(),
                true,
                LoadOutcome::Recovered {
                    backup: path.to_owned(),
                    error: error.to_owned(),
                },
            )
        }
    }
}

fn write_atomic(path: &Path, file: &ConfigFile) -> Result<(), ConfigError> {
    use std::io::Write;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let text = toml::to_string_pretty(file)?;
    let tmp = path.with_extension("toml.tmp");
    {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(text.as_bytes())?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use estuary_engine::setup::AppRule;
    use estuary_engine::{AppKey, RouteTarget};

    fn store_in(dir: &tempfile::TempDir) -> (ConfigStore, LoadOutcome) {
        ConfigStore::load(dir.path().join("estuary").join("config.toml"))
    }

    #[test]
    fn fresh_then_saved_then_loaded() {
        let dir = tempfile::tempdir().unwrap();
        let (mut store, outcome) = store_in(&dir);
        assert_eq!(outcome, LoadOutcome::Fresh);
        store.file_mut().setup.app_rules.push(AppRule {
            app: AppKey("bin:spotify".into()),
            target: RouteTarget::Default,
            display_name: "Spotify".into(),
        });
        store.file_mut().settings.first_run_done = true;
        store.flush_sync().unwrap();
        assert!(
            !dir.path().join("estuary").join("config.toml.tmp").exists(),
            "temp file renamed"
        );
        let (again, outcome) = store_in(&dir);
        assert_eq!(outcome, LoadOutcome::Loaded);
        assert_eq!(again.file(), store.file());
    }

    #[test]
    fn corrupt_file_is_backed_up_and_reported_with_its_line() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "schema_version = 1\n[settings\nclose_action = 1\n").unwrap();
        let (store, outcome) = ConfigStore::load(path.clone());
        let LoadOutcome::Recovered { backup, error } = outcome else {
            panic!("expected Recovered")
        };
        assert!(
            backup
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("config.toml.broken-")
        );
        assert!(
            backup.exists() && !path.exists(),
            "moved aside, never deleted"
        );
        assert!(error.contains("line"), "error mentions the line: {error}");
        assert!(!store.is_read_only());
        // A second broken file in the same second gets a distinct name.
        std::fs::write(&path, "garbage =").unwrap();
        let (_, outcome) = ConfigStore::load(path);
        let LoadOutcome::Recovered { backup: second, .. } = outcome else {
            panic!("expected Recovered")
        };
        assert_ne!(second, backup);
    }

    #[test]
    fn newer_schema_is_read_only() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            "schema_version = 2\nfuture_thing = true\n[settings]\nfirst_run_done = true\n",
        )
        .unwrap();
        let (mut store, outcome) = ConfigStore::load(path.clone());
        assert_eq!(outcome, LoadOutcome::NewerSchema { found: 2 });
        assert!(store.is_read_only());
        assert!(
            store.file().settings.first_run_done,
            "understood fields are kept"
        );
        assert!(matches!(store.flush_sync(), Err(ConfigError::ReadOnly)));
        assert!(
            std::fs::read_to_string(&path)
                .unwrap()
                .contains("future_thing"),
            "never overwritten"
        );
    }

    #[test]
    fn invalid_entries_are_repaired_not_written_back() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let text = "schema_version = 1\n[[app_rules]]\napp = \"bin:x\"\ntarget = { kind = \"channel\", id = \"00000000-0000-0000-0000-000000000000\" }\n";
        std::fs::write(&path, text).unwrap();
        let (store, outcome) = ConfigStore::load(path.clone());
        assert!(matches!(outcome, LoadOutcome::Repaired { ref issues } if issues.len() == 1));
        assert!(store.file().setup.app_rules.is_empty());
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            text,
            "original kept for inspection"
        );
    }

    #[test]
    fn save_failure_is_reported() {
        let dir = tempfile::tempdir().unwrap();
        // The config "directory" is a file, so creating it fails.
        let blocker = dir.path().join("estuary");
        std::fs::write(&blocker, "not a dir").unwrap();
        let (mut store, _) = ConfigStore::load(blocker.join("config.toml"));
        assert!(store.flush_sync().is_err());
        assert!(store.last_error().is_some());
    }
}
