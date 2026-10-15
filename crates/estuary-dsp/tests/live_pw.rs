//! Live smoke tests (dsp M7.8). Run:
//! `cargo test -p estuary-dsp --features live-pw -- --ignored --test-threads=1`
//! Silent: the test tone has node.autoconnect=false and is only ever linked into our filters.
#![cfg(feature = "live-pw")]
#![allow(
    clippy::unwrap_used,
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss
)]

use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use estuary_dsp::{
    FilterNode, FilterState, Processor, STEREO_IN, STEREO_OUT, new_denoiser_node, new_gain_node,
};
use pipewire::context::ContextRc;
use pipewire::core::CoreRc;
use pipewire::loop_::Timeout;
use pipewire::main_loop::MainLoopRc;
use pipewire::properties::properties;

struct Pw {
    ml: MainLoopRc,
    _ctx: ContextRc,
    core: CoreRc,
}

fn connect() -> Pw {
    pipewire::init();
    let ml = MainLoopRc::new(None).unwrap();
    let ctx = ContextRc::new(&ml, None).unwrap();
    let core = ctx.connect_rc(None).unwrap();
    Pw {
        ml,
        _ctx: ctx,
        core,
    }
}

/// Run the client loop for `d` (our filters only answer the server while it iterates).
fn pump(pw: &Pw, d: Duration) {
    let end = Instant::now() + d;
    while Instant::now() < end {
        pw.ml
            .loop_()
            .iterate(Timeout::Finite(Duration::from_millis(5)));
    }
}

fn pump_until(pw: &Pw, timeout: Duration, mut done: impl FnMut() -> bool) -> bool {
    let end = Instant::now() + timeout;
    while Instant::now() < end {
        if done() {
            return true;
        }
        pw.ml
            .loop_()
            .iterate(Timeout::Finite(Duration::from_millis(5)));
    }
    done()
}

fn test_props(name: &str) -> pipewire::properties::PropertiesBox {
    properties! {
        "node.name" => name,
        "media.type" => "Audio",
        "media.category" => "Filter",
        "media.role" => "DSP",
        "node.autoconnect" => "false",
        "estuary.role" => "test",
    }
}

fn wait_paused(pw: &Pw, node: &FilterNode) {
    let ok = pump_until(pw, Duration::from_secs(3), || {
        matches!(node.state(), FilterState::Paused | FilterState::Streaming)
            && node.node_id().is_some()
    });
    assert!(
        ok,
        "filter reached Paused with a node id (state {:?})",
        node.state()
    );
}

fn pw_cli_ls(kind: &str) -> String {
    let out = Command::new("pw-cli").args(["ls", kind]).output().unwrap();
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// Records the peak it receives (max since the last reset), outputs silence.
struct Probe(Arc<AtomicU32>);
impl Processor for Probe {
    fn process(&mut self, ins: &[&[f32]], outs: &mut [&mut [f32]], n: usize, _rate: u32) {
        let mut peak = 0.0f32;
        for s in ins {
            for v in &s[..n] {
                peak = peak.max(v.abs());
            }
        }
        // Non-negative floats order like their bit patterns.
        self.0.fetch_max(peak.to_bits(), Ordering::Relaxed);
        for o in outs.iter_mut() {
            o[..n].fill(0.0);
        }
    }
}

/// A 0.5-amplitude 1 kHz tone stream that WirePlumber never links anywhere.
fn tone(name: &str) -> (Child, std::path::PathBuf) {
    let path = std::env::temp_dir().join(format!("{name}.raw"));
    let mut pcm = Vec::new();
    for i in 0..48_000 * 20 {
        let v = (0.5 * (2.0 * std::f32::consts::PI * 1000.0 * i as f32 / 48_000.0).sin() * 32767.0)
            as i16;
        pcm.extend_from_slice(&v.to_le_bytes());
        pcm.extend_from_slice(&v.to_le_bytes());
    }
    std::fs::write(&path, pcm).unwrap();
    let child = Command::new("pw-cat")
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
            &format!("{{ node.name = \"{name}\" node.autoconnect = false }}"),
        ])
        .arg(&path)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    (child, path)
}

/// `pw-link` in the background (it waits for the server; our loop keeps iterating meanwhile).
#[allow(dead_code)] // used by the churn test
fn link(out: &str, inp: &str) -> Child {
    Command::new("pw-link")
        .args([out, inp])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap()
}

