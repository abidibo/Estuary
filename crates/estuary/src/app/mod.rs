//! Root component (UI M3.3–M3.5, M11): owns the engine, forwards its events, switches between
//! the blocking status page and the main Apps/Matrix view, and runs the desktop lifecycle
//! (`lifecycle.rs`).

mod lifecycle;

use std::cell::Cell;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use adw::prelude::*;
use estuary_engine::{
    Command, ConnectionState, Engine, EngineConfig, EngineError, EngineWarning, Event,
    GraphSnapshot, MeterTarget,
};
use relm4::{
    Component, ComponentController, ComponentParts, ComponentSender, Controller, adw, gtk,
};

use crate::config::{ConfigError, ConfigStore, LoadOutcome};
use crate::config_build::APP_ID;
use crate::desktop::autostart::AutostartOutcome;
use crate::desktop::close::BackgroundPermission;
use crate::desktop::env::DesktopEnv;
use crate::desktop::quit::{QuitController, QuitInput, QuitTimer, QuitTrigger};
use crate::desktop::tray::TrayHandle;
use crate::dialogs::preferences::PreferencesHandle;
use crate::i18n::{i18n, i18n_f, ni18n_f};
use crate::pages::apps::{AppsMsg, AppsOutput, AppsPage};
use crate::pages::matrix::{MatrixMsg, MatrixOutput, MatrixPage};
use crate::pages::status::{RootView, root_view, status_page};
use crate::state::edit::{Edit, apply_edit};
use crate::state::preset::PresetKind;
use crate::widgets::meter_hub::MeterHub;
use estuary_engine::SetupIssue;
use estuary_engine::setup::CloseAction;

/// Debounce for recomputing which meters are visible (UI M4.4).
const METER_RECOMPUTE: Duration = Duration::from_millis(100);

const REPOSITORY: &str = "https://github.com/abidibo/Estuary";

pub struct AppModel {
    engine: Option<Engine>,
    /// Desired state (C6). In memory until M6 loads and saves it.
    config: ConfigStore,
    /// Load result to show once the window is visible (UI M6.3).
    pending_notice: Option<LoadOutcome>,
    /// F15: toast each distinct save error once.
    toasted_save_error: Option<String>,
    connection: Option<ConnectionState>,
    fatal: Option<EngineError>,
    /// Status page needs rebuilding (connection or fatal state changed).
    status_dirty: Cell<bool>,
    snapshot: Arc<GraphSnapshot>,
    apps_page: Controller<AppsPage>,
    matrix_page: Controller<MatrixPage>,
    /// Setup issues already toasted (by `SetupIssue::key`), so each shows once.
    toasted_issues: std::collections::HashSet<String>,
    /// Engine warnings shown as badges (UI M10.2 / M11.5).
    warnings: crate::state::warnings::Warnings,
    toasts: adw::ToastOverlay,
    main_view: adw::ViewStack,
    meters: MeterHub,
    meter_recompute_pending: bool,
    /// Last subscription sent to the engine; `None` forces a resend (new engine).
    meter_targets_sent: Option<Vec<MeterTarget>>,
    // Desktop lifecycle (M11).
    env: DesktopEnv,
    quit: QuitController,
    quit_timers: HashMap<QuitTimer, gtk::glib::SourceId>,
    quit_dialog: Option<adw::AlertDialog>,
    /// Keeps the process alive while hidden; dropped by the quit `Finish`.
    hold: Option<gtk::gio::ApplicationHoldGuard>,
    tray: Option<TrayHandle>,
    tray_retried: bool,
    background: BackgroundPermission,
    /// Not persisted: the "closing quits" explanation shows once per session.
    denied_notice_shown: bool,
    /// A close notice is open: further ✕ are ignored.
    notice_open: bool,
    welcome_open: bool,
    snapshot_received: bool,
    prefs: Option<PreferencesHandle>,
    autostart: Option<Result<AutostartOutcome, String>>,
    /// The one banner: read-only (F16) or a long reconnect (F4).
    banner: adw::Banner,
    reconnect_tick: bool,
}

