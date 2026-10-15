//! Matrix page (UI M9.4): channels and inputs down the side, outputs and virtual mics across
//! the top, one send per crosspoint.
//!
//! Three grids share scroll adjustments so the headers stay pinned while the body scrolls.
//! Fixed cell and header sizes keep them aligned. A snapshot that keeps the same rows and
//! columns updates widgets in place; any other change rebuilds the grid.

pub mod cell;
pub mod header;
pub mod keyboard;

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use adw::prelude::*;
use estuary_engine::{CellKey, GraphSnapshot, MeterTarget, RouteTarget, Setup};
use relm4::{ComponentParts, ComponentSender, SimpleComponent, adw, gtk};

use self::cell::{CELL_HEIGHT, CELL_WIDTH, CellButton};
use self::header::{ColHeader, DenoiseEvent, HeaderAction, ROW_HEADER_WIDTH, RowHeader};
use self::keyboard::Nav;
use crate::dialogs::{confirm, name};
use crate::i18n::{i18n, i18n_f};
use crate::state::edit::Edit;
use crate::state::matrix_layout::{self, ColKey, MatrixLayout, RowKey};
use crate::state::warnings::Warnings;
use crate::widgets::meter_hub::MeterHub;

const SPACING: i32 = 4;
/// Height of a group label row (CHANNELS, MICROPHONES).
const GROUP_HEIGHT: i32 = 26;

pub struct MatrixPage {
    hub: MeterHub,
    setup: Arc<Setup>,
    layout: MatrixLayout,
    shape: Option<(Vec<RowKey>, Vec<ColKey>)>,
    rebuilds: u64,
    cells: HashMap<CellKey, CellButton>,
    rows: Vec<RowHeader>,
    cols: Vec<ColHeader>,
    body: gtk::Grid,
    row_grid: gtk::Grid,
    col_grid: gtk::Grid,
    row_scroller: gtk::ScrolledWindow,
    col_scroller: gtk::ScrolledWindow,
    nav: Rc<RefCell<Nav>>,
    stack: gtk::Stack,
    /// Each row's header and slots (and each column's header and slots) share a size group,
    /// so the three grids can't drift apart whatever the labels contain.
    size_groups: Vec<gtk::SizeGroup>,
}

impl std::fmt::Debug for MatrixPage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MatrixPage")
            .field("rows", &self.layout.rows.len())
            .field("cols", &self.layout.cols.len())
            .field("rebuilds", &self.rebuilds)
            .finish_non_exhaustive()
    }
}

#[derive(Debug)]
pub enum MatrixMsg {
    Update {
        snapshot: Arc<GraphSnapshot>,
        setup: Arc<Setup>,
        warnings: Arc<Warnings>,
    },
    AddChannel,
    AddMic,
    Row(RowKey, HeaderAction),
    Col(ColKey, HeaderAction),
    /// From a cell, header or dialog.
    Edit(Edit),
}

#[derive(Debug)]
pub enum MatrixOutput {
    Edit(Edit),
    /// Scrolling or a rebuild changed which meters are on screen.
    MetersMoved,
}

impl SimpleComponent for MatrixPage {
    type Init = MeterHub;
    type Input = MatrixMsg;
    type Output = MatrixOutput;
    type Root = gtk::Stack;
    type Widgets = ();

    fn init_root() -> gtk::Stack {
        let stack = gtk::Stack::new();
        stack.set_transition_type(gtk::StackTransitionType::Crossfade);
        stack
    }

