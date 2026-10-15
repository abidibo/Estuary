//! Live integration tests against the running PipeWire session (engine M2.10).
//! Run: `cargo test -p estuary-engine --features live-pw -- --ignored --test-threads=1`
#![cfg(feature = "live-pw")]
#![allow(clippy::unwrap_used)]

use std::time::{Duration, Instant};

use estuary_engine::{ConnectionState, Engine, EngineConfig, Event};

/// Engine logs in live tests: `ESTUARY_LOG=estuary_engine=debug cargo test ...`.
fn init_logs() {
    let filter = tracing_subscriber::EnvFilter::try_from_env("ESTUARY_LOG")
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn"));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_test_writer()
        .try_init();
}

fn recv_until<T>(
    rx: &async_channel::Receiver<Event>,
    timeout: Duration,
    mut f: impl FnMut(&Event) -> Option<T>,
) -> Option<T> {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        match rx.try_recv() {
            Ok(ev) => {
                if let Some(t) = f(&ev) {
                    return Some(t);
                }
            }
            Err(_) => std::thread::sleep(Duration::from_millis(10)),
        }
    }
    None
}

#[test]
#[ignore = "live-pw: needs a running PipeWire + WirePlumber session"]
fn connects_and_snapshots() {
    let (engine, rx) = Engine::start(EngineConfig::default()).unwrap();
    let started = Instant::now();
    let versions = recv_until(&rx, Duration::from_secs(3), |ev| match ev {
        Event::Connection(ConnectionState::Connected {
            pipewire_version,
            wireplumber_version,
        }) => Some((pipewire_version.clone(), wireplumber_version.clone())),
        Event::Connection(
            other @ (ConnectionState::Unsupported { .. } | ConnectionState::PermissionDenied),
        ) => {
            panic!("unexpected connection state: {other:?}")
        }
        _ => None,
    })
    .expect("Connected within 3 s");
    let connected_after = started.elapsed();
    let snap = recv_until(&rx, Duration::from_secs(1), |ev| match ev {
        Event::Snapshot(s) => Some(s.clone()),
        _ => None,
    })
    .expect("a snapshot right after Connected");

    eprintln!(
        "connected in {connected_after:?}: PipeWire {}, WirePlumber {:?}",
        versions.0, versions.1
    );
    eprintln!(
        "default output: {:?}, default input: {:?}",
        snap.default_output, snap.default_input
    );
    for d in &snap.outputs {
        eprintln!(
            "  output: {} ({}) channels={} volume={:?}",
            d.description, d.key.0, d.channels, d.volume
        );
    }
    for d in &snap.inputs {
        eprintln!(
            "  input:  {} ({}) channels={} volume={:?}",
            d.description, d.key.0, d.channels, d.volume
        );
    }
    for a in &snap.apps {
        eprintln!(
            "  app:    {} [{}] streams={} target={:?}",
            a.display_name,
            a.key.0,
            a.streams.len(),
            a.current_target
        );
    }

    assert!(versions.1.is_some(), "WirePlumber version is reported");
    let default = snap
        .default_output
        .clone()
        .expect("a default output exists");
    assert!(
        snap.outputs.iter().any(|d| d.key == default),
        "default output is listed"
    );
    engine.shutdown(Duration::from_secs(1)).unwrap();
}

#[test]
#[ignore = "live-pw: needs a running PipeWire + WirePlumber session"]
fn second_engine_instance_coexists_and_shutdown_is_clean() {
    let (a, rx_a) = Engine::start(EngineConfig::default()).unwrap();
    let (b, rx_b) = Engine::start(EngineConfig::default()).unwrap();
    for rx in [&rx_a, &rx_b] {
        recv_until(rx, Duration::from_secs(3), |ev| {
            matches!(ev, Event::Connection(ConnectionState::Connected { .. })).then_some(())
        })
        .expect("both engines connect");
    }
    let t = Instant::now();
    a.shutdown(Duration::from_secs(1)).unwrap();
    b.shutdown(Duration::from_secs(1)).unwrap();
    assert!(t.elapsed() < Duration::from_secs(1), "shutdown is prompt");
}

/// F4: restarting PipeWire + WirePlumber → Reconnecting → Connected → Restored.
/// Disrupts audio for ~1 s, so it only runs with `ESTUARY_LIVE_RESTART=1`.
#[test]
#[ignore = "live-pw: restarts PipeWire; set ESTUARY_LIVE_RESTART=1"]
fn reconnects_after_pipewire_restart() {
    if std::env::var("ESTUARY_LIVE_RESTART").as_deref() != Ok("1") {
        eprintln!("skipped: set ESTUARY_LIVE_RESTART=1 to allow restarting PipeWire");
        return;
    }
    let (engine, rx) = Engine::start(EngineConfig::default()).unwrap();
    recv_until(&rx, Duration::from_secs(3), |ev| {
        matches!(ev, Event::Connection(ConnectionState::Connected { .. })).then_some(())
    })
    .expect("initial Connected");

    let t0 = Instant::now();
    let status = std::process::Command::new("systemctl")
        .args(["--user", "restart", "pipewire", "wireplumber"])
        .status()
        .expect("run systemctl");
    assert!(status.success(), "systemctl restart failed");

    let mut seen = Vec::new();
    let restored = recv_until(&rx, Duration::from_secs(10), |ev| {
        match ev {
            Event::Connection(c) => seen.push(format!("{:?} @ {:?}", c, t0.elapsed())),
            Event::Restored => {
                seen.push(format!("Restored @ {:?}", t0.elapsed()));
                return Some(t0.elapsed());
            }
            _ => {}
        }
        None
    });
    for s in &seen {
        eprintln!("  {s}");
    }
    let took = restored.expect("Restored within 10 s of the restart");
    assert!(
        seen.iter().any(|s| s.starts_with("Reconnecting")),
        "went through Reconnecting"
    );
    assert!(
        took < Duration::from_secs(5),
        "master acceptance: rebuilt within 5 s (took {took:?})"
    );
    engine.shutdown(Duration::from_secs(1)).unwrap();
}

