//! Meter binding and visibility (UI M4.3/M4.4).
//!
//! Pages register each `PeakMeter` with its `MeterTarget`. The hub binds meters to the slots
//! the engine assigned (`Event::MeterAssignments`) and reports which targets are on screen, so
//! only those are metered (`Command::SetMeterTargets`, priority = top-to-bottom order).

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::Arc;

use estuary_engine::{MeterBank, MeterSlotId, MeterTarget};
use relm4::gtk;
use relm4::gtk::prelude::*;

use super::peak_meter::{PeakMeter, WeakPeakMeter};

#[derive(Default)]
struct HubInner {
    bank: Option<Arc<MeterBank>>,
    assignments: HashMap<MeterTarget, Vec<MeterSlotId>>,
    failed: HashSet<MeterTarget>,
    meters: Vec<(WeakPeakMeter, MeterTarget)>,
}

#[derive(Clone, Default)]
pub struct MeterHub(Rc<RefCell<HubInner>>);

impl std::fmt::Debug for MeterHub {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MeterHub")
            .field("meters", &self.0.borrow().meters.len())
            .finish()
    }
}

impl MeterHub {
    /// A new engine (start or restart) brings a new bank; old assignments are void.
    pub fn set_bank(&self, bank: Arc<MeterBank>) {
        let mut h = self.0.borrow_mut();
        h.bank = Some(bank);
        h.assignments.clear();
        h.failed.clear();
        drop(h);
        self.rebind_all();
    }

    pub fn register(&self, meter: &PeakMeter, target: MeterTarget) {
        let mut h = self.0.borrow_mut();
        h.meters.retain(|(m, _)| m.upgrade().is_some());
        Self::apply(&h, meter, &target);
        h.meters.push((meter.downgrade(), target));
    }

    pub fn set_assignments(&self, a: Vec<(MeterTarget, Vec<MeterSlotId>)>) {
        let mut h = self.0.borrow_mut();
        h.assignments = a.into_iter().collect();
        // A target that got slots again is no longer failed.
        let assigned: HashSet<MeterTarget> = h.assignments.keys().cloned().collect();
        h.failed.retain(|t| !assigned.contains(t));
        drop(h);
        self.rebind_all();
    }

    pub fn set_failed(&self, target: MeterTarget) {
        self.0.borrow_mut().failed.insert(target);
        self.rebind_all();
    }

    fn rebind_all(&self) {
        let mut h = self.0.borrow_mut();
        h.meters.retain(|(m, _)| m.upgrade().is_some());
        for (weak, target) in &h.meters {
            if let Some(meter) = weak.upgrade() {
                Self::apply(&h, &meter, target);
            }
        }
    }

    fn apply(h: &HubInner, meter: &PeakMeter, target: &MeterTarget) {
        meter.set_failed(h.failed.contains(target));
        match (&h.bank, h.assignments.get(target)) {
            (Some(bank), Some(slots)) => meter.bind(bank.clone(), slots.clone()),
            _ => meter.unbind(),
        }
    }

    /// Targets whose meters are mapped and inside `scroller`'s viewport, top-to-bottom.
    pub fn visible_targets(&self, scroller: &gtk::ScrolledWindow) -> Vec<MeterTarget> {
        let height = scroller.height() as f32;
        let width = scroller.width() as f32;
        let mut found: Vec<(f32, f32, MeterTarget)> = Vec::new();
        for (weak, target) in &self.0.borrow().meters {
            let Some(meter) = weak.upgrade() else {
                continue;
            };
            let area = meter.widget();
            if !area.is_mapped() {
                continue;
            }
            if let Some(b) = area.compute_bounds(scroller) {
                let inside = b.y() + b.height() >= 0.0
                    && b.y() <= height
                    && b.x() + b.width() >= 0.0
                    && b.x() <= width;
                if inside {
                    found.push((b.y(), b.x(), target.clone()));
                }
            }
        }
        found.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.total_cmp(&b.1)));
        let mut out: Vec<MeterTarget> = Vec::new();
        for (_, _, t) in found {
            if !out.contains(&t) {
                out.push(t);
            }
        }
        out
    }
}
