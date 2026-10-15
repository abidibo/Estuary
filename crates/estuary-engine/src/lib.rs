//! Estuary PipeWire engine (master C2–C9, C13).
//!
//! The engine runs on its own thread (`estuary-pw`) and owns every PipeWire object. The UI talks
//! to it only through [`Command`]s and [`Event`]s, and reads meter peaks from [`MeterBank`].
#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used))]

pub mod appkey;
mod core;
pub mod error;
mod graph;
pub mod meter;
pub mod model;
mod pw;
pub mod setup;
#[cfg(test)]
mod testing;

use std::sync::{Arc, OnceLock, mpsc};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

pub use error::{
    Backoff, EngineError, EngineWarning, SetupIssue, UnsupportedReason, UsageKind,
    VirtualDeviceUsage, WarnedNode,
};
pub use meter::{MeterBank, MeterReading, MeterSlotId, MeterTarget};
pub use model::*;
pub use setup::Setup;

use pw::thread::{Control, panic_message};

#[derive(Debug, Clone)]
pub struct EngineConfig {
    /// `"io.github.abidibo.Estuary"` (C5 `application.id`).
    pub app_id: &'static str,
    /// `"Estuary"` (C5 `application.name`).
    pub app_name: &'static str,
    pub reconnect_backoff: Backoff,
}

impl Default for EngineConfig {
    fn default() -> Self {
        Self {
            app_id: "io.github.abidibo.Estuary",
            app_name: "Estuary",
            reconnect_backoff: Backoff::default(),
        }
    }
}

/// UI → engine (C3).
#[derive(Debug, Clone)]
pub enum Command {
    /// Full desired state. Idempotent; the engine reconciles against it.
    SetSetup(Setup),
    SetCellGain {
        cell: CellKey,
        gain_db: f32,
    },
    SetChannelVolume {
        id: ChannelId,
        volume: Volume,
    },
    SetVirtualMicVolume {
        id: VirtualMicId,
        volume: Volume,
    },
    SetDeviceVolume {
        key: DeviceKey,
        volume: Volume,
    },
    SetAppVolume {
        app: AppKey,
        volume: Volume,
    },
    SetVadGate {
        input: DeviceKey,
        gate: f32,
    },
    /// Replaces the meter subscription set (visible rows only).
    SetMeterTargets(Vec<MeterTarget>),
    /// Replies `Event::QuitCheck`.
    PrepareQuit,
    /// Replies `Event::TeardownComplete`.
    Teardown,
}

/// Engine → UI (C3).
#[derive(Debug, Clone)]
pub enum Event {
    Connection(ConnectionState),
    /// Coalesced: at most one per 50 ms.
    Snapshot(Arc<GraphSnapshot>),
    MeterAssignments(Vec<(MeterTarget, Vec<MeterSlotId>)>),
    ExternalRetarget {
        app: AppKey,
        target: RouteTarget,
    },
    SetupIssues(Vec<SetupIssue>),
    /// Setup rebuilt after a PipeWire reconnect.
    Restored,
    QuitCheck(Vec<VirtualDeviceUsage>),
    TeardownComplete,
    Warning(EngineWarning),
    Fatal(EngineError),
}

#[derive(Debug, Clone, PartialEq)]
pub enum ConnectionState {
    Connecting {
        attempt: u32,
    },
    Connected {
        pipewire_version: String,
        wireplumber_version: Option<String>,
    },
    Unsupported {
        reason: UnsupportedReason,
    },
    PermissionDenied,
    Reconnecting {
        since: Instant,
        attempt: u32,
    },
}

static PW_INIT: OnceLock<()> = OnceLock::new();

/// Handle to the engine thread.
pub struct Engine {
    ctl_tx: pipewire::channel::Sender<Control>,
    join: Option<JoinHandle<()>>,
    done_rx: mpsc::Receiver<()>,
    meters: Arc<MeterBank>,
}

impl std::fmt::Debug for Engine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Engine")
            .field("running", &self.join.is_some())
            .finish_non_exhaustive()
    }
}

