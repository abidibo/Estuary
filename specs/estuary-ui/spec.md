# estuary-ui: Relm4 user interface (sub-spec)

> Sub-spec of [`specs/estuary/spec.md`](../estuary/spec.md) (the master). The master contracts **C1–C14 are binding**: this document cites them and never redefines them. Where this spec needs something the master does not state, it says so under [Contract notes](#contract-notes).
>
> Detail level: **high**. Crate: `crates/estuary` (binary). Milestones covered: **M3, M4 (widget), M5 (UI), M6 (I/O), M9, M10 (UI), M11 (UI parts)**.
> Date: 2026-10-14. Status: draft for review.

---

## Summary

This spec covers the GTK4/libadwaita UI of Estuary, built with Relm4 0.11:

- the main window, with an Apps page and a Matrix page;
- connection status pages;
- the custom `PeakMeter` widget;
- the per-app output dropdown and volume;
- the matrix grid with per-cell gain;
- noise-suppression controls;
- the welcome, preferences, background-notice and quit dialogs;
- the TOML config store.

The UI owns the user's desired state (`ConfigFile`, C6) and translates every edit into `Command`s for the engine (C3).

## Problem Statement

The engine (C3) exposes a message API and a lock-free meter bank (C9), but the user needs a desktop UI that:

1. **Shows live state:** which apps play, which devices and channels exist, and what is connected. It updates within one snapshot (≤ 50 ms) and draws meters at the display frame rate.
2. **Edits the desired setup safely,** with a single source of truth. Edits must never fight engine state, must persist atomically (C12), and must survive corrupt or newer config files (F14–F16).
3. **Makes a dense matrix usable** with mouse, touchpad and keyboard. Per-cell gain (R2) must be adjustable with no clicks or lag, and screen-reader labels must describe each crosspoint.
4. **Handles lifecycle UI:** first run (R5), background mode, Quit with a warning (C10), and recovery surfaces for F4–F7, F10 and F13–F16, all without blocking the GTK main loop.

## Solution Overview

- **Single source of truth.** `ConfigStore` owns the persisted `ConfigFile` (C6). `UiState` holds the latest `Arc<GraphSnapshot>` (C4), `ConnectionState` and meter assignments. Every user action becomes an `Edit` value. A **pure function** `apply_edit(&mut ConfigFile, &GraphSnapshot, Edit) -> EditOutcome` mutates the config and returns:
  - the engine `Command`s to send: `SetSetup(setup.clone())` for structural edits, a fast-path command for volume and gain drags (C3);
  - toasts;
  - whether to schedule a save.

  Widgets never mutate state directly. This keeps all state logic unit-testable without GTK.
- **Relm4 structure.** A root `AppModel` component (`relm4::Component`) owns the `Engine`, the `ConfigStore` (which owns the `ConfigFile`) and `UiState`. It has two child components: `AppsPage` (with a `FactoryVecDeque<AppRow>` inside a `gtk::ListBox`) and `MatrixPage`. Dialogs are short-lived child components or plain libadwaita dialogs built on demand.
- **The matrix grid is not a factory.** Its row and column positions shift whenever channels or devices are added, so `MatrixPage` rebuilds the grid only when the *row/column key sets* change. Otherwise it updates existing cell and header widgets in place, through a `HashMap<CellKey, CellButton>`.
- **Meters.** `PeakMeter` is a gtk4-rs `glib` subclass of `gtk::Widget`. It draws in `snapshot()` and runs ballistics in its own `add_tick_callback`, reading the engine's `MeterBank` (C9) directly. Meter updates never go through Relm4 messages. A `MeterVisibility` tracker computes which `MeterTarget`s are on screen and sends `SetMeterTargets`.
- **Engine events** are forwarded with `relm4::spawn_local` from the `async_channel::Receiver<Event>` into `AppMsg::Engine(Event)` (C2).
- **Config I/O** (`config.rs`, C12) runs on the UI thread. Writes are small (KB), so it uses synchronous `std::fs` on a 500 ms debounce driven by `glib::timeout_add_local_once`.
- **Desktop integration** belongs to the **estuary-desktop** sub-spec (C10 ownership): single instance, `decide_close()`, the single `QuitController` state machine and its effect executor with `finish()`, signals, background portal, autostart and tray. This spec provides the root component, fields and `AppMsg` variants that the desktop sub-spec requires, plus the dialogs its effects present (`show_quit_confirm`, `show_background_notice`, `show_background_denied_notice`, `show_toast`) and the Preferences rendering of `AutostartOutcome`. See [Interfaces used from other sub-specs](#interfaces-used-from-other-sub-specs).

## Technical Context

Greenfield (master §Technical Context). Relevant verified facts:

| Fact | Status |
|---|---|
| relm4 0.11.0 source: `RelmApp::from_app(app)`, `.visible_on_activate(bool)`, `.run::<C>(init)`; `relm4::spawn_local(fut) -> glib::JoinHandle`; `ComponentSender::{input, output, command, oneshot_command, input_sender}` | Verified (crate source `src/app.rs`, `src/lib.rs:162`, `src/channel/component.rs`) |
| relm4 0.11.0 `FactoryVecDeque::builder().launch(parent).forward(sender, map)`, `.guard()`, `.send(i, msg)`, `.broadcast(msg)`; `FactoryComponent { type ParentWidget; type Init; type Index; init_model; init_widgets; update }` | Verified (`src/factory/sync/collections/vec_deque.rs`, `src/factory/sync/traits.rs`) |
| relm4 0.11.0 `FactoryView` impls include `gtk::Box`, `gtk::Grid`, `gtk::ListBox`, `gtk::FlowBox`, `adw::PreferencesGroup` | Verified (`src/factory/widgets/`) |
| relm4 feature `gnome_46` = `gtk/gnome_46` + `adw/v1_5` | Verified (`Cargo.toml` of relm4 0.11.0) |
| libadwaita 0.9.2: `adw::AlertDialog`, `adw::PreferencesDialog`, `adw::Dialog` need `v1_5`; `adw::Banner` `v1_3`; `adw::SwitchRow`, `adw::SpinRow`, `adw::ToolbarView`, `adw::Breakpoint` `v1_4`; `adw::ToastOverlay` base (only `dismiss_all` is `v1_7`); **`adw::Spinner` needs `v1_6` → not available, use `gtk::Spinner`** | Verified (docs.rs feature gates) |
| gtk4 0.11.5 has `gtk::AccessibleRole::Meter` (GTK ≥ 4.14) | Verified (local cargo registry `gtk4-0.11.5/src/auto/enums.rs`) |
| Host GTK 4.14.5 / libadwaita 1.5.0 → features `v4_14`, `v1_5` (C1) | Live-verified (master) |

Conventions (no existing code, so ecosystem practice applies):

- Relm4 `view!` macro for static layouts.
- Manual widget code for the dynamic matrix.
- One `Component` per page.
- `#![forbid(unsafe_code)]` (C1).
- All user-visible strings go through gettext (see M3.2).
- GNOME HIG: header bar with an `adw::ViewSwitcher`, a primary menu, toasts for transient feedback, banners for persistent state, and `adw::StatusPage` for blocking states.

### Module layout (`crates/estuary/src`)

```
main.rs                    # tracing init (ESTUARY_LOG, C14), i18n::init(), desktop::app::build_application(), RelmApp::from_app(app).visible_on_activate(true).run::<AppModel>(AppInit)
i18n.rs                    # gettext setup; i18n(), i18n_f(), ni18n_f() helpers
app.rs                     # root component App: AppModel, AppMsg, AppCmd; owns Engine, ConfigStore, UiState
config.rs                  # ConfigStore (C12): load / schedule_save / flush; LoadOutcome
state/mod.rs               # UiState
state/edit.rs              # Edit enum, apply_edit(), EditOutcome  (pure, unit-tested)
state/matrix_layout.rs     # MatrixLayout computation from (Setup, GraphSnapshot) (pure, unit-tested)
state/preset.rs            # build_preset(&GraphSnapshot) -> Setup (pure, unit-tested)
state/volume.rs            # cubic volume mapping, dB helpers (pure, unit-tested)
pages/status.rs            # connection status page builder (adw::StatusPage variants)
pages/apps/mod.rs          # AppsPage component
pages/apps/row.rs          # AppRow FactoryComponent
pages/apps/remembered.rs   # RememberedRow FactoryComponent (rules for apps not playing)
pages/matrix/mod.rs        # MatrixPage component (grid build/update, synced scrolling)
pages/matrix/cell.rs       # CellButton widget (glib subclass of gtk::Widget)
pages/matrix/header.rs     # RowHeader / ColumnHeader builders
pages/matrix/keyboard.rs   # roving-focus navigation
widgets/peak_meter/mod.rs  # PeakMeter public wrapper
widgets/peak_meter/imp.rs  # glib subclass impl (snapshot, tick callback)
widgets/peak_meter/ballistics.rs  # pure ballistics math (unit-tested)
widgets/meter_visibility.rs       # MeterVisibility tracker → SetMeterTargets
dialogs/welcome.rs         # first-run dialog
dialogs/preferences.rs     # adw::PreferencesDialog
dialogs/name.rs            # add/rename channel or virtual mic
dialogs/notices.rs         # background notice, broken-config notice, quit confirmation (adw::AlertDialog builders)
desktop/                   # OWNED BY estuary-desktop sub-spec (app, env, window, close, actions, quit, signals, autostart/, tray)
```

### Relm4 component tree

```
AppModel (Component, Root = adw::ApplicationWindow)
├── adw::ToastOverlay
│   └── adw::ToolbarView
│       ├── top: adw::HeaderBar [ViewSwitcher title, primary menu (Preferences, About, Quit)]
│       ├── top: adw::Banner (read-only F16 | reconnect > 30 s F4; one at a time, read-only wins)
│       ├── content: gtk::Stack "root_stack"
│       │   ├── "status"  → adw::StatusPage (Connecting / Unsupported / PermissionDenied / Fatal)
│       │   └── "main"    → adw::ViewStack
│       │        ├── "apps"   → AppsPage   (Controller<AppsPage>)
│       │        └── "matrix" → MatrixPage (Controller<MatrixPage>)
│       └── bottom: adw::ViewSwitcherBar (revealed by adw::Breakpoint max-width: 550sp)
└── transient dialogs (built on demand): Welcome, Preferences, Name, AlertDialogs
```

---

## Implementation Steps

Steps are numbered `M<milestone>.<n>` (master §Implementation Steps). Each step ends with `cargo build`, `cargo clippy -- -D warnings` and `cargo test -p estuary` green.

### M3: UI shell and live Apps list

**Implementation notes (M3 as built, 2026-10-14).** These deviate from or refine the text above. Where they conflict, the notes are authoritative.

- **Scope.** The root component holds only what exists at M3: `engine`, `connection`, `fatal`, `snapshot`, the two page controllers and the toast overlay. The desktop-owned fields and `AppMsg` variants (`quit`, `quit_timers`, `hold`, `tray`, `background`, `env`, close/quit messages) are added with estuary-desktop in M11. `config` comes with M6. `AppMsg` is `Engine(Event) | RestartEngine | ShowAbout`.
- **Startup.** `main.rs` keeps the M1 single-instance guard (relm4 remote-instance hang, see packaging M1.5 notes) instead of `desktop::app::build_application()`, which arrives in M11.D1. The engine gets no `SetSetup` at M3 (no config yet). It runs read-only, as M2 allows.
- **Temporary `app.quit`.** A plain `app.quit()` action plus `<Control>q` exist until desktop M11.D4 replaces them with the Quit flow. `app.about` (UI-owned) and `win.show-apps` / `win.show-matrix` (`<Alt>1` / `<Alt>2`) are final.
- **Root component written by hand.** `Component` is implemented manually rather than with the `view!` macro. `update_view` swaps the status page only when a `Cell<bool>` dirty flag is set (connection or fatal state changed). `root_view()` is a pure, tested mapping.
- **Rows.** `AppRow` and `OutputRow` are hand-written `FactoryComponent`s on `gtk::ListBox` with `adw::ActionRow` roots. A generic `pages/reconcile.rs` (`KeyedRow` + `reconcile()`) removes, inserts, moves and updates rows in place (`guard.get_mut` → `update_view`), never rebuilding.
- **Empty state.** "Nothing is playing" is a `gtk::ListBox` placeholder inside "Playing Now", not a page-wide `adw::StatusPage`, so the Outputs group stays visible while nothing plays.
- **Output rows** show the description, "Default output" as the subtitle when applicable, and a read-only perceptual volume percentage (cubic mapping, `state/volume.rs::percent`). Meters arrive in M4, sliders in M5.
- **Window** has `set_size_request(360, 294)`, which libadwaita requires once breakpoints are used. The 550sp breakpoint reveals the bottom `ViewSwitcherBar` and hides the header title.
- **Matrix page** is a placeholder `adw::StatusPage` until M9.
- **Wayland note for M11.** A second launch from a terminal has no activation token, so GNOME shows an "Estuary is ready" notice instead of raising the window. Launches from the app grid or desktop file carry a token and raise it.

#### M3.1 Crate dependencies and module skeleton

`crates/estuary/Cargo.toml` uses workspace deps only (C1): `relm4`, `gtk`, `adw`, `estuary-engine`, `async-channel`, `serde`, `toml`, `thiserror`, `tracing`, `tracing-subscriber`, `uuid`, `gettext-rs`. Dev-deps: `tempfile = "3"`. Create the empty module files from the layout above. Add `#![forbid(unsafe_code)]` in `main.rs`.

#### M3.2 i18n

`i18n.rs`:

```rust
pub const GETTEXT_PACKAGE: &str = "estuary";                      // C14 gettext domain
pub const LOCALEDIR: &str = match option_env!("ESTUARY_LOCALEDIR") { Some(d) => d, None => "/usr/share/locale" };

pub fn init() {
    gettextrs::setlocale(gettextrs::LocaleCategory::LcAll, "");
    let _ = gettextrs::bindtextdomain(GETTEXT_PACKAGE, LOCALEDIR);
    let _ = gettextrs::bind_textdomain_codeset(GETTEXT_PACKAGE, "UTF-8");
    let _ = gettextrs::textdomain(GETTEXT_PACKAGE);
}
pub fn i18n(msgid: &str) -> String { gettextrs::gettext(msgid) }
/// Positional placeholders "{}" replaced in order, AFTER translation (translators keep "{}").
pub fn i18n_f(msgid: &str, args: &[&str]) -> String;
pub fn ni18n_f(single: &str, plural: &str, n: u32, args: &[&str]) -> String;   // ngettext + placeholders
```

Rules:

- Every user-visible literal is wrapped: `i18n("Music")`.
- Never use `format!` on a translated template. Use `i18n_f`.
- Each translatable string that needs one gets a `// Translators:` comment on the preceding line.
- `po/` (`POTFILES.in`, `LINGUAS` = `it`, `estuary.pot`, `it.po`) and the build/install of `.mo` files belong to estuary-packaging (M12). This spec provides the `it` translations as part of M12 hand-off. `ESTUARY_LOCALEDIR` is set by the build (Flatpak: `/app/share/locale`).

#### M3.3 Root component and window

`app.rs`. The root is named `AppModel`, and its field and message names follow the desktop sub-spec's "Assumptions about the UI root component" verbatim.

```rust
pub struct AppInit;                                         // launch mode is handled by desktop::app::build_application() (desktop M11.D1)

pub struct AppModel {
    // ---- fields required by estuary-desktop (names are normative) ----
    pub engine: Option<estuary_engine::Engine>,             // None after Fatal until RestartEngine, and after quit Finish
    pub config: ConfigStore,                                // owns the ConfigFile; settings()/update_settings()/flush_sync() (M6)
    pub quit: desktop::quit::QuitController,                // the single quit state machine (C10)
    pub quit_timers: HashMap<desktop::quit::QuitTimer, glib::SourceId>,   // used by desktop::quit::execute
    pub hold: Option<gio::ApplicationHoldGuard>,            // desktop M11.D2
    pub tray: Option<ksni::Handle<desktop::tray::EstuaryTray>>,           // desktop M11.D12
    pub background: desktop::close::BackgroundPermission,   // desktop M11.D3/D10
    pub env: desktop::DesktopEnv,                           // desktop M11.D8, detected once in init
    pub denied_notice_shown_this_session: bool,             // desktop M11.D3, not persisted
    pub notice_open: bool,                                  // desktop M11.D3: ignore a second ✕ while a close notice is open
    // ---- UI-owned fields ----
    meters: Arc<estuary_engine::MeterBank>,
    state: UiState,                                         // snapshot, connection, meter_slots, pending notices, autostart outcome
    apps_page: Controller<AppsPage>,
    matrix_page: Controller<MatrixPage>,
    toasts: adw::ToastOverlay,
    shown_issue_keys: HashSet<String>,                      // de-duplicates SetupIssue / warning toasts
    quit_dialog: Option<adw::AlertDialog>,                  // open confirmation, closed if a signal overrides it (C10)
}

#[derive(Debug)]
pub enum AppMsg {
    // ---- variants required by estuary-desktop (names are normative) ----
    Engine(estuary_engine::Event),
    ShowWindow,                                             // tray, app.show, second launch
    WindowCloseRequested,                                   // close-request → desktop::close::decide_close (M11.3)
    RequestQuit(desktop::quit::QuitTrigger),                // menu, Ctrl+Q, tray, D-Bus app.quit, signals
    Quit(desktop::quit::QuitInput),                         // timer expiries, forced finish
    QuitConfirmed,                                          // from show_quit_confirm
    QuitCancelled,
    BackgroundNoticeDismissed,                              // from show_background_notice → update_settings + hide (M11.3)
    BackgroundDeniedNoticeDismissed,                        // from show_background_denied_notice → RequestQuit(CloseButton)
    OpenPreferences,
    SetStartAtLogin(bool),                                  // Preferences switch (M11.2)
    AutostartApplied(Result<desktop::autostart::AutostartOutcome, desktop::autostart::AutostartError>),
    BackgroundStatusChanged(desktop::close::BackgroundPermission),
    TrayReady(ksni::Handle<desktop::tray::EstuaryTray>),    // desktop M11.D12
    // ---- UI-owned variants ----
    Edit(Edit),                                             // from pages and dialogs
    SetCloseAction(CloseAction),                            // Preferences combo (M11.2)
    MeterTargets(Vec<MeterTarget>),                         // from MeterVisibility
    SaveTick,                                               // debounce timer fired (M6.2)
    WelcomeChosen(PresetKind),
    RestartEngine,                                          // F5 button, F4 banner Retry
    WindowShown,                                            // window notify::visible → true: flush queued notices, welcome (M6.3, M11.1)
}

impl Component for AppModel {
    type Init = AppInit;
    type Input = AppMsg;
    type Output = ();
    type CommandOutput = ();
    type Root = adw::ApplicationWindow;
    type Widgets = AppWidgets;
    fn init_root() -> Self::Root;
    fn init(init: AppInit, root: Self::Root, sender: ComponentSender<Self>) -> ComponentParts<Self>;
    fn update(&mut self, msg: AppMsg, sender: ComponentSender<Self>, root: &Self::Root);
}
```

UI services required by the desktop sub-spec, implemented as `impl AppModel` methods in `app.rs`, with the dialog builders in `dialogs/notices.rs`:

```rust
pub fn show_quit_confirm(&mut self, usages: &[VirtualDeviceUsage], root: &adw::ApplicationWindow, sender: &ComponentSender<Self>); // M11.4: emits QuitConfirmed / QuitCancelled
pub fn show_background_notice(&mut self, root: &adw::ApplicationWindow, sender: &ComponentSender<Self>);        // M11.3: emits BackgroundNoticeDismissed
pub fn show_background_denied_notice(&mut self, root: &adw::ApplicationWindow, sender: &ComponentSender<Self>); // M11.3: emits BackgroundDeniedNoticeDismissed
pub fn show_toast(&self, text: &str);                                                                           // adw::Toast, 3 s
```

- Window: default size 960×640, `set_hide_on_close(false)`. `desktop::window::install_close_handler(&root, sender.clone())` connects `close-request` → `AppMsg::WindowCloseRequested` (desktop M11.D3).
- `root.connect_visible_notify`: on becoming visible → `WindowShown`. On any visibility change → `MeterVisibility::schedule_recompute()` (M4.4; hidden ⇒ `SetMeterTargets([])`).
- `init` **must not** present the window. relm4's `activate` handler and `desktop::window::show_window()` are the only places it is shown (desktop M11.D1).
- Actions and accelerators (`app.quit`, `app.preferences`, `app.show`, `<Control>q`, `<Control>comma`, `<Control>w`) are installed by `desktop::actions::install_actions` (desktop M11.D4). The UI only adds `win.show-apps` (`<Alt>1`) and `win.show-matrix` (`<Alt>2`) for page switching, plus the primary menu model referencing `app.preferences`, `app.about` (UI-owned action) and `app.quit`.
- About: `adw::AboutDialog` (libadwaita 1.5) with app id, MIT OR Apache-2.0 and a link to the repository.

#### M3.4 Engine startup and event forwarding

In `AppModel::init`:

1. `let (config, outcome) = ConfigStore::load(config::config_file())` (M6.1). Queue the load-outcome notices in `state.pending_notices` (M6.3).
2. `let env = desktop::DesktopEnv::detect()` (desktop M11.D8). Call `desktop::window::acquire_hold(&relm4::main_adw_application())` → `hold`, then `desktop::actions::install_actions(&app, sender.clone())`, `desktop::window::install_close_handler(&root, sender.clone())`, `desktop::tray::spawn_tray(env.sandboxed, icon_name, sender.clone())` (result → `AppMsg::TrayReady`) and `desktop::signals::install_signal_handlers(sender.clone())`, per the desktop sub-spec. `config.attach(sender.input_sender().clone())` enables debounced saves.
3. `let (engine, rx) = Engine::start(EngineConfig { app_id: "io.github.abidibo.Estuary", app_name: "estuary", reconnect_backoff: Default::default() })`.
4. Event forwarding (C2). The loop's tail turns a closed channel into a fatal event (UI-4):

   ```rust
   let s = sender.input_sender().clone();
   relm4::spawn_local(async move {
       while let Ok(ev) = rx.recv().await { if s.send(AppMsg::Engine(ev)).is_err() { return; } }
       let _ = s.send(AppMsg::Engine(Event::Fatal(EngineError::ThreadGone)));
   });
   ```

5. Send `Command::SetSetup(config.file().setup.clone())`, or an empty `Setup` if the welcome is pending (C10 step 3).
6. If the welcome is not pending, start the autostart sync now (`relm4::spawn(desktop::autostart::apply_autostart(settings.start_at_login, env.clone()))` → `AutostartApplied`). Otherwise defer it until `WelcomeChosen` (desktop M11.D11).
7. If `Engine::start` returns `Err(e)`, keep `engine = None` and handle it as `Event::Fatal(e)` (M11.5).

`update(AppMsg::Engine(ev))` dispatch:

| Event | Handling |
|---|---|
| `Connection(s)` | `state.connection = s`; switch `root_stack` and banner (M3.5, M11.5); on the first `Connected`, present the welcome if pending and the window is visible (M11.1) |
| `Snapshot(snap)` | `state.snapshot = snap`; `apps_page.emit(AppsMsg::Update(..))`, `matrix_page.emit(MatrixMsg::Update(..))` (both receive `Arc<GraphSnapshot>` plus `Arc<Setup>`) |
| `MeterAssignments(v)` | store in `state.meter_slots: HashMap<MeterTarget, Vec<MeterSlotId>>`; broadcast `BindMeters` to pages |
| `ExternalRetarget{..}` | `self.dispatch_edit(Edit::ExternalRetarget{..})` (M5.5) |
| `SetupIssues(v)` | pages refresh statuses; toast for new issue keys (M11.5) |
| `Restored` | `show_toast(&i18n("Audio system restarted, setup restored"))` (F4) |
| `QuitCheck(u)` | `for e in self.quit.handle(QuitInput::QuitCheck(u)) { desktop::quit::execute(e, self, &sender, root) }` (desktop M11.D6) |
| `TeardownComplete` | same with `QuitInput::TeardownComplete` |
| `Warning(w)` | M11.5 warning mapping table |
| `Fatal(e)` | `engine = None`; status page "Fatal" (M11.5) |

`dispatch_edit(edit)` is the only mutation path for the `Setup` and for first-run settings:

```rust
fn dispatch_edit(&mut self, edit: Edit, sender: &ComponentSender<Self>) {
    let out = state::edit::apply_edit(self.config.file_mut(), &self.state.snapshot, edit);
    for cmd in out.commands { if let Some(e) = &self.engine { let _ = e.send(cmd); } }
    for t in out.toasts { self.toasts.add_toast(t.into_adw()); }
    if out.save { self.config.schedule_save(); }
    if out.structure_changed { self.push_setup_to_pages(); }   // re-render with the new desired state
}
```

The remaining settings (`close_action`, `start_at_login`, `background_notice_shown`) change through `config.update_settings(..)`, as the desktop sub-spec does.

#### M3.5 Connection status pages

`pages/status.rs`: `fn status_page(state: &ConnectionState, fatal: Option<&EngineError>) -> adw::StatusPage`.

| State | Icon | Title / description | Actions | `root_stack` |
|---|---|---|---|---|
| `Connecting{attempt}` | `gtk::Spinner` as the paintable child | "Waiting for PipeWire…" / "estuary will connect as soon as the audio system is available." | none | `status` |
| `Connected{..}` | none | none | none | `main` |
| `Reconnecting{since,..}` | none | content stays visible but insensitive (`main.set_sensitive(false)`); a spinner appears in the header bar end slot | banner after 30 s (M11.5) | `main` |
| `Unsupported{reason}` | `dialog-warning-symbolic` | "Unsupported audio setup" / reason text: "WirePlumber 0.5 or newer is required (found {})", "PipeWire 1.0 or newer is required (found {})", "WirePlumber is not running" | "Learn more" (opens the README URL with `gtk::UriLauncher`) | `status` |
| `PermissionDenied` | `action-unavailable-symbolic` | "estuary can't change audio routing" / "The audio system only granted read access. If you run Estuary as a Flatpak, make sure it is up to date." | "Learn more" | `status` |
| Fatal (M11.5) | `computer-fail-symbolic` | "The audio engine stopped" / error text | "Restart engine" → `RestartEngine` | `status` |

The status page is replaced (not mutated) on every state change.

#### M3.6 Apps page: read-only list

`pages/apps/mod.rs`:

```rust
pub struct AppsPage { rows: FactoryVecDeque<AppRow>, remembered: FactoryVecDeque<RememberedRow>, setup: Arc<Setup>,
                      snapshot: Arc<GraphSnapshot>, meters: Arc<MeterBank>, empty: gtk::Stack }
#[derive(Debug)]
pub enum AppsMsg { Update { snapshot: Arc<GraphSnapshot>, setup: Arc<Setup> }, BindMeters(Arc<HashMap<MeterTarget, Vec<MeterSlotId>>>) }
#[derive(Debug)]
pub enum AppsOutput { Edit(Edit) }
impl SimpleComponent for AppsPage { type Init = Arc<MeterBank>; type Input = AppsMsg; type Output = AppsOutput; /* … */ }
```

- Layout: `gtk::ScrolledWindow` → `adw::Clamp` (max 720) → `gtk::Box` (vertical, spacing 24). It contains a "Playing now" `adw::PreferencesGroup` holding a `gtk::ListBox` (`boxed-list` style) driven by `rows`, and a "Remembered apps" group (M5.6).
- **Empty state:** `adw::StatusPage` "Nothing is playing" / "Start playing audio in any app and it will show up here." (`audio-x-generic-symbolic`).
- **Reconciling `rows` on `Update`:** compute the new ordered `Vec<AppKey>` (C4 order). Use `rows.guard()` to (a) remove rows whose key vanished, (b) insert new rows at their index, (c) send `AppRowMsg::Update(AppGroup)` to existing rows. Never `clear()` and rebuild: keyboard focus and open dropdown popovers must survive snapshots.

`pages/apps/row.rs`:

```rust
pub struct AppRow { group: AppGroup, targets: Vec<TargetChoice>, selected: u32, volume_slider_value: f64, dragging: bool }
pub struct TargetChoice { pub target: RouteTarget, pub label: String, pub available: bool }
#[derive(Debug)] pub enum AppRowMsg { Update(AppGroup, Vec<TargetChoice>), Bind(Vec<MeterSlotId>), TargetSelected(u32), VolumeChanged(f64), DragEnd }
#[derive(Debug)] pub enum AppRowOutput { Edit(Edit) }
impl FactoryComponent for AppRow { type ParentWidget = gtk::ListBox; type Init = (AppGroup, Vec<TargetChoice>); type Index = DynamicIndex; /* … */ }
```

Row widget: `adw::ActionRow` with:

- prefix: `gtk::Image` from `icon_name`, falling back to `audio-x-generic-symbolic`;
- title: `display_name`; subtitle: `media_name` of the first stream, plus "· N streams" when > 1;
- suffix, in order: a horizontal `PeakMeter` (120×6), a `gtk::Scale` volume (width 140, disabled in M3), a `gtk::DropDown` (disabled in M3).

In M3, rows show the name and current target as plain subtitle text only.

**Devices section.** In M3 the Apps page also shows an "Outputs" group (rows for present hardware outputs: name, meter, volume). Inputs are shown in the Matrix page only, which keeps the Apps page about playback.

### M4: Level meters (widget side)

**Implementation notes (M4 as built, 2026-10-14).**

- **`PeakMeter` wraps a `gtk::DrawingArea`** (`Rc<Inner>`), not a glib subclass. Drawing is a cairo draw function; the frame-clock tick runs only while bound and mapped. The accessible role is `Meter` (set at construction), with label "Level of {name}". The failed state draws a short dash and sets the tooltip "Level unavailable". The visuals are the same as specified: stereo L/R bars, green/yellow/red zones, 2 px hold tick. The user confirmed stereo bars over a single bar (2026-10-14). The high-contrast outline moves to M11.5.
- **`MeterHub` replaces `BindMeters` broadcasting and `MeterVisibility`.** It's a shared registry (weak references to meters plus their targets) that rows register with when created. It rebinds on `MeterAssignments`, on `MeterFailed` and on a new engine's bank (`set_bank`), and computes visible targets (mapped meters inside the Apps scroller, top-to-bottom). The root debounces recomputes by 100 ms, triggered by scroller adjustments, ViewStack page changes, window visibility and every snapshot, and sends `SetMeterTargets` only on change. A new or restarted engine forces a resend.
- **Measured CPU:** 0.6 % of one core with 2 live meters (debug build). The 16-meter acceptance check waits until the matrix provides enough rows.

#### M4.1 Ballistics (pure)

`widgets/peak_meter/ballistics.rs`:

```rust
pub const FLOOR_DB: f32 = -60.0;
pub const RELEASE_DB_PER_S: f32 = 20.0;          // C9
pub const HOLD_S: f32 = 1.5;                     // C9
pub const STALE_S: f32 = 0.5;                    // C9: no seq change → decay to silence
pub const WARN_DB: f32 = -12.0;                  // C9 colors
pub const CLIP_DB: f32 = -3.0;

#[derive(Clone, Copy, Default)]
pub struct ChannelBallistics { pub level_db: f32, pub hold_db: f32, pub hold_left_s: f32 }

pub fn lin_to_db(x: f32) -> f32 { if x <= 1e-6 { FLOOR_DB } else { (20.0 * x.log10()).max(FLOOR_DB) } }

/// `input_db` = newest peak (or FLOOR_DB when stale). `dt` = seconds since last frame (clamped to 0..=0.25).
pub fn step(b: &mut ChannelBallistics, input_db: f32, dt: f32) {
    b.level_db = if input_db >= b.level_db { input_db } else { (b.level_db - RELEASE_DB_PER_S * dt).max(input_db) };  // instant attack
    if input_db >= b.hold_db { b.hold_db = input_db; b.hold_left_s = HOLD_S; }
    else { b.hold_left_s -= dt; if b.hold_left_s <= 0.0 { b.hold_db = (b.hold_db - RELEASE_DB_PER_S * dt).max(b.level_db); } }
}
pub enum Zone { Normal, Warn, Clip }
pub fn zone(db: f32, raw_peak: f32) -> Zone;   // Clip if db > CLIP_DB or raw_peak >= 1.0; Warn if db >= WARN_DB
/// Map dB to a 0..=1 bar fraction: frac = ((db - FLOOR_DB) / -FLOOR_DB).clamp(0,1).powf(1.5) (more resolution near 0 dBFS).
pub fn fraction(db: f32) -> f32;
```

#### M4.2 `PeakMeter` widget

`widgets/peak_meter/imp.rs`: a `#[derive(Default)] pub struct PeakMeter { state: RefCell<MeterState> }` with `#[glib::object_subclass] impl ObjectSubclass for PeakMeter { const NAME: &str = "EstuaryPeakMeter"; type Type = super::PeakMeter; type ParentType = gtk::Widget; }`.

```rust
struct MeterState {
    binding: Option<(Arc<MeterBank>, Vec<MeterSlotId>)>,
    ch: [ChannelBallistics; 2],
    last_seq: Vec<u32>, last_change_us: i64, last_frame_us: i64,
    raw_peak: [f32; 2],
    orientation: gtk::Orientation,
    failed: bool,                  // F13: draw "—" label instead of bars
    tick: Option<gtk::TickCallbackId>,
}
```

- **Public wrapper** (`mod.rs`): `PeakMeter::new(orientation) -> Self`; `bind(&self, bank: Arc<MeterBank>, slots: Vec<MeterSlotId>)`; `unbind(&self)`; `set_failed(&self, bool)`.
- **Tick lifecycle:** `bind()` installs `add_tick_callback` if none is installed. `unbind()` removes it and resets ballistics to the floor. `unmap()` removes the tick and `map()` re-adds it when bound. A hidden page costs nothing.
- **Tick body:**
  1. `now = frame_clock.frame_time()`.
  2. For each slot, `bank.read(slot)` → `MeterReading { seq, peak }` (C9). Take the max across slots per channel (C9: App targets with several streams).
  3. If any seq changed, set `last_change_us = now`.
  4. Stale if `now - last_change_us > STALE_S`; then use the floor as input.
  5. `ballistics::step` per channel with `dt`.
  6. `queue_draw()` only if any displayed value moved by > 0.1 dB.
  7. Return `glib::ControlFlow::Continue`.
- **Drawing (`snapshot`):** two bars (L/R) split along the cross axis with a 1 px gap.
  - background: `widget.color()` at alpha 0.12;
  - level fill: `gsk` color rects split into zone segments at `fraction(WARN_DB)` and `fraction(CLIP_DB)`;
  - zone colors: libadwaita palette constants green `#33d17a`, yellow `#f6d32d`, red `#e01b24`;
  - hold: a 2 px tick at `fraction(hold_db)`;
  - when `failed`, render nothing and show a child `gtk::Label("—")` instead (`set_tooltip_text(i18n("Level unavailable"))`).
- **Size:** `measure()` returns a minimum of 6 px across and 48 px along, natural 8 and 120. Accessibility: `AccessibleRole::Meter`, label set by the owner ("Level of Spotify"), and no value updates (avoids screen-reader spam).
- **CSS:** class `estuary-meter`. High contrast (`adw::StyleManager::is_high_contrast`) draws a 1 px outline.

#### M4.3 Meter binding

On `AppMsg::Engine(Event::MeterAssignments(v))`, the root builds `Arc<HashMap<MeterTarget, Vec<MeterSlotId>>>` and sends `BindMeters` to both pages. Each page keeps `HashMap<MeterTarget, PeakMeter>` (AppsPage forwards to rows via `rows.broadcast`). It calls `bind` for targets present in the map and `unbind` for the rest. `Warning(MeterFailed(target))` → `set_failed(true)` on that meter until the next assignment that includes it.

#### M4.4 Visibility tracking → `SetMeterTargets`

`widgets/meter_visibility.rs`:

```rust
pub struct MeterVisibility { entries: Vec<(glib::WeakRef<PeakMeter>, MeterTarget)>, last_sent: Vec<MeterTarget>,
                             sender: relm4::Sender<AppMsg>, pending: Option<glib::SourceId> }
impl MeterVisibility {
    pub fn register(&mut self, meter: &PeakMeter, target: MeterTarget);   // called by pages when creating meters
    pub fn clear_page(&mut self, page: PageId);
    pub fn schedule_recompute(&mut self);   // debounced 100 ms via glib::timeout_add_local_once
}
```

- **Triggers for `schedule_recompute`:** the vertical and horizontal adjustments' `value-changed` and `changed` on every page `ScrolledWindow`; `adw::ViewStack` `notify::visible-child`; window `notify::visible` and `notify::is-active`; any page rebuild.
- **Recompute:**
  1. If the window is not visible → targets `[]` (no metering while hidden in background mode).
  2. Otherwise, for each live entry where `meter.is_mapped()` and `meter.compute_bounds(&scroller)` intersects the scroller's allocation, collect its target ordered top-to-bottom, then left-to-right.
  3. Dedup. If different from `last_sent`, send `AppMsg::MeterTargets(v)`, and the root sends `Command::SetMeterTargets(v)`.

  The 64-slot budget (C9) is enforced by the engine. Order equals priority.

### M5: Per-app routing UI

**Implementation notes (M5 as built, 2026-10-14).**

- **Rules live in memory** (`AppModel.config: ConfigFile`) until M6 persists them; `EditOutcome.save` is computed but not acted on yet. The engine gets `SetSetup(config.setup)` at every (re)start.
- **`Edit`** contains the M5 variants only (`AssignApp`, `ExternalRetarget`, `ForgetAppRule`, `AppVolume`, `DeviceVolume`). Matrix, noise-suppression and preset variants are added with M9–M11 (tasks moved there). `EditOutcome` has no toasts yet.
- **`VolumeControl`** (`pages/apps/controls.rs`) is shared by app and output rows: a 0–100 % cubic slider plus a mute toggle. A guard stops programmatic updates echoing as edits, edits are throttled to 30 Hz, and snapshots don't move the knob for 600 ms after a user change (instead of a drag gesture).
- **Output dropdown** is a `gtk::DropDown` over a `StringList` with plain labels: "Default output (…)", "Channel: …", devices, and "… (disconnected)" for unavailable current or rule targets. Selection updates are guarded.
- **Remembered Apps** group implemented per M5.6.
- Verified live by the user with a temporary silent output: routing switches instantly, app and device volume and mute work, and a remembered rule re-applies when the app restarts.

#### M5.1 Volume math (pure)

`state/volume.rs`:

```rust
/// Slider position s ∈ [0,1] ↔ PipeWire linear volume v = s³ (pavucontrol-style cubic mapping).
pub fn slider_to_linear(s: f64) -> f32 { (s.clamp(0.0, 1.0).powi(3)) as f32 }
pub fn linear_to_slider(v: f32) -> f64 { (v.max(0.0) as f64).cbrt().min(1.0) }
/// Apply a slider value to a Volume preserving channel balance: scale all channels so that max == new value.
pub fn set_volume_level(vol: &Volume, s: f64) -> Volume;
pub fn volume_level(vol: &Volume) -> f64;          // linear_to_slider(max channel)
pub const CELL_DB_MIN: f32 = -60.0;               // C6 clamp
pub const CELL_DB_MAX: f32 = 6.0;
pub fn clamp_db(db: f32) -> f32;                   // NaN → 0.0, clamp to range, round to 0.1
pub fn format_db(db: f32) -> String;               // "0 dB", "−18 dB", "+3.5 dB" (U+2212 minus), localized decimal separator
```

- Volume sliders go from 0 to 100 %, with no over-amplification in v1.
- Mute is a toggle button (`audio-volume-muted-symbolic`) next to each slider. It sets `Volume.muted` and keeps the channel levels.

#### M5.2 Edit model (pure core)

`state/edit.rs`:

```rust
#[derive(Debug, Clone)]
pub enum Edit {
    // routing (R1, R3, C7)
    AssignApp { app: AppKey, display_name: String, target: RouteTarget },
    ExternalRetarget { app: AppKey, target: RouteTarget },
    ForgetAppRule { app: AppKey },
    // volumes (fast paths, C3)
    AppVolume { app: AppKey, level: f64, muted: bool },          // not persisted (C3)
    DeviceVolume { key: DeviceKey, level: f64, muted: bool },    // not persisted (device state lives in WP)
    ChannelVolume { id: ChannelId, level: f64, muted: bool, commit: bool },
    MicVolume { id: VirtualMicId, level: f64, muted: bool, commit: bool },
    // matrix (C6)
    AddChannel { name: String }, RenameChannel { id: ChannelId, name: String }, DeleteChannel { id: ChannelId },
    AddMic { name: String }, RenameMic { id: VirtualMicId, name: String }, DeleteMic { id: VirtualMicId },
    ToggleCell { key: CellKey },                                 // absent → create enabled at 0 dB; present → flip enabled
    SetCellGain { key: CellKey, gain_db: f32, commit: bool },     // commit=false during drag (fast path only)
    RemoveCell { key: CellKey },
    ForgetDevice { key: DeviceKey },
    // noise suppression (C6, M10)
    SetDenoise { input: DeviceKey, enabled: bool },
    SetVadGate { input: DeviceKey, gate: f32, commit: bool },
    // first run (C6, C10)
    ApplyPreset(PresetKind),
}
// Settings other than first_run_done change via ConfigStore::update_settings (M6.1), driven by
// AppMsg::SetCloseAction / AppMsg::SetStartAtLogin and by the desktop close/notice flow (M11.2, M11.3).

pub struct EditOutcome {
    pub commands: Vec<Command>,
    pub toasts: Vec<ToastSpec>,           // { text: String, timeout_s: u32 }
    pub save: bool,
    pub structure_changed: bool,          // rows/columns/cells set changed → pages re-render from Setup
}

pub fn apply_edit(cfg: &mut ConfigFile, snap: &GraphSnapshot, edit: Edit) -> EditOutcome;
```

Normative semantics per edit:

| Edit | Config mutation | Commands | save | structure |
|---|---|---|---|---|
| `AssignApp` | upsert `app_rules[app] = { target, display_name }`; if target is a `Device(k)` not in `known_devices`, add it from snapshot (C6 invariant) | `SetSetup` | ✓ | ✗ |
| `ExternalRetarget` | same upsert (display name from snapshot `AppGroup`, else the existing rule's, else the key); **no-op** if the rule already equals the target (C7 anti-ping-pong) | `SetSetup` if changed | ✓ if changed | ✗ |
| `ForgetAppRule` | remove the rule | `SetSetup` | ✓ | ✗ |
| `AppVolume` / `DeviceVolume` | none | `SetAppVolume` / `SetDeviceVolume` with `set_volume_level(current, level)` + muted | ✗ | ✗ |
| `ChannelVolume` / `MicVolume` | update `volume` | `SetChannelVolume` / `SetVirtualMicVolume` | ✓ only when `commit` | ✗ |
| `AddChannel` / `AddMic` | push with `Uuid::new_v4()`, name trimmed (empty → `i18n("Channel")` / `i18n("Virtual mic")` + " N"), volume `[1.0, 1.0]`, no position | `SetSetup` | ✓ | ✓ |
| `Rename*` | set name (trimmed, non-empty, ≤ 64 chars) | `SetSetup` | ✓ | ✓ |
| `DeleteChannel` | remove channel, its cells, and **rewrite rules targeting it to `Default`** (F6, C7) | `SetSetup` | ✓ | ✓ |
| `DeleteMic` | remove mic and its cells | `SetSetup` | ✓ | ✓ |
| `ToggleCell` | absent → push `CellDef { gain_db: 0.0, enabled: true }` (and add any device to `known_devices`); present → flip `enabled` | `SetSetup` | ✓ | ✓ |
| `SetCellGain` | set `gain_db = clamp_db(..)` | `SetCellGain` | ✓ only when `commit` | ✗ |
| `RemoveCell` | remove the cell | `SetSetup` | ✓ | ✓ |
| `ForgetDevice` | allowed only if the device is not present in the snapshot; remove it from `known_devices`, plus every cell, denoiser and `denoised` cell referencing it; rules targeting it → `Default` | `SetSetup` | ✓ | ✓ |
| `SetDenoise` | upsert `denoisers[input]` with `enabled`; creating one adds `known_devices` entry; **disabling keeps the entry and its `denoised` cells** (they show as inactive) | `SetSetup` | ✓ | ✓ |
| `SetVadGate` | clamp 0.0..=0.95, set | `Command::SetVadGate { input, gate }` (C3 fast path) during drag; `SetSetup` on commit | ✓ on commit | ✗ |
| `ApplyPreset` | M11.1 (also sets `settings.first_run_done = true`) | `SetSetup` | ✓ (followed by `flush_sync`) | ✓ |

Toasts (F6):

- When `DeleteChannel` or `ForgetDevice` rewrites any rules: `ni18n_f("{} moved to default output", "{} apps moved to default output", n, ..)`.
- In read-only mode (F16), `save` is computed as usual but `ConfigStore` ignores it (M6.2).

#### M5.3 Output dropdown

`AppsPage` builds a `Vec<TargetChoice>` per row from snapshot + setup:

1. `Default`: label `i18n_f("Default output ({})", &[default_output description])`.
2. Channels in Setup order, those with `present`, labelled by name with the prefix "Channel: " in the list factory subtitle.
3. Present hardware outputs by description.
4. If the row's `current_target` or its rule target is unavailable (offline device), it is appended, marked `available: false` and shown dimmed with "(disconnected)".

The `gtk::DropDown` uses a `gtk::StringList` plus a custom `gtk::SignalListItemFactory` (title + subtitle). `selected` is set from `AppGroup.current_target`. A **programmatic** selection change must not emit `TargetSelected`: guard with a `updating: Cell<bool>` flag around `set_selected`.

`TargetSelected(i)` → `AppRowOutput::Edit(Edit::AssignApp { app, display_name, target: targets[i].target })`.

`movable == false` (F7) → dropdown insensitive with tooltip `i18n("This app does not allow moving its audio")`.

#### M5.4 App and device volume sliders

- `gtk::Scale` (0..1, step 0.01, `draw_value=false`, tooltip shows the percentage), initialised with `volume_level(group.volume)`. The value is not overwritten from snapshots while `dragging` (a `GestureDrag` on the scale toggles it, and `DragEnd` clears it), so the knob doesn't jump.
- `value-changed` → `VolumeChanged(v)`, throttled to 30 Hz (keep the last value and send on a 33 ms `glib::timeout_add_local_once` if none is pending) → `Edit::AppVolume`.
- The same applies to the "Outputs" device rows (`Edit::DeviceVolume`) and to channel and mic headers (M9.3, `commit=false` while dragging, `commit=true` on release).

#### M5.5 External retarget

`Event::ExternalRetarget { app, target }` → `dispatch_edit(Edit::ExternalRetarget { .. })`. No toast; the dropdown just follows the next snapshot. Per C7 the engine does not re-apply the old rule, and the no-op guard in `apply_edit` prevents loops.

#### M5.6 Remembered apps

A second group on the Apps page, "Remembered apps", lists `app_rules` whose `app` is **not** in the snapshot. Each row shows `display_name`, its target label (or "Default output"), and a remove button (`user-trash-symbolic`, accessible label `i18n_f("Forget {}", ..)`) → `Edit::ForgetAppRule`. The group is hidden when empty. *(Judgment call beyond the master: rules would otherwise be invisible and impossible to delete.)*

### M6: Config I/O

**Implementation notes (M6 as built, 2026-10-14).**

- **`ConfigStore`** is implemented per M6.1/M6.2. `attach(callback)` replaces `attach(Sender<AppMsg>)` (the callback emits `SaveTick`). The pending debounce source clears itself when it fires, so it is never removed after running. `settings()`, `update_settings()` and `welcome_pending()` arrive with the desktop and welcome work (M11).
- **Missing vs corrupt.** A read error where the file doesn't exist (including an unusable parent directory, `ENOTDIR`) is `Fresh`. Only an existing but unreadable or unparsable file is `Recovered`. If the corrupt file can't be moved aside, the store goes read-only so it is never overwritten.
- **Notices.** The load outcome is shown when the window is first **mapped** (`connect_map` → `AppMsg::WindowShown`). The newer-schema banner is added at init. Pending changes are also flushed in the root component's `shutdown`.
- **Dropdown fix (found while testing M6).** `TargetSelected` now carries the `RouteTarget`, resolved from the shown choices at click time, instead of an index resolved later. A snapshot that reordered the list in between could otherwise have picked the wrong output.
- **Verified live:** a hand-written rule in `config.toml` was applied to a playing stream at startup, and a dropdown change was written to `config.toml` within 0.5 s. The test config was removed afterwards.

#### M6.1 `ConfigStore::load`

`config.rs`. `ConfigStore` owns the `ConfigFile`, so the desktop sub-spec can use `settings()`, `update_settings()` and `flush_sync()` on `AppModel::config` directly.

```rust
pub use estuary_engine::setup::{CloseAction, ConfigFile, Settings, Setup};   // desktop/close.rs imports crate::config::{CloseAction, Settings}

pub struct ConfigStore {
    path: PathBuf, file: ConfigFile, read_only: bool,
    pending: Option<glib::SourceId>, notify: Option<relm4::Sender<AppMsg>>, last_error: Option<String>,
}

pub enum LoadOutcome {
    Fresh,                                                  // missing → defaults, welcome pending
    Loaded,
    Recovered { backup: PathBuf, error: String },           // F14
    NewerSchema { found: u32 },                             // F16 → read-only
    Repaired { issues: Vec<SetupIssue> },                   // C12 semantic drops, from setup::sanitize
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError { #[error("{0}")] Io(#[from] std::io::Error), #[error("{0}")] Serialize(#[from] toml::ser::Error), #[error("read-only configuration")] ReadOnly }

pub fn config_file() -> PathBuf { glib::user_config_dir().join("estuary").join("config.toml") }   // C12

impl ConfigStore {
    pub fn load(path: PathBuf) -> (ConfigStore, LoadOutcome);
    pub fn attach(&mut self, notify: relm4::Sender<AppMsg>);         // called in AppModel::init; enables debounced saves
    pub fn file(&self) -> &ConfigFile;
    pub fn file_mut(&mut self) -> &mut ConfigFile;                   // used by dispatch_edit; caller decides on schedule_save
    pub fn settings(&self) -> &Settings;                             // desktop sub-spec
    pub fn update_settings(&mut self, f: impl FnOnce(&mut Settings)); // desktop sub-spec: mutate + schedule_save()
    pub fn schedule_save(&mut self);
    pub fn flush_sync(&mut self) -> Result<(), ConfigError>;         // desktop finish(); also SaveTick and after ApplyPreset
    pub fn is_read_only(&self) -> bool;
    pub fn welcome_pending(&self) -> bool;                           // !settings.first_run_done
}
```

Algorithm:

1. Read the file. `ErrorKind::NotFound` → `(defaults, Fresh)`. Any other I/O error → treat as corrupt (step 3) when the file exists but is unreadable, and log at `warn`.
2. Parse the `toml::Value` first and read `schema_version`. If it is > 1, deserialize leniently (unknown fields ignored, which is serde's default without `deny_unknown_fields`), set `read_only = true`, and return `NewerSchema`.
3. Deserialize `ConfigFile`. On error, rename to `config.toml.broken-<local time %Y-%m-%dT%H-%M-%S>` (`glib::DateTime::now_local().format(..)`; on a name collision append `-1`, `-2`), then return `(defaults, Recovered { backup, error: e.to_string() })`. `toml::de::Error`'s Display includes the line and column (F14).
4. Repair with `let (setup, issues) = estuary_engine::setup::sanitize(file.setup)` (C3 / engine sub-spec). It drops only dangling or invalid entries, as C12 allows. If `issues` is non-empty, return `Repaired { issues }`. Repaired configs are **not** written back until the next real edit, so the original file stays inspectable.
5. Defaults: `ConfigFile { schema_version: 1, settings: Settings { close_action: Hide, start_at_login: true, background_notice_shown: false, first_run_done: false }, setup: Setup::default() }`.

#### M6.2 Save

- `schedule_save`: if read-only or not attached → return. Cancel any pending source, then `glib::timeout_add_local_once(500ms, move || notify.emit(AppMsg::SaveTick))`. `AppModel` handles `SaveTick` by calling `flush_sync`.
- `flush_sync`: if read-only → `Err(ReadOnly)`, and callers ignore it silently. Otherwise cancel the pending source, then:
  1. `create_dir_all(parent)`.
  2. Serialize with `toml::to_string_pretty`.
  3. Write to `config.toml.tmp` via `File::create`, `write_all`, `sync_all`.
  4. `rename` over `config.toml`.
- On error: keep `last_error`, return `Err`. `AppModel` shows the toast `i18n("Couldn't save settings")` at most once per distinct error string (F15). The next edit schedules another attempt. A successful flush after an error clears `last_error`.
- `flush_sync` is also called by the desktop `finish()` (C10 step 4) and right after `ApplyPreset`.

#### M6.3 Load-outcome surfaces

Queued in `UiState.pending_notices` and shown on `AppMsg::WindowShown`, which fires immediately when the window is presented at launch. Under `--background` they wait until the window is first shown.

- `Recovered` → `adw::AlertDialog` titled `i18n("Your saved setup couldn't be read")`, with body `i18n_f("It was kept as {}.\n\n{}", &[backup file name, error])` and the response "OK". The welcome dialog follows (F14).
- `NewerSchema` → `adw::Banner` `i18n("This configuration was saved by a newer version of estuary. Changes won't be saved.")`, permanently revealed (F16). The banner is set in `init`; it doesn't need the window to be visible.
- `Repaired` → toast `ni18n_f("Removed {} invalid entry from the saved setup", "Removed {} invalid entries from the saved setup", n, ..)`. Details are logged at `warn` with `issue_text()` (M11.5).

### M9: Matrix grid UI

**Implementation notes (M9 as built, 2026-10-15).** These deviate from or refine the text below. Where they conflict, the notes are authoritative.

- **Gain keys.** Arrow keys belong to grid navigation (M9.5, capture phase), so a cell never sees Up/Down. Gain keys are `+`/`=`/`KP_Add` and `−`/`KP_Subtract` (±1 dB), Page Up/Down (±6), Home (0 dB) and End (−60 dB). Fine values go through the popover's spin button. Shift+arrow modifiers aren't used.
- **Click vs. double click.** Turning a send on is instant. Turning an *active* send off waits for the GTK double-click time, so a double click resets to 0 dB without toggling off and on (which would fade out, destroy and recreate the node, an audible dropout).
- **`CellButton`** is a real `gtk::Widget` subclass (BinLayout, CSS name `cellbutton`, `AccessibleRole::Slider`). This works under `#![forbid(unsafe_code)]` once the wrapper lists `Accessible`, `Buildable` and `ConstraintTarget`. The look is CSS on an inner label pill (`cell::install_css`), with a spinner and badge overlay for Pending, Missing and Error. Snapshots don't overwrite a gain the user changed in the last 600 ms (same rule as `VolumeControl`), which also covers drags.
- **Sizes.** Cells are 72×44. Row headers are 220 wide and exactly 44 tall; column headers exactly 72 wide and **108** tall (96 couldn't fit label, meter, scale and menu). Only the cell-aligned dimension must match, and the outer 2×2 grid aligns the rest. The left column sets `hexpand(false)` explicitly: the header labels' `hexpand` would otherwise propagate and split the spare width.
- **Header volumes** are bare sliders (no mute button, no room); mute is kept as it is. Edits are throttled to 30 Hz with `commit=false`, then `commit=true` 400 ms after the last change, instead of detecting the scale's release. Inputs and outputs send `DeviceVolume` (not saved).
- **`PeakMeter`** gained `horizontal(w, h)` and `vertical(w, h)` constructors. Vertical meters draw the horizontal picture rotated.
- **Dialogs** are presented by the page itself (`dialogs/name.rs`, `dialogs/confirm.rs`); the page emits only `MatrixOutput::Edit` and `MetersMoved`. There is no `OpenNameDialog` output.
- **Setup issues.** Cell failures already show through `CellStatus::Error` in the snapshot, so there is no `MatrixMsg::Issues`. `AppModel` toasts issues without a cell, once per `SetupIssue::key`. `EditOutcome.toasts` is `Vec<String>` (3 s each).
- **Decimal separator** in `format_db` is a translatable string with context (`pi18n("decimal separator", ".")`), since the crate forbids the unsafe libc locale calls.
- **Empty state**: the status page shows while there are no channels and no virtual mics; the grid (with hardware rows and columns) appears once one exists.
- **Dev aids** in `app.rs`: `ESTUARY_PAGE=matrix` opens on that page, `ESTUARY_DEV_INERT=1` makes the content non-interactive (for measurements), and `ESTUARY_SNAPSHOT=out.png` renders the window to a PNG with GTK's renderer after 2.5 s, then quits. All layout checks used these with a scratch `XDG_CONFIG_HOME`.
- **CPU (M4 deferred check):** 17 visible meters, 8 of them moving, release build, window untouched: **1.6–2.05 %** of one core (about 1.5 % is the PipeWire data loop running the meter streams; the GTK thread uses 0.2–0.5 %). Debug build: about 2.9 %. Runs where the window was being used read 3–9 %; that was the interaction, not the meters.
- Engine test fixtures behind `test-util` (moved task) weren't needed: the layout and edit tests build snapshots directly.

#### M9.1 Matrix layout (pure)

`state/matrix_layout.rs`:

```rust
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum RowKey { Channel(ChannelId), Input(DeviceKey), Denoised(DeviceKey) }
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ColKey { Output(DeviceKey), VirtualMic(VirtualMicId) }

pub struct RowSpec { pub key: RowKey, pub source: MatrixSource, pub label: String, pub present: bool,
                     pub volume: Option<Volume>, pub meter: MeterTarget, pub denoise: Option<DenoiseView> }
pub struct ColSpec { pub key: ColKey, pub dest: MatrixDest, pub label: String, pub present: bool,
                     pub volume: Option<Volume>, pub meter: MeterTarget }
pub struct CellView { pub key: CellKey, pub exists: bool, pub enabled: bool, pub gain_db: f32, pub status: Option<CellStatus> }
pub struct DenoiseView { pub enabled: bool, pub active: bool, pub vad_gate: f32, pub xrun_warning: bool }

pub struct MatrixLayout { pub rows: Vec<RowSpec>, pub cols: Vec<ColSpec>, pub cells: Vec<Vec<CellView>> }   // cells[row][col]

pub fn compute(setup: &Setup, snap: &GraphSnapshot) -> MatrixLayout;
impl MatrixLayout { pub fn shape_key(&self) -> (Vec<RowKey>, Vec<ColKey>) }
```

Rules:

- **Rows:**
  - channels in Setup order;
  - then hardware inputs: `snap.inputs`, which per C4 already includes known-offline ones, sorted by description;
  - each input that has a `DenoiserDef` is **immediately followed** by its `Denoised(key)` row, labelled `i18n_f("{} (noise-suppressed)", ..)`.
- **Columns:** `snap.outputs` (present and known-offline, by description), then virtual mics in Setup order.
- `present` comes from the snapshot state.
- `CellView.status` comes from `snap.cells` matched by key. A cell in Setup with no snapshot entry gets `Pending`.
- Inputs that are offline **and** not referenced by any cell or denoiser are still listed (they are in `known_devices`), so the user can forget them.

#### M9.2 `CellButton` widget

`pages/matrix/cell.rs`: a glib subclass of `gtk::Widget` (`NAME = "EstuaryCellButton"`), fixed size **72×44 px** (required for grid alignment, see M9.4), focusable, CSS name `cellbutton`.

```rust
pub struct CellButton;     // wrapper
impl CellButton {
    pub fn new(key: CellKey, a11y_label: String) -> Self;
    pub fn set_view(&self, v: &CellView);                 // no signal emission
    pub fn connect_edit<F: Fn(Edit) + 'static>(&self, f: F);
}
```

**Visual states:**

| State | Look |
|---|---|
| Absent | dim "+" glyph on hover only |
| Present + disabled | outlined pill with the dB value greyed. The engine builds no node for it (C8, amended); the snapshot still reports it with its stored gain. `status` is ignored while disabled |
| Present + enabled | filled accent pill with `format_db(gain)` |
| `Pending` | spinner glyph |
| `SourceMissing` / `DestinationMissing` | pill with the `network-offline-symbolic` overlay, tooltip "Disconnected, will reconnect automatically" (F10) |
| `Error(msg)` | `dialog-error-symbolic` overlay, tooltip = msg |

**Interaction** (all edits are emitted through `connect_edit`):

| Input | Effect |
|---|---|
| Primary click (press+release without drag > 4 px) | `ToggleCell` |
| Vertical drag on a present cell | gain += −dy × 0.25 dB/px (Shift: 0.05 dB/px); emits `SetCellGain{commit:false}` throttled to 30 Hz, and `commit:true` on release |
| Scroll (`EventControllerScroll`, vertical, discrete or smooth) | ±1 dB per step (Shift ±0.1 dB); emits `SetCellGain{commit:false}`, then `commit:true` 400 ms after the last scroll |
| Double click | gain → 0 dB (`commit:true`) |
| Secondary click / long press / `Menu` key / `Shift+F10` | popover menu: "Turn on/off", "Set to 0 dB", "Remove send", plus a `gtk::SpinButton` (−60…+6, 0.5 step) for exact entry |
| `Space` / `Return` | `ToggleCell` |
| `Up` / `Down` | ±1 dB (`Shift`: ±0.1); commit debounced 400 ms |
| `Page_Up` / `Page_Down` | ±6 dB |
| `Home` | 0 dB |
| `End` | −60 dB |
| `Delete` | `RemoveCell` |

- Gain gestures on an **absent** cell do nothing. On a **disabled** cell they adjust the stored gain without enabling it.
- **Accessibility:** `AccessibleRole::Slider`. Label `i18n_f("{} to {}", &[row label, col label])`. Properties `ValueMin=-60`, `ValueMax=6`, `ValueNow=gain`, `ValueText="−18 dB, on" | "off" | "no send"`. Values are updated on every `set_view`.

#### M9.3 Headers

`pages/matrix/header.rs`:

```rust
pub fn build_row_header(spec: &RowSpec, meters: &mut MeterVisibility, on_edit: Rc<dyn Fn(Edit)>, on_menu: Rc<dyn Fn(RowMenuAction)>) -> RowHeader;
pub fn build_col_header(spec: &ColSpec, meters: &mut MeterVisibility, on_edit: Rc<dyn Fn(Edit)>, on_menu: Rc<dyn Fn(ColMenuAction)>) -> ColHeader;
impl RowHeader { pub fn update(&self, spec: &RowSpec); pub fn widget(&self) -> &gtk::Widget; pub fn meter(&self) -> &PeakMeter; }
```

- **Row header (220×44):** label (ellipsized, tooltip with the full name), a vertical `PeakMeter` (6×36), a compact volume `gtk::Scale` (100 px) for channels (`ChannelVolume`) and inputs (`DeviceVolume`), and a menu button (`view-more-symbolic`).
  - Channel menu: Rename…, Delete.
  - Input menu: a "Noise suppression" checkbox (M10.1) and "Forget device" (only when `!present`).
  - Denoised row: indented 16 px with `↳`, showing the VAD gate control instead of a volume (M10.2).
  - Offline rows: `dim-label` class and the subtitle "Disconnected".
- **Column header (72×96):** a label that is not rotated (GTK4's `gtk::Label` has no angle property): wrapping, `max_width_chars(8)`, `lines(2)`, ellipsized, with the full name in its tooltip. Below it: a horizontal `PeakMeter` (56×6), a vertical `gtk::Scale` (inverted, 60 px) for the device or mic volume, and a menu button.
  - Virtual-mic menu: Rename…, Delete.
  - Output menu: "Forget device" when offline.
- Volume scales follow the M5.4 dragging and throttling rules.

#### M9.4 `MatrixPage` grid

`pages/matrix/mod.rs`:

```rust
pub struct MatrixPage {
    layout: MatrixLayout, shape: (Vec<RowKey>, Vec<ColKey>),
    cells: HashMap<CellKey, CellButton>, row_headers: HashMap<RowKey, RowHeader>, col_headers: HashMap<ColKey, ColHeader>,
    body: gtk::Grid, row_grid: gtk::Grid, col_grid: gtk::Grid, meters: Rc<RefCell<MeterVisibility>>,
    empty: adw::StatusPage,
}
#[derive(Debug)] pub enum MatrixMsg { Update { snapshot: Arc<GraphSnapshot>, setup: Arc<Setup> }, BindMeters(Arc<HashMap<MeterTarget, Vec<MeterSlotId>>>),
                                      AddChannel, AddMic, RowMenu(RowKey, RowMenuAction), ColMenu(ColKey, ColMenuAction), Issues(Vec<SetupIssue>) }
#[derive(Debug)] pub enum MatrixOutput { Edit(Edit), OpenNameDialog(NameDialogKind) }
```

Layout: a 2×2 `gtk::Grid`, with the action bar on top:

```
┌ corner (220×96): "Add channel" / "Add virtual mic" buttons ┬ col_scroller: ScrolledWindow(hpolicy External, vpolicy Never) → col_grid ┐
├ row_scroller: ScrolledWindow(hpolicy Never, vpolicy External) → row_grid ┼ body_scroller: ScrolledWindow(Automatic both) → body grid ┘
```

- `col_scroller.set_hadjustment(body_scroller.hadjustment())` and `row_scroller.set_vadjustment(body_scroller.vadjustment())` share adjustments, so the headers stay pinned while the body scrolls.
- Fixed cell size (72×44) and fixed header sizes guarantee alignment without size groups. Row/column spacing is 4 px in all three grids.
- **Update algorithm** on `Update`:
  1. `new = matrix_layout::compute(&setup, &snap)`.
  2. If `new.shape_key() != self.shape`: **rebuild**. Remember the focused `CellKey`, destroy all children and maps, recreate headers and cells, re-register meters with `MeterVisibility` (page `Matrix`), restore focus to the same `CellKey` if it still exists, then `schedule_recompute()`.
  3. Else: **in-place update**. `set_view` on each `CellButton`, `update` on each header. A cell currently being dragged ignores incoming gain (it keeps a local `dragging` flag), as in M5.4.
- **Empty state** (no channels and no virtual mics): `adw::StatusPage` titled "Build your mix", with the description "Add channels for groups of apps and virtual microphones for calls." and two buttons, `Add channel` and `Add virtual mic`. Hardware-only rows and columns are still shown below once at least one exists.

#### M9.5 Keyboard navigation and accessibility

`pages/matrix/keyboard.rs`: roving tab stop.

- Only one cell is focusable at a time (`set_focusable(true)` on the "current" cell, `false` on the others), so `Tab` enters the grid once and leaves it on the next `Tab`.
- An `EventControllerKey` on `body` (capture phase) handles `Left/Right/Up/Down` without modifiers: move the current `(r, c)`, clamped. With `Control`, jump to the row or column edge. Focus the new cell and `scroll_to` it by adjusting the body adjustments so the cell's bounds are visible.
- Row and column headers are reachable with `Shift+Tab` / `Tab` in their own regions (their scales and menu buttons are normal widgets).
- Grid `AccessibleRole::Grid`, cells as M9.2, headers with `AccessibleRole::RowHeader` / `ColumnHeader` and their labels.
- After a structural rebuild, the current position is restored by key (M9.4).

#### M9.6 Add/rename dialogs

`dialogs/name.rs`:

```rust
pub enum NameDialogKind { AddChannel, AddMic, RenameChannel(ChannelId, String), RenameMic(VirtualMicId, String) }
pub fn present(parent: &impl IsA<gtk::Widget>, kind: NameDialogKind, on_done: impl Fn(Edit) + 'static);
```

An `adw::AlertDialog` with an extra child `adw::EntryRow` inside an `adw::PreferencesGroup`, and responses "Cancel" / "Add" (or "Rename", suggested). The default response is enabled only when the trimmed text is non-empty and ≤ 64 chars. `Return` activates. Default text: "Channel N" / "Virtual mic N", the first free N.

#### M9.7 Delete with F6

Delete channel or virtual mic → `adw::AlertDialog` confirmation (destructive appearance). The body lists consequences:

- "Its sends will be removed."
- For channels with rules: `ni18n_f("{} app will move to the default output.", "{} apps will move to the default output.", n)`.

On confirm → `Edit::DeleteChannel` / `Edit::DeleteMic` (rule rewrite and toast per M5.2).

#### M9.8 Offline devices

Rows and columns with `present == false` stay visible (F10, keep-and-restore). Styling: `dim-label` header, subtitle "Disconnected", cells drawn with the missing overlay per `CellStatus`. The "Forget device" menu entry → confirmation (`adw::AlertDialog`: "Forget {}? Its sends and noise suppression settings will be removed.") → `Edit::ForgetDevice`. Forget is unavailable while the device is present.

#### M9.9 Setup issues display

`Event::SetupIssues(v)` → `MatrixMsg::Issues(v)`. Issues tied to a `CellKey` are already reflected in `CellStatus` via the snapshot. Issues without a cell (`SetupIssue::cell()` is `None`, C3) go to a toast in `AppModel` (M11.5). `SetupIssue::CellFailed` marks its cell with the error overlay.

### M10: Noise suppression UI

**Implementation notes (M10 as built, 2026-10-15).**

- **Off still passes audio.** The engine keeps the denoiser node in bypass when suppression is off (constant latency, click-free toggle, C11), so the noise-suppressed row keeps carrying the *unprocessed* mic and its sends keep working. The header shows "Off" in that case; `DenoiseView.active` means "node present", not "suppressing".
- **Input menu:** a "Noise Suppression" `gtk::CheckButton` → `Edit::SetDenoise` (`HeaderAction::Denoise(bool)`; `RowSpec.suppression` holds the state for inputs). The first enable toasts the routing hint.
- **Noise-suppressed row header:** a short "↳ Noise-suppressed" label (full name in the tooltip), a `gtk::Switch`, a 56 px voice-gate scale (0–0.95, step 0.05; no marks, they don't fit) with the spec tooltip, and a badge image. The gate edits use the shared throttle (30 Hz `commit=false`, `commit=true` 400 ms after the last change).
- **Warnings (`state/warnings.rs`):** the denoiser part of the M11.5 mapping: Xruns → warning badge, NodeCreateFailed → error badge, DenoiseRateUnsupported → error badge plus one toast per rate. Badges clear after 60 s without a repeat (`AppMsg::ExpireWarnings` every 10 s). The rest of the mapping arrives with M11. `DenoiseView.badge` replaces `xrun_warning`.

#### M10.1 Per-input toggle

Each input row's menu has a "Noise suppression" check item. Toggling sends `Edit::SetDenoise { input, enabled }`:

- **First enable** creates the `DenoiserDef` (with `vad_gate: 0.0`) and the `Denoised` row appears under the input. The UI **does not** auto-create cells for it; a toast offers a shortcut: `i18n("Noise suppression on. Send the noise-suppressed row to a virtual mic in the matrix.")`.
- **Disable** keeps the def and its cells (the row shows "Off" and its cells render as present-but-inactive via the snapshot `DenoiserState.active == false`).

#### M10.2 Denoised row controls

The `Denoised` row header replaces the volume scale with:

- a `gtk::Switch` (enabled, same edit as M10.1);
- a "Voice gate" `gtk::Scale` (0..0.95, step 0.05, marks at 0 "Off" and 0.5), giving `Edit::SetVadGate { commit: false }` during drag and `commit: true` on release. Tooltip: `i18n("Mutes sound that is unlikely to be speech. Off by default.")`;
- a warning icon, visible when `DenoiseView.xrun_warning` is set (from `Warning(Xruns{ node: WarnedNode::Denoiser(key), .. })`, F12), and an error icon for `NodeCreateFailed`/`DenoiseRateUnsupported` on that denoiser. Texts and clearing follow the M11.5 warning mapping.

The row meter uses `MeterTarget::Denoised(key)`.

### M11: Lifecycle UI

#### M11.1 Welcome dialog and preset

`dialogs/welcome.rs`: an `adw::Dialog` (`content_width` 480, not closable by Escape; closing with ✕ counts as "Start empty"). Content: an `adw::StatusPage` (app icon, title "Welcome to Estuary", description "How would you like to start?") with two `adw::ActionRow`s in a boxed list:

- "Recommended setup": "Music, Games and Chat channels plus a virtual microphone with noise suppression" → `WelcomeChosen(PresetKind::Recommended)`.
- "Start empty": "Only your real devices; build your own mix" → `WelcomeChosen(PresetKind::Empty)`.

The dialog is presented after the first `Connected` while `!settings.first_run_done`, so the snapshot has default devices.

`state/preset.rs` `build_preset(snap: &GraphSnapshot) -> Setup` (C10):

```rust
pub enum PresetKind { Recommended, Empty }
// Recommended:
// channels: i18n("Music"), i18n("Games"), i18n("Chat") (localized at creation, C10), volume [1.0, 1.0]
// virtual_mics: i18n("Virtual mic")
// cells: each channel → Output(default_output) at 0 dB enabled   (skipped if default_output is None)
// if let Some(inp) = default_input: denoisers += { input: inp, enabled: true, vad_gate: 0.0 };
//                                   cells += Denoised(inp) → VirtualMic at 0 dB enabled
// known_devices: default_output / default_input with description from snapshot
// app_rules: none
// Empty: Setup::default()
```

`Edit::ApplyPreset(kind)` replaces `cfg.setup` with `build_preset(..)` (Empty keeps `known_devices`; there are none at first run), sets `settings.first_run_done = true` and sends `SetSetup`. `AppModel` then calls `config.flush_sync()` immediately and starts the deferred autostart sync (`relm4::spawn(desktop::autostart::apply_autostart(settings.start_at_login, env.clone()))` → `AutostartApplied`; desktop M11.D11 runs it after the welcome so first launch never shows two dialogs at once). If `default_output` is `None`, show the toast `i18n("No output device found. Connect channels to an output in the Matrix page.")`. The Matrix page is selected after Recommended, the Apps page after Empty.

#### M11.2 Preferences dialog

`dialogs/preferences.rs`: `adw::PreferencesDialog` with one page, "General", and one group, "Background":

- `adw::ComboRow` "When closing the window": `Keep running in the background` (Hide) / `Quit` → `AppMsg::SetCloseAction(a)` → `config.update_settings(|s| s.close_action = a)`.
- `adw::SwitchRow` "Start at login" → `AppMsg::SetStartAtLogin(v)`. The desktop sub-spec's handling (M11.D11) applies: `config.update_settings(|s| s.start_at_login = v)`, then `relm4::spawn(apply_autostart(v, env.clone()))` → `AppMsg::AutostartApplied`. The switch shows **the user's intent** (`settings.start_at_login`). The effective state is explained in the row subtitle, and **the setting is never reverted**, not on error and not on a mismatch (desktop rule).
- Rendering of the last `AutostartOutcome.state` (kept in `UiState.autostart: Option<Result<AutostartOutcome, String>>`), pure function `fn autostart_row(intent: bool, last: Option<&Result<AutostartOutcome, String>>) -> AutostartRowSpec { sensitive: bool, subtitle: Option<String>, icon: Option<&'static str> }`:

| Last outcome | Switch | Subtitle (msgid) |
|---|---|---|
| none yet (sync not run / in flight) | sensitive | none |
| `Enabled{Portal}` / `Disabled{Portal}` matching intent | sensitive | none |
| `Enabled{DesktopFile(path)}` / `Disabled{DesktopFile(_)}` (native builds) | sensitive | none (when enabled: tooltip shows the path) |
| `Disabled{..}` while intent is on (portal granted background but refused autostart) | sensitive | "Your desktop didn't allow starting at login." |
| `Denied` | sensitive (toggling re-asks the portal) | "Background activity isn't allowed for estuary. You can allow it in Settings → Apps → estuary." |
| `ManagedElsewhere(path)` | sensitive | `i18n_f("Another autostart entry exists at {}. estuary won't change it.", &[path])` |
| `Unavailable{NoBackgroundPortal}` (Flatpak on desktops without the Background portal, e.g. COSMIC) | **insensitive** | "Not available on this desktop: it has no background portal. Estuary still keeps running while its window is closed." |
| `Err(e)` | sensitive | `i18n_f("Couldn't update start at login: {}", &[e])` |

- `AutostartApplied(Err(e))` also shows the toast `i18n("Couldn't update start at login")` (desktop M11.D11). `AutostartApplied(Ok(o))` stores `o.background` in `AppModel::background` (desktop) and re-renders the row if the dialog is open.
- No other preferences in v1.

#### M11.3 Window close and background notice

`AppMsg::WindowCloseRequested` → if `notice_open` → ignore. Otherwise `match desktop::close::decide_close(config.settings(), self.background, self.denied_notice_shown_this_session)` (desktop M11.D3):

- `Hide` → `desktop::window::hide_window(root)`.
- `ShowBackgroundNoticeThenHide` → `show_background_notice(root, sender)` **while the window is still visible** (C10, amended). The dialog is an `adw::AlertDialog` with heading `i18n("estuary keeps running")`, body `i18n("Your audio routing keeps working in the background. Use Quit in the menu to stop estuary.")` and the response "OK" (default). It sets `notice_open = true`. Any response or close emits `AppMsg::BackgroundNoticeDismissed` → `notice_open = false`, `config.update_settings(|s| s.background_notice_shown = true)`, `hide_window(root)`.
- `Quit` → `RequestQuit(QuitTrigger::CloseButton)`.
- `QuitBecauseBackgroundDenied { show_notice: true }` → `show_background_denied_notice(root, sender)`: an `adw::AlertDialog` with heading `i18n("estuary will quit")`, body `i18n("Background activity is not allowed for estuary, so closing the window quits it. You can allow it in Settings → Apps → estuary.")` and the response "OK". It sets `notice_open = true` and `denied_notice_shown_this_session = true`. Dismissal emits `AppMsg::BackgroundDeniedNoticeDismissed` → `notice_open = false`, `RequestQuit(QuitTrigger::CloseButton)`.
- `QuitBecauseBackgroundDenied { show_notice: false }` → `RequestQuit(QuitTrigger::CloseButton)`.

Hiding triggers `MeterVisibility` → `SetMeterTargets([])` (M4.4).

`AppMsg::ShowWindow` → `desktop::window::show_window(root)`. The `WindowShown` message (from `notify::visible`) then shows any queued load notices (M6.3), then the welcome dialog if it is pending and the engine has reached `Connected`.

`AppMsg::BackgroundStatusChanged(p)` → `self.background = p` (desktop-owned semantics), re-render Preferences if open.

#### M11.4 Quit confirmation and teardown state (UI side)

There is exactly one quit state machine: the desktop sub-spec's `desktop::quit::QuitController` (C10 ownership, desktop M11.D5/M11.D6). The UI routes and renders; it does not decide.

Routing in `AppModel::update` (all effects go through `desktop::quit::execute(effect, self, &sender, root)`):

| Message | Input fed to `self.quit.handle(..)` |
|---|---|
| `RequestQuit(trigger)` | `QuitInput::Request { trigger, engine_connected: matches!(state.connection, ConnectionState::Connected{..}), engine_present: self.engine.is_some() }` |
| `Quit(input)` | `input` (timers, `Force`) |
| `QuitConfirmed` / `QuitCancelled` | `UserConfirmed` / `UserCancelled` |
| `Engine(Event::QuitCheck(u))` | `QuitCheck(u)` |
| `Engine(Event::TeardownComplete)` | `TeardownComplete` |

`show_quit_confirm(usages, root, sender)`, called by the executor for `QuitEffect::ShowConfirm` after it has shown the window. It builds an `adw::AlertDialog` (`dialogs/notices.rs`), stores it in `quit_dialog`, and presents it:

- heading `i18n("Quit estuary?")`;
- body: one line per usage, worded by `UsageKind` (C3):
  - `PlayingToChannel` → `i18n_f("{} is playing into “{}”.", &[app_display_name, device_name])`;
  - `RecordingFromVirtualMic` → `i18n_f("{} is recording from “{}”.", &[app_display_name, device_name])`;
  - followed by `i18n("Quitting will switch these apps back to your default devices.")`;
- responses "Cancel" (default, close response) / "Quit" (destructive) → `AppMsg::QuitCancelled` / `AppMsg::QuitConfirmed`. `quit_dialog` is cleared on response.

If a signal overrides an open confirmation (C10, desktop transition `Confirming + Request{Signal}`), the desktop executor emits `QuitEffect::DismissConfirm`, which calls the UI's `close_quit_confirm()` (`force_close()` on `quit_dialog`, emitting neither `QuitConfirmed` nor `QuitCancelled`).

**Teardown presentation.** While `self.quit.state()` is `Checking` or `TearingDown`, `root_stack` is insensitive and a `gtk::Spinner` shows in the header bar end slot. In practice the window is usually hidden by `QuitEffect::HideWindow`. `finish()` (desktop) performs engine shutdown, `config.flush_sync()`, tray shutdown, hold release and `app.quit()`. The UI adds nothing to it.

#### M11.5 Toasts, banners and recovery surfaces

| Trigger | Surface | Text (msgid) | Action |
|---|---|---|---|
| `Restored` (F4) | toast, 3 s | "Audio system restarted, setup restored" | none |
| `Reconnecting{since}` with `since.elapsed() > 30 s` (F4) | banner (checked by a 1 s `glib::timeout_add_local` while reconnecting) | "Can't reach the audio system. Retrying…" | button "Retry" → `RestartEngine` |
| `Fatal(e)` (F5) | status page | "The audio engine stopped" + `e.to_string()` | "Restart engine" → `RestartEngine` |
| `RestartEngine` | none | none | if engine is `Some`: `shutdown(1 s)`; then `Engine::start`, re-forward events, `SetSetup(current)`, `SetMeterTargets(last)` |
| Rule rewrite on delete/forget (F6) | toast | see M5.2 | none |
| Save failure (F15) | toast, once per distinct error | "Couldn't save settings" | none |
| Corrupt config (F14) | alert dialog | M6.3 | none |
| Newer schema (F16) | banner, permanent | M6.3 | none |
| `SetupIssues(v)`: issues with `cell().is_none()` and a new `key()` | toast, 5 s | "Some routes couldn't be created" | button "Details" → `adw::AlertDialog` listing `issue_text(i)` |
| Meter failure (F13) | inline "—" | "Level unavailable" (tooltip) | none |

`fn issue_text(i: &SetupIssue) -> String` wraps each known variant in gettext (C3: `message()` is English):

| `SetupIssue` | Text |
|---|---|
| `DuplicateId(id)` | `i18n_f("Duplicate entry “{}” was ignored", ..)` |
| `DanglingReference{what, missing}` | `i18n_f("{} refers to a missing item ({})", ..)` |
| `DuplicateCell(c)` | `i18n_f("Duplicate send {} was ignored", ..)` |
| `InvalidGain{cell, value}` | `i18n_f("Invalid gain {} for {} was corrected", ..)` |
| `MissingKnownDevice(k)` | `i18n_f("Device {} was added to the known devices", ..)` |
| `DenoisedWithoutDenoiser(k)` | `i18n_f("Noise-suppressed source for {} has no noise suppressor", ..)` |
| `CellFailed{cell, message}` | shown on the cell (error overlay + tooltip `message`), not toasted |
| unknown future variant | `i.message()` |

Warning mapping (`Event::Warning`, C3). Badges clear after 60 s without a repeat. Toasts are de-duplicated through `shown_issue_keys` with the key `warn:<variant>:<node>`.

| `EngineWarning` | `WarnedNode` → badge location | Badge / tooltip | Toast (once per key) |
|---|---|---|---|
| `Xruns{node, per_minute}` (F12) | `Cell` → cell overlay; `Denoiser` → denoised row header; `Channel` → channel row header; `VirtualMic` → mic column header; `Device` → device row/column header | `dialog-warning-symbolic`, `ni18n_f("Audio processing is overloaded ({} dropout per minute)", "… ({} dropouts per minute)", per_minute, ..)` | none |
| `LinkFailed{cell: Some(k), description, message}` | cell `k` | `dialog-error-symbolic`, tooltip `message` | none (persistent failures become `SetupIssue::CellFailed`) |
| `LinkFailed{cell: None, description, ..}` | none | none | `i18n_f("Couldn't connect {}", &[description])` |
| `NodeCreateFailed{node, message}` | per `WarnedNode` as above | `dialog-error-symbolic`, tooltip `message` | `i18n_f("Couldn't create “{}”", &[node label])` |
| `MeterFailed(target)` (F13) | that meter | "—" with "Level unavailable" | none |
| `VolumeWriteFailed{node}` | none (the slider snaps back on the next snapshot) | none | `i18n_f("Couldn't change the volume of {}", &[node label])` |
| `DenoiseRateUnsupported{input, rate}` | denoised row of `input` | `dialog-error-symbolic`, `i18n_f("Noise suppression doesn't support {} Hz", &[rate])` | same text |

`node label` comes from the layout: channel/mic name, device description, or `"{row} → {col}"` for cells.

Banner priority: read-only (F16) wins over reconnect (F4). Only one `adw::Banner` exists, and its title, button and revealed state are recomputed by `fn banner_state(&UiState, read_only: bool) -> Option<BannerSpec>` (pure).

---

## Visual Aids

### Apps page (mockup)

```
┌───────────────────────────── estuary ─────────────────────── ≡ ┐
│                    [ ♪ Apps ]  [ ▦ Matrix ]                     │
├─────────────────────────────────────────────────────────────────┤
│  Playing now                                                    │
│ ┌─────────────────────────────────────────────────────────────┐ │
│ │ 🎵 Spotify            ▮▮▮▮▮▯▯▯  ───●──── 🔈  [ Music      ▾]│ │
│ │    Bohemian Rhapsody                                         │ │
│ ├─────────────────────────────────────────────────────────────┤ │
│ │ 🌐 Firefox · 3 streams▮▮▯▯▯▯▯▯  ──────●─ 🔈  [ Default (Sp…▾]│ │
│ ├─────────────────────────────────────────────────────────────┤ │
│ │ 🎮 Game               ▮▮▮▮▮▮▮▯  ─────●── 🔈  [ Games      ▾]│ │
│ └─────────────────────────────────────────────────────────────┘ │
│  Outputs                                                        │
│ ┌─────────────────────────────────────────────────────────────┐ │
│ │ Speakers (built-in)   ▮▮▮▯▯▯▯▯  ────●─── 🔈                  │ │
│ │ WH-1000XM4            ▮▮▮▮▮▯▯▯  ───●──── 🔈                  │ │
│ └─────────────────────────────────────────────────────────────┘ │
│  Remembered apps                                                │
│ ┌─────────────────────────────────────────────────────────────┐ │
│ │ Discord                                   → Chat       [🗑] │ │
│ └─────────────────────────────────────────────────────────────┘ │
└─────────────────────────────────────────────────────────────────┘
```

### Matrix page (mockup)

```
┌───────────────────────────── estuary ─────────────────────── ≡ ┐
│                    [ ♪ Apps ]  [ ▦ Matrix ]                     │
├──────────────────────┬──────────┬──────────┬──────────┬─────────┤
│ [+ Channel] [+ Mic]  │ Speakers │ WH-1000  │ Virtual  │         │
│                      │ ▮▮▯▯     │ ▮▮▮▯     │ mic ▮▯▯▯ │         │
│                      │  ┃ vol   │  ┃ vol   │  ┃ vol   │         │
├──────────────────────┼──────────┼──────────┼──────────┤         │
│ Music   ▮ ──●── ⋮    │    +     │ [ 0 dB ] │ [−18 dB] │         │
│ Games   ▮ ───●─ ⋮    │    +     │ [ 0 dB ] │    +     │         │
│ Chat    ▮ ──●── ⋮    │    +     │ [ 0 dB ] │    +     │         │
│ Laptop mic ▮ ─●─ ⋮   │    +     │    +     │ ( 0 dB ) │  ← outlined = off
│  ↳ noise-suppr. ▮ ⏻ gate─●─ │    +     │    +     │ [ 0 dB ] │         │
│ USB mic (Disconnected) ⋮ │  dim   │   dim    │   dim    │         │
└──────────────────────┴──────────┴──────────┴──────────┴─────────┘
  [ x dB ] = enabled send   ( x dB ) = disabled send   + = no send
```

### Edit flow (single source of truth)

```mermaid
flowchart LR
  W["Widget gesture<br/>(dropdown, cell drag, dialog)"] -->|Edit| A["AppModel::dispatch_edit"]
  A --> F["apply_edit (pure)<br/>mutates ConfigFile"]
  F -->|commands| E["Engine: SetSetup / fast path"]
  F -->|save=true| S["ConfigStore.schedule_save<br/>500 ms debounce, atomic"]
  F -->|toasts| T["ToastOverlay"]
  E -->|Snapshot / Issues| A2["AppModel: UiState.snapshot"]
  A2 -->|Update(snapshot, setup)| P["AppsPage / MatrixPage<br/>(in-place or rebuild)"]
```

## Failure Modes & Recovery

UI handling of the master failure modes. Recovery logic that lives in the engine is not repeated here.

| # | Failure | UI recovery | User sees |
|---|---|---|---|
| F4 | PipeWire restart | Content insensitive while `Reconnecting`; 1 s poll for the 30 s threshold; `Restored` toast; Retry → `RestartEngine` | Header spinner → banner after 30 s → toast |
| F5 | Engine panic / `Engine::start` error | `engine = None`; status page; `RestartEngine` reconstructs | "The audio engine stopped" + Restart |
| F6 | Channel deleted with rules | Rules rewritten in `apply_edit` (same `SetSetup`) | Confirmation lists affected apps; toast after |
| F7 | `movable == false` | Dropdown insensitive | Tooltip |
| F10 | Device disappears | Layout keeps known-offline rows/columns; cells show missing overlay; Forget only when offline | Dimmed "Disconnected" row/column |
| F13 | Meter failed / slot budget | `set_failed(true)`; unbound meters stay at floor | "—" with tooltip |
| F14 | Corrupt config | Backup rename, defaults, alert dialog, welcome | Alert with backup name + parse error |
| F15 | Save failure | Keep in memory; toast once per distinct error; retry on next edit | Toast |
| F16 | Newer schema | `read_only`; saves skipped | Permanent banner |
| UI-1 | Dialog presented while window hidden | The quit confirmation is preceded by `show_window` (desktop executor); load notices and the welcome queue until `WindowShown`; close notices are shown while the window is still visible (C10) | Window appears with dialog |
| UI-2 | Snapshot arrives during a drag | Dragged widget ignores incoming values until release | Knob doesn't jump |
| UI-3 | `apply_autostart` fails or differs from intent | Setting **not** reverted (desktop rule); toast on `Err`; Preferences subtitle explains the effective state (M11.2) | Toast; explanatory subtitle; switch keeps the user's choice |
| UI-4 | Event channel closed (engine thread gone without `Fatal`) | Forwarding loop ends → send `AppMsg::Engine(Event::Fatal(EngineError::ThreadGone))` from the loop's tail | Fatal status page |
| UI-5 | Programmatic widget updates re-emitting signals | `updating` guard flags on dropdowns, scales and switches | Nothing (no spurious edits) |

## Acceptance Criteria

- [ ] `cargo test -p estuary` covers `apply_edit` (every `Edit` variant, including F6 rewrite and the `ExternalRetarget` no-op), `matrix_layout::compute` (row order, denoised placement, offline inclusion, `Pending` status), `build_preset` (with and without default devices), `volume` math (round-trip `slider ↔ linear` within 1e-6, balance preservation, `clamp_db` NaN/limits), `ballistics::step` (instant attack, 20 dB/s release, 1.5 s hold, stale decay), `banner_state`, `autostart_row` (every `AutostartState` plus `Err`, intent never rewritten), `issue_text` (every `SetupIssue` variant), and `ConfigStore` load/save in a `tempfile` dir (missing, valid, corrupt → backup name pattern, newer schema → read-only, atomic save leaves no `.tmp` on success). None of these tests initialise GTK.
- [ ] With the engine connected, the Apps page lists a running `pw-play` with `application.name` set, within 100 ms of its snapshot. Its meter moves while audio plays, and the row disappears when it stops.
- [ ] Selecting a channel in an app's dropdown changes `config.toml` (after ≤ 1 s) and moves the audio. Moving the app externally (pavucontrol) updates the dropdown and the rule with no ping-pong (no second metadata write in `pw-mon`).
- [ ] Opening a dropdown, then receiving snapshots, does not close the popover or reset its selection.
- [ ] Matrix: clicking an empty cell creates a 0 dB send. Dragging changes gain smoothly with the label following. Scrolling changes ±1 dB. Double-click resets to 0 dB. Releasing commits once (exactly one save within 1 s, observed via file mtime).
- [ ] Matrix keyboard: `Tab` enters the grid on one cell, arrows move between cells, `Space` toggles, `Up/Down` adjust gain, `Tab` leaves the grid. Orca reads "Music to Headphones, slider, −18 dB, on".
- [ ] Adding or removing a channel rebuilds the grid and keeps focus on the same cell key when it still exists. Volume changes do not rebuild (verified by a debug counter in `tracing` at `debug` level).
- [ ] Headers stay pinned while scrolling a 10×10 matrix in both directions.
- [ ] Bluetooth headphones off → column dims with "Disconnected" and cells show the offline overlay. Back on → normal, no user action. "Forget device" is only offered while disconnected.
- [ ] First launch with empty config: the welcome dialog appears after connect. "Recommended setup" produces Music/Games/Chat + Virtual mic with cells per C10 and selects the Matrix page.
- [ ] ✕ with default settings shows the background notice once, then hides. Later ✕ hides silently. The meter subscription becomes empty while hidden (engine log).
- [ ] Quit with a recording app on the virtual mic shows the confirmation (“Discord is recording from “Virtual mic”.”). Cancel keeps everything. Quit tears down within 2 s and the process exits. `SIGTERM` while the confirmation is open closes it and quits.
- [ ] Preferences on a Flatpak build under a desktop without the Background portal shows "Start at login" insensitive with the explanation. On a native build the switch writes/removes the autostart file. A failed or denied autostart never flips the switch back.
- [ ] `LANGUAGE=it` shows all UI strings in Italian (no untranslated literal found by a grep for `"` literals not wrapped in `i18n*` in `crates/estuary/src`, excluding identifiers, CSS and log messages).
- [ ] Window usable at 360 px width: the bottom view switcher appears and the matrix scrolls in both directions without overlapping headers.

## Alternatives Considered

| Alternative | Why considered | Why rejected |
|---|---|---|
| Matrix via `FactoryVecDeque` on `gtk::Grid` (`GridPosition`) | relm4 supports `gtk::Grid` as a factory view (verified) | Positions are two-dimensional and shift when a column is inserted; factories are one-dimensional collections. Diffing two factory axes is more complex than rebuilding on shape change plus in-place updates |
| `gtk::ColumnView` for the matrix | Virtualized, native headers | Columns are static per model type; per-cell custom widgets with drag/scroll gestures and roving focus fight ColumnView's row selection model; matrix sizes (≤ ~20×20) don't need virtualization |
| Meter updates through Relm4 messages | Uniform architecture | 60 fps × N meters of messages and view updates; C9 explicitly bypasses channels; direct atomic reads in a tick callback are cheaper and jitter-free |
| Background notice as a desktop notification after hiding | Doesn't delay hiding | Notifications can be suppressed or missed; master asks for an `adw::AlertDialog` (C10). Showing it before hiding is the only way an in-window dialog is visible (C10, amended) |
| Async config I/O (gio / tokio) | Never blocks the main loop | Config is a few KB; a debounced synchronous write plus `fsync` takes ~ms; keeps C12 semantics simple and testable with `tempfile` |

## Contract notes

Resolved by the master amendment log: C10 notice-before-hide, C9 `MeterBank::read`, C3 `SetVadGate`, C8 disabled cells, C10 lifecycle ownership (desktop state machine, UI dialogs), C3 `EngineWarning`/`WarnedNode`/`SetupIssue`/`VirtualDeviceUsage`, setup repair via `sanitize`, and UNVERIFIED-2 now covering input devices. Remaining items:

1. **Disabled cells (C8 amended, confirmation).** The engine has no node for `enabled=false` cells, but `GraphSnapshot.cells` still reports them with their stored gain. `CellButton` renders a present-but-disabled cell from `CellView { exists: true, enabled: false, gain_db }` (outlined pill, greyed dB) and **ignores `status`** while disabled, so no `Pending` spinner is shown for cells the engine intentionally does not build. No contract change.
2. **Remembered apps (M5.6).** New UI surface beyond the master: lists and deletes rules for apps not currently playing. No contract change.
3. **Input device rows on the Apps page.** Inputs appear only in the Matrix page (rows); outputs appear on both pages. No contract change.

## Interfaces used from other sub-specs

**estuary-engine** (as defined in C3/C4/C9 and `specs/estuary-engine/spec.md`):

- `Engine::{start, send, meters, shutdown}`, `Command` (including `SetVadGate`), `Event`, `ConnectionState`, `EngineError` (`ThreadGone` is used for UI-4), `UnsupportedReason`, `EngineWarning`, `WarnedNode`, `SetupIssue` (`key()`, `cell()`, `message()`), `VirtualDeviceUsage { app_display_name, device_name, kind: UsageKind { PlayingToChannel, RecordingFromVirtualMic } }`.
- `MeterBank::read(&self, MeterSlotId) -> MeterReading { seq, peak }` (C9).
- `estuary_engine::setup::{ConfigFile, Settings, CloseAction, Setup, ChannelDef, VirtualMicDef, DenoiserDef, CellDef, AppRule, KnownDevice, DeviceKind, Position}` (`Setup: Clone + Default`), `setup::validate(&Setup) -> Vec<SetupIssue>`, `setup::sanitize(Setup) -> (Setup, Vec<SetupIssue>)`.
- Key types (`AppKey`, `DeviceKey`, `ChannelId`, `VirtualMicId`, `CellKey`, `MeterTarget`) are used as `HashMap` keys: the UI needs `Clone + Eq + Hash + Debug` on them. The engine spec stores `CellKey` in a `HashMap`, so `Hash` is implied there. **To confirm** for `MeterTarget` and `RowKey`/`ColKey` inputs (`ChannelId`, `VirtualMicId`, `DeviceKey`).

**estuary-desktop** (module `crate::desktop`, compiled into this crate; free functions, no `Desktop` object):

- `desktop::app::build_application() -> adw::Application` (M11.D1), used in `main.rs` with `RelmApp::from_app(app).visible_on_activate(true).run::<AppModel>(AppInit)`.
- `desktop::DesktopEnv::detect()` (M11.D8); `desktop::window::{acquire_hold, show_window, hide_window, install_close_handler}` (M11.D2/D3); `desktop::close::{decide_close, CloseDecision, BackgroundPermission}` (M11.D3); `desktop::actions::install_actions(app, sender)` (M11.D4); `desktop::quit::{QuitController, QuitInput, QuitEffect, QuitTrigger, QuitTimer, QuitState, execute}` (M11.D5/D6, `finish()` inside the executor); `desktop::signals::install_signal_handlers(sender)` (M11.D7); `desktop::autostart::{apply_autostart, AutostartOutcome, AutostartState, AutostartBackend, UnavailableReason, AutostartError}` (M11.D11); `desktop::tray::{spawn_tray, EstuaryTray}` (M11.D12).
- Root fields, `AppMsg` variants and UI services required by the desktop's "Assumptions about the UI root component" are all provided (M3.3, M11.2–M11.4).
