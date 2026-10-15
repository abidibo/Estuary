//! SPA pods for filter params (dsp M10.6). Built off the RT thread.

use std::io::Cursor;

use libspa::param::ParamType;
use libspa::pod::serialize::PodSerializer;
use libspa::pod::{Object, Property, PropertyFlags, Value};
use libspa::utils::SpaTypes;

/// `ProcessLatency { ns }`: how much latency the node adds, for PipeWire's latency reporting.
pub(crate) fn process_latency_param(ns: i64) -> Vec<u8> {
    let obj = Object {
        type_: SpaTypes::ObjectParamProcessLatency.as_raw(),
        id: ParamType::ProcessLatency.as_raw(),
        properties: vec![Property {
            key: libspa_sys::SPA_PARAM_PROCESS_LATENCY_ns,
            flags: PropertyFlags::empty(),
            value: Value::Long(ns),
        }],
    };
    PodSerializer::serialize(Cursor::new(Vec::new()), &Value::Object(obj))
        .map(|(c, _)| c.into_inner())
        .unwrap_or_default()
}