fn read_peak(pw: &Pw, probe: &Arc<AtomicU32>, settle: Duration, measure: Duration) -> f32 {
    pump(pw, settle);
    probe.store(0, Ordering::Relaxed);
    pump(pw, measure);
    f32::from_bits(probe.load(Ordering::Relaxed))
}

#[test]
#[ignore = "live-pw: needs a running PipeWire session"]
fn filter_appears_in_registry_with_its_ports() {
    let pw = connect();
    let (node, _h) = new_gain_node(
        &pw.core,
        "estuary.test.gain",
        test_props("estuary.test.gain"),
        0.0,
    )
    .unwrap();
    wait_paused(&pw, &node);
    let id = node.node_id().unwrap();
    pump(&pw, Duration::from_millis(200));
    let ports = pw_cli_ls("Port");
    let mine: Vec<&str> = ports
        .split("\tid ")
        .filter(|b| b.contains(&format!("node.id = \"{id}\"")))
        .collect();
    let has = |name: &str, ch: &str| {
        mine.iter().any(|b| {
            b.contains(&format!("port.name = \"{name}\""))
                && b.contains(&format!("audio.channel = \"{ch}\""))
        })
    };
    assert_eq!(mine.len(), 4, "four ports on node {id}");
    for (n, c) in [
        ("input_FL", "FL"),
        ("input_FR", "FR"),
        ("output_FL", "FL"),
        ("output_FR", "FR"),
    ] {
        assert!(has(n, c), "port {n} / {c}");
    }
    // C11: props passed to FilterNode::new appear in the node's info props.
    let dump = Command::new("pw-dump")
        .arg(id.to_string())
        .output()
        .unwrap();
    assert!(
        String::from_utf8_lossy(&dump.stdout).contains("\"estuary.role\": \"test\""),
        "props reach the node info (C11)"
    );
}

#[test]
#[ignore = "live-pw: needs a running PipeWire session"]
fn gain_applies_in_graph() {
    let pw = connect();
    let (gain, handle) = new_gain_node(
        &pw.core,
        "estuary.test.gain",
        test_props("estuary.test.gain"),
        0.0,
    )
    .unwrap();
    let peak = Arc::new(AtomicU32::new(0));
    let probe = FilterNode::new(
        &pw.core,
        "estuary.test.probe",
        test_props("estuary.test.probe"),
        &STEREO_IN,
        &STEREO_OUT,
        Probe(Arc::clone(&peak)),
    )
    .unwrap();
    wait_paused(&pw, &gain);
    wait_paused(&pw, &probe);
    let (mut player, path) = tone("estuary-test-tone");
    pump(&pw, Duration::from_millis(500));
    let mut links: Vec<Child> = Vec::new();
    let mut run_link = |args: &[String]| {
        let mut c = Command::new("pw-link")
            .args(args)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let ok = pump_until(&pw, Duration::from_secs(2), || {
            c.try_wait().ok().flatten().is_some()
        });
        assert!(ok, "pw-link {args:?} finished");
        links.push(c);
    };
    // Reference: the tone straight into the probe. Its level depends on the stream volume
    // WirePlumber restores for pw-cat, so the gain node is judged relative to it.
    for ch in ["FL", "FR"] {
        run_link(&[
            format!("estuary-test-tone:output_{ch}"),
            format!("estuary.test.probe:input_{ch}"),
        ]);
    }
    let reference = read_peak(
        &pw,
        &peak,
        Duration::from_millis(800),
        Duration::from_millis(500),
    );
    for ch in ["FL", "FR"] {
        run_link(&[
            "-d".into(),
            format!("estuary-test-tone:output_{ch}"),
            format!("estuary.test.probe:input_{ch}"),
        ]);
        run_link(&[
            format!("estuary-test-tone:output_{ch}"),
            format!("estuary.test.gain:input_{ch}"),
        ]);
        run_link(&[
            format!("estuary.test.gain:output_{ch}"),
            format!("estuary.test.probe:input_{ch}"),
        ]);
    }
    let unity = read_peak(
        &pw,
        &peak,
        Duration::from_millis(500),
        Duration::from_millis(500),
    );
    handle.set_db(-18.0);
    let reduced = read_peak(
        &pw,
        &peak,
        Duration::from_millis(200),
        Duration::from_millis(500),
    );
    let unity_db = 20.0 * (unity / reference).log10();
    let ratio_db = 20.0 * (reduced / unity).log10();
    eprintln!(
        "direct = {reference:.4}, via gain 0 dB = {unity:.4} ({unity_db:+.2} dB), at -18 dB = {reduced:.4} ({ratio_db:+.2} dB)"
    );

    let _ = player.kill();
    let _ = player.wait();
    let _ = std::fs::remove_file(path);
    assert!(
        reference > 0.05,
        "the tone reached the probe (got {reference})"
    );
    assert!(
        unity_db.abs() < 0.2,
        "0 dB passes the signal unchanged (got {unity_db:+.2} dB)"
    );
    assert!(
        (ratio_db + 18.0).abs() < 0.5,
        "-18 dB applied (got {ratio_db:+.2} dB)"
    );
    assert!(gain.stats().processed_blocks.load(Ordering::Relaxed) > 0);
    assert!(!gain.stats().poisoned.load(Ordering::Relaxed));
}

