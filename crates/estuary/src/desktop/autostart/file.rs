//! XDG autostart desktop file (desktop M11.D9). Native builds only. The file has the name the
//! Background portal uses, plus a marker, so a portal-owned or hand-made entry is recognised as
//! foreign and never touched.

use std::io::{self, Write};
use std::path::Path;

pub const FILE_NAME: &str = "io.github.abidibo.Estuary.desktop";
const MARKER: &str = "X-Estuary-Autostart=true";

const CONTENT: &str = "[Desktop Entry]
Type=Application
Name=estuary
Comment=Audio matrix running in the background
Icon=io.github.abidibo.Estuary
Exec=estuary --background
Terminal=false
NoDisplay=true
X-GNOME-Autostart-enabled=true
X-Estuary-Autostart=true
";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileOwnership {
    Absent,
    /// Has our marker and isn't a portal (`X-Flatpak`) entry.
    Ours,
    /// Portal-written or user-made: never modified or removed.
    Foreign,
}

pub fn inspect(path: &Path) -> io::Result<FileOwnership> {
    match std::fs::read_to_string(path) {
        Ok(text) => {
            let has = |k: &str| text.lines().any(|l| l.trim() == k);
            let portal = text
                .lines()
                .any(|l| l.trim_start().starts_with("X-Flatpak="));
            Ok(if has(MARKER) && !portal {
                FileOwnership::Ours
            } else {
                FileOwnership::Foreign
            })
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(FileOwnership::Absent),
        Err(e) => Err(e),
    }
}

/// Atomic: temp file, fsync, rename (the C12 pattern). The temp file is removed on failure.
pub fn write(path: &Path) -> io::Result<()> {
    let dir = path
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "no parent directory"))?;
    std::fs::create_dir_all(dir)?;
    let tmp = path.with_extension("desktop.tmp");
    let result = (|| {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(CONTENT.as_bytes())?;
        f.sync_all()?;
        std::fs::rename(&tmp, path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

/// `true` if our file was there and is now gone.
pub fn remove_if_ours(path: &Path) -> io::Result<bool> {
    if inspect(path)? == FileOwnership::Ours {
        std::fs::remove_file(path)?;
        return Ok(true);
    }
    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_inspect_remove() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("autostart").join(FILE_NAME);
        assert_eq!(inspect(&path).unwrap(), FileOwnership::Absent);
        write(&path).unwrap();
        assert_eq!(inspect(&path).unwrap(), FileOwnership::Ours);
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("Exec=estuary --background"));
        assert!(
            !path.with_extension("desktop.tmp").exists(),
            "no temp file left"
        );
        assert!(remove_if_ours(&path).unwrap());
        assert_eq!(inspect(&path).unwrap(), FileOwnership::Absent);
        assert!(!remove_if_ours(&path).unwrap());
    }

    #[test]
    fn foreign_entries_are_never_ours() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(FILE_NAME);
        std::fs::write(&path, "[Desktop Entry]\nExec=estuary\n").unwrap();
        assert_eq!(inspect(&path).unwrap(), FileOwnership::Foreign, "hand-made");
        assert!(!remove_if_ours(&path).unwrap());
        assert!(path.exists());
        std::fs::write(
            &path,
            format!("{CONTENT}X-Flatpak=io.github.abidibo.Estuary\n"),
        )
        .unwrap();
        assert_eq!(
            inspect(&path).unwrap(),
            FileOwnership::Foreign,
            "portal-written"
        );
    }
}
