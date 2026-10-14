//! Live level meter (UI M4.2, master C9).
//!
//! A `gtk::DrawingArea` driven by the frame clock. While bound and mapped, a tick callback
//! reads the engine's lock-free `MeterBank`, runs the ballistics and redraws only when the
//! picture changes. Meter values never travel through Relm4 messages.

pub mod ballistics;

use std::cell::RefCell;
use std::rc::{Rc, Weak};
use std::sync::Arc;

use estuary_engine::{MeterBank, MeterSlotId};
use relm4::gtk;
use relm4::gtk::prelude::*;

use self::ballistics::{ChannelBallistics, FLOOR_DB, STALE_S, Zone, fraction, lin_to_db, step};
use crate::i18n::i18n;

const GREEN: (f64, f64, f64) = (
    0x33 as f64 / 255.0,
    0xd1 as f64 / 255.0,
    0x7a as f64 / 255.0,
);
const YELLOW: (f64, f64, f64) = (
    0xf6 as f64 / 255.0,
    0xd3 as f64 / 255.0,
    0x2d as f64 / 255.0,
);
const RED: (f64, f64, f64) = (
    0xe0 as f64 / 255.0,
    0x1b as f64 / 255.0,
    0x24 as f64 / 255.0,
);

#[derive(Debug, Default)]
struct MeterState {
    binding: Option<(Arc<MeterBank>, Vec<MeterSlotId>)>,
    ch: [ChannelBallistics; 2],
    raw_peak: [f32; 2],
    last_seq: Vec<u32>,
    last_change_us: i64,
    last_frame_us: i64,
    failed: bool,
    tick: Option<gtk::TickCallbackId>,
}

#[derive(Debug)]
struct Inner {
    area: gtk::DrawingArea,
    state: RefCell<MeterState>,
}

/// Cheap to clone; the hub keeps only weak references.
#[derive(Debug, Clone)]
pub struct PeakMeter(Rc<Inner>);

#[derive(Debug, Clone)]
pub struct WeakPeakMeter(Weak<Inner>);

impl WeakPeakMeter {
    pub fn upgrade(&self) -> Option<PeakMeter> {
        self.0.upgrade().map(PeakMeter)
    }
}

impl PeakMeter {
    /// A horizontal meter, 120×8 natural size. `label` names it for screen readers.
    pub fn new(label: &str) -> Self {
        let area: gtk::DrawingArea = gtk::glib::Object::builder()
            .property("accessible-role", gtk::AccessibleRole::Meter)
            .build();
        area.set_content_width(120);
        area.set_content_height(8);
        area.set_size_request(48, 6);
        area.set_valign(gtk::Align::Center);
        area.add_css_class("estuary-meter");
        area.update_property(&[gtk::accessible::Property::Label(label)]);
        let inner = Rc::new(Inner {
            area,
            state: RefCell::new(MeterState::default()),
        });

        let weak = Rc::downgrade(&inner);
        inner.area.set_draw_func(move |area, cr, w, h| {
            if let Some(inner) = weak.upgrade() {
                draw(area, &inner.state.borrow(), cr, w, h);
            }
        });
        // A hidden meter costs nothing: the tick only runs while mapped and bound.
        let weak = Rc::downgrade(&inner);
        inner.area.connect_map(move |_| {
            if let Some(inner) = weak.upgrade() {
                PeakMeter(inner).ensure_tick();
            }
        });
        let weak = Rc::downgrade(&inner);
        inner.area.connect_unmap(move |_| {
            if let Some(inner) = weak.upgrade() {
                PeakMeter(inner).stop_tick();
            }
        });
        Self(inner)
    }

    pub fn widget(&self) -> &gtk::DrawingArea {
        &self.0.area
    }

    pub fn downgrade(&self) -> WeakPeakMeter {
        WeakPeakMeter(Rc::downgrade(&self.0))
    }

    pub fn bind(&self, bank: Arc<MeterBank>, slots: Vec<MeterSlotId>) {
        {
            let mut st = self.0.state.borrow_mut();
            let same = st
                .binding
                .as_ref()
                .is_some_and(|(b, s)| Arc::ptr_eq(b, &bank) && *s == slots);
            if same && !st.failed {
                return;
            }
            st.last_seq = slots.iter().map(|s| bank.read(*s).seq).collect();
            st.binding = Some((bank, slots));
            st.failed = false;
        }
        self.ensure_tick();
    }

    pub fn unbind(&self) {
        self.stop_tick();
        let mut st = self.0.state.borrow_mut();
        if st.binding.take().is_some() {
            st.ch = Default::default();
            st.raw_peak = [0.0; 2];
            drop(st);
            self.0.area.queue_draw();
        }
    }