#[test]
#[ignore = "live-pw: needs a running PipeWire session"]
fn create_link_drop_repeatedly_leaves_nothing_behind() {
    let pw = connect();
    let (mut player, path) = tone("estuary-test-churn");
    pump(&pw, Duration::from_millis(300));
    for i in 0..100 {
        let (node, _h) = new_gain_node(
            &pw.core,
            "estuary.test.churn",
            test_props("estuary.test.churn"),
            0.0,
        )
        .unwrap();
        wait_paused(&pw, &node);
        let mut l = link(
            "estuary-test-churn:output_FL",
            "estuary.test.churn:input_FL",
        );
        pump(&pw, Duration::from_millis(15));
        drop(node);
        pump(&pw, Duration::from_millis(5));
        let _ = l.kill();
        let _ = l.wait();
        if i % 25 == 0 {
            eprintln!("iteration {i}");
        }
    }
    pump(&pw, Duration::from_millis(300));
    let _ = player.kill();
    let _ = player.wait();
    let _ = std::fs::remove_file(path);
    assert!(
        !pw_cli_ls("Node").contains("estuary.test.churn"),
        "no leaked nodes"
    );
}

/// dsp M10.6: the denoiser node has the C5 ports and reports its 20 ms ProcessLatency.
#[test]
#[ignore = "live-pw: needs a running PipeWire session"]
fn denoiser_node_reports_its_latency() {
    let pw = connect();
    let (node, h) = new_denoiser_node(
        &pw.core,
        "estuary.test.denoise",
        test_props("estuary.test.denoise"),
    )
    .unwrap();
    h.set_vad_gate(0.5);
    wait_paused(&pw, &node);
    let id = node.node_id().unwrap();
    pump(&pw, Duration::from_millis(200));
    let ports = pw_cli_ls("Port");
    let mine = ports
        .split("\tid ")
        .filter(|b| b.contains(&format!("node.id = \"{id}\"")))
        .count();
    assert_eq!(mine, 4, "four ports on node {id}");
    // pw-dump may need our loop to answer, so keep pumping while it runs.
    let mut dump = Command::new("pw-dump")
        .arg(id.to_string())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut out = String::new();
    let done = pump_until(&pw, Duration::from_secs(3), || {
        matches!(dump.try_wait(), Ok(Some(_)))
    });
    assert!(done, "pw-dump finished");
    std::io::Read::read_to_string(dump.stdout.as_mut().unwrap(), &mut out).unwrap();
    let compact: String = out.split_whitespace().collect();
    assert!(
        compact.contains("\"ns\":20000000"),
        "ProcessLatency of 20 ms in the node's params:\n{out}"
    );
}

/// Sum of squares (×1e12, integer) and sample count of what it receives; outputs silence.
struct RmsProbe(
    Arc<std::sync::atomic::AtomicU64>,
    Arc<std::sync::atomic::AtomicU64>,
);
impl Processor for RmsProbe {
    fn process(&mut self, ins: &[&[f32]], outs: &mut [&mut [f32]], n: usize, _rate: u32) {
        let sq: f64 = ins[0][..n].iter().map(|v| f64::from(*v).powi(2)).sum();
        self.0.fetch_add((sq * 1e12) as u64, Ordering::Relaxed);
        self.1.fetch_add(n as u64, Ordering::Relaxed);
        for o in outs.iter_mut() {
            o[..n].fill(0.0);
        }
    }
}

