//! The root's lifecycle flows (UI M11, desktop M11.D2–D12): closing, quitting, first run,
//! preferences, start at login, tray, signals and the banner.

use adw::prelude::*;
use estuary_engine::{Command, ConnectionState, SetupIssue};
use relm4::{ComponentSender, adw, gtk};

use super::{AppModel, AppMsg};
use crate::desktop::autostart::{apply_autostart, portal};
use crate::desktop::close::{CloseDecision, decide_close};
use crate::desktop::quit::{
    ENGINE_SHUTDOWN_TIMEOUT, QuitEffect, QuitInput, QuitTimer, QuitTrigger,
};
use crate::desktop::tray::{self, TrayEvent, TraySpec};
use crate::desktop::{signals, window};
use crate::dialogs::{notices, preferences, welcome};
use crate::i18n::i18n;
use crate::state::edit::Edit;
use crate::state::preset::PresetKind;
use crate::state::surfaces::banner_state;

impl AppModel {
    // ---- Window ----

    pub(super) fn show_window(&self, root: &adw::ApplicationWindow) {
        window::show_window(root);
        self.set_background_status(None);
    }

    pub(super) fn hide_window(&self, root: &adw::ApplicationWindow) {
        window::hide_window(root);
        self.set_background_status(Some(i18n("Routing audio")));
    }

    /// Sandboxed only: the line under Estuary in GNOME's Background Apps (best effort).
    fn set_background_status(&self, message: Option<String>) {
        if self.env.sandboxed {
            relm4::spawn(portal::set_background_status(message));
        }
    }

    /// Desktop M11.D3 / UI M11.3.
    pub(super) fn on_close_requested(
        &mut self,
        root: &adw::ApplicationWindow,
        sender: &ComponentSender<Self>,
    ) {
        if self.notice_open {
            return;
        }
        match decide_close(
            self.config.settings(),
            self.background,
            self.denied_notice_shown,
        ) {
            CloseDecision::Hide => self.hide_window(root),
            CloseDecision::ShowBackgroundNoticeThenHide => {
                self.notice_open = true;
                let s = sender.input_sender().clone();
                notices::background_notice(root, move || s.emit(AppMsg::BackgroundNoticeDismissed));
            }
            CloseDecision::Quit => self.request_quit(QuitTrigger::CloseButton, root, sender),
            CloseDecision::QuitBecauseBackgroundDenied { show_notice: true } => {
                self.notice_open = true;
                self.denied_notice_shown = true;
                let s = sender.input_sender().clone();
                notices::background_denied_notice(root, move || {
                    s.emit(AppMsg::BackgroundDeniedNoticeDismissed);
                });
            }
            CloseDecision::QuitBecauseBackgroundDenied { show_notice: false } => {
                self.request_quit(QuitTrigger::CloseButton, root, sender);
            }
        }
    }

    // ---- Quit (desktop M11.D5/D6) ----

    pub(super) fn request_quit(
        &mut self,
        trigger: QuitTrigger,
        root: &adw::ApplicationWindow,
        sender: &ComponentSender<Self>,
    ) {
        // A second signal while quitting forces the finish.
        let input = if matches!(trigger, QuitTrigger::Signal(_)) && self.quit.busy() {
            QuitInput::Force
        } else {
            QuitInput::Request {
                trigger,
                engine_connected: matches!(
                    self.connection,
                    Some(ConnectionState::Connected { .. })
                ),
                engine_present: self.engine.is_some(),
            }
        };
        self.feed_quit(input, root, sender);
    }

    pub(super) fn feed_quit(
        &mut self,
        input: QuitInput,
        root: &adw::ApplicationWindow,
        sender: &ComponentSender<Self>,
    ) {
        if let QuitInput::TimerFired(t) = &input {
            self.quit_timers.remove(t); // it fired: nothing left to disarm
        }
        let mut pending = std::collections::VecDeque::from(self.quit.handle(input));
        while let Some(effect) = pending.pop_front() {
            // Effects that fail feed the machine at once (engine thread gone, D-F2).
            if let Some(next) = self.execute(effect, root, sender) {
                pending.extend(self.quit.handle(next));
            }
        }
    }