impl std::fmt::Debug for AppModel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AppModel")
            .field("connection", &self.connection)
            .field("fatal", &self.fatal)
            .finish_non_exhaustive()
    }
}

#[derive(Debug)]
pub struct AppWidgets {
    root_stack: gtk::Stack,
    status_bin: adw::Bin,
    main_view: adw::ViewStack,
    busy: gtk::Spinner,
}

#[derive(Debug)]
pub enum AppMsg {
    Engine(Event),
    /// F5 "Restart Engine" button.
    RestartEngine,
    ShowAbout,
    /// Something affecting meter visibility changed (scroll, page, window, rows).
    ScheduleMeters,
    RecomputeMeters,
    /// A user action from a page (UI M5.2).
    Edit(Edit),
    /// The debounced save is due (UI M6.2).
    SaveTick,
    /// The window became visible: show queued notices (UI M6.3).
    WindowShown,
    /// Periodic: drop warning badges older than a minute.
    ExpireWarnings,
    // Desktop lifecycle (M11).
    ShowWindow,
    WindowCloseRequested,
    RequestQuit(QuitTrigger),
    /// Timers and forced finishes.
    Quit(QuitInput),
    QuitConfirmed,
    QuitCancelled,
    BackgroundNoticeDismissed,
    BackgroundDeniedNoticeDismissed,
    TrayReady(Option<TrayHandle>),
    RetryTray,
    OpenPreferences,
    SetCloseAction(CloseAction),
    SetStartAtLogin(bool),
    AutostartApplied(Result<AutostartOutcome, String>),
    WelcomeChosen(PresetKind),
    ReconnectTick,
    ShowIssueDetails(Vec<SetupIssue>),
}

impl Component for AppModel {
    type Init = ();
    type Input = AppMsg;
    type Output = ();
    type CommandOutput = ();
    type Root = adw::ApplicationWindow;
    type Widgets = AppWidgets;

    fn init_root() -> adw::ApplicationWindow {
        let window = adw::ApplicationWindow::new(&relm4::main_adw_application());
        window.set_title(Some("Estuary")); // i18n-ok: the app name
        window.set_default_size(960, 640);
        // Required with breakpoints; the GNOME HIG minimum for adaptive windows.
        window.set_size_request(360, 294);
        if crate::config_build::DEVEL {
            window.add_css_class("devel");
        }
        window
    }

