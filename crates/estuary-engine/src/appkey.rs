//! Application identity (master C7, amended).

use crate::graph::Props;
use crate::model::AppKey;

/// Binaries that host many unrelated programs; they never identify an app on their own.
const GENERIC_BINARIES: &[&str] = &[
    "python",
    "python3",
    "java",
    "node",
    "electron",
    "wine",
    "wine64",
    "wine-preloader",
    "wine64-preloader",
    "bwrap",
    "flatpak-spawn",
    "pw-play",
    "pw-cat",
    "paplay",
    "aplay",
    "gst-launch-1.0",
];

fn is_generic(binary: &str) -> bool {
    if GENERIC_BINARIES.contains(&binary) {
        return true;
    }
    // Versioned interpreters: python3.12, python3.13, ...
    binary
        .strip_prefix("python3.")
        .is_some_and(|rest| !rest.is_empty() && rest.chars().all(|c| c.is_ascii_digit()))
}

/// Derive the `AppKey` of a playback stream from its properties (C7 order).
pub fn derive(props: &Props) -> AppKey {
    if let Some(bin) = props.get("application.process.binary") {
        let bin = bin.to_lowercase();
        if !bin.is_empty() && !is_generic(&bin) {
            return AppKey(format!("bin:{bin}"));
        }
    }
    if let Some(name) = props.get("application.name").filter(|n| !n.is_empty()) {
        return AppKey(format!("name:{name}"));
    }
    AppKey(format!(
        "node:{}",
        props.get("node.name").map_or("", String::as_str)
    ))
}

/// Human-readable name: `application.name`, else binary, else `node.description`, else `node.name`.
pub fn display_name(props: &Props) -> String {
    [
        "application.name",
        "application.process.binary",
        "node.description",
        "node.name",
    ]
    .iter()
    .find_map(|k| props.get(*k).filter(|v| !v.is_empty()).cloned())
    .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn props(pairs: &[(&str, &str)]) -> Props {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect()
    }

    #[test]
    fn binary_wins_and_is_lowercased() {
        let p = props(&[
            ("application.process.binary", "Spotify"),
            ("application.name", "spotify"),
        ]);
        assert_eq!(derive(&p), AppKey("bin:spotify".into()));
    }

    #[test]
    fn generic_binaries_fall_back_to_name() {
        for bin in ["python3", "python3.12", "pw-play", "wine64-preloader"] {
            let p = props(&[
                ("application.process.binary", bin),
                ("application.name", "Lutris"),
            ]);
            assert_eq!(derive(&p), AppKey("name:Lutris".into()), "binary {bin}");
        }
        // "python3.x" with non-digits is not a versioned interpreter
        let p = props(&[("application.process.binary", "python3.tool")]);
        assert_eq!(derive(&p), AppKey("bin:python3.tool".into()));
    }

    #[test]
    fn falls_back_to_node_name() {
        let p = props(&[("node.name", "some.stream")]);
        assert_eq!(derive(&p), AppKey("node:some.stream".into()));
    }

    #[test]
    fn display_name_order() {
        assert_eq!(
            display_name(&props(&[("application.process.binary", "mpv")])),
            "mpv"
        );
        assert_eq!(
            display_name(&props(&[
                ("application.name", "Firefox"),
                ("node.name", "x")
            ])),
            "Firefox"
        );
    }
}
