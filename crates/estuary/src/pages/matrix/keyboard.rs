//! Grid keyboard navigation (UI M9.5): a roving tab stop.
//!
//! Only the current cell is focusable, so Tab enters the grid once and leaves it on the next
//! Tab. Arrow keys move between cells (Control: to the edge) and scroll the cell into view.

use std::cell::RefCell;
use std::rc::Rc;

use estuary_engine::CellKey;
use relm4::gtk;
use relm4::gtk::glib;
use relm4::gtk::prelude::*;

use super::cell::CellButton;

#[derive(Debug, Default)]
pub struct Nav {
    cells: Vec<Vec<CellButton>>,
    current: (usize, usize),
}

impl Nav {
    /// New grid contents; the tab stop goes to `focus` if it still exists, else the first cell.
    pub fn reset(&mut self, cells: Vec<Vec<CellButton>>, focus: Option<&CellKey>) {
        self.cells = cells;
        let found = focus.and_then(|k| {
            self.cells
                .iter()
                .enumerate()
                .find_map(|(r, row)| row.iter().position(|c| &c.key() == k).map(|c| (r, c)))
        });
        self.current = found.unwrap_or((0, 0));
        for (r, row) in self.cells.iter().enumerate() {
            for (c, cell) in row.iter().enumerate() {
                cell.set_focusable((r, c) == self.current);
            }
        }
    }

    pub fn current_cell(&self) -> Option<&CellButton> {
        self.cells.get(self.current.0)?.get(self.current.1)
    }

    /// A cell got focus some other way (click): it becomes the tab stop.
    pub fn focused(&mut self, key: &CellKey) {
        let pos = self
            .cells
            .iter()
            .enumerate()
            .find_map(|(r, row)| row.iter().position(|c| &c.key() == key).map(|c| (r, c)));
        if let Some(pos) = pos {
            self.move_to(pos);
        }
    }

    fn move_to(&mut self, pos: (usize, usize)) -> Option<CellButton> {
        let next = self.cells.get(pos.0)?.get(pos.1)?.clone();
        if let Some(old) = self.current_cell() {
            old.set_focusable(false);
        }
        next.set_focusable(true);
        self.current = pos;
        Some(next)
    }

    fn step(&self, key: gtk::gdk::Key, to_edge: bool) -> Option<(usize, usize)> {
        use gtk::gdk::Key;
        let rows = self.cells.len();
        let cols = self.cells.first().map_or(0, Vec::len);
        if rows == 0 || cols == 0 {
            return None;
        }
        let (r, c) = self.current;
        Some(match (key, to_edge) {
            (Key::Left, false) => (r, c.saturating_sub(1)),
            (Key::Right, false) => (r, (c + 1).min(cols - 1)),
            (Key::Up, false) => (r.saturating_sub(1), c),
            (Key::Down, false) => ((r + 1).min(rows - 1), c),
            (Key::Left, true) => (r, 0),
            (Key::Right, true) => (r, cols - 1),
            (Key::Up, true) => (0, c),
            (Key::Down, true) => (rows - 1, c),
            _ => return None,
        })
    }
}

/// Arrow keys on `body` (capture phase, so cells never see them).
pub fn install(body: &gtk::Grid, scroller: &gtk::ScrolledWindow, nav: &Rc<RefCell<Nav>>) {
    let keys = gtk::EventControllerKey::new();
    keys.set_propagation_phase(gtk::PropagationPhase::Capture);
    keys.connect_key_pressed({
        let (nav, scroller, body) = (nav.clone(), scroller.clone(), body.clone());
        move |_, key, _, state| {
            use gtk::gdk::ModifierType as M;
            let other = state.intersects(M::SHIFT_MASK | M::ALT_MASK | M::SUPER_MASK);
            if other {
                return glib::Propagation::Proceed;
            }
            let to_edge = state.contains(M::CONTROL_MASK);
            let target = nav.borrow().step(key, to_edge);
            let Some(pos) = target else {
                return glib::Propagation::Proceed;
            };
            let cell = nav.borrow_mut().move_to(pos);
            if let Some(cell) = cell {
                cell.grab_focus();
                scroll_into_view(&scroller, &body, &cell);
            }
            glib::Propagation::Stop
        }
    });
    body.add_controller(keys);
}

fn scroll_into_view(scroller: &gtk::ScrolledWindow, body: &gtk::Grid, cell: &CellButton) {
    let Some(b) = cell.compute_bounds(body) else {
        return;
    };
    let reveal = |adj: gtk::Adjustment, start: f32, len: f32| {
        let (start, end) = (f64::from(start), f64::from(start + len));
        let (value, page) = (adj.value(), adj.page_size());
        if start < value {
            adj.set_value(start);
        } else if end > value + page {
            adj.set_value(end - page);
        }
    };
    reveal(scroller.hadjustment(), b.x(), b.width());
    reveal(scroller.vadjustment(), b.y(), b.height());
}