    fn init(
        _: (),
        root: adw::ApplicationWindow,
        sender: ComponentSender<Self>,
    ) -> ComponentParts<Self> {
        let meters = MeterHub::default();
        let apps_page = AppsPage::builder().launch(meters.clone()).forward(
            sender.input_sender(),
            |o| match o {
                AppsOutput::Edit(e) => AppMsg::Edit(e),
            },
        );
        let matrix_page = MatrixPage::builder().launch(meters.clone()).forward(
            sender.input_sender(),
            |o| match o {
                MatrixOutput::Edit(e) => AppMsg::Edit(e),
                MatrixOutput::MetersMoved => AppMsg::ScheduleMeters,
            },
        );

        // Pages
        let main_view = adw::ViewStack::new();
        main_view.add_titled_with_icon(
            apps_page.widget(),
            Some("apps"),
            &i18n("Apps"),
            "audio-x-generic-symbolic",
        );
        main_view.add_titled_with_icon(
            matrix_page.widget(),
            Some("matrix"),
            &i18n("Matrix"),
            "view-grid-symbolic",
        );

        // Header bar: view switcher, busy spinner, primary menu
        let switcher = adw::ViewSwitcher::new();
        switcher.set_stack(Some(&main_view));
        switcher.set_policy(adw::ViewSwitcherPolicy::Wide);
        let header = adw::HeaderBar::new();
        header.set_title_widget(Some(&switcher));
        let menu = gtk::gio::Menu::new();
        menu.append(Some(&i18n("Preferences")), Some("app.preferences"));
        menu.append(Some(&i18n("About Estuary")), Some("app.about"));
        menu.append(Some(&i18n("Quit")), Some("app.quit"));
        let menu_button = gtk::MenuButton::new();
        menu_button.set_icon_name("open-menu-symbolic");
        menu_button.set_menu_model(Some(&menu));
        menu_button.set_primary(true);
        menu_button.set_tooltip_text(Some(&i18n("Main Menu")));
        header.pack_end(&menu_button);
        let busy = gtk::Spinner::new();
        busy.set_tooltip_text(Some(&i18n("Reconnecting to the audio system…")));
        busy.set_visible(false);
        header.pack_end(&busy);

        // Narrow windows: switcher moves to a bottom bar.
        let switcher_bar = adw::ViewSwitcherBar::new();
        switcher_bar.set_stack(Some(&main_view));

        let status_bin = adw::Bin::new();
        let root_stack = gtk::Stack::new();
        root_stack.set_transition_type(gtk::StackTransitionType::Crossfade);
        root_stack.add_named(&status_bin, Some("status"));
        root_stack.add_named(&main_view, Some("main"));

        let toolbar = adw::ToolbarView::new();
        toolbar.add_top_bar(&header);
        toolbar.set_content(Some(&root_stack));
        toolbar.add_bottom_bar(&switcher_bar);
        let toasts = adw::ToastOverlay::new();
        toasts.set_child(Some(&toolbar));
        root.set_content(Some(&toasts));

        match adw::BreakpointCondition::parse("max-width: 550sp") {
            Ok(condition) => {
                let bp = adw::Breakpoint::new(condition);
                bp.add_setter(&switcher_bar, "reveal", Some(&true.to_value()));
                bp.add_setter(&header, "show-title", Some(&false.to_value()));
                root.add_breakpoint(bp);
            }
            Err(e) => tracing::warn!("breakpoint: {e}"),
        }

        let (mut config, outcome) = ConfigStore::load(crate::config::config_file());
        config.attach({
            let s = sender.input_sender().clone();
            move || s.emit(AppMsg::SaveTick)
        });
        let banner = adw::Banner::new("");
        banner.connect_button_clicked({
            let s = sender.input_sender().clone();
            move |_| s.emit(AppMsg::RestartEngine)
        });
        toolbar.add_top_bar(&banner);
        // Never close silently: the root decides (desktop M11.D3).
        root.connect_close_request({
            let s = sender.input_sender().clone();
            move |_| {
                s.emit(AppMsg::WindowCloseRequested);
                gtk::glib::Propagation::Stop
            }
        });
        root.connect_map({
            let s = sender.input_sender().clone();
            move |_| s.emit(AppMsg::WindowShown)
        });

        install_actions(&root, &main_view, &sender);
        {
            let s = sender.input_sender().clone();
            gtk::glib::timeout_add_seconds_local(10, move || {
                s.emit(AppMsg::ExpireWarnings);
                gtk::glib::ControlFlow::Continue
            });
        }
        dev_snapshot(&root, &main_view);
        connect_meter_visibility(&root, &main_view, apps_page.widget(), &sender);

        let mut model = AppModel {
            engine: None,
            config,
            pending_notice: Some(outcome),
            toasted_save_error: None,
            connection: None,
            fatal: None,
            status_dirty: Cell::new(true),
            snapshot: Arc::default(),
            apps_page,
            matrix_page,
            toasted_issues: std::collections::HashSet::new(),
            warnings: crate::state::warnings::Warnings::default(),
            toasts,
            main_view: main_view.clone(),
            meters,
            meter_recompute_pending: false,
            meter_targets_sent: None,
            env: DesktopEnv::detect(),
            quit: QuitController::default(),
            quit_timers: HashMap::new(),
            quit_dialog: None,
            hold: Some(crate::desktop::window::acquire_hold(
                &relm4::main_adw_application(),
            )),
            tray: None,
            tray_retried: false,
            background: BackgroundPermission::NotApplicable,
            denied_notice_shown: false,
            notice_open: false,
            welcome_open: false,
            snapshot_received: false,
            prefs: None,
            autostart: None,
            banner,
            reconnect_tick: false,
        };
        if model.env.sandboxed {
            model.background = BackgroundPermission::Unknown;
        }
        tracing::debug!(
            sandboxed = model.env.sandboxed,
            flatpak_id = ?model.env.flatpak_id,
            autostart_dir = ?model.env.autostart_dir,
            "desktop"
        );
        model.start_engine(&sender);
        model.refresh_banner(&sender);
        model.start_tray(&sender);
        AppModel::install_signals(&sender);
        // Start at login: synced now, unless the welcome is pending (it syncs after the choice).
        if model.config.settings().first_run_done {
            model.spawn_autostart(model.config.settings().start_at_login, &sender);
        }
        let mut widgets = AppWidgets {
            root_stack,
            status_bin,
            main_view,
            busy,
        };
        model.update_view(&mut widgets, sender);
        ComponentParts { model, widgets }
    }

