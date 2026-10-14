//! Peak-meter capture streams (engine M4.3, master C5/C9).
//!
//! `stream.monitor=true` makes audioconvert deliver peak values instead of audio. The `process`
//! callback runs on the PipeWire RT thread: no allocation, no locks, no logging, no panics. It
//! only reads the buffer and writes two atomics into the shared `MeterBank`.

use std::io::Cursor;
use std::sync::Arc;

use pipewire::core::CoreRc;
use pipewire::properties::properties;
use pipewire::spa::param::ParamType;
use pipewire::spa::param::audio::{AudioFormat, AudioInfoRaw};
use pipewire::spa::pod::serialize::PodSerializer;
use pipewire::spa::pod::{Object, Pod, Value};
use pipewire::spa::utils::{Direction, SpaTypes};
use pipewire::stream::{Stream, StreamFlags, StreamListener, StreamRc, StreamState};

use crate::meter::{MeterBank, MeterSlotId};

/// Peak values per second requested from audioconvert (C5).
const PEAK_RATE: u32 = 50;

pub(crate) struct MeterStream {
    // Field order is drop order: the listener goes before the stream.
    _listener: StreamListener<MeterUd>,
    stream: StreamRc,
}

impl std::fmt::Debug for MeterStream {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MeterStream").finish_non_exhaustive()
    }
}

impl Drop for MeterStream {
    fn drop(&mut self) {
        let _ = self.stream.disconnect();
    }
}

struct MeterUd {
    bank: Arc<MeterBank>,
    slot: MeterSlotId,
    /// Set once the stream's volume was reset to unity (see `param_changed`).
    volume_reset: bool,
}

pub(crate) struct MeterSpec<'a> {
    pub slot: MeterSlotId,
    pub target_serial: u64,
    pub capture_sink: bool,
    pub autoconnect: bool,
    pub instance: &'a str,
}

/// Creates and connects a meter stream. `on_error` runs on the main loop if the stream fails.
pub(crate) fn create_meter(
    core: &CoreRc,
    bank: Arc<MeterBank>,
    spec: &MeterSpec<'_>,
    on_error: impl Fn(MeterSlotId, String) + 'static,
) -> Result<MeterStream, pipewire::Error> {
    let slot = spec.slot;
    let name = format!("estuary.meter.{}", slot.0);
    let mut props = properties! {
        "node.name" => name.as_str(),
        "media.type" => "Audio",
        "media.category" => "Capture",
        "media.role" => "DSP",
        "stream.monitor" => "true",
        "resample.peaks" => "true",
        "node.rate" => "1/25",
        "node.latency" => "1/25",
        "node.passive" => "true",
        // M4.1: keeps GNOME Shell's recording indicator off; the stream only carries peaks.
        "application.id" => "org.PulseAudio.pavucontrol",
        "estuary.role" => "meter",
        "estuary.instance" => spec.instance,
    };
    if spec.autoconnect {
        props.insert("target.object", spec.target_serial.to_string());
        if spec.capture_sink {
            props.insert("stream.capture.sink", "true");
        }
    } else {
        props.insert("node.autoconnect", "false");
    }

    let stream = StreamRc::new(core.clone(), &name, props)?;
    let listener = stream
        .add_local_listener_with_user_data(MeterUd {
            bank,
            slot,
            volume_reset: false,
        })
        .process(process)
        .param_changed(|stream, ud, id, param| {
            // WirePlumber may restore a stored volume onto the stream, which would scale the
            // peaks. Reset it to unity once (pwvucontrol does the same).
            if id == ParamType::Props.as_raw() && !ud.volume_reset && param.is_some() {
                ud.volume_reset = true;
                let _ = stream.set_control(libspa::sys::SPA_PROP_channelVolumes, &[1.0, 1.0]);
            }
        })
        .state_changed(move |_, ud, _old, new| {
            if let StreamState::Error(message) = new {
                on_error(ud.slot, message);
            }
        })
        .register()?;

    let format = format_pod();
    let Some(pod) = Pod::from_bytes(&format) else {
        return Err(pipewire::Error::CreationFailed);
    };
    let mut flags = StreamFlags::MAP_BUFFERS | StreamFlags::RT_PROCESS;
    if spec.autoconnect {
        flags |= StreamFlags::AUTOCONNECT;
    }
    // Never DONT_RECONNECT (C5): it would stop WirePlumber from relinking the meter.
    stream.connect(Direction::Input, None, flags, &mut [pod])?;
    Ok(MeterStream {
        _listener: listener,
        stream,
    })
}

/// F32LE, stereo FL/FR, `PEAK_RATE` frames per second.
fn format_pod() -> Vec<u8> {
    let mut info = AudioInfoRaw::new();
    info.set_format(AudioFormat::F32LE);
    info.set_rate(PEAK_RATE);
    info.set_channels(2);
    let mut position = [0u32; libspa::sys::SPA_AUDIO_MAX_CHANNELS as usize];
    position[0] = libspa::sys::SPA_AUDIO_CHANNEL_FL;
    position[1] = libspa::sys::SPA_AUDIO_CHANNEL_FR;
    info.set_position(position);
    let obj = Object {
        type_: SpaTypes::ObjectParamFormat.as_raw(),
        id: ParamType::EnumFormat.as_raw(),
        properties: info.into(),
    };
    PodSerializer::serialize(Cursor::new(Vec::new()), &Value::Object(obj))
        .map(|(c, _)| c.into_inner())
        .unwrap_or_default()
}

/// RT thread. Reads interleaved F32LE peak frames, keeps the max per channel, publishes it.
fn process(stream: &Stream, ud: &mut MeterUd) {
    let Some(mut buffer) = stream.dequeue_buffer() else {
        return;
    };
    let Some(data) = buffer.datas_mut().first_mut() else {
        return;
    };
    let offset = data.chunk().offset() as usize;
    let size = data.chunk().size() as usize;
    let Some(bytes) = data.data() else { return };
    let end = offset.saturating_add(size).min(bytes.len());
    let Some(frames) = bytes.get(offset.min(end)..end) else {
        return;
    };
    ud.bank.write(ud.slot, peaks(frames));
}

/// Max |sample| per channel of interleaved stereo F32LE (even = FL, odd = FR).
fn peaks(frames: &[u8]) -> [f32; 2] {
    let mut peak = [0.0f32; 2];
    for (i, chunk) in frames.chunks_exact(4).enumerate() {
        let v = f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]).abs();
        let ch = &mut peak[i % 2];
        if v > *ch {
            *ch = v;
        }
    }
    peak
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bytes(samples: &[f32]) -> Vec<u8> {
        samples.iter().flat_map(|s| s.to_le_bytes()).collect()
    }

    #[test]
    fn peaks_per_channel() {
        assert_eq!(peaks(&bytes(&[0.1, -0.5, -0.8, 0.2, 0.3, 0.4])), [0.8, 0.5]);
        assert_eq!(peaks(&[]), [0.0, 0.0]);
        // A trailing partial sample is ignored.
        let mut b = bytes(&[0.25, 0.5]);
        b.push(0xff);
        assert_eq!(peaks(&b), [0.25, 0.5]);
    }

    #[test]
    fn format_pod_is_valid() {
        let pod = format_pod();
        assert!(!pod.is_empty());
        assert!(Pod::from_bytes(&pod).is_some());
    }
}
