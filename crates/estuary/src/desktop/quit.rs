//! The quit state machine (desktop M11.D5, C10). Pure: inputs in, effects out; the root
//! component executes the effects.

use std::time::Duration;

use estuary_engine::VirtualDeviceUsage;

pub const QUIT_CHECK_TIMEOUT: Duration = Duration::from_secs(1);
pub const TEARDOWN_TIMEOUT: Duration = Duration::from_secs(2);
pub const ENGINE_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(1);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QuitTrigger {
    /// Menu, Ctrl+Q, `app.quit` over D-Bus (indistinguishable).
    Action,
    Tray,
    CloseButton,
    Signal(i32),
}

impl QuitTrigger {
    fn skips_confirmation(self) -> bool {
        matches!(self, Self::Signal(_))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum QuitTimer {
    Check,
    Teardown,
}

#[derive(Debug)]
pub enum QuitInput {
    Request {
        trigger: QuitTrigger,
        engine_connected: bool,
        engine_present: bool,
    },
    QuitCheck(Vec<VirtualDeviceUsage>),
    TimerFired(QuitTimer),
    UserConfirmed,
    UserCancelled,
    TeardownComplete,
    /// A second signal while quitting.
    Force,
}

#[derive(Debug, PartialEq)]
pub enum QuitEffect {
    SendPrepareQuit,
    /// The root presents the window first if it's hidden.
    ShowConfirm(Vec<VirtualDeviceUsage>),
    SendTeardown,
    ArmTimer(QuitTimer, Duration),
    DisarmTimer(QuitTimer),
    HideWindow,
    /// Close an open confirmation without a response (a signal overrides it).
    DismissConfirm,
    /// Engine shutdown, config flush, drop hold, tray shutdown, `app.quit()`.
    Finish,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QuitState {
    Idle,
    Checking { skip_confirm: bool },
    Confirming,
    TearingDown,
    Finished,
}

#[derive(Debug)]
pub struct QuitController {
    state: QuitState,
}

impl Default for QuitController {
    fn default() -> Self {
        Self {
            state: QuitState::Idle,
        }
    }
}

fn teardown() -> Vec<QuitEffect> {
    vec![
        QuitEffect::HideWindow,
        QuitEffect::SendTeardown,
        QuitEffect::ArmTimer(QuitTimer::Teardown, TEARDOWN_TIMEOUT),
    ]
}

impl QuitController {
    #[cfg_attr(not(test), allow(dead_code))] // the root asks `busy()`; tests read the state
    pub fn state(&self) -> QuitState {
        self.state
    }

    /// True while quitting is under way (the UI shows a spinner, content insensitive).
    pub fn busy(&self) -> bool {
        matches!(
            self.state,
            QuitState::Checking { .. } | QuitState::TearingDown
        )
    }

    pub fn handle(&mut self, input: QuitInput) -> Vec<QuitEffect> {
        use QuitEffect as E;
        use QuitInput as I;
        use QuitState as S;
        match (self.state, input) {
            (S::Finished, _) => Vec::new(),
            (
                S::Idle,
                I::Request {
                    trigger,
                    engine_connected,
                    engine_present,
                },
            ) => {
                if !engine_present || !engine_connected {
                    self.state = S::Finished;
                    vec![E::HideWindow, E::Finish]
                } else {
                    self.state = S::Checking {
                        skip_confirm: trigger.skips_confirmation(),
                    };
                    vec![
                        E::SendPrepareQuit,
                        E::ArmTimer(QuitTimer::Check, QUIT_CHECK_TIMEOUT),
                    ]
                }
            }
            (S::Checking { skip_confirm }, I::QuitCheck(u)) => {
                let mut fx = vec![E::DisarmTimer(QuitTimer::Check)];
                if u.is_empty() || skip_confirm {
                    self.state = S::TearingDown;
                    fx.extend(teardown());
                } else {
                    self.state = S::Confirming;
                    fx.push(E::ShowConfirm(u));
                }
                fx
            }
            // Fail open: never block quitting on an engine that doesn't answer.
            (S::Checking { .. }, I::TimerFired(QuitTimer::Check)) => {
                self.state = S::TearingDown;
                teardown()
            }
            (S::Confirming, I::UserConfirmed) => {
                self.state = S::TearingDown;
                teardown()
            }
            (S::Confirming, I::UserCancelled) => {
                self.state = S::Idle;
                Vec::new()
            }
            (
                S::Confirming,
                I::Request {
                    trigger: QuitTrigger::Signal(_),
                    ..
                },
            ) => {
                self.state = S::TearingDown;
                let mut fx = vec![E::DismissConfirm];
                fx.extend(teardown());
                fx
            }
            (S::TearingDown, I::TeardownComplete) => {
                self.state = S::Finished;
                vec![E::DisarmTimer(QuitTimer::Teardown), E::Finish]
            }
            (S::TearingDown, I::TimerFired(QuitTimer::Teardown)) => {
                self.state = S::Finished;
                vec![E::Finish]
            }
            (S::Idle, I::Force) => Vec::new(),
            (_, I::Force) => {
                self.state = S::Finished;
                vec![E::Finish]
            }
            // Already quitting, a dialog already shown, or stale inputs: nothing.
            _ => Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use estuary_engine::UsageKind;

    fn req(trigger: QuitTrigger) -> QuitInput {
        QuitInput::Request {
            trigger,
            engine_connected: true,
            engine_present: true,
        }
    }

    fn usage() -> Vec<VirtualDeviceUsage> {
        vec![VirtualDeviceUsage {
            app_display_name: "Discord".into(),
            device_name: "Virtual mic".into(),
            kind: UsageKind::RecordingFromVirtualMic,
        }]
    }

    fn teardown_fx() -> Vec<QuitEffect> {
        teardown()
    }

    #[test]
    fn engine_absent_or_disconnected_finishes_at_once() {
        for (present, connected) in [(false, false), (true, false)] {
            let mut q = QuitController::default();
            let fx = q.handle(QuitInput::Request {
                trigger: QuitTrigger::Action,
                engine_connected: connected,
                engine_present: present,
            });
            assert_eq!(fx, vec![QuitEffect::HideWindow, QuitEffect::Finish]);
            assert_eq!(q.state(), QuitState::Finished);
        }
    }

    #[test]
    fn no_usages_tears_down_without_asking() {
        let mut q = QuitController::default();
        assert_eq!(
            q.handle(req(QuitTrigger::Action)),
            vec![
                QuitEffect::SendPrepareQuit,
                QuitEffect::ArmTimer(QuitTimer::Check, QUIT_CHECK_TIMEOUT)
            ]
        );
        assert!(q.busy());
        let mut expected = vec![QuitEffect::DisarmTimer(QuitTimer::Check)];
        expected.extend(teardown_fx());
        assert_eq!(q.handle(QuitInput::QuitCheck(Vec::new())), expected);
        assert_eq!(q.state(), QuitState::TearingDown);
        assert_eq!(
            q.handle(QuitInput::TeardownComplete),
            vec![
                QuitEffect::DisarmTimer(QuitTimer::Teardown),
                QuitEffect::Finish
            ]
        );
        assert_eq!(q.state(), QuitState::Finished);
        assert!(
            q.handle(req(QuitTrigger::Action)).is_empty(),
            "finished is final"
        );
    }

    #[test]
    fn usages_ask_and_cancel_returns_to_idle() {
        let mut q = QuitController::default();
        q.handle(req(QuitTrigger::Tray));
        let fx = q.handle(QuitInput::QuitCheck(usage()));
        assert_eq!(
            fx,
            vec![
                QuitEffect::DisarmTimer(QuitTimer::Check),
                QuitEffect::ShowConfirm(usage())
            ]
        );
        assert!(
            q.handle(req(QuitTrigger::Action)).is_empty(),
            "dialog already shown"
        );
        assert!(q.handle(QuitInput::UserCancelled).is_empty());
        assert_eq!(q.state(), QuitState::Idle);
        // A stale timer after cancel does nothing.
        assert!(q.handle(QuitInput::TimerFired(QuitTimer::Check)).is_empty());
        assert_eq!(q.state(), QuitState::Idle);
    }

    #[test]
    fn confirm_tears_down() {
        let mut q = QuitController::default();
        q.handle(req(QuitTrigger::CloseButton));
        q.handle(QuitInput::QuitCheck(usage()));
        assert_eq!(q.handle(QuitInput::UserConfirmed), teardown_fx());
        assert_eq!(
            q.handle(QuitInput::TimerFired(QuitTimer::Teardown)),
            vec![QuitEffect::Finish],
            "F17: teardown timeout still finishes"
        );
    }

    #[test]
    fn check_timeout_fails_open() {
        let mut q = QuitController::default();
        q.handle(req(QuitTrigger::Action));
        assert_eq!(
            q.handle(QuitInput::TimerFired(QuitTimer::Check)),
            teardown_fx()
        );
        assert_eq!(q.state(), QuitState::TearingDown);
    }

    #[test]
    fn signals_skip_the_dialog_and_override_it() {
        let mut q = QuitController::default();
        q.handle(req(QuitTrigger::Signal(15)));
        let mut expected = vec![QuitEffect::DisarmTimer(QuitTimer::Check)];
        expected.extend(teardown_fx());
        assert_eq!(
            q.handle(QuitInput::QuitCheck(usage())),
            expected,
            "no dialog for signals"
        );

        let mut q = QuitController::default();
        q.handle(req(QuitTrigger::Action));
        q.handle(QuitInput::QuitCheck(usage()));
        let mut expected = vec![QuitEffect::DismissConfirm];
        expected.extend(teardown_fx());
        assert_eq!(q.handle(req(QuitTrigger::Signal(2))), expected);
    }

    #[test]
    fn double_request_is_idempotent_and_force_finishes() {
        let mut q = QuitController::default();
        q.handle(req(QuitTrigger::Action));
        assert!(q.handle(req(QuitTrigger::Action)).is_empty());
        assert_eq!(q.handle(QuitInput::Force), vec![QuitEffect::Finish]);
        let mut idle = QuitController::default();
        assert!(idle.handle(QuitInput::Force).is_empty(), "nothing to force");
    }

    #[test]
    fn stale_events_are_ignored() {
        let mut q = QuitController::default();
        assert!(q.handle(QuitInput::TeardownComplete).is_empty());
        assert!(q.handle(QuitInput::QuitCheck(usage())).is_empty());
        assert!(q.handle(QuitInput::UserConfirmed).is_empty());
        assert_eq!(q.state(), QuitState::Idle);
    }
}
