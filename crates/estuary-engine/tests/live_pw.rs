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
