//! Live integration tests against the running PipeWire session (engine M2.10).
//! Run: `cargo test -p estuary-engine --features live-pw -- --ignored --test-threads=1`
#![cfg(feature = "live-pw")]
#![allow(clippy::unwrap_used)]

use std::time::{Duration, Instant};

use estuary_engine::{ConnectionState, Engine, EngineConfig, Event};

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