    fn init(
        hub: MeterHub,
        root: gtk::Stack,
        sender: ComponentSender<Self>,
    ) -> ComponentParts<Self> {
        cell::install_css();
        let add_buttons = |orientation| {
            let b = gtk::Box::new(orientation, 6);
            let labelled = |label: &str| {
                let content = adw::ButtonContent::new();
                content.set_icon_name("list-add-symbolic");
                content.set_label(label);
                let button = gtk::Button::new();
                button.set_child(Some(&content));
                button
            };
            let channel = labelled(&i18n("Add Channel"));
            channel.connect_clicked({
                let s = sender.input_sender().clone();
                move |_| s.emit(MatrixMsg::AddChannel)
            });
            let mic = labelled(&i18n("Add Virtual Mic"));
            mic.connect_clicked({
                let s = sender.input_sender().clone();
                move |_| s.emit(MatrixMsg::AddMic)
            });
            b.append(&channel);
            b.append(&mic);
            b
        };

        // Empty state: nothing of ours yet.
        let empty = adw::StatusPage::new();
        empty.set_icon_name(Some("view-grid-symbolic"));
        empty.set_title(&i18n("Build your mix"));
        empty.set_description(Some(&i18n(
            "Add channels for groups of apps and virtual microphones for calls.",
        )));
        let empty_buttons = add_buttons(gtk::Orientation::Horizontal);
        empty_buttons.set_halign(gtk::Align::Center);
        for b in [empty_buttons.first_child(), empty_buttons.last_child()]
            .into_iter()
            .flatten()
        {
            b.add_css_class("pill");
        }
        if let Some(b) = empty_buttons.first_child() {
            b.add_css_class("suggested-action");
        }
        empty.set_child(Some(&empty_buttons));
        root.add_named(&empty, Some("empty"));

        // What this page is, in one line, plus the add buttons.
        let intro = gtk::Label::new(Some(&i18n(
            "Each row is a sound source, each column a destination. Click a slot to send the source there, then drag up or down on it (or scroll) to set the level. Click a name for its options.",
        )));
        intro.set_wrap(true);
        intro.set_xalign(0.0);
        intro.set_hexpand(true);
        intro.add_css_class("dim-label");
        let actions = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        actions.append(&how_it_works());
        let top_buttons = add_buttons(gtk::Orientation::Horizontal);
        top_buttons.set_hexpand(true);
        top_buttons.set_halign(gtk::Align::End);
        actions.append(&top_buttons);
        let top = gtk::Box::new(gtk::Orientation::Vertical, 8);
        top.set_margin_top(12);
        top.set_margin_start(12);
        top.set_margin_end(12);
        top.append(&intro);
        top.append(&actions);

        // Grid: (empty) corner | column headers / row headers | body.
        let corner = gtk::Box::new(gtk::Orientation::Vertical, 0);
        corner.set_size_request(ROW_HEADER_WIDTH, -1);
        let grid = |accessible: Option<gtk::AccessibleRole>| {
            let mut b = gtk::glib::Object::builder::<gtk::Grid>()
                .property("row-spacing", SPACING)
                .property("column-spacing", SPACING)
                .property("row-homogeneous", false)
                .property("column-homogeneous", true);
            if let Some(role) = accessible {
                b = b.property("accessible-role", role);
            }
            let g = b.build();
            // Columns stretch to the available width (equal widths in the header and body
            // grids, which share their scroller's width); rows stay at their size.
            g.set_halign(gtk::Align::Fill);
            g.set_valign(gtk::Align::Start);
            g
        };
        let body = grid(Some(gtk::AccessibleRole::Grid));
        body.update_property(&[gtk::accessible::Property::Label(&i18n("Sends"))]);
        let row_grid = grid(None);
        let col_grid = grid(None);
        let body_scroller = gtk::ScrolledWindow::new();
        body_scroller.set_child(Some(&body));
        body_scroller.set_hexpand(true);
        body_scroller.set_vexpand(true);
        let col_scroller = gtk::ScrolledWindow::new();
        col_scroller.set_policy(gtk::PolicyType::External, gtk::PolicyType::Never);
        col_scroller.set_child(Some(&col_grid));
        col_scroller.set_hadjustment(Some(&body_scroller.hadjustment()));
        col_scroller.set_hexpand(true);
        let row_scroller = gtk::ScrolledWindow::new();
        row_scroller.set_policy(gtk::PolicyType::Never, gtk::PolicyType::External);
        row_scroller.set_child(Some(&row_grid));
        row_scroller.set_vadjustment(Some(&body_scroller.vadjustment()));
        row_scroller.set_vexpand(true);
        // The header labels expand; that must not leak into the outer grid's columns.
        row_scroller.set_hexpand(false);
        corner.set_hexpand(false);
        corner.set_vexpand(false);
        col_scroller.set_vexpand(false);
        let outer = gtk::Grid::new();
        outer.set_row_spacing(SPACING.unsigned_abs());
        outer.set_column_spacing(SPACING.unsigned_abs());
        outer.set_margin_top(12);
        outer.set_margin_start(12);
        outer.set_margin_end(12);
        outer.set_margin_bottom(12);
        outer.attach(&corner, 0, 0, 1, 1);
        outer.attach(&col_scroller, 1, 0, 1, 1);
        outer.attach(&row_scroller, 0, 1, 1, 1);
        outer.attach(&body_scroller, 1, 1, 1, 1);
        let page = gtk::Box::new(gtk::Orientation::Vertical, 0);
        page.append(&top);
        // On very wide windows the table stops growing and stays centred.
        let clamp = adw::Clamp::new();
        clamp.set_maximum_size(1200);
        clamp.set_tightening_threshold(1200);
        clamp.set_child(Some(&outer));
        clamp.set_vexpand(true);
        page.append(&clamp);
        root.add_named(&page, Some("grid"));
        root.set_visible_child_name("empty");

        for adj in [body_scroller.hadjustment(), body_scroller.vadjustment()] {
            let s = sender.output_sender().clone();
            adj.connect_value_changed(move |_| {
                let _ = s.send(MatrixOutput::MetersMoved);
            });
        }

        let nav = Rc::new(RefCell::new(Nav::default()));
        keyboard::install(&body, &body_scroller, &nav);

        let model = MatrixPage {
            hub,
            setup: Arc::default(),
            layout: MatrixLayout::default(),
            shape: None,
            rebuilds: 0,
            cells: HashMap::new(),
            rows: Vec::new(),
            cols: Vec::new(),
            body,
            row_grid,
            col_grid,
            row_scroller,
            col_scroller,
            nav,
            stack: root,
            size_groups: Vec::new(),
        };
        ComponentParts { model, widgets: () }
    }