    /// F13: the engine couldn't meter this target; show "—" instead of bars.
    pub fn set_failed(&self, failed: bool) {
        let mut st = self.0.state.borrow_mut();
        if st.failed != failed {
            st.failed = failed;
            self.0
                .area
                .set_tooltip_text(failed.then(|| i18n("Level unavailable")).as_deref());
            drop(st);
            self.0.area.queue_draw();
        }
    }

    fn ensure_tick(&self) {
        let mut st = self.0.state.borrow_mut();
        if st.tick.is_some() || st.binding.is_none() || !self.0.area.is_mapped() {
            return;
        }
        let weak = Rc::downgrade(&self.0);
        st.tick = Some(
            self.0
                .area
                .add_tick_callback(move |_, clock| match weak.upgrade() {
                    Some(inner) => {
                        PeakMeter(inner).on_tick(clock.frame_time());
                        gtk::glib::ControlFlow::Continue
                    }
                    None => gtk::glib::ControlFlow::Break,
                }),
        );
    }

    fn stop_tick(&self) {
        if let Some(id) = self.0.state.borrow_mut().tick.take() {
            id.remove();
        }
    }

    #[allow(clippy::cast_precision_loss)] // microsecond deltas within a few seconds
    fn on_tick(&self, now_us: i64) {
        let mut st = self.0.state.borrow_mut();
        let Some((bank, slots)) = st.binding.clone() else {
            return;
        };
        let mut peak = [0.0f32; 2];
        let mut changed = false;
        for (i, slot) in slots.iter().enumerate() {
            let r = bank.read(*slot);
            if st.last_seq.get(i) != Some(&r.seq) {
                changed = true;
                if let Some(s) = st.last_seq.get_mut(i) {
                    *s = r.seq;
                }
            }
            peak[0] = peak[0].max(r.peak[0]);
            peak[1] = peak[1].max(r.peak[1]);
        }
        if changed {
            st.last_change_us = now_us;
        }
        let dt = if st.last_frame_us == 0 {
            0.0
        } else {
            (now_us - st.last_frame_us) as f32 / 1e6
        };
        st.last_frame_us = now_us;
        let stale = (now_us - st.last_change_us) as f32 / 1e6 > STALE_S;

        let before = st.ch;
        for (c, p) in st.ch.iter_mut().zip(peak) {
            let input = if stale { FLOOR_DB } else { lin_to_db(p) };
            step(c, input, dt);
        }
        st.raw_peak = if stale { [0.0; 2] } else { peak };
        let moved = before.iter().zip(&st.ch).any(|(a, b)| {
            (a.level_db - b.level_db).abs() > 0.1 || (a.hold_db - b.hold_db).abs() > 0.1
        });
        drop(st);
        if moved {
            self.0.area.queue_draw();
        }
    }
}

fn draw(area: &gtk::DrawingArea, st: &MeterState, cr: &gtk::cairo::Context, w: i32, h: i32) {
    let (w, h) = (f64::from(w), f64::from(h));
    let fg = area.color();
    if st.failed {
        // A dash in the middle: "level unavailable".
        cr.set_source_rgba(fg.red().into(), fg.green().into(), fg.blue().into(), 0.5);
        cr.rectangle(w * 0.4, (h / 2.0 - 1.0).max(0.0), w * 0.2, 2.0);
        let _ = cr.fill();
        return;
    }
    let bar_h = ((h - 1.0) / 2.0).max(1.0);
    for (i, c) in st.ch.iter().enumerate() {
        let y = if i == 0 { 0.0 } else { bar_h + 1.0 };
        // Track
        cr.set_source_rgba(fg.red().into(), fg.green().into(), fg.blue().into(), 0.12);
        cr.rectangle(0.0, y, w, bar_h);
        let _ = cr.fill();
        // Level, split into zone segments
        let level = f64::from(fraction(c.level_db)) * w;
        let warn_x = f64::from(fraction(ballistics::WARN_DB)) * w;
        let clip_x = f64::from(fraction(ballistics::CLIP_DB)) * w;
        let raw = st.raw_peak.get(i).copied().unwrap_or(0.0);
        for (start, end, color) in [
            (0.0, warn_x, GREEN),
            (warn_x, clip_x, YELLOW),
            (clip_x, w, RED),
        ] {
            let seg_end = level.min(end);
            if seg_end > start {
                cr.set_source_rgb(color.0, color.1, color.2);
                cr.rectangle(start, y, seg_end - start, bar_h);
                let _ = cr.fill();
            }
        }
        // Peak hold tick
        if c.hold_db > FLOOR_DB {
            let x = (f64::from(fraction(c.hold_db)) * w - 2.0).max(0.0);
            let color = match ballistics::zone(c.hold_db, raw) {
                Zone::Normal => GREEN,
                Zone::Warn => YELLOW,
                Zone::Clip => RED,
            };
            cr.set_source_rgb(color.0, color.1, color.2);
            cr.rectangle(x, y, 2.0, bar_h);
            let _ = cr.fill();
        }
    }
}