impl Engine {
    /// Spawns the engine thread and starts connecting. Never blocks on PipeWire availability.
    pub fn start(
        cfg: EngineConfig,
    ) -> Result<(Engine, async_channel::Receiver<Event>), EngineError> {
        PW_INIT.get_or_init(pipewire::init);
        let (ev_tx, ev_rx) = async_channel::unbounded();
        let meters = Arc::new(MeterBank::new());
        let (ctl_tx, ctl_rx) = pipewire::channel::channel::<Control>();
        let (done_tx, done_rx) = mpsc::sync_channel::<()>(1);
        let join = std::thread::Builder::new()
            .name("estuary-pw".into())
            .spawn({
                let meters = meters.clone();
                let ctl_tx = ctl_tx.clone();
                move || {
                    let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        pw::thread::run(cfg, ctl_tx, ctl_rx, ev_tx.clone(), meters)
                    }));
                    let fatal = match res {
                        Ok(Ok(())) => None,
                        Ok(Err(e)) => Some(e),
                        Err(p) => Some(EngineError::Panicked(panic_message(p.as_ref()))),
                    };
                    if let Some(e) = fatal {
                        tracing::error!("engine stopped: {e}");
                        let _ = ev_tx.try_send(Event::Fatal(e));
                    }
                    let _ = done_tx.send(());
                }
            })
            .map_err(|e| EngineError::Spawn(e.to_string()))?;
        Ok((
            Engine {
                ctl_tx,
                join: Some(join),
                done_rx,
                meters,
            },
            ev_rx,
        ))
    }

    /// `Err` only if the engine thread is gone.
    pub fn send(&self, cmd: Command) -> Result<(), EngineError> {
        // A pipewire channel keeps accepting messages after its receiver is gone, so liveness
        // is taken from the thread itself.
        if self.join.as_ref().is_none_or(JoinHandle::is_finished) {
            return Err(EngineError::ThreadGone);
        }
        self.ctl_tx
            .send(Control::Cmd(cmd))
            .map_err(|_| EngineError::ThreadGone)
    }

    pub fn meters(&self) -> Arc<MeterBank> {
        self.meters.clone()
    }

    /// Stops the loop, drops all PipeWire objects (owned nodes die with the connection), joins.
    pub fn shutdown(mut self, timeout: Duration) -> Result<(), EngineError> {
        let _ = self.ctl_tx.send(Control::Shutdown);
        match self.done_rx.recv_timeout(timeout) {
            Ok(()) => {
                if let Some(j) = self.join.take() {
                    let _ = j.join();
                }
                Ok(())
            }
            // The thread is detached; process exit cleans up.
            Err(_) => Err(EngineError::ShutdownTimeout),
        }
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        if self.join.is_some() {
            let _ = self.ctl_tx.send(Control::Shutdown);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // These need libpipewire but no running daemon: without one the engine keeps retrying.

    #[test]
    fn send_fails_once_the_thread_is_gone() {
        let (engine, rx) = Engine::start(EngineConfig::default()).unwrap();
        match rx.recv_blocking().unwrap() {
            // Running (which connection report depends on the host): stop the loop without
            // consuming the handle, as a crashed thread would.
            Event::Connection(_) => {
                assert!(engine.send(Command::SetMeterTargets(Vec::new())).is_ok());
                engine.ctl_tx.send(Control::Shutdown).unwrap();
                engine.done_rx.recv_timeout(Duration::from_secs(1)).unwrap();
            }
            // No PipeWire configuration on the host (CI runners): the thread already ended.
            Event::Fatal(_) => {}
            other => panic!("unexpected first event {other:?}"),
        }
        let deadline = Instant::now() + Duration::from_secs(1);
        while !engine.join.as_ref().unwrap().is_finished() && Instant::now() < deadline {
            std::thread::yield_now();
        }
        assert_eq!(
            engine.send(Command::PrepareQuit).unwrap_err(),
            EngineError::ThreadGone
        );
    }

    #[test]
    fn shutdown_is_prompt() {
        let (engine, _rx) = Engine::start(EngineConfig::default()).unwrap();
        let t = Instant::now();
        engine.shutdown(Duration::from_secs(1)).unwrap();
        assert!(t.elapsed() < Duration::from_millis(500));
    }
}
