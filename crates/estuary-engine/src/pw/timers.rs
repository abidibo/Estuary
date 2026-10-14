//! One-shot timers for the engine loop.
//!
//! pipewire-rs `TimerSource<'l>` borrows the loop, so it cannot live in the shell's shared state
//! next to the loop it borrows. Instead a small thread keeps the deadlines and posts expirations
//! back into the PipeWire loop through the control channel. No periodic wakeups when idle.

use std::collections::HashMap;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use super::thread::Control;
use crate::core::effects::TimerKind;

enum Req {
    Arm(TimerKind, Instant),
    Cancel(TimerKind),
}

#[derive(Debug)]
pub(crate) struct Timers {
    tx: mpsc::Sender<Req>,
}

impl Timers {
    /// Spawns the timer thread. It exits when `Timers` is dropped.
    pub fn spawn(ctl: pipewire::channel::Sender<Control>) -> std::io::Result<Self> {
        let (tx, rx) = mpsc::channel::<Req>();
        std::thread::Builder::new()
            .name("estuary-timers".into())
            .spawn(move || run(&rx, &ctl))?;
        Ok(Self { tx })
    }

    /// Arm (or re-arm) `kind` to fire once after `after`.
    pub fn arm(&self, kind: TimerKind, after: Duration) {
        let _ = self.tx.send(Req::Arm(kind, Instant::now() + after));
    }

    pub fn cancel(&self, kind: TimerKind) {
        let _ = self.tx.send(Req::Cancel(kind));
    }
}

fn run(rx: &mpsc::Receiver<Req>, ctl: &pipewire::channel::Sender<Control>) {
    let mut deadlines: HashMap<TimerKind, Instant> = HashMap::new();
    loop {
        let now = Instant::now();
        let due: Vec<TimerKind> = deadlines
            .iter()
            .filter(|(_, at)| **at <= now)
            .map(|(k, _)| *k)
            .collect();
        for kind in due {
            deadlines.remove(&kind);
            if ctl.send(Control::Timer(kind)).is_err() {
                return; // engine loop is gone
            }
        }
        let req = match deadlines.values().min() {
            Some(next) => match rx.recv_timeout(next.saturating_duration_since(Instant::now())) {
                Ok(r) => r,
                Err(mpsc::RecvTimeoutError::Timeout) => continue,
                Err(mpsc::RecvTimeoutError::Disconnected) => return,
            },
            None => match rx.recv() {
                Ok(r) => r,
                Err(_) => return,
            },
        };
        match req {
            Req::Arm(kind, at) => {
                deadlines.insert(kind, at);
            }
            Req::Cancel(kind) => {
                deadlines.remove(&kind);
            }
        }
    }
}