    fn update(&mut self, msg: MatrixMsg, sender: ComponentSender<Self>) {
        match msg {
            MatrixMsg::Update {
                snapshot,
                setup,
                warnings,
            } => {
                // App names as the Apps page shows them (e.g. Electron apps' real names).
                let mut snap = (*snapshot).clone();
                for a in &mut snap.apps {
                    crate::pages::apps::identity::resolve(a);
                }
                self.layout = matrix_layout::compute(&setup, &snap, &warnings);
                self.setup = setup;
                self.stack
                    .set_visible_child_name(if self.layout.has_owned_nodes() {
                        "grid"
                    } else {
                        "empty"
                    });
                let shape = self.layout.shape_key();
                if self.shape.as_ref() == Some(&shape) {
                    self.update_in_place();
                } else {
                    self.rebuild(&sender);
                    self.shape = Some(shape);
                    let _ = sender.output(MatrixOutput::MetersMoved);
                }
            }
            MatrixMsg::Edit(e) => {
                let _ = sender.output(MatrixOutput::Edit(e));
            }
            MatrixMsg::AddChannel => {
                let taken: Vec<&str> = self
                    .setup
                    .channels
                    .iter()
                    .map(|c| c.name.as_str())
                    .collect();
                name::present(
                    &self.stack,
                    name::NameDialogKind::AddChannel,
                    &taken,
                    self.edit_sink(&sender),
                );
            }
            MatrixMsg::AddMic => {
                let taken: Vec<&str> = self
                    .setup
                    .virtual_mics
                    .iter()
                    .map(|m| m.name.as_str())
                    .collect();
                name::present(
                    &self.stack,
                    name::NameDialogKind::AddMic,
                    &taken,
                    self.edit_sink(&sender),
                );
            }
            MatrixMsg::Row(key, action) => self.on_row_action(&key, action, &sender),
            MatrixMsg::Col(key, action) => self.on_col_action(&key, action, &sender),
        }
    }
}

impl MatrixPage {
    /// Targets of the header meters on screen, top-to-bottom then left-to-right (UI M4.4).
    pub fn visible_meter_targets(&self) -> Vec<MeterTarget> {
        if self.stack.visible_child_name().as_deref() != Some("grid") {
            return Vec::new();
        }
        let mut out = self.hub.visible_targets(&self.col_scroller);
        for t in self.hub.visible_targets(&self.row_scroller) {
            if !out.contains(&t) {
                out.push(t);
            }
        }
        out
    }