    fn update(
        &mut self,
        msg: AppMsg,
        sender: ComponentSender<Self>,
        root: &adw::ApplicationWindow,
    ) {
        if self.quit.state() == crate::desktop::quit::QuitState::Finished
            && matches!(msg, AppMsg::Engine(_))
        {
            return; // quitting: the pages may already be gone
        }
        match msg {
            AppMsg::Engine(Event::QuitCheck(u)) => {
                self.feed_quit(QuitInput::QuitCheck(u), root, &sender);
            }
            AppMsg::Engine(Event::TeardownComplete) => {
                self.feed_quit(QuitInput::TeardownComplete, root, &sender);
            }
            AppMsg::Engine(Event::SetupIssues(issues)) => self.on_setup_issues(issues, &sender),
            AppMsg::Engine(ev) => {
                let rows_may_change = matches!(ev, Event::Snapshot(_));
                let connection = matches!(ev, Event::Connection(_));
                self.on_engine_event(ev);
                if rows_may_change {
                    self.snapshot_received = true;
                    self.schedule_meters(&sender);
                }
                if connection {
                    self.refresh_banner(&sender);
                }
                if rows_may_change || connection {
                    self.maybe_welcome(root, &sender);
                }
            }
            AppMsg::ShowWindow => self.show_window(root),
            AppMsg::WindowCloseRequested => self.on_close_requested(root, &sender),
            AppMsg::RequestQuit(trigger) => self.request_quit(trigger, root, &sender),
            AppMsg::Quit(input) => self.feed_quit(input, root, &sender),
            AppMsg::QuitConfirmed => {
                self.quit_dialog = None;
                self.feed_quit(QuitInput::UserConfirmed, root, &sender);
            }
            AppMsg::QuitCancelled => {
                self.quit_dialog = None;
                self.feed_quit(QuitInput::UserCancelled, root, &sender);
            }
            AppMsg::BackgroundNoticeDismissed => {
                self.notice_open = false;
                self.config
                    .update_settings(|s| s.background_notice_shown = true);
                self.hide_window(root);
            }
            AppMsg::BackgroundDeniedNoticeDismissed => {
                self.notice_open = false;
                self.request_quit(QuitTrigger::CloseButton, root, &sender);
            }
            AppMsg::TrayReady(h) => {
                tracing::debug!(present = h.is_some(), "tray");
                if h.is_none() && !self.tray_retried {
                    // The shell's tray host may start after us at login (D-F10): retry once.
                    self.tray_retried = true;
                    let s = sender.input_sender().clone();
                    gtk::glib::timeout_add_local_once(Duration::from_secs(10), move || {
                        s.emit(AppMsg::RetryTray);
                    });
                }
                self.tray = h;
            }
            AppMsg::RetryTray => {
                if self.tray.is_none() {
                    self.start_tray(&sender);
                }
            }
            AppMsg::OpenPreferences => {
                self.show_window(root);
                self.open_preferences(root, &sender);
            }
            AppMsg::SetCloseAction(a) => self.config.update_settings(|s| s.close_action = a),
            AppMsg::SetStartAtLogin(v) => {
                self.config.update_settings(|s| s.start_at_login = v);
                self.spawn_autostart(v, &sender);
            }
            AppMsg::AutostartApplied(r) => self.on_autostart_applied(r),
            AppMsg::WelcomeChosen(kind) => self.on_welcome_chosen(kind, &sender),
            AppMsg::ReconnectTick => {
                self.reconnect_tick = false;
                self.refresh_banner(&sender);
            }
            AppMsg::ShowIssueDetails(issues) => {
                crate::dialogs::notices::issue_details(root, &issues);
            }
            AppMsg::RestartEngine => {
                if let Some(engine) = self.engine.take()
                    && let Err(e) = engine.shutdown(Duration::from_secs(1))
                {
                    tracing::warn!("engine shutdown before restart: {e}");
                }
                self.start_engine(&sender);
                self.schedule_meters(&sender);
            }
            AppMsg::ShowAbout => show_about(root),
            AppMsg::ScheduleMeters => self.schedule_meters(&sender),
            AppMsg::Edit(edit) => self.dispatch_edit(edit),
            AppMsg::SaveTick => self.save_now(),
            AppMsg::ExpireWarnings => {
                if self.warnings.expire(std::time::Instant::now()) {
                    self.push_to_pages();
                }
            }
            AppMsg::WindowShown => {
                tracing::debug!("window shown");
                if let Some(outcome) = self.pending_notice.take() {
                    self.show_load_notice(outcome, root);
                }
                self.maybe_welcome(root, &sender);
            }
            AppMsg::RecomputeMeters => {
                self.meter_recompute_pending = false;
                self.recompute_meters(root);
            }
        }
    }