/// dsp M10: the denoiser node runs in a live graph: bypass is transparent, the voice gate
/// closes on noise, no underruns.
#[test]
#[ignore = "live-pw: needs a running PipeWire session"]
fn denoiser_runs_in_graph() {
    let pw = connect();
    let (dn, handle) = new_denoiser_node(
        &pw.core,
        "estuary.test.denoise",
        test_props("estuary.test.denoise"),
    )
    .unwrap();
    let sum = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let count = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let probe = FilterNode::new(
        &pw.core,
        "estuary.test.probe",
        test_props("estuary.test.probe"),
        &STEREO_IN,
        &STEREO_OUT,
        RmsProbe(Arc::clone(&sum), Arc::clone(&count)),
    )
    .unwrap();
    wait_paused(&pw, &dn);
    wait_paused(&pw, &probe);
    // White noise at −30 dBFS RMS.
    let path = std::env::temp_dir().join("estuary-test-noise.raw");
    let amp = 10f32.powf(-30.0 / 20.0) * 3f32.sqrt();
    let mut seed = 9_u64; // the unit test's fixture (see denoise_attenuates_noise)
    let mut pcm = Vec::new();
    for _ in 0..48_000 * 20 {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        let u = (seed >> 40) as f32 / (1u64 << 24) as f32 * 2.0 - 1.0;
        let v = (u * amp * 32767.0) as i16;
        pcm.extend_from_slice(&v.to_le_bytes());
        pcm.extend_from_slice(&v.to_le_bytes());
    }
    std::fs::write(&path, pcm).unwrap();
    let mut player = Command::new("pw-cat")
        .args(["--playback", "--raw", "--format", "s16", "--rate", "48000", "--channels", "2"])
        .args(["--volume", "1.0"])
        .args([
            "-P",
            "{ node.name = \"estuary-test-noise\" node.autoconnect = false state.restore-props = false }",
        ])
        .arg(&path)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    pump(&pw, Duration::from_millis(500));
    for (o, i) in [
        (
            "estuary-test-noise:output_FL",
            "estuary.test.denoise:input_FL",
        ),
        (
            "estuary-test-noise:output_FR",
            "estuary.test.denoise:input_FR",
        ),
        (
            "estuary.test.denoise:output_FL",
            "estuary.test.probe:input_FL",
        ),
        (
            "estuary.test.denoise:output_FR",
            "estuary.test.probe:input_FR",
        ),
    ] {
        let mut c = link(o, i);
        assert!(pump_until(&pw, Duration::from_secs(2), || c
            .try_wait()
            .ok()
            .flatten()
            .is_some()));
    }
    let measure = |pw: &Pw| -> f64 {
        pump(pw, Duration::from_millis(1500));
        sum.store(0, Ordering::Relaxed);
        count.store(0, Ordering::Relaxed);
        pump(pw, Duration::from_secs(2));
        let ms =
            sum.load(Ordering::Relaxed) as f64 / 1e12 / count.load(Ordering::Relaxed).max(1) as f64;
        10.0 * ms.max(1e-18).log10()
    };
    let on = measure(&pw);
    handle.set_vad_gate(0.9);
    let gated = measure(&pw);
    handle.set_enabled(false);
    let off = measure(&pw);
    let stats = dn.stats();
    eprintln!(
        "in-graph RMS: off {off:.1} dBFS, on {on:.1} dBFS, gate 0.9 {gated:.1} dBFS; underruns {}, xruns {}, unsupported {}, rate {}, quantum {}",
        stats.underruns.load(Ordering::Relaxed),
        stats.xruns.load(Ordering::Relaxed),
        stats.unsupported_rate.load(Ordering::Relaxed),
        stats.last_rate.load(Ordering::Relaxed),
        stats.last_quantum.load(Ordering::Relaxed)
    );
    let _ = player.kill();
    let _ = player.wait();
    let _ = std::fs::remove_file(path);
    // RNNoise's attenuation of synthetic noise depends on the exact realization and its start
    // offset (offline `denoise_attenuates_noise`); the voice gate closing on noise doesn't.
    assert!(
        (off + 30.0).abs() < 0.5,
        "bypass passes the noise unchanged ({off:.1} dBFS)"
    );
    assert!(
        gated < -60.0,
        "the voice gate closed on noise ({gated:.1} dBFS)"
    );
    assert!(
        on < off + 1.0,
        "suppression doesn't amplify ({on:.1} vs {off:.1})"
    );
    assert_eq!(stats.underruns.load(Ordering::Relaxed), 0);
    assert_eq!(stats.unsupported_rate.load(Ordering::Relaxed), 0);
}