/// Deletes a node by `node.name` (test cleanup).
fn destroy_node_named(name: &str) {
    let out = std::process::Command::new("pw-cli")
        .args(["ls", "Node"])
        .output()
        .expect("pw-cli");
    let mut id = None;
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        if let Some(rest) = line.trim_start().strip_prefix("id ") {
            id = rest.split(',').next().map(str::to_owned);
        } else if line.contains(&format!("node.name = \"{name}\""))
            && let Some(id) = &id
        {
            let _ = std::process::Command::new("pw-cli")
                .args(["destroy", id])
                .status();
        }
    }
}

/// M4.5 `meter_moves`: a 0.5-amplitude 1 kHz tone played into a temporary (silent) null sink
/// is visible on the sink's meter and on the app stream's meter.
#[test]
#[ignore = "live-pw: needs a running PipeWire + WirePlumber session"]
fn meter_moves() {
    use estuary_engine::{AppKey, Command, DeviceKey, MeterTarget};
    const SINK: &str = "estuary-test-null-sink";
    const APP: &str = "Estuary Meter Test";
    init_logs();

    // A silent virtual output so nothing is heard; lingers until destroyed below.
    destroy_node_named(SINK);
    let ok = std::process::Command::new("pw-cli")
        .args([
            "create-node",
            "adapter",
            &format!(
                "{{ factory.name=support.null-audio-sink node.name={SINK} media.class=Audio/Sink \
                 audio.position=[FL FR] object.linger=true monitor.channel-volumes=false }}"
            ),
        ])
        .status()
        .expect("pw-cli")
        .success();
    assert!(ok, "create null sink");

    // 1 kHz sine at 0.5 amplitude, s16 stereo, 10 s.
    let path = std::env::temp_dir().join("estuary-meter-test.raw");
    let mut pcm = Vec::new();
    for i in 0..48_000 * 10 {
        #[allow(clippy::cast_possible_truncation)] // |0.5 · sin| · 32767 fits in i16
        let v = (0.5 * (2.0 * std::f32::consts::PI * 1000.0 * i as f32 / 48_000.0).sin() * 32767.0)
            as i16;
        pcm.extend_from_slice(&v.to_le_bytes());
        pcm.extend_from_slice(&v.to_le_bytes());
    }
    std::fs::write(&path, pcm).unwrap();
    let mut player = std::process::Command::new("pw-cat")
        .args([
            "--playback",
            "--raw",
            "--format",
            "s16",
            "--rate",
            "48000",
            "--channels",
            "2",
        ])
        .args([
            "--target",
            SINK,
            "-P",
            &format!("{{ application.name = \"{APP}\" }}"),
        ])
        .arg(&path)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("pw-cat");

    let (engine, rx) = Engine::start(EngineConfig::default()).unwrap();
    recv_until(&rx, Duration::from_secs(3), |ev| {
        matches!(ev, Event::Connection(ConnectionState::Connected { .. })).then_some(())
    })
    .expect("Connected");
    std::thread::sleep(Duration::from_millis(300)); // let the stream appear
    let sink_t = MeterTarget::Device(DeviceKey(SINK.into()));
    let app_t = MeterTarget::App(AppKey(format!("name:{APP}")));
    engine
        .send(Command::SetMeterTargets(vec![
            sink_t.clone(),
            app_t.clone(),
        ]))
        .unwrap();

    let assignments = recv_until(&rx, Duration::from_secs(2), |ev| match ev {
        Event::MeterAssignments(a) if a.len() == 2 => Some(a.clone()),
        _ => None,
    });
    let result = (|| {
        let a = assignments.ok_or("no assignments for both targets")?;
        let bank = engine.meters();
        let deadline = Instant::now() + Duration::from_secs(2);
        let mut best = [0.0f32; 2];
        while Instant::now() < deadline {
            for (i, (_, slots)) in a.iter().enumerate() {
                for s in slots {
                    let r = bank.read(*s);
                    best[i] = best[i].max(r.peak[0].max(r.peak[1]));
                }
            }
            if best.iter().all(|p| *p > 0.1) {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        eprintln!(
            "peaks: sink={:.3} app={:.3} (tone amplitude 0.5)",
            best[0], best[1]
        );
        if best.iter().all(|p| *p > 0.1) {
            Ok(())
        } else {
            Err("peaks too low")
        }
    })();

    let _ = player.kill();
    let _ = player.wait();
    let _ = std::fs::remove_file(&path);
    engine.shutdown(Duration::from_secs(1)).unwrap();
    destroy_node_named(SINK);
    result.unwrap();
}

fn create_null_sink(name: &str) {
    destroy_node_named(name);
    let ok = std::process::Command::new("pw-cli")
        .args([
            "create-node",
            "adapter",
            &format!(
                "{{ factory.name=support.null-audio-sink node.name={name} media.class=Audio/Sink \
                 audio.position=[FL FR] object.linger=true }}"
            ),
        ])
        .status()
        .expect("pw-cli")
        .success();
    assert!(ok, "create {name}");
}

/// The node an app stream is linked to, by `pw-link -l` output ("<node>:<port>").
fn linked_sink_of(stream_node_name: &str) -> Option<String> {
    let out = std::process::Command::new("pw-link")
        .arg("-l")
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let mut lines = text.lines();
    while let Some(line) = lines.next() {
        if line.starts_with(&format!("{stream_node_name}:output_")) {
            let next = lines.next()?;
            let target = next.trim().strip_prefix("|->")?.trim();
            return target.split(':').next().map(str::to_owned);
        }
    }
    None
}

fn node_serial(name: &str) -> Option<String> {
    let out = std::process::Command::new("pw-cli")
        .args(["ls", "Node"])
        .output()
        .ok()?;
    let mut serial = None;
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        let t = line.trim();
        if t.starts_with("id ") {
            serial = None;
        } else if let Some(v) = t.strip_prefix("object.serial = ") {
            serial = Some(v.trim_matches('"').to_owned());
        } else if t == format!("node.name = \"{name}\"") {
            return serial;
        }
    }
    None
}

fn node_id(name: &str) -> Option<String> {
    let out = std::process::Command::new("pw-cli")
        .args(["ls", "Node"])
        .output()
        .ok()?;
    let mut id = None;
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        if let Some(rest) = line.trim_start().strip_prefix("id ") {
            id = rest.split(',').next().map(str::to_owned);
        } else if line.contains(&format!("node.name = \"{name}\"")) {
            return id;
        }
    }
    None
}