    fn edit_sink(&self, sender: &ComponentSender<Self>) -> impl Fn(Edit) + 'static {
        let s = sender.input_sender().clone();
        move |e| s.emit(MatrixMsg::Edit(e))
    }

    fn update_in_place(&self) {
        for (r, spec) in self.layout.rows.iter().enumerate() {
            if let Some(h) = self.rows.get(r) {
                h.update(spec);
            }
        }
        for (c, spec) in self.layout.cols.iter().enumerate() {
            if let Some(h) = self.cols.get(c) {
                h.update(spec);
            }
        }
        for view in self.layout.cells.iter().flatten() {
            if let Some(cell) = self.cells.get(&view.key) {
                cell.set_view(view);
            }
        }
    }

    fn rebuild(&mut self, sender: &ComponentSender<Self>) {
        self.rebuilds += 1;
        tracing::debug!(
            rebuilds = self.rebuilds,
            rows = self.layout.rows.len(),
            cols = self.layout.cols.len(),
            "matrix rebuild"
        );
        let focused = self
            .cells
            .values()
            .find(|c| c.has_focus())
            .map(CellButton::key)
            .or_else(|| self.nav.borrow().current_cell().map(CellButton::key));
        for grid in [&self.body, &self.row_grid, &self.col_grid] {
            while let Some(child) = grid.first_child() {
                grid.remove(&child);
            }
        }
        self.cells.clear();
        self.rows.clear();
        self.cols.clear();
        self.size_groups.clear();
        let row_groups: Vec<gtk::SizeGroup> = (0..self.layout.rows.len())
            .map(|_| gtk::SizeGroup::new(gtk::SizeGroupMode::Vertical))
            .collect();
        let col_groups: Vec<gtk::SizeGroup> = (0..self.layout.cols.len())
            .map(|_| gtk::SizeGroup::new(gtk::SizeGroupMode::Horizontal))
            .collect();

        // Rows, with a label row before each group (and a matching spacer row in the body).
        let n_cols = grid_index(self.layout.cols.len().max(1));
        let mut grid_rows = Vec::with_capacity(self.layout.rows.len());
        let mut next = 0;
        let mut last_group = None;
        let mut groups = 0;
        for spec in &self.layout.rows {
            let group = row_group(&spec.key);
            if last_group != Some(group) {
                last_group = Some(group);
                let label = group_label(&group_title(group));
                self.row_grid.attach(&label, 0, next, 1, 1);
                let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
                spacer.set_size_request(-1, GROUP_HEIGHT);
                self.body.attach(&spacer, 0, next, n_cols, 1);
                let pair = gtk::SizeGroup::new(gtk::SizeGroupMode::Vertical);
                pair.add_widget(&label);
                pair.add_widget(&spacer);
                self.size_groups.push(pair);
                groups += 1;
                next += 1;
            }
            let on_volume = row_volume(&spec.key, sender);
            let on_action = {
                let (s, key) = (sender.input_sender().clone(), spec.key.clone());
                Rc::new(move |a| s.emit(MatrixMsg::Row(key.clone(), a))) as header::ActionFn
            };
            let on_denoise = row_denoise(&spec.key, sender);
            let h = RowHeader::new(spec, &self.hub, on_volume, &on_action, &on_denoise);
            row_groups[self.rows.len()].add_widget(h.widget());
            self.row_grid.attach(h.widget(), 0, next, 1, 1);
            self.rows.push(h);
            grid_rows.push(next);
            next += 1;
        }
        // Columns: group labels across the top, then the headers.
        let mut c = 0;
        while c < self.layout.cols.len() {
            let group = col_group(&self.layout.cols[c].key);
            let span = self.layout.cols[c..]
                .iter()
                .take_while(|s| col_group(&s.key) == group)
                .count();
            let label = group_label(&group_title(group));
            self.col_grid
                .attach(&label, grid_index(c), 0, grid_index(span), 1);
            c += span;
        }
        for (c, spec) in self.layout.cols.iter().enumerate() {
            let on_volume = col_volume(&spec.key, sender);
            let on_action = {
                let (s, key) = (sender.input_sender().clone(), spec.key.clone());
                Rc::new(move |a| s.emit(MatrixMsg::Col(key.clone(), a))) as header::ActionFn
            };
            let h = ColHeader::new(spec, &self.hub, on_volume, &on_action);
            col_groups[c].add_widget(h.widget());
            self.col_grid.attach(h.widget(), grid_index(c), 1, 1, 1);
            self.cols.push(h);
        }
        let mut nav_cells = Vec::with_capacity(self.layout.rows.len());
        for (r, row) in self.layout.cells.iter().enumerate() {
            let mut nav_row = Vec::with_capacity(row.len());
            for (c, view) in row.iter().enumerate() {
                // Translators: accessible name of a send, e.g. "Music to Headphones"
                let name = i18n_f(
                    "{} to {}",
                    &[&self.layout.rows[r].label, &self.layout.cols[c].label],
                );
                let cell = CellButton::new(view.key.clone(), name);
                cell.set_view(view);
                cell.connect_edit(self.edit_sink(sender));
                let focus = gtk::EventControllerFocus::new();
                focus.connect_enter({
                    let (nav, key) = (self.nav.clone(), view.key.clone());
                    move |_| nav.borrow_mut().focused(&key)
                });
                cell.add_controller(focus);
                row_groups[r].add_widget(&cell);
                col_groups[c].add_widget(&cell);
                self.body.attach(&cell, grid_index(c), grid_rows[r], 1, 1);
                self.cells.insert(view.key.clone(), cell.clone());
                nav_row.push(cell);
            }
            nav_cells.push(nav_row);
        }
        self.size_groups.extend(row_groups);
        self.size_groups.extend(col_groups);
        let had_focus = focused.is_some() && self.body.root().and_then(|w| w.focus()).is_none();
        self.nav.borrow_mut().reset(nav_cells, focused.as_ref());
        if had_focus && let Some(cell) = self.nav.borrow().current_cell() {
            cell.grab_focus();
        }
        // Keep header and body sizes in lockstep even with an empty dimension.
        let rows = grid_index(self.layout.rows.len());
        let height =
            rows * CELL_HEIGHT + groups * GROUP_HEIGHT + (rows + groups - 1).max(0) * SPACING;
        self.body
            .set_size_request(span(self.layout.cols.len(), CELL_WIDTH), height);
    }

    fn on_row_action(&self, key: &RowKey, action: HeaderAction, sender: &ComponentSender<Self>) {
        let Some(spec) = self.layout.rows.iter().find(|r| &r.key == key) else {
            return;
        };
        let send = self.edit_sink(sender);
        match (key, action) {
            (RowKey::Channel(id), HeaderAction::Rename) => {
                let kind = name::NameDialogKind::RenameChannel(*id, spec.label.clone());
                name::present(&self.stack, kind, &[], send);
            }
            (RowKey::Channel(id), HeaderAction::Delete) => {
                let target = RouteTarget::Channel { id: *id };
                let apps = self
                    .setup
                    .app_rules
                    .iter()
                    .filter(|r| r.target == target)
                    .count();
                let id = *id;
                confirm::delete(
                    &self.stack,
                    &spec.label,
                    u32::try_from(apps).unwrap_or(u32::MAX),
                    move || {
                        send(Edit::DeleteChannel { id });
                    },
                );
            }
            (RowKey::Input(k), HeaderAction::Denoise(enabled)) => send(Edit::SetDenoise {
                input: k.clone(),
                enabled,
            }),
            (RowKey::Input(k), HeaderAction::Forget) if !spec.present => {
                let key = k.clone();
                confirm::forget(&self.stack, &spec.label, move || {
                    send(Edit::ForgetDevice { key: key.clone() });
                });
            }
            _ => {}
        }
    }

    fn on_col_action(&self, key: &ColKey, action: HeaderAction, sender: &ComponentSender<Self>) {
        let Some(spec) = self.layout.cols.iter().find(|c| &c.key == key) else {
            return;
        };
        let send = self.edit_sink(sender);
        match (key, action) {
            (ColKey::VirtualMic(id), HeaderAction::Rename) => {
                let kind = name::NameDialogKind::RenameMic(*id, spec.label.clone());
                name::present(&self.stack, kind, &[], send);
            }
            (ColKey::VirtualMic(id), HeaderAction::Delete) => {
                let id = *id;
                confirm::delete(&self.stack, &spec.label, 0, move || {
                    send(Edit::DeleteMic { id });
                });
            }
            (ColKey::Output(k), HeaderAction::Forget) if !spec.present => {
                let key = k.clone();
                confirm::forget(&self.stack, &spec.label, move || {
                    send(Edit::ForgetDevice { key: key.clone() });
                });
            }
            _ => {}
        }
    }
}