    /// Application shutdown: write any pending change (C12 flush on quit).
    fn shutdown(&mut self, _widgets: &mut AppWidgets, _output: relm4::Sender<()>) {
        if let Err(e) = self.config.flush_sync()
            && !matches!(e, ConfigError::ReadOnly)
        {
            tracing::error!("cannot save settings on quit: {e}");
        }
    }

    fn update_view(&self, widgets: &mut AppWidgets, sender: ComponentSender<Self>) {
        let view = root_view(self.connection.as_ref(), self.fatal.is_some());
        if view == RootView::Status && self.status_dirty.replace(false) {
            let s = sender.input_sender().clone();
            let page = status_page(self.connection.as_ref(), self.fatal.as_ref(), move || {
                s.emit(AppMsg::RestartEngine);
            });
            widgets.status_bin.set_child(Some(&page));
        }
        widgets.root_stack.set_visible_child_name(match view {
            RootView::Status => "status",
            RootView::Main => "main",
        });
        let reconnecting = matches!(self.connection, Some(ConnectionState::Reconnecting { .. }));
        let quitting = self.quit.busy();
        widgets.main_view.set_sensitive(!reconnecting);
        widgets.root_stack.set_sensitive(!quitting);
        widgets.busy.set_visible(reconnecting || quitting);
        widgets.busy.set_spinning(reconnecting || quitting);
    }
}

impl AppModel {
    /// Transient feedback (3 s), e.g. "Audio system restarted, setup restored".
    pub fn show_toast(&self, text: &str) {
        let toast = adw::Toast::new(text);
        toast.set_timeout(3);
        self.toasts.add_toast(toast);
    }