/// M5.8 `routes_test_app`: a rule moves a stream; a move made elsewhere is reported, not undone.
#[test]
#[ignore = "live-pw: needs a running PipeWire + WirePlumber session"]
fn routes_test_app() {
    use estuary_engine::{AppKey, Command, DeviceKey, RouteTarget, Setup, setup::AppRule};
    const A: &str = "estuary-test-sink-a";
    const B: &str = "estuary-test-sink-b";
    const APP: &str = "Estuary Route Test";
    const STREAM: &str = "estuary-route-test-stream";
    init_logs();
    create_null_sink(A);
    create_null_sink(B);
    let path = std::env::temp_dir().join("estuary-route-test.raw");
    std::fs::write(&path, vec![0u8; 48_000 * 4 * 15]).unwrap(); // 15 s of silence
    let mut player = std::process::Command::new("pw-cat")
        .args([
            "--playback",
            "--raw",
            "--format",
            "s16",
            "--rate",
            "48000",
            "--channels",
            "2",
        ])
        .args(["--target", A])
        .args([
            "-P",
            &format!("{{ application.name = \"{APP}\" node.name = \"{STREAM}\" }}"),
        ])
        .arg(&path)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("pw-cat");

    let result = (|| -> Result<(), String> {
        let wait_link = |want: &str, within: Duration| {
            let deadline = Instant::now() + within;
            while Instant::now() < deadline {
                if linked_sink_of(STREAM).as_deref() == Some(want) {
                    return true;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            false
        };
        if !wait_link(A, Duration::from_secs(2)) {
            return Err(format!("stream did not start on {A}"));
        }

        let (engine, rx) = Engine::start(EngineConfig::default()).map_err(|e| e.to_string())?;
        recv_until(&rx, Duration::from_secs(3), |ev| {
            matches!(ev, Event::Connection(ConnectionState::Connected { .. })).then_some(())
        })
        .ok_or("not connected")?;
        let app = AppKey(format!("name:{APP}"));
        let rule = |target| Setup {
            app_rules: vec![AppRule {
                app: app.clone(),
                target,
                display_name: APP.into(),
            }],
            ..Default::default()
        };
        let t0 = Instant::now();
        engine
            .send(Command::SetSetup(rule(RouteTarget::Device {
                key: DeviceKey(B.into()),
            })))
            .map_err(|e| e.to_string())?;
        if !wait_link(B, Duration::from_secs(2)) {
            return Err(format!("rule did not move the stream to {B}"));
        }
        eprintln!("moved by rule in {:?}", t0.elapsed());

        // Move it back from outside, like GNOME Settings or pavucontrol would.
        let id = node_id(STREAM).ok_or("stream id")?;
        let serial_a = node_serial(A).ok_or("serial A")?;
        let ok = std::process::Command::new("pw-metadata")
            .args([&id, "target.object", &serial_a, "Spa:Id"])
            .stdout(std::process::Stdio::null())
            .status()
            .map_err(|e| e.to_string())?
            .success();
        if !ok {
            return Err("pw-metadata failed".into());
        }
        let ext = recv_until(&rx, Duration::from_secs(2), |ev| match ev {
            Event::ExternalRetarget { app: a, target } if *a == app => Some(target.clone()),
            _ => None,
        })
        .ok_or("no ExternalRetarget")?;
        if ext
            != (RouteTarget::Device {
                key: DeviceKey(A.into()),
            })
        {
            return Err(format!("unexpected external target {ext:?}"));
        }
        // Not undone: it stays on A.
        std::thread::sleep(Duration::from_millis(500));
        if linked_sink_of(STREAM).as_deref() != Some(A) {
            return Err("the engine fought the external move".into());
        }
        engine
            .shutdown(Duration::from_secs(1))
            .map_err(|e| e.to_string())?;
        Ok(())
    })();

    let _ = player.kill();
    let _ = player.wait();
    let _ = std::fs::remove_file(&path);
    destroy_node_named(A);
    destroy_node_named(B);
    result.unwrap();
}

/// Exploratory regression for the M6 live finding: changing a rule twice (B, then A).
#[test]
#[ignore = "live-pw: needs a running PipeWire + WirePlumber session"]
fn rule_changed_twice() {
    use estuary_engine::{AppKey, Command, DeviceKey, RouteTarget, Setup, setup::AppRule};
    const A: &str = "estuary-test-sink-a";
    const B: &str = "estuary-test-sink-b";
    const APP: &str = "Estuary Twice Test";
    const STREAM: &str = "estuary-twice-stream";
    init_logs();
    create_null_sink(A);
    create_null_sink(B);
    let path = std::env::temp_dir().join("estuary-twice-test.raw");
    std::fs::write(&path, vec![0u8; 48_000 * 4 * 15]).unwrap();
    let mut player = std::process::Command::new("pw-cat")
        .args([
            "--playback",
            "--raw",
            "--format",
            "s16",
            "--rate",
            "48000",
            "--channels",
            "2",
        ])
        .args([
            "-P",
            &format!("{{ application.name = \"{APP}\" node.name = \"{STREAM}\" }}"),
        ])
        .arg(&path)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("pw-cat");
    let result = (|| -> Result<(), String> {
        std::thread::sleep(Duration::from_millis(800));
        let (engine, rx) = Engine::start(EngineConfig::default()).map_err(|e| e.to_string())?;
        recv_until(&rx, Duration::from_secs(3), |ev| {
            matches!(ev, Event::Connection(ConnectionState::Connected { .. })).then_some(())
        })
        .ok_or("not connected")?;
        let app = AppKey(format!("name:{APP}"));
        let rule = |key: &str| Setup {
            app_rules: vec![AppRule {
                app: app.clone(),
                target: RouteTarget::Device {
                    key: DeviceKey(key.into()),
                },
                display_name: APP.into(),
            }],
            ..Default::default()
        };
        for (step, sink) in [(1, B), (2, A), (3, B)] {
            engine
                .send(Command::SetSetup(rule(sink)))
                .map_err(|e| e.to_string())?;
            let deadline = Instant::now() + Duration::from_millis(1500);
            while Instant::now() < deadline {
                if let Ok(ev) = rx.try_recv() {
                    if let Event::ExternalRetarget { .. } = ev {
                        eprintln!("step {step}: UNEXPECTED {ev:?}");
                    }
                } else {
                    std::thread::sleep(Duration::from_millis(10));
                }
            }
            let now = linked_sink_of(STREAM);
            eprintln!("step {step}: rule → {sink}, linked to {now:?}");
            if now.as_deref() != Some(sink) {
                return Err(format!("step {step}: expected {sink}, got {now:?}"));
            }
        }
        engine
            .shutdown(Duration::from_secs(1))
            .map_err(|e| e.to_string())?;
        Ok(())
    })();
    let _ = player.kill();
    let _ = player.wait();
    let _ = std::fs::remove_file(&path);
    destroy_node_named(A);
    destroy_node_named(B);
    result.unwrap();
}

// ---------------------------------------------------------------------------------------------
// M8: reconciler against the live graph. Every sound goes into null sinks; nothing is audible.

/// `node.name`s starting with `estuary.` that are channels, virtual mics or DSP nodes.
fn estuary_nodes() -> std::collections::BTreeSet<String> {
    let out = std::process::Command::new("pw-cli")
        .args(["ls", "Node"])
        .output()
        .expect("pw-cli");
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| l.trim().strip_prefix("node.name = \""))
        .map(|n| n.trim_end_matches('"').to_owned())
        .filter(|n| {
            [
                "estuary.channel.",
                "estuary.vmic.",
                "estuary.cell.",
                "estuary.denoise.",
            ]
            .iter()
            .any(|p| n.starts_with(p))
        })
        .collect()
}

/// `pw-link -l` lines "<out node>:<port> -> <in node>:<port>".
fn links() -> Vec<(String, String)> {
    let out = std::process::Command::new("pw-link")
        .arg("-l")
        .output()
        .expect("pw-link");
    let text = String::from_utf8_lossy(&out.stdout);
    let mut v = Vec::new();
    let mut current = String::new();
    for line in text.lines() {
        if let Some(to) = line.trim().strip_prefix("|->") {
            v.push((current.clone(), to.trim().to_owned()));
        } else if !line.trim().starts_with("|<-") {
            current = line.trim().to_owned();
        }
    }
    v
}

fn wait_for(within: Duration, mut f: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + within;
    while Instant::now() < deadline {
        if f() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    f()
}

/// s16 stereo 48 kHz: a 1 kHz sine at `amp` (0 for silence).
fn write_tone(path: &std::path::Path, secs: usize, amp: f32) {
    let mut pcm = Vec::with_capacity(48_000 * secs * 4);
    for i in 0..48_000 * secs {
        #[allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)]
        let v = (amp * (2.0 * std::f32::consts::PI * 1000.0 * i as f32 / 48_000.0).sin() * 32767.0)
            as i16;
        pcm.extend_from_slice(&v.to_le_bytes());
        pcm.extend_from_slice(&v.to_le_bytes());
    }
    std::fs::write(path, pcm).unwrap();
}

fn play_into(target: &str, path: &std::path::Path, app: &str) -> std::process::Child {
    std::process::Command::new("pw-cat")
        .args([
            "--playback",
            "--raw",
            "--format",
            "s16",
            "--rate",
            "48000",
            "--channels",
            "2",
        ])
        .args(["--target", target])
        // WirePlumber would otherwise restore a volume saved for an earlier pw-cat stream.
        .args(["--volume", "1.0"])
        .args([
            "-P",
            &format!("{{ application.name = \"{app}\" state.restore-props = false }}"),
        ])
        .arg(path)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("pw-cat")
}

fn connected_engine() -> (Engine, async_channel::Receiver<Event>) {
    let (engine, rx) = Engine::start(EngineConfig::default()).unwrap();
    recv_until(&rx, Duration::from_secs(3), |ev| {
        matches!(ev, Event::Connection(ConnectionState::Connected { .. })).then_some(())
    })
    .expect("Connected");
    (engine, rx)
}

/// Waits for a snapshot in which every listed cell is Active.
fn wait_cells_active(
    rx: &async_channel::Receiver<Event>,
    cells: &[estuary_engine::CellKey],
    within: Duration,
) -> Result<(), String> {
    let mut last = Vec::new();
    recv_until(rx, within, |ev| match ev {
        Event::Snapshot(s) => {
            last = s.cells.iter().map(|c| c.status.clone()).collect();
            cells
                .iter()
                .all(|k| {
                    s.cells
                        .iter()
                        .any(|c| &c.key == k && c.status == estuary_engine::CellStatus::Active)
                })
                .then_some(())
        }
        _ => None,
    })
    .ok_or_else(|| format!("cells not active; last statuses {last:?}"))
}

/// Max peak per target over `window`, plus the fraction of reads below `floor` (stalls).
fn sample_peaks(
    engine: &Engine,
    assignments: &[(
        estuary_engine::MeterTarget,
        Vec<estuary_engine::MeterSlotId>,
    )],
    window: Duration,
    floor: f32,
) -> Vec<(f32, f32)> {
    let bank = engine.meters();
    let mut best = vec![0.0f32; assignments.len()];
    let mut low = vec![0u32; assignments.len()];
    let mut reads = 0u32;
    let deadline = Instant::now() + window;
    while Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
        reads += 1;
        for (i, (_, slots)) in assignments.iter().enumerate() {
            let p = slots
                .iter()
                .map(|s| {
                    let r = bank.read(*s);
                    r.peak[0].max(r.peak[1])
                })
                .fold(0.0f32, f32::max);
            best[i] = best[i].max(p);
            if p < floor {
                low[i] += 1;
            }
        }
    }
    #[allow(clippy::cast_precision_loss)]
    best.into_iter()
        .zip(low)
        .map(|(b, l)| (b, l as f32 / reads.max(1) as f32))
        .collect()
}

fn meter(
    engine: &Engine,
    rx: &async_channel::Receiver<Event>,
    targets: Vec<estuary_engine::MeterTarget>,
) -> Vec<(
    estuary_engine::MeterTarget,
    Vec<estuary_engine::MeterSlotId>,
)> {
    let n = targets.len();
    engine
        .send(estuary_engine::Command::SetMeterTargets(targets))
        .unwrap();
    let mut last = None;
    recv_until(rx, Duration::from_secs(2), |ev| match ev {
        Event::MeterAssignments(a) if a.len() == n && a.iter().all(|(_, s)| !s.is_empty()) => {
            Some(a.clone())
        }
        Event::MeterAssignments(a) => {
            last = Some(a.clone());
            None
        }
        _ => None,
    })
    .unwrap_or_else(|| panic!("meter assignments; last {last:?}"))
}

/// M8.14 `preset_builds` + `cell_gain_audible` + `teardown_leaves_nothing`, on null sinks.
/// Also UNVERIFIED-4 (part one): a virtual mic fed only by cells delivers steady audio.
#[test]
#[ignore = "live-pw: needs a running PipeWire + WirePlumber session"]
fn preset_builds_gain_is_audible_and_teardown_is_clean() {
    use estuary_engine::setup::{CellDef, ChannelDef, VirtualMicDef};
    use estuary_engine::{
        ChannelId, Command, DeviceKey, MatrixDest, MatrixSource, MeterTarget, Setup, VirtualMicId,
        Volume,
    };
    const OUT: &str = "estuary-test-out";
    init_logs();
    create_null_sink(OUT);
    let before = estuary_nodes();
    let path = std::env::temp_dir().join("estuary-preset-test.raw");
    write_tone(&path, 20, 0.5);

    let music = ChannelId::new_random();
    let vmic = VirtualMicId::new_random();
    let to_out = CellDef {
        source: MatrixSource::Channel { id: music },
        dest: MatrixDest::Output {
            key: DeviceKey(OUT.into()),
        },
        gain_db: 0.0,
        enabled: true,
    };
    let to_vmic = CellDef {
        source: MatrixSource::Channel { id: music },
        dest: MatrixDest::VirtualMic { id: vmic },
        gain_db: -6.0,
        enabled: true,
    };
    let setup = Setup {
        channels: vec![ChannelDef {
            id: music,
            name: "Test Music".into(),
            volume: Volume::unity(2),
            position: None,
        }],
        virtual_mics: vec![VirtualMicDef {
            id: vmic,
            name: "Test Mic".into(),
            volume: Volume::unity(2),
            position: None,
        }],
        cells: vec![to_out.clone(), to_vmic.clone()],
        ..Default::default()
    };
    let channel_node = format!("estuary.channel.{}", music.0.simple());
    let (engine, rx) = connected_engine();
    let mut player = None;

    let result = (|| -> Result<(), String> {
        let t0 = Instant::now();
        engine
            .send(Command::SetSetup(setup.clone()))
            .map_err(|e| e.to_string())?;
        if let Err(e) =
            wait_cells_active(&rx, &[to_out.key(), to_vmic.key()], Duration::from_secs(3))
        {
            eprintln!("nodes: {:?}", estuary_nodes());
            let l: Vec<_> = links()
                .into_iter()
                .filter(|(a, b)| a.starts_with("estuary") || b.starts_with("estuary"))
                .collect();
            eprintln!("links: {l:#?}");
            let _ = std::process::Command::new("sh").args(["-c", "pw-cli ls Port | grep -B2 -A12 'estuary\\.'| grep -E 'id |port.name|node.id|audio.channel|port.direction|port.monitor'"]).status();
            return Err(e);
        }
        eprintln!("preset active in {:?}", t0.elapsed());
        let created: Vec<String> = estuary_nodes().difference(&before).cloned().collect();
        eprintln!("created nodes: {created:?}");
        if created.len() != 4 {
            return Err(format!(
                "expected channel + vmic + 2 cells, got {created:?}"
            ));
        }
        let ours: Vec<_> = links()
            .into_iter()
            .filter(|(a, b)| a.starts_with("estuary.") || b.starts_with("estuary."))
            .filter(|(a, b)| !a.contains("meter") && !b.contains("meter"))
            .collect();
        eprintln!("links: {ours:#?}");
        if ours.len() != 8 {
            return Err(format!(
                "expected 8 links (2 cells × 4), got {}",
                ours.len()
            ));
        }

        player = Some(play_into(&channel_node, &path, "Estuary Preset Test"));
        let a = meter(
            &engine,
            &rx,
            vec![
                MeterTarget::Device(DeviceKey(OUT.into())),
                MeterTarget::VirtualMic(vmic),
            ],
        );
        std::thread::sleep(Duration::from_millis(500));
        let p = sample_peaks(&engine, &a, Duration::from_secs(3), 0.1);
        eprintln!(
            "0 dB: out peak {:.3} (low {:.0}%), vmic peak {:.3} (low {:.0}%)",
            p[0].0,
            p[0].1 * 100.0,
            p[1].0,
            p[1].1 * 100.0
        );
        if !(0.4..0.6).contains(&p[0].0) {
            return Err(format!("out peak {:.3}, expected ≈0.5", p[0].0));
        }
        if !(0.18..0.32).contains(&p[1].0) {
            return Err(format!("vmic peak {:.3}, expected ≈0.25 (−6 dB)", p[1].0));
        }
        if p[1].1 > 0.05 {
            return Err(format!("vmic stalled {:.0}% of the time", p[1].1 * 100.0));
        }

        // Cell gain: −60 dB is effectively silent, then back.
        engine
            .send(Command::SetCellGain {
                cell: to_out.key(),
                gain_db: -60.0,
            })
            .map_err(|e| e.to_string())?;
        std::thread::sleep(Duration::from_millis(300));
        let p = sample_peaks(&engine, &a, Duration::from_millis(600), 0.1);
        eprintln!("−60 dB: out peak {:.4}", p[0].0);
        if p[0].0 > 0.01 {
            return Err(format!("out peak {:.4} at −60 dB", p[0].0));
        }
        engine
            .send(Command::SetCellGain {
                cell: to_out.key(),
                gain_db: 0.0,
            })
            .map_err(|e| e.to_string())?;
        std::thread::sleep(Duration::from_millis(300));
        let p = sample_peaks(&engine, &a, Duration::from_millis(600), 0.1);
        if p[0].0 < 0.4 {
            return Err(format!("out peak {:.3} back at 0 dB", p[0].0));
        }

        // Teardown removes all of it.
        engine.send(Command::Teardown).map_err(|e| e.to_string())?;
        recv_until(&rx, Duration::from_secs(3), |ev| {
            matches!(ev, Event::TeardownComplete).then_some(())
        })
        .ok_or("no TeardownComplete")?;
        if !wait_for(Duration::from_secs(1), || estuary_nodes() == before) {
            return Err(format!(
                "left behind: {:?}",
                estuary_nodes().difference(&before).collect::<Vec<_>>()
            ));
        }
        Ok(())
    })();

    if let Some(mut p) = player {
        let _ = p.kill();
        let _ = p.wait();
    }
    let _ = engine.shutdown(Duration::from_secs(1));
    let _ = std::fs::remove_file(&path);
    destroy_node_named(OUT);
    result.unwrap();
}

/// Child half of `sigkill_leaves_nothing`: builds a channel and a cell, then waits to be killed.
#[test]
#[ignore = "live-pw: helper process for sigkill_leaves_nothing"]
fn sigkill_helper() {
    use estuary_engine::setup::{CellDef, ChannelDef};
    use estuary_engine::{ChannelId, Command, DeviceKey, MatrixDest, MatrixSource, Setup, Volume};
    let Ok(out) = std::env::var("ESTUARY_SIGKILL_OUT") else {
        return;
    };
    let id = ChannelId::new_random();
    let cell = CellDef {
        source: MatrixSource::Channel { id },
        dest: MatrixDest::Output {
            key: DeviceKey(out),
        },
        gain_db: 0.0,
        enabled: true,
    };
    let (engine, rx) = connected_engine();
    engine
        .send(Command::SetSetup(Setup {
            channels: vec![ChannelDef {
                id,
                name: "Kill Me".into(),
                volume: Volume::unity(2),
                position: None,
            }],
            cells: vec![cell.clone()],
            ..Default::default()
        }))
        .unwrap();
    wait_cells_active(&rx, &[cell.key()], Duration::from_secs(3)).unwrap();
    #[allow(clippy::print_stdout)] // the parent test reads this line
    {
        println!("ESTUARY-READY");
    }
    std::thread::sleep(Duration::from_secs(30));
}

/// M8.14 `sigkill_leaves_nothing`: our nodes are client-owned and vanish with the process.
#[test]
#[ignore = "live-pw: needs a running PipeWire + WirePlumber session"]
fn sigkill_leaves_nothing() {
    use std::io::BufRead;
    const OUT: &str = "estuary-test-kill-out";
    init_logs();
    create_null_sink(OUT);
    let before = estuary_nodes();
    let mut child = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "sigkill_helper",
            "--exact",
            "--ignored",
            "--nocapture",
            "--test-threads=1",
        ])
        .env("ESTUARY_SIGKILL_OUT", OUT)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let stdout = child.stdout.take().unwrap();
    let ready = std::io::BufReader::new(stdout)
        .lines()
        .map_while(Result::ok)
        .any(|l| l.contains("ESTUARY-READY"));
    let result = (|| -> Result<(), String> {
        if !ready {
            return Err("helper never got ready".into());
        }
        let created: Vec<String> = estuary_nodes().difference(&before).cloned().collect();
        eprintln!("helper created {created:?}");
        if created.len() != 2 {
            return Err(format!("expected channel + cell, got {created:?}"));
        }
        let t0 = Instant::now();
        let _ = child.kill(); // SIGKILL
        let _ = child.wait();
        if !wait_for(Duration::from_secs(3), || estuary_nodes() == before) {
            return Err(format!(
                "left behind: {:?}",
                estuary_nodes().difference(&before).collect::<Vec<_>>()
            ));
        }
        eprintln!("gone {:?} after SIGKILL", t0.elapsed());
        Ok(())
    })();
    let _ = child.kill();
    let _ = child.wait();
    destroy_node_named(OUT);
    result.unwrap();
}

