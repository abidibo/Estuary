//! Meter data path (master C9). At M2 only the shared bank and target types exist; meter
//! streams and the slot allocator arrive in M4.

use std::sync::atomic::{AtomicU32, Ordering};

use crate::model::{AppKey, ChannelId, DeviceKey, VirtualMicId};

pub const METER_SLOTS: usize = 64;

/// Lock-free peak slots written by the RT thread and read by the UI on its frame clock.
#[derive(Debug)]
pub struct MeterBank {
    slots: Box<[MeterSlot]>,
}

#[derive(Debug, Default)]
struct MeterSlot {
    peak: [AtomicU32; 2],
    seq: AtomicU32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct MeterSlotId(pub u16);

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MeterReading {
    pub seq: u32,
    pub peak: [f32; 2],
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum MeterTarget {
    App(AppKey),
    Device(DeviceKey),
    Channel(ChannelId),
    VirtualMic(VirtualMicId),
    Denoised(DeviceKey),
}

impl MeterBank {
    pub fn new() -> Self {
        Self {
            slots: (0..METER_SLOTS).map(|_| MeterSlot::default()).collect(),
        }
    }

    /// UI-side read: `seq` with Acquire, then peaks with Relaxed (C9).
    pub fn read(&self, id: MeterSlotId) -> MeterReading {
        let Some(slot) = self.slots.get(usize::from(id.0)) else {
            return MeterReading {
                seq: 0,
                peak: [0.0; 2],
            };
        };
        let seq = slot.seq.load(Ordering::Acquire);
        let peak = [
            f32::from_bits(slot.peak[0].load(Ordering::Relaxed)),
            f32::from_bits(slot.peak[1].load(Ordering::Relaxed)),
        ];
        MeterReading { seq, peak }
    }

    /// RT-side write: peaks with Relaxed, then bump `seq` with Release (C9).
    #[allow(dead_code)] // called by the meter streams' process callback (M4)
    pub(crate) fn write(&self, id: MeterSlotId, peak: [f32; 2]) {
        if let Some(slot) = self.slots.get(usize::from(id.0)) {
            slot.peak[0].store(peak[0].to_bits(), Ordering::Relaxed);
            slot.peak[1].store(peak[1].to_bits(), Ordering::Relaxed);
            slot.seq.fetch_add(1, Ordering::Release);
        }
    }
}

impl Default for MeterBank {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_then_read() {
        let bank = MeterBank::new();
        let id = MeterSlotId(3);
        assert_eq!(bank.read(id).seq, 0);
        bank.write(id, [0.5, 0.25]);
        let r = bank.read(id);
        assert_eq!(r.seq, 1);
        assert_eq!(r.peak, [0.5, 0.25]);
        assert_eq!(bank.read(MeterSlotId(999)).peak, [0.0, 0.0]);
    }
}