    /// The only mutation path for the desired state (UI M3.4/M5.2).
    fn dispatch_edit(&mut self, edit: Edit) {
        tracing::debug!(?edit, "edit");
        let out = apply_edit(self.config.file_mut(), &self.snapshot, edit);
        for cmd in out.commands {
            if let Some(e) = &self.engine {
                let _ = e.send(cmd);
            }
        }
        if out.save {
            self.config.schedule_save();
        }
        for text in &out.toasts {
            self.show_toast(text);
        }
        if out.structure_changed {
            self.push_to_pages();
        }
    }

    fn push_to_pages(&self) {
        let setup = Arc::new(self.config.file().setup.clone());
        // `send`, not `emit`: after quitting, a late engine event must not panic on a closed page.
        let _ = self.apps_page.sender().send(AppsMsg::Update {
            snapshot: self.snapshot.clone(),
            setup: setup.clone(),
        });
        let _ = self.matrix_page.sender().send(MatrixMsg::Update {
            snapshot: self.snapshot.clone(),
            setup,
            warnings: Arc::new(self.warnings.clone()),
        });
    }

    /// F15: keep state in memory on failure, toast each distinct error once, retry on the
    /// next edit. Read-only mode (F16) never writes.
    fn save_now(&mut self) {
        match self.config.flush_sync() {
            Ok(()) => self.toasted_save_error = None,
            Err(ConfigError::ReadOnly) => {}
            Err(e) => {
                let msg = e.to_string();
                tracing::error!("cannot save settings: {msg}");
                if self.toasted_save_error.as_deref() != Some(msg.as_str()) {
                    self.show_toast(&i18n("Couldn't save settings"));
                    self.toasted_save_error = Some(msg);
                }
            }
        }
    }

    fn show_load_notice(&self, outcome: LoadOutcome, root: &adw::ApplicationWindow) {
        match outcome {
            LoadOutcome::Recovered { backup, error } => {
                let name = backup.file_name().map_or_else(
                    || backup.display().to_string(),
                    |n| n.to_string_lossy().into_owned(),
                );
                let dialog = adw::AlertDialog::new(
                    Some(&i18n("Your saved setup couldn't be read")),
                    // Translators: {} are a file name and a technical error message
                    Some(&i18n_f("It was kept as {}.\n\n{}", &[&name, &error])),
                );
                dialog.add_response("ok", &i18n("OK"));
                dialog.present(Some(root));
            }
            LoadOutcome::Repaired { issues } => {
                let n = u32::try_from(issues.len()).unwrap_or(u32::MAX);
                self.show_toast(&ni18n_f(
                    "Removed {} invalid entry from the saved setup",
                    "Removed {} invalid entries from the saved setup",
                    n,
                    &[&n.to_string()],
                ));
            }
            LoadOutcome::Fresh | LoadOutcome::Loaded | LoadOutcome::NewerSchema { .. } => {}
        }
    }

    fn schedule_meters(&mut self, sender: &ComponentSender<Self>) {
        if self.meter_recompute_pending {
            return;
        }
        self.meter_recompute_pending = true;
        let s = sender.input_sender().clone();
        gtk::glib::timeout_add_local_once(METER_RECOMPUTE, move || s.emit(AppMsg::RecomputeMeters));
    }

    /// Meter only what is on screen (UI M4.4); nothing while the window is hidden.
    fn recompute_meters(&mut self, root: &adw::ApplicationWindow) {
        let page = self.main_view.visible_child_name();
        let targets = match page.as_deref() {
            _ if !root.is_visible() => Vec::new(),
            Some("apps") => self.meters.visible_targets(self.apps_page.widget()),
            Some("matrix") => self.matrix_page.model().visible_meter_targets(),
            _ => Vec::new(),
        };
        if self.meter_targets_sent.as_ref() == Some(&targets) {
            return;
        }
        tracing::debug!(targets = targets.len(), "meter targets");
        if let Some(engine) = &self.engine
            && engine
                .send(Command::SetMeterTargets(targets.clone()))
                .is_ok()
        {
            self.meter_targets_sent = Some(targets);
        }
    }

