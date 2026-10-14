pub const APP_ID: &str = "io.github.abidibo.Estuary"; // C14
pub const GETTEXT_PACKAGE: &str = "estuary"; // C14
/// Set by the Flatpak / install build via `ESTUARY_LOCALEDIR`; falls back to the
/// in-tree `target/locale` produced by `build-aux/compile-mo.sh` for `cargo run`.
pub const LOCALEDIR: &str = match option_env!("ESTUARY_LOCALEDIR") {
    Some(dir) => dir,
    None => concat!(env!("CARGO_MANIFEST_DIR"), "/../../target/locale"),
};
