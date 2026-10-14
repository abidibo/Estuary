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
