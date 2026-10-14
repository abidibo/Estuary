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