    fn start_engine(&mut self, sender: &ComponentSender<Self>) {
        self.fatal = None;
        self.connection = None;
        self.status_dirty.set(true);
        match Engine::start(EngineConfig::default()) {
            Ok((engine, rx)) => {
                self.meters.set_bank(engine.meters());
                self.meter_targets_sent = None;
                // C10 step 3: the engine applies the desired state as soon as it is connected.
                let _ = engine.send(Command::SetSetup(self.config.file().setup.clone()));
                self.engine = Some(engine);
                // C2: forward engine events into the component. A closed channel without a
                // Fatal event still means the engine is gone (UI-4).
                let s = sender.input_sender().clone();
                relm4::spawn_local(async move {
                    while let Ok(ev) = rx.recv().await {
                        if s.send(AppMsg::Engine(ev)).is_err() {
                            return;
                        }
                    }
                    let _ = s.send(AppMsg::Engine(Event::Fatal(EngineError::ThreadGone)));
                });
            }
            Err(e) => {
                tracing::error!("cannot start the engine: {e}");
                self.fatal = Some(e);
            }
        }
    }

    fn on_engine_event(&mut self, ev: Event) {
        match ev {
            Event::Connection(state) => {
                if self.connection.as_ref() != Some(&state) {
                    self.status_dirty.set(true);
                }
                self.connection = Some(state);
            }
            Event::Snapshot(snap) => {
                tracing::trace!("snapshot");
                self.snapshot = snap;
                self.push_to_pages();
            }
            Event::ExternalRetarget { app, target } => {
                self.dispatch_edit(Edit::ExternalRetarget { app, target });
            }
            Event::Restored => self.show_toast(&i18n("Audio system restarted, setup restored")),
            Event::MeterAssignments(a) => self.meters.set_assignments(a),
            Event::Warning(EngineWarning::MeterFailed(target)) => self.meters.set_failed(target),
            Event::Warning(w) => {
                if let Some(text) = self.warnings.on_warning(&w, std::time::Instant::now()) {
                    self.show_toast(&text);
                }
                self.push_to_pages();
            }
            Event::Fatal(e) => {
                // Only the first fatal error matters; the forwarder's ThreadGone may follow it.
                if self.fatal.is_none() {
                    tracing::error!("engine stopped: {e}");
                    self.fatal = Some(e);
                    self.status_dirty.set(true);
                }
                self.engine = None;
            }
            other => tracing::debug!(?other, "engine event not handled here"),
        }
    }
}

fn install_actions(
    root: &adw::ApplicationWindow,
    main_view: &adw::ViewStack,
    sender: &ComponentSender<AppModel>,
) {
    use gtk::gio::SimpleAction;
    let app = relm4::main_adw_application();

    let about = SimpleAction::new("about", None);
    about.connect_activate({
        let s = sender.input_sender().clone();
        move |_, _| s.emit(AppMsg::ShowAbout)
    });
    app.add_action(&about);

    // Desktop M11.D4: exported on the session bus too (GNOME's Background Apps may use quit).
    for (name, msg) in [
        (
            "quit",
            (|| AppMsg::RequestQuit(QuitTrigger::Action)) as fn() -> AppMsg,
        ),
        ("preferences", || AppMsg::OpenPreferences),
        ("show", || AppMsg::ShowWindow),
    ] {
        let action = SimpleAction::new(name, None);
        let s = sender.input_sender().clone();
        action.connect_activate(move |_, _| s.emit(msg()));
        app.add_action(&action);
    }
    app.set_accels_for_action("app.quit", &["<Control>q"]);
    app.set_accels_for_action("app.preferences", &["<Control>comma"]);
    app.set_accels_for_action("window.close", &["<Control>w"]);

    for (name, page, accel) in [
        ("show-apps", "apps", "<Alt>1"),
        ("show-matrix", "matrix", "<Alt>2"),
    ] {
        let action = SimpleAction::new(name, None);
        let view = main_view.clone();
        action.connect_activate(move |_, _| view.set_visible_child_name(page));
        root.add_action(&action);
        app.set_accels_for_action(&format!("win.{name}"), &[accel]);
    }
}