fn grid_index(i: usize) -> i32 {
    i32::try_from(i).unwrap_or(i32::MAX)
}

fn span(n: usize, size: i32) -> i32 {
    let n = grid_index(n);
    if n == 0 {
        0
    } else {
        n * size + (n - 1) * SPACING
    }
}

fn row_volume(key: &RowKey, sender: &ComponentSender<MatrixPage>) -> header::VolumeFn {
    let s = sender.input_sender().clone();
    let key = key.clone();
    Rc::new(move |level, muted, commit| {
        let edit = match &key {
            RowKey::Channel(id) => Edit::ChannelVolume {
                id: *id,
                level,
                muted,
                commit,
            },
            // Device volume isn't saved; the commit repeat is harmless.
            RowKey::Input(k) => Edit::DeviceVolume {
                key: k.clone(),
                level,
                muted,
            },
            RowKey::Denoised(_) => return,
        };
        s.emit(MatrixMsg::Edit(edit));
    })
}

fn col_volume(key: &ColKey, sender: &ComponentSender<MatrixPage>) -> header::VolumeFn {
    let s = sender.input_sender().clone();
    let key = key.clone();
    Rc::new(move |level, muted, commit| {
        let edit = match &key {
            ColKey::VirtualMic(id) => Edit::MicVolume {
                id: *id,
                level,
                muted,
                commit,
            },
            ColKey::Output(k) => Edit::DeviceVolume {
                key: k.clone(),
                level,
                muted,
            },
        };
        s.emit(MatrixMsg::Edit(edit));
    })
}

