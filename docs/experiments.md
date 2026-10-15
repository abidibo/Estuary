# Live experiments

Results of the live verification items listed in the specs. Each entry records the date, the environment, the commands, the outcome, and which contract it confirms or amends.

## M2.1 (UNVERIFIED-5 + registry props), 2026-10-14

Environment: Pop!_OS 24.04, GNOME Wayland, PipeWire 1.5.85 (headers 1.6.8), WirePlumber 0.5.12.

**(a) Registry global props.**
`pw-cli ls Port`: audio ports' **global** props include `node.id`, `audio.channel`, `port.name`, `port.direction` (`in`/`out`), `port.monitor` (only on monitor ports), `format.dsp`. `pw-cli ls Link` (with a temporary silent `pw-cat` stream): link global props include `link.output.node`, `link.output.port`, `link.input.node`, `link.input.port`, `client.id`.
→ Ports and links are **not bound**; the mirror is built from global props (engine M2.6).

**(b) WirePlumber version (UNVERIFIED-5).**
`pw-dump` WirePlumber client info props: `application.version = "0.5.12"`, `core.version = "1.5.85"` (the libpipewire version WP links against, *not* the WP version).
→ `ConnectionState::Connected.wireplumber_version` = the WirePlumber client's `application.version`. The WP ≥ 0.5 gate stays the presence of the `sm-settings` metadata (C13, unchanged).

**(c) Own-client identification.**
`cargo run -p estuary-engine --example registry_dump --features live-pw`: our client's **global** props carry `application.name` but not `estuary.instance`. After binding, the client's **info** props carry `estuary.instance` (matching ours) and `application.process.id` (matching our pid).
→ `own_client_id` is set from bound client info props where `estuary.instance` equals the current instance. C5 ownership wording unchanged.

## M2 (F4: reconnect after a PipeWire restart), 2026-10-14

`ESTUARY_LIVE_RESTART=1 cargo test -p estuary-engine --features live-pw --test live_pw reconnects_after_pipewire_restart -- --ignored --nocapture`, which runs `systemctl --user restart pipewire wireplumber` while the engine is connected.

Result: `Reconnecting` after 65 ms, `Connected` + `Restored` after **540 ms** (acceptance target: 5 s). Disconnection is detected through the core `error(PW_ID_CORE, -EPIPE)` path, as designed.

Side note: the restart brought up **PipeWire 1.6.8**. Installing `libpipewire-0.3-dev` had upgraded the PipeWire packages, while the 1.5.85 daemon kept running until this restart. WirePlumber stayed at 0.5.12. The dev machine now runs PipeWire 1.6.8.

## M4.1 (UNVERIFIED-3: meter streams and GNOME Shell's recording indicator), 2026-10-14

Method: `pw-record --target <default sink>` with `stream.capture.sink=true stream.monitor=true resample.peaks=true node.passive=true media.role=DSP` (A). Then the same with `application.id=org.PulseAudio.pavucontrol` added (B). The user watched the GNOME top bar while each ran.

- **A:** the microphone indicator **appears**, and disappears when the stream stops (baseline checked).
- **B:** **no** indicator.

→ GNOME Shell keys the indicator on the application id, not on `stream.monitor`. **C5 is kept as written:** meter streams set `application.id=org.PulseAudio.pavucontrol`, like pavucontrol and pwvucontrol. This is legitimate because these streams receive only peak values (`resample.peaks`), never audio content. The hardware-input variant wasn't repeated, since the indicator decision is per application id regardless of target.

## M5.1 (UNVERIFIED-2: device volume path), 2026-10-14

`pw-cli set-param <sink> Props '{ channelVolumes: [0.125, 0.125] }'` on the built-in ALSA sink changed only the node's own Props. `wpctl get-volume` still reported 0.73, and the device's `Route` (`analog-output-speaker`) kept `channelVolumes [0.3868, 0.3868]`. Restored afterwards with `wpctl set-volume <sink> 0.73`.

→ **Outcome B.** Device volume is written through the card's `Route` param: `Device::set_param(Route, Object{ObjectParamRoute: index, device, props: {channelVolumes, mute}, save: true})`. The node props `device.id` (the card) and `card.profile.device` (matches the route's `device` field) select the route. The route `index` comes from the card's enumerated `Route` params. Nodes without `device.id` (virtual sinks, Estuary channels) keep using node `Props`. Same mechanism for inputs (the mic route showed the same layout).