fn show_about(root: &adw::ApplicationWindow) {
    let about = adw::AboutDialog::builder()
        .application_name("Estuary")
        .application_icon(APP_ID)
        .version(env!("CARGO_PKG_VERSION"))
        .developer_name("abidibo")
        .comments(i18n("Route, mix and clean up your audio with PipeWire."))
        .website(REPOSITORY)
        .issue_url(format!("{REPOSITORY}/issues"))
        .license_type(gtk::License::Custom)
        .license(i18n(
            "Estuary is available under either the MIT or the Apache 2.0 license, at your option.",
        ))
        .build();
    about.present(Some(root));
}

fn connect_meter_visibility(
    root: &adw::ApplicationWindow,
    main_view: &adw::ViewStack,
    scroller: &gtk::ScrolledWindow,
    sender: &ComponentSender<AppModel>,
) {
    let schedule = {
        let s = sender.input_sender().clone();
        move || s.emit(AppMsg::ScheduleMeters)
    };
    let adj = scroller.vadjustment();
    adj.connect_value_changed({
        let f = schedule.clone();
        move |_| f()
    });
    adj.connect_changed({
        let f = schedule.clone();
        move |_| f()
    });
    main_view.connect_visible_child_notify({
        let f = schedule.clone();
        move |_| f()
    });
    root.connect_visible_notify(move |_| schedule());
}

/// Development aids: `ESTUARY_PAGE=matrix` opens on that page; `ESTUARY_DEV_SIZE=1000x700` sets
/// the window size (store screenshots); `ESTUARY_DEV_INERT=1` makes the
/// content non-interactive (measurements); `ESTUARY_SNAPSHOT=out.png` renders the window to a PNG
/// after 2.5 s with GTK's own renderer (no screen capture), then quits.
fn dev_snapshot(root: &adw::ApplicationWindow, main_view: &adw::ViewStack) {
    if let Ok(page) = std::env::var("ESTUARY_PAGE") {
        main_view.set_visible_child_name(&page);
        // Also on first map: startup through the command line resets the stack's page.
        let view = main_view.clone();
        root.connect_map(move |_| view.set_visible_child_name(&page));
    }
    if let Some((w, h)) = std::env::var("ESTUARY_DEV_SIZE")
        .ok()
        .and_then(|v| {
            v.split_once('x')
                .map(|(w, h)| (w.parse().ok(), h.parse().ok()))
        })
        .and_then(|(w, h)| w.zip(h))
    {
        root.set_default_size(w, h);
    }
    if std::env::var("ESTUARY_DEV_INERT").is_ok() {
        main_view.set_sensitive(false);
    }
    let Ok(path) = std::env::var("ESTUARY_SNAPSHOT") else {
        return;
    };
    let root = root.clone();
    gtk::glib::timeout_add_local_once(Duration::from_millis(2500), move || {
        let (w, h) = (root.width(), root.height());
        let paintable = gtk::WidgetPaintable::new(Some(&root));
        let snapshot = gtk::Snapshot::new();
        paintable.snapshot(
            snapshot.upcast_ref::<gtk::gdk::Snapshot>(),
            f64::from(w),
            f64::from(h),
        );
        let rendered = snapshot
            .to_node()
            .zip(root.native().and_then(|n| n.renderer()));
        if let Some((node, renderer)) = rendered {
            #[allow(clippy::cast_precision_loss)] // window sizes
            let bounds = gtk::graphene::Rect::new(0.0, 0.0, w as f32, h as f32);
            let texture = renderer.render_texture(node, Some(&bounds));
            match texture.save_to_png(&path) {
                Ok(()) => tracing::info!(%path, "window snapshot saved"),
                Err(e) => tracing::error!("window snapshot: {e}"),
            }
        }
        relm4::main_application().quit();
    });
}
