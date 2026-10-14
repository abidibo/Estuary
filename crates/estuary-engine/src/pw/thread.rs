//! Engine thread entry, panic containment and the control channel (engine M2.5).

use std::cell::RefCell;
use std::panic::AssertUnwindSafe;
use std::sync::Arc;

use pipewire::context::ContextRc;
use pipewire::main_loop::MainLoopRc;

use super::Shell;
use super::timers::Timers;
use crate::core::effects::TimerKind;
use crate::meter::MeterBank;
use crate::{Command, EngineConfig, EngineError, Event};

/// Messages into the engine loop. `Shutdown` and timers stay out of the public `Command` (C3).
#[derive(Debug)]
pub(crate) enum Control {
    Cmd(Command),
    Shutdown,
    Timer(TimerKind),
    /// Drop connections retired inside a callback, now that no callback is running.
    DropGraveyard,
}

thread_local! {
    static PANICKED: RefCell<Option<String>> = const { RefCell::new(None) };
}

pub(crate) fn panic_message(p: &(dyn std::any::Any + Send)) -> String {
    p.downcast_ref::<&str>()
        .map(|s| (*s).to_owned())
        .or_else(|| p.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "unknown panic".to_owned())
}

/// Wraps every PipeWire callback body. pipewire-rs invokes closures from `extern "C"`
/// trampolines, where an unwinding panic would abort the process. A panic is recorded, the loop
/// is stopped, and `run` re-raises it outside the loop so the thread boundary reports `Fatal`.
pub(crate) fn guarded<R: Default>(ml: &MainLoopRc, f: impl FnOnce() -> R) -> R {
    match std::panic::catch_unwind(AssertUnwindSafe(f)) {
        Ok(r) => r,
        Err(p) => {
            let msg = panic_message(p.as_ref());
            PANICKED.with(|c| *c.borrow_mut() = Some(msg));
            ml.quit();
            R::default()
        }
    }
}

pub(crate) fn run(
    cfg: EngineConfig,
    ctl_tx: pipewire::channel::Sender<Control>,
    ctl_rx: pipewire::channel::Receiver<Control>,
    ev_tx: async_channel::Sender<Event>,
    meters: Arc<MeterBank>,
) -> Result<(), EngineError> {
    let pw_err = |e: pipewire::Error| EngineError::PipeWire(e.to_string());
    let ml = MainLoopRc::new(None).map_err(pw_err)?;
    let ctx = ContextRc::new(&ml, None).map_err(pw_err)?;
    let timers = Timers::spawn(ctl_tx.clone()).map_err(|e| EngineError::Spawn(e.to_string()))?;
    let instance = uuid::Uuid::new_v4().simple().to_string();

    let shell = Shell::new(
        ml.clone(),
        ctx,
        cfg,
        instance,
        ev_tx,
        timers,
        ctl_tx,
        meters,
    );
    let _ctl = ctl_rx.attach(ml.loop_(), {
        let shell = shell.clone();
        let ml = ml.clone();
        move |c| guarded(&ml, || shell.on_control(c))
    });
    guarded(&ml, || shell.start());
    // A panic in `start` called `quit()` before `run()`, which would not stop the loop.
    if PANICKED.with(|c| c.borrow().is_none()) {
        ml.run();
    }
    shell.finish();

    if let Some(msg) = PANICKED.with(|c| c.borrow_mut().take()) {
        std::panic::resume_unwind(Box::new(msg));
    }
    Ok(())
}