/// Node state from `pw-cli info` ("suspended", "idle", "running", …).
fn node_state(name: &str) -> Option<String> {
    let id = node_id(name)?;
    let out = std::process::Command::new("pw-cli")
        .args(["info", &id])
        .output()
        .ok()?;
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .find_map(|l| {
            l.trim_start_matches(['*', ' ', '\t'])
                .strip_prefix("state: ")
        })
        .map(|s| s.trim_matches('"').to_owned())
}

/// UNVERIFIED-1 (engine M8.1): with passive cells, the real default output still suspends when
/// idle and wakes when an app plays into a channel. Plays silence only. Needs real hardware, so
/// it runs only with `ESTUARY_LIVE_HW=1`.
#[test]
#[ignore = "live-pw: real hardware; set ESTUARY_LIVE_HW=1"]
fn passive_cells_let_the_output_suspend() {
    use estuary_engine::setup::{CellDef, ChannelDef};
    use estuary_engine::{ChannelId, Command, MatrixDest, MatrixSource, Setup, Volume};
    if std::env::var("ESTUARY_LIVE_HW").is_err() {
        eprintln!("skipped: set ESTUARY_LIVE_HW=1");
        return;
    }
    init_logs();
    let (engine, rx) = connected_engine();
    let out = recv_until(&rx, Duration::from_secs(2), |ev| match ev {
        Event::Snapshot(s) => s.default_output.clone(),
        _ => None,
    })
    .expect("default output");
    let path = std::env::temp_dir().join("estuary-suspend-test.raw");
    write_tone(&path, 4, 0.0);
    let id = ChannelId::new_random();
    let cell = CellDef {
        source: MatrixSource::Channel { id },
        dest: MatrixDest::Output { key: out.clone() },
        gain_db: 0.0,
        enabled: true,
    };
    let result = (|| -> Result<(), String> {
        let state = || node_state(&out.0).unwrap_or_default();
        eprintln!("default output {} is {}", out.0, state());
        engine
            .send(Command::SetSetup(Setup {
                channels: vec![ChannelDef {
                    id,
                    name: "Suspend Test".into(),
                    volume: Volume::unity(2),
                    position: None,
                }],
                cells: vec![cell.clone()],
                ..Default::default()
            }))
            .map_err(|e| e.to_string())?;
        wait_cells_active(&rx, &[cell.key()], Duration::from_secs(3))?;
        let idle = wait_for(Duration::from_secs(9), || state() == "suspended");
        eprintln!(
            "with the cell and no app: {} (suspended within 9 s: {idle})",
            state()
        );
        let mut p = play_into(
            &format!("estuary.channel.{}", id.0.simple()),
            &path,
            "Estuary Suspend Test",
        );
        let woke = wait_for(Duration::from_secs(2), || state() == "running");
        eprintln!(
            "app playing into the channel: {} (running: {woke})",
            state()
        );
        let _ = p.wait();
        let back = wait_for(Duration::from_secs(9), || state() == "suspended");
        eprintln!(
            "after the app stopped: {} (suspended again: {back})",
            state()
        );
        if !(idle && woke && back) {
            return Err(format!("idle={idle} woke={woke} back={back}"));
        }
        Ok(())
    })();
    let _ = engine.send(Command::Teardown);
    let _ = recv_until(&rx, Duration::from_secs(3), |ev| {
        matches!(ev, Event::TeardownComplete).then_some(())
    });
    let _ = engine.shutdown(Duration::from_secs(1));
    let _ = std::fs::remove_file(&path);
    result.unwrap();
}