fn row_denoise(key: &RowKey, sender: &ComponentSender<MatrixPage>) -> header::DenoiseFn {
    let s = sender.input_sender().clone();
    let key = key.clone();
    Rc::new(move |ev| {
        let RowKey::Denoised(input) = &key else {
            return;
        };
        let edit = match ev {
            DenoiseEvent::Enabled(enabled) => Edit::SetDenoise {
                input: input.clone(),
                enabled,
            },
            DenoiseEvent::Gate { gate, commit } => Edit::SetVadGate {
                input: input.clone(),
                gate,
                commit,
            },
        };
        s.emit(MatrixMsg::Edit(edit));
    })
}

/// Groups of rows and columns, in layout order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Group {
    Channels,
    Microphones,
    Outputs,
    VirtualMics,
}

fn row_group(key: &RowKey) -> Group {
    match key {
        RowKey::Channel(_) => Group::Channels,
        RowKey::Input(_) | RowKey::Denoised(_) => Group::Microphones,
    }
}

fn col_group(key: &ColKey) -> Group {
    match key {
        ColKey::Output(_) => Group::Outputs,
        ColKey::VirtualMic(_) => Group::VirtualMics,
    }
}

fn group_title(g: Group) -> String {
    match g {
        // Translators: matrix group titles, shown in capitals
        Group::Channels => i18n("CHANNELS"),
        Group::Microphones => i18n("MICROPHONES"),
        Group::Outputs => i18n("OUTPUTS"),
        Group::VirtualMics => i18n("VIRTUAL MICS"),
    }
}

fn group_label(text: &str) -> gtk::Label {
    let l = gtk::Label::new(Some(text));
    l.add_css_class("matrix-group");
    l.set_xalign(0.0);
    l.set_valign(gtk::Align::End);
    l.set_ellipsize(gtk::pango::EllipsizeMode::End);
    l.set_size_request(-1, GROUP_HEIGHT);
    l
}

/// The "How it works" button: what channels and virtual mics are, with an example.
fn how_it_works() -> gtk::MenuButton {
    let button = gtk::MenuButton::new();
    let label = adw::ButtonContent::new();
    label.set_icon_name("help-about-symbolic");
    label.set_label(&i18n("How it works"));
    button.set_child(Some(&label));
    button.add_css_class("flat");
    button.set_valign(gtk::Align::Center);
    let content = gtk::Box::new(gtk::Orientation::Vertical, 10);
    content.set_margin_top(14);
    content.set_margin_bottom(14);
    content.set_margin_start(14);
    content.set_margin_end(14);
    let title = gtk::Label::new(Some(&i18n("How it works")));
    title.add_css_class("title-4");
    title.set_xalign(0.0);
    content.append(&title);
    for text in [
        i18n(
            "Channels group apps. On the Apps page, choose a channel for each app: Spotify into Music, a game into Games, Discord into Chat.",
        ),
        i18n(
            "Each row sends its sound to the columns you switch on: your speakers or headphones, and virtual microphones. One slot, one level.",
        ),
        i18n(
            "A virtual microphone is a microphone for other apps. Choose \u{201c}Virtual mic\u{201d} as the microphone in Discord or OBS, and they hear exactly what you send to it.",
        ),
        i18n(
            "Example: Music and Games go to your headphones; Games and your noise-free microphone also go to the virtual mic. Your friends hear you and the game, not your music.",
        ),
        i18n(
            "Click a slot to switch a send on or off. Drag up or down on it, or scroll, to change its level. Double-click resets it to 0 dB.",
        ),
    ] {
        let l = gtk::Label::new(Some(&text));
        l.set_wrap(true);
        l.set_max_width_chars(48);
        l.set_xalign(0.0);
        content.append(&l);
    }
    let popover = gtk::Popover::new();
    popover.set_child(Some(&content));
    button.set_popover(Some(&popover));
    button
}
