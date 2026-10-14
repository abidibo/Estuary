//! Keyed reconciliation of a `FactoryVecDeque` against a new ordered list (UI M3.6).
//!
//! Rows are removed, inserted, moved and updated in place, never cleared and rebuilt, so
//! keyboard focus and open popovers survive snapshots.

use relm4::factory::{DynamicIndex, FactoryComponent, FactoryVecDeque};

/// A factory row identified by a stable key and refreshed from new data.
pub trait KeyedRow:
    FactoryComponent<Index = DynamicIndex, Init = <Self as KeyedRow>::Data>
{
    type Key: PartialEq;
    type Data;
    fn data_key(data: &Self::Data) -> Self::Key;
    fn key(&self) -> Self::Key;
    /// Replace the row's data; the guard re-runs `update_view` for changed rows.
    fn set_data(&mut self, data: Self::Data);
}

pub fn reconcile<C: KeyedRow>(factory: &mut FactoryVecDeque<C>, items: Vec<C::Data>) {
    let keys: Vec<C::Key> = items.iter().map(C::data_key).collect();
    let mut g = factory.guard();

    let mut i = 0;
    while i < g.len() {
        let gone = g.get(i).is_some_and(|row| !keys.contains(&row.key()));
        if gone {
            g.remove(i);
        } else {
            i += 1;
        }
    }

    for (target, data) in items.into_iter().enumerate() {
        let key = C::data_key(&data);
        let found = (target..g.len()).find(|&p| g.get(p).is_some_and(|row| row.key() == key));
        match found {
            Some(p) => {
                if p != target {
                    g.move_to(p, target);
                }
                if let Some(row) = g.get_mut(target) {
                    row.set_data(data);
                }
            }
            None => {
                g.insert(target, data);
            }
        }
    }
}