/// UNVERIFIED-4 (engine M8): a virtual mic fed by the hardware mic (its ALSA clock) and by a
/// channel, read by a recorder (our meter stream), delivers steady audio. Opens the mic for a
/// few seconds; nothing is stored or played. Runs only with `ESTUARY_LIVE_HW=1`.
#[test]
#[ignore = "live-pw: real hardware; set ESTUARY_LIVE_HW=1"]
fn virtual_mic_mixes_two_clocks() {
    use estuary_engine::setup::{CellDef, ChannelDef, VirtualMicDef};
    use estuary_engine::{
        ChannelId, Command, MatrixDest, MatrixSource, MeterTarget, Setup, VirtualMicId, Volume,
    };
    if std::env::var("ESTUARY_LIVE_HW").is_err() {
        eprintln!("skipped: set ESTUARY_LIVE_HW=1");
        return;
    }
    init_logs();
    let (engine, rx) = connected_engine();
    let mic = recv_until(&rx, Duration::from_secs(2), |ev| match ev {
        Event::Snapshot(s) => s.default_input.clone(),
        _ => None,
    })
    .expect("default input");
    let path = std::env::temp_dir().join("estuary-vmic-test.raw");
    write_tone(&path, 8, 0.5);
    let ch = ChannelId::new_random();
    let vm = VirtualMicId::new_random();
    let cells = vec![
        CellDef {
            source: MatrixSource::Input { key: mic.clone() },
            dest: MatrixDest::VirtualMic { id: vm },
            gain_db: 0.0,
            enabled: true,
        },
        CellDef {
            source: MatrixSource::Channel { id: ch },
            dest: MatrixDest::VirtualMic { id: vm },
            gain_db: -6.0,
            enabled: true,
        },
    ];
    let mut player = None;
    let result = (|| -> Result<(), String> {
        eprintln!("mic {}", mic.0);
        engine
            .send(Command::SetSetup(Setup {
                channels: vec![ChannelDef {
                    id: ch,
                    name: "Clock Test".into(),
                    volume: Volume::unity(2),
                    position: None,
                }],
                virtual_mics: vec![VirtualMicDef {
                    id: vm,
                    name: "Clock Test Mic".into(),
                    volume: Volume::unity(2),
                    position: None,
                }],
                cells: cells.clone(),
                ..Default::default()
            }))
            .map_err(|e| e.to_string())?;
        wait_cells_active(
            &rx,
            &cells.iter().map(CellDef::key).collect::<Vec<_>>(),
            Duration::from_secs(3),
        )?;
        player = Some(play_into(
            &format!("estuary.channel.{}", ch.0.simple()),
            &path,
            "Estuary Clock Test",
        ));
        let a = meter(&engine, &rx, vec![MeterTarget::VirtualMic(vm)]);
        std::thread::sleep(Duration::from_millis(500));
        let p = sample_peaks(&engine, &a, Duration::from_secs(5), 0.15);
        eprintln!(
            "vmic peak {:.3}, below 0.15 in {:.1}% of 50 ms reads",
            p[0].0,
            p[0].1 * 100.0
        );
        let warned = std::iter::from_fn(|| rx.try_recv().ok())
            .filter(|e| matches!(e, Event::Warning(_)))
            .count();
        eprintln!("engine warnings: {warned}");
        if p[0].1 > 0.02 || p[0].0 < 0.2 {
            return Err("virtual mic stalled or too quiet".into());
        }
        Ok(())
    })();
    if let Some(mut p) = player {
        let _ = p.kill();
        let _ = p.wait();
    }
    let _ = engine.send(Command::Teardown);
    let _ = recv_until(&rx, Duration::from_secs(3), |ev| {
        matches!(ev, Event::TeardownComplete).then_some(())
    });
    let _ = engine.shutdown(Duration::from_secs(1));
    let _ = std::fs::remove_file(&path);
    result.unwrap();
}

