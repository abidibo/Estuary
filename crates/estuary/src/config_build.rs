pub const APP_ID: &str = "io.github.abidibo.Estuary"; // C14
pub const GETTEXT_PACKAGE: &str = "estuary"; // C14
/// Set by the Flatpak / install build via `ESTUARY_LOCALEDIR`; falls back to the
/// in-tree `target/locale` produced by `build-aux/compile-mo.sh` for `cargo run`.
pub const LOCALEDIR: &str = match option_env!("ESTUARY_LOCALEDIR") {
    Some(dir) => dir,
    None => concat!(env!("CARGO_MANIFEST_DIR"), "/../../target/locale"),
};
/// `ESTUARY_PROFILE` at build time (packaging M12.6): `devel` styles the window as a
/// development build. The app ID never changes.
pub const DEVEL: bool = match option_env!("ESTUARY_PROFILE") {
    Some(p) => const_eq(p, "devel"),
    None => false,
};

const fn const_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    let mut i = 0;
    while i < a.len() {
        if a[i] != b[i] {
            return false;
        }
        i += 1;
    }
    true
}
