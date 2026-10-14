# Estuary

Estuary is an audio matrix and isolator for Linux, built natively on PipeWire with Rust, GTK4 and libadwaita. It routes each application to an output or a virtual channel, mixes channels, microphones and noise-suppressed inputs into outputs and virtual microphones through a matrix with a gain on every crosspoint, and shows live level meters for everything. See the design in [`specs/estuary/spec.md`](specs/estuary/spec.md).

> Status: early development (scaffold). Nothing user-facing works yet.

## Requirements

- PipeWire ≥ 1.0
- WirePlumber ≥ 0.5

## Development setup

| Distro | Command |
|---|---|
| Pop!_OS / Ubuntu ≥ 24.04 / Debian 13 | `sudo apt install build-essential pkg-config clang libpipewire-0.3-dev libgtk-4-dev libadwaita-1-dev gettext` |
| Fedora ≥ 42 | `sudo dnf install gcc pkgconf-pkg-config clang pipewire-devel gtk4-devel libadwaita-devel gettext` |
| Arch / Manjaro | `sudo pacman -S --needed base-devel clang pipewire gtk4 libadwaita gettext` |

`clang` is needed by `bindgen` in the PipeWire `-sys` crates. Check the libraries with:

```sh
pkg-config --modversion libpipewire-0.3 gtk4 libadwaita-1   # expect ≥ 1.0, ≥ 4.14, ≥ 1.5
```

Rust ≥ 1.93 (stable) is required; `rust-toolchain.toml` selects it automatically with rustup.

## Run

```sh
cargo run -p estuary
ESTUARY_LOG=debug cargo run -p estuary     # verbose logs (tracing EnvFilter syntax)
```

## Tests

```sh
cargo test --workspace
# Live tests need a running PipeWire + WirePlumber session (never run in CI):
cargo test --workspace --features estuary-engine/live-pw -- --include-ignored
```

## Translations

```sh
build-aux/update-pot.sh    # extract strings into po/estuary.pot
build-aux/compile-mo.sh    # build .mo files into target/locale for cargo run
```

(Scripts arrive with milestone M12.)

## Flatpak

```sh
flatpak run org.flatpak.Builder --user --install --force-clean build-dir \
  build-aux/io.github.abidibo.Estuary.Devel.yml
```

(Manifest arrives with milestone M12.)

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE-MIT](LICENSE-MIT))

at your option.

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in the work by you, as defined in the Apache-2.0 license, shall be dual licensed as above, without any additional terms or conditions.