    fn execute(
        &mut self,
        effect: QuitEffect,
        root: &adw::ApplicationWindow,
        sender: &ComponentSender<Self>,
    ) -> Option<QuitInput> {
        match effect {
            QuitEffect::SendPrepareQuit => {
                let sent = self
                    .engine
                    .as_ref()
                    .is_some_and(|e| e.send(Command::PrepareQuit).is_ok());
                (!sent).then_some(QuitInput::TimerFired(QuitTimer::Check))
            }
            QuitEffect::SendTeardown => {
                let sent = self
                    .engine
                    .as_ref()
                    .is_some_and(|e| e.send(Command::Teardown).is_ok());
                (!sent).then_some(QuitInput::TeardownComplete)
            }
            QuitEffect::ShowConfirm(usages) => {
                self.show_window(root);
                let s = sender.input_sender().clone();
                self.quit_dialog = Some(notices::quit_confirm(root, &usages, move |yes| {
                    s.emit(if yes {
                        AppMsg::QuitConfirmed
                    } else {
                        AppMsg::QuitCancelled
                    });
                }));
                None
            }
            QuitEffect::DismissConfirm => {
                if let Some(d) = self.quit_dialog.take() {
                    d.force_close();
                }
                None
            }
            QuitEffect::ArmTimer(t, after) => {
                let s = sender.input_sender().clone();
                let id = gtk::glib::timeout_add_local_once(after, move || {
                    s.emit(AppMsg::Quit(QuitInput::TimerFired(t)));
                });
                if let Some(old) = self.quit_timers.insert(t, id) {
                    old.remove();
                }
                None
            }
            QuitEffect::DisarmTimer(t) => {
                if let Some(id) = self.quit_timers.remove(&t) {
                    id.remove();
                }
                None
            }
            QuitEffect::HideWindow => {
                window::hide_window(root);
                None
            }
            QuitEffect::Finish => {
                self.finish();
                None
            }
        }
    }

    /// C10 step 4: engine shutdown, config flush, tray, hold, quit.
    fn finish(&mut self) {
        tracing::info!("quitting");
        if let Some(engine) = self.engine.take()
            && let Err(e) = engine.shutdown(ENGINE_SHUTDOWN_TIMEOUT)
        {
            tracing::warn!("engine shutdown: {e}");
        }
        if let Err(e) = self.config.flush_sync()
            && !matches!(e, crate::config::ConfigError::ReadOnly)
        {
            tracing::error!("config flush on quit: {e}");
        }
        if let Some(t) = self.tray.take() {
            drop(t.0.shutdown());
        }
        self.hold.take();
        relm4::main_application().quit();
    }

    // ---- First run (UI M11.1) ----

    /// Presents the welcome once the window is visible and the engine has devices to offer.
    pub(super) fn maybe_welcome(
        &mut self,
        root: &adw::ApplicationWindow,
        sender: &ComponentSender<Self>,
    ) {
        let ready = !self.config.settings().first_run_done
            && !self.config.is_read_only()
            && !self.welcome_open
            && root.is_visible()
            && self.snapshot_received
            && matches!(self.connection, Some(ConnectionState::Connected { .. }));
        if !ready {
            return;
        }
        self.welcome_open = true;
        let s = sender.input_sender().clone();
        welcome::present(root, move |kind| s.emit(AppMsg::WelcomeChosen(kind)));
    }

    pub(super) fn on_welcome_chosen(&mut self, kind: PresetKind, sender: &ComponentSender<Self>) {
        self.welcome_open = false;
        self.dispatch_edit(Edit::ApplyPreset(kind));
        self.save_now();
        // Deferred until now so the first launch never shows two dialogs at once (M11.D11).
        self.spawn_autostart(self.config.settings().start_at_login, sender);
        self.main_view.set_visible_child_name(match kind {
            PresetKind::Recommended => "matrix",
            PresetKind::Empty => "apps",
        });
        if kind == PresetKind::Recommended && self.snapshot.default_output.is_none() {
            self.show_toast(&i18n(
                "No output device found. Connect channels to an output in the Matrix page.",
            ));
        }
    }

    // ---- Preferences and start at login (UI M11.2, desktop M11.D11) ----

    pub(super) fn open_preferences(
        &mut self,
        root: &adw::ApplicationWindow,
        sender: &ComponentSender<Self>,
    ) {
        let (s1, s2) = (sender.input_sender().clone(), sender.input_sender().clone());
        let handle = preferences::present(
            root,
            move |a| s1.emit(AppMsg::SetCloseAction(a)),
            move |v| s2.emit(AppMsg::SetStartAtLogin(v)),
        );
        self.prefs = Some(handle);
        self.refresh_preferences();
    }

    pub(super) fn refresh_preferences(&self) {
        if let Some(p) = self.prefs.as_ref().filter(|p| p.is_open()) {
            let s = self.config.settings();
            p.update(s.close_action, s.start_at_login, self.autostart.as_ref());
        }
    }