/// M10.4 `denoise_attenuates`: white noise into a fake mic (a virtual source), through the
/// denoiser and a send into a virtual mic. The voice gate (set live) silences the noise and
/// bypass is transparent. (RNNoise's attenuation of synthetic noise isn't predictable; see the
/// dsp test `denoise_attenuates_noise`.)
/// Silent: null nodes and meter streams only.
#[test]
#[ignore = "live-pw: needs a running PipeWire + WirePlumber session"]
fn denoise_attenuates() {
    use estuary_engine::setup::{CellDef, DenoiserDef, VirtualMicDef};
    use estuary_engine::{
        Command, DeviceKey, MatrixDest, MatrixSource, MeterTarget, Setup, VirtualMicId, Volume,
    };
    const MIC: &str = "estuary-test-noisy-mic";
    init_logs();
    destroy_node_named(MIC);
    let ok = std::process::Command::new("pw-cli")
        .args([
            "create-node",
            "adapter",
            &format!(
                "{{ factory.name=support.null-audio-sink node.name={MIC} node.description=\"Noisy Mic\" \
                 media.class=Audio/Source/Virtual audio.position=[FL FR] object.linger=true }}"
            ),
        ])
        .status()
        .unwrap()
        .success();
    assert!(ok, "create the fake mic");
    // −30 dBFS RMS white noise, 12 s.
    let path = std::env::temp_dir().join("estuary-noise-test.raw");
    let amp = 10f32.powf(-30.0 / 20.0) * 3f32.sqrt();
    let mut seed = 9_u64; // the unit test's fixture (see denoise_attenuates_noise)
    let mut pcm = Vec::with_capacity(48_000 * 12 * 4);
    for _ in 0..48_000 * 12 {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        #[allow(clippy::cast_precision_loss)]
        let u = (seed >> 40) as f32 / (1u64 << 24) as f32 * 2.0 - 1.0;
        #[allow(clippy::cast_possible_truncation)] // |u·amp| < 1
        let v = (u * amp * 32767.0) as i16;
        pcm.extend_from_slice(&v.to_le_bytes());
        pcm.extend_from_slice(&v.to_le_bytes());
    }
    std::fs::write(&path, pcm).unwrap();
    let vmic = VirtualMicId::new_random();
    let mic = DeviceKey(MIC.into());
    let cell = CellDef {
        source: MatrixSource::Denoised { key: mic.clone() },
        dest: MatrixDest::VirtualMic { id: vmic },
        gain_db: 0.0,
        enabled: true,
    };
    let setup = |enabled| Setup {
        virtual_mics: vec![VirtualMicDef {
            id: vmic,
            name: "Denoise Test".into(),
            volume: Volume::unity(2),
            position: None,
        }],
        denoisers: vec![DenoiserDef {
            input: mic.clone(),
            enabled,
            vad_gate: 0.0,
        }],
        cells: vec![cell.clone()],
        ..Default::default()
    };
    let (engine, rx) = connected_engine();
    let mut player = None;
    let mut linker = Vec::new();
    let result = (|| -> Result<(), String> {
        engine
            .send(Command::SetSetup(setup(true)))
            .map_err(|e| e.to_string())?;
        wait_cells_active(&rx, &[cell.key()], Duration::from_secs(3))?;
        player = Some(
            std::process::Command::new("pw-cat")
                .args(["--playback", "--raw", "--format", "s16", "--rate", "48000", "--channels", "2"])
                .args(["--volume", "1.0"])
                .args([
                    "-P",
                    "{ application.name = \"Estuary Noise Test\" node.name = \"estuary-noise-player\" \
                     node.autoconnect = false state.restore-props = false }",
                ])
                .arg(&path)
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
                .map_err(|e| e.to_string())?,
        );
        std::thread::sleep(Duration::from_millis(500));
        for ch in ["FL", "FR"] {
            linker.push(
                std::process::Command::new("pw-link")
                    .args([
                        format!("estuary-noise-player:output_{ch}"),
                        format!("{MIC}:input_{ch}"),
                    ])
                    .status()
                    .map_err(|e| e.to_string())?,
            );
        }
        let a = meter(
            &engine,
            &rx,
            vec![
                MeterTarget::VirtualMic(vmic),
                MeterTarget::Device(mic.clone()),
                MeterTarget::Denoised(mic.clone()),
            ],
        );
        // Mean peak in dBFS per target over 2 s: [vmic, raw mic, denoiser output].
        let levels = |engine: &Engine| -> Vec<f64> {
            let bank = engine.meters();
            let mut sum = vec![0.0f64; a.len()];
            let mut n = 0u32;
            let end = Instant::now() + Duration::from_secs(2);
            while Instant::now() < end {
                std::thread::sleep(Duration::from_millis(50));
                n += 1;
                for (i, (_, slots)) in a.iter().enumerate() {
                    for s in slots {
                        let r = bank.read(*s);
                        sum[i] += f64::from(r.peak[0].max(r.peak[1]));
                    }
                }
            }
            sum.iter()
                .map(|x| 20.0 * (x / f64::from(n.max(1))).max(1e-9).log10())
                .collect()
        };
        std::thread::sleep(Duration::from_millis(1000));
        let on = levels(&engine);
        // The voice gate closes on noise: a large, deterministic effect of the wet path.
        engine
            .send(Command::SetVadGate {
                input: mic.clone(),
                gate: 0.9,
            })
            .map_err(|e| e.to_string())?;
        std::thread::sleep(Duration::from_millis(1000));
        let gated = levels(&engine);
        engine
            .send(Command::SetSetup(setup(false)))
            .map_err(|e| e.to_string())?;
        std::thread::sleep(Duration::from_millis(500));
        let off = levels(&engine);
        eprintln!("[vmic, raw mic, denoised] on {on:.1?} | gate 0.9 {gated:.1?} | off {off:.1?}");
        if off[0] < -60.0 {
            return Err(format!(
                "no signal reached the virtual mic ({:.1} dBFS)",
                off[0]
            ));
        }
        if gated[2] > -60.0 || gated[0] > -60.0 {
            return Err(format!("the voice gate didn't close: {gated:.1?}"));
        }
        if (off[0] - off[1]).abs() > 1.0 {
            return Err(format!("bypass changed the level: {off:.1?}"));
        }
        Ok(())
    })();
    if let Some(mut p) = player {
        let _ = p.kill();
        let _ = p.wait();
    }
    let _ = engine.send(Command::Teardown);
    let _ = recv_until(&rx, Duration::from_secs(3), |ev| {
        matches!(ev, Event::TeardownComplete).then_some(())
    });
    let _ = engine.shutdown(Duration::from_secs(1));
    let _ = std::fs::remove_file(&path);
    destroy_node_named(MIC);
    result.unwrap();
}