## M8.1 (UNVERIFIED-1: passive cells and device suspend), 2026-10-14

`ESTUARY_LIVE_HW=1 cargo test -p estuary-engine --features live-pw --test live_pw passive_cells_let_the_output_suspend -- --ignored --nocapture`. The engine builds one channel and one cell (`node.passive=true`, `link.passive=true` on both cell links) into the real default output (`alsa_output.pci-0000_00_1f.3.analog-stereo`). Then `pw-cat` plays 4 s of **silence** into the channel. The output node's state comes from `pw-cli info`.

- With the cell and no app: the output stays **suspended**.
- App playing into the channel: **running** within 2 s.
- After the app stops: **suspended** again (WirePlumber's suspend timeout).

→ **Outcome A.** Passivity on cell nodes and on both cell links keeps devices suspendable and still drives the chain when an app plays. C5 is kept as written (`CELL_LINKS_PASSIVE = true`). The planned `examples/passive_probe.rs` wasn't needed: the engine itself served as the probe.

## M8 (UNVERIFIED-4: a virtual mic across two clocks), 2026-10-14

`ESTUARY_LIVE_HW=1 cargo test -p estuary-engine --features live-pw --test live_pw virtual_mic_mixes_two_clocks -- --ignored --nocapture`. One virtual mic is fed by two cells: the hardware mic (ALSA capture clock) at 0 dB, and a channel carrying a 0.5-amplitude 1 kHz tone from `pw-cat` at −6 dB. A recorder (the engine's meter stream on the virtual mic) reads it for 5 s.

Result: peak **0.251** (the tone at −6 dB plus room noise). **0 %** of 50 ms reads fell below 0.15, so there were no stalls or gaps, and the engine raised no xrun warnings. Null-sink only variant (`preset_builds_gain_is_audible_and_teardown_is_clean`): vmic 0.251, output 0.500, 0 % low reads.

→ No change needed. PipeWire puts the virtual mic, the cells and both sources in one driver group and resamples the follower adaptively.

## M8 live findings (engine bugs fixed), 2026-10-14

1. **Control-channel deadlock.** pipewire-rs 0.10 runs `channel::Receiver::attach` callbacks with the channel mutex held (`queue.drain(..).for_each(callback)` under the lock). Sending to the same channel from inside a control callback self-deadlocks the loop thread, and then `Engine::drop`/`shutdown` block on that mutex too. The shell did exactly that with `Control::DropGraveyard` when a command or timer destroyed a cell or a meter stream (meter retarget, Teardown). Fix: the graveyard has its own `pipewire::channel<()>`. Rule: **never send to a channel from inside its own callback.**
2. **Reconcile retry swallowed the debounce.** After a creating pass, the 2 s pending-timeout retry was armed, and `schedule_reconcile` ignored the 30 ms debounce asked for by our links appearing. Statuses stayed `Pending` for 2 s, and foreign-link cleanup would have waited as long. Fix: track the due instant and re-arm when an earlier one is requested. Preset to `Active` now takes about **120 ms**.
3. **Spurious "volume write failed".** A fresh adapter answers our initial `subscribe_params(Props)` enumeration with ENOENT before it is configured. The subscription stays and later changes still arrive. Enumeration errors are now logged at debug and no longer reported as volume-write failures.
4. **"unknown resource N" on teardown.** Destroying our own adapters and links both through `registry.destroy_global` and by dropping their proxies made the proxy drop talk to a removed resource. Our objects (`object.linger=false`) are now destroyed by dropping their proxy alone. `destroy_global` is kept for objects we hold no proxy for, such as leftovers from an earlier connection.
5. **Test pitfall.** WirePlumber's restore-stream applied a saved volume of 0.532 (−5.5 dB) to every `pw-cat` stream. Test players now pass `--volume 1.0` and `state.restore-props=false`.

## M10 (RNNoise on synthetic noise), 2026-10-15

While writing the live attenuation test, the denoiser cut −30 dBFS white noise by only 1.2 dB in a live graph against 14.8 dB offline. Bisecting:

- **Not the pipeline.** Raw nnnoiseless (no FIFOs, crossfade or resampling of ours) on the same signals gives the same numbers within about 1 dB. Rate (48000) and quantum (2048) in the live graph were as expected; FTZ makes no difference.
- **The noise realization decides.** White noise, 6 s, same level, different xorshift seeds: 1.6, 4.6, 14.8, 26.8 dB. Pink noise: 6.1 to 44.4 dB. Over 20 s, one seed stays around 2 dB throughout while another climbs from 2 to 59 dB.
- **Even the start offset decides.** The same seed-9 noise after 0 / 1 / 100 / 240 / 479 / 4800 / 23456 samples of silence: 15.9 / 11.8 / 15.8 / 3.1 / 12.8 / 15.9 / 1.4 dB.

→ RNNoise (trained on real recordings) has no predictable behaviour on synthetic stationary noise. Automated tests pin one realization offline (`denoise_attenuates_noise`, regression only). Live tests check what is deterministic: the voice gate at 0.9 silences noise exactly (−180 dBFS), bypass is transparent, no underruns. Whether real fan noise and typing are suppressed is judged by ear with a real mic (the master's M10 exit check).

## M10 (virtual sources' capture ports), 2026-10-15

A `support.null-audio-sink` with `media.class=Audio/Source/Virtual` exposes `capture_FL/FR` with **`port.monitor = "true"`** (checked with `pw-cli ls Port`). The engine's Capture role took only non-monitor outputs, so such sources (made by tools like Easy Effects) could never be routed. Fixed (C5 amended): a source's capture ports are its non-monitor outputs, else all of them.

## M12 (Flatpak build and live findings), 2026-10-15

**Routing bug (engine, fixed).** Store screenshots showed demo apps on their initial sink instead of their channels. The streams were already playing when the engine connected, while their rule targets (channels) didn't exist yet. Link-based external detection (C7) then saw "settled, not on the rule target, not on the default sink", reported an `ExternalRetarget`, and the UI adopted it as the rule, overwriting Music with Headphones. At login this would rewrite rules whenever apps start before Estuary has built its channels. Fix: each stream remembers the node it was last linked to; only a *change* of it can be an external move (C7 amended). Regression tests `where_a_stream_already_plays_is_not_an_external_move`, `a_later_move_by_links_is_external`. Verified live: all three apps land in their channels and the rules survive.

**Quit panic (UI, fixed).** A late engine event after `Finish` pushed to an already dropped page through relm4's `Controller::emit`, which unwraps. Page updates now use `sender().send()`, and engine events are ignored once quitting finished.

**Flatpak build notes.**
- `pipewire-sys` runs bindgen, which needs libclang: the manifest adds `org.freedesktop.Sdk.Extension.llvm22` (the only LLVM extension on the 26.08 base) with `LIBCLANG_PATH`.
- This dev shell exports `SSL_CERT_FILE`/`SSL_CERT_DIR` with Ubuntu paths. Flatpak passes them into the Builder sandbox, where they don't exist, so source downloads failed TLS verification. Build with `env -u SSL_CERT_FILE -u SSL_CERT_DIR flatpak run org.flatpak.Builder …` on such hosts.
- Flatpak always sets `XDG_CONFIG_HOME` to `~/.var/app/<id>/config` (`--env` can't override it). Test configs go there.

**Flatpak runtime checks.** The sandboxed app connects with write access (it creates its nodes; no PermissionDenied), detects the sandbox, spawns the tray, and quits cleanly on SIGTERM. The GNOME Background portal granted background activity without a dialog and, because `start_at_login` defaults to true (C6), created `~/.config/autostart/io.github.abidibo.Estuary.desktop` with `Exec=flatpak run --command=estuary io.github.abidibo.Estuary --background` and `X-Flatpak=io.github.abidibo.Estuary` (**UNVERIFIED-D2 resolved**: the portal's file name and keys are exactly what the native build treats as foreign). The test entry and permission were removed afterwards.

**GNOME 51 SDK:** xgettext 1.0 (Rust parser) and `/usr/share/gettext/its/metainfo.its` are present. The Rust parser and the host's C-parser fallback extract the same 159 messages.