    pub(super) fn spawn_autostart(&self, desired: bool, sender: &ComponentSender<Self>) {
        let (env, reason, s) = (
            self.env.clone(),
            portal::reason(),
            sender.input_sender().clone(),
        );
        relm4::spawn(async move {
            let r = apply_autostart(desired, env, reason)
                .await
                .map_err(|e| e.to_string());
            s.emit(AppMsg::AutostartApplied(r));
        });
    }

    pub(super) fn on_autostart_applied(
        &mut self,
        r: Result<crate::desktop::autostart::AutostartOutcome, String>,
    ) {
        match &r {
            Ok(o) => {
                tracing::debug!(?o, "start at login");
                self.background = o.background;
            }
            Err(e) => {
                tracing::warn!("start at login: {e}");
                self.show_toast(&i18n("Couldn't update start at login"));
            }
        }
        self.autostart = Some(r);
        self.refresh_preferences();
    }

    // ---- Tray and signals ----

    pub(super) fn start_tray(&self, sender: &ComponentSender<Self>) {
        let (tx, rx) = async_channel::unbounded();
        let s = sender.input_sender().clone();
        relm4::spawn_local(async move {
            while let Ok(ev) = rx.recv().await {
                s.emit(match ev {
                    TrayEvent::Show => AppMsg::ShowWindow,
                    TrayEvent::Quit => AppMsg::RequestQuit(QuitTrigger::Tray),
                });
            }
        });
        let icon_name = gtk::gdk::Display::default()
            .map(|d| gtk::IconTheme::for_display(&d))
            .filter(|t| t.has_icon(&format!("{}-symbolic", crate::config_build::APP_ID)))
            .map_or_else(
                || "audio-card-symbolic".to_owned(),
                |_| format!("{}-symbolic", crate::config_build::APP_ID),
            );
        let spec = TraySpec {
            icon_name,
            icon_pixmaps: tray::render_tray_pixmaps(),
            label_show: i18n("Show Estuary"),
            label_quit: i18n("Quit"),
            tooltip: "Estuary".into(),
        };
        let s = sender.input_sender().clone();
        tray::spawn_tray(spec, self.env.sandboxed, tx, move |h| {
            s.emit(AppMsg::TrayReady(h));
        });
    }

    pub(super) fn install_signals(sender: &ComponentSender<Self>) {
        let s = sender.input_sender().clone();
        let _ = signals::install_signal_handlers(move |sig| {
            s.emit(AppMsg::RequestQuit(QuitTrigger::Signal(sig)));
        });
    }

    // ---- Banner and issues (UI M11.5) ----

    pub(super) fn refresh_banner(&mut self, sender: &ComponentSender<Self>) {
        let reconnecting_for = match &self.connection {
            Some(ConnectionState::Reconnecting { since, .. }) => Some(since.elapsed()),
            _ => None,
        };
        match banner_state(self.config.is_read_only(), reconnecting_for) {
            Some(b) => {
                self.banner.set_title(&b.title);
                self.banner
                    .set_button_label(b.retry.then(|| i18n("Retry")).as_deref());
                self.banner.set_revealed(true);
            }
            None => self.banner.set_revealed(false),
        }
        // Poll once a second while reconnecting, to reveal the banner after 30 s.
        if reconnecting_for.is_some() && !self.reconnect_tick {
            self.reconnect_tick = true;
            let s = sender.input_sender().clone();
            gtk::glib::timeout_add_local_once(std::time::Duration::from_secs(1), move || {
                s.emit(AppMsg::ReconnectTick);
            });
        }
    }

    /// Issues without a cell: one toast per new issue set, with the details on demand.
    pub(super) fn on_setup_issues(
        &mut self,
        issues: Vec<SetupIssue>,
        sender: &ComponentSender<Self>,
    ) {
        let fresh: Vec<SetupIssue> = issues
            .into_iter()
            .filter(|i| i.cell().is_none())
            .filter(|i| self.toasted_issues.insert(i.key()))
            .collect();
        if fresh.is_empty() {
            return;
        }
        let toast = adw::Toast::new(&i18n("Some routes couldn't be created"));
        toast.set_timeout(5);
        toast.set_button_label(Some(&i18n("Details")));
        let s = sender.input_sender().clone();
        toast.connect_button_clicked(move |_| s.emit(AppMsg::ShowIssueDetails(fresh.clone())));
        self.toasts.add_toast(toast);
    }
}
