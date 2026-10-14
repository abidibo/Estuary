//! SPA pods for volumes (engine M2.6, M5.7): parse node Props, build Props and Route writes.

use std::io::Cursor;

use libspa::param::ParamType;
use libspa::pod::deserialize::PodDeserializer;
use libspa::pod::serialize::PodSerializer;
use libspa::pod::{Object, Property, PropertyFlags, Value, ValueArray};
use libspa::utils::SpaTypes;

use crate::model::Volume;

/// `channelVolumes` and `mute` from a Props param. `None` fields were absent in this update.
#[derive(Debug, Default, PartialEq)]
pub(crate) struct PropsVolume {
    pub channels: Option<Vec<f32>>,
    pub muted: Option<bool>,
}

pub(crate) fn parse_props_volume(bytes: &[u8]) -> Option<PropsVolume> {
    let (_, value) = PodDeserializer::deserialize_any_from(bytes).ok()?;
    let Value::Object(obj) = value else {
        return None;
    };
    let mut out = PropsVolume::default();
    for prop in obj.properties {
        match (prop.key, prop.value) {
            (libspa::sys::SPA_PROP_channelVolumes, Value::ValueArray(ValueArray::Float(v))) => {
                out.channels = Some(v);
            }
            (libspa::sys::SPA_PROP_mute, Value::Bool(b)) => out.muted = Some(b),
            _ => {}
        }
    }
    Some(out)
}

fn prop(key: u32, value: Value) -> Property {
    Property {
        key,
        flags: PropertyFlags::empty(),
        value,
    }
}

fn volume_props(v: &Volume) -> Object {
    Object {
        type_: SpaTypes::ObjectParamProps.as_raw(),
        id: ParamType::Props.as_raw(),
        properties: vec![
            prop(
                libspa::sys::SPA_PROP_channelVolumes,
                Value::ValueArray(ValueArray::Float(v.channels.clone())),
            ),
            prop(libspa::sys::SPA_PROP_mute, Value::Bool(v.muted)),
        ],
    }
}

fn serialize(obj: Object) -> Vec<u8> {
    PodSerializer::serialize(Cursor::new(Vec::new()), &Value::Object(obj))
        .map(|(c, _)| c.into_inner())
        .unwrap_or_default()
}

/// Node `Props` write: `channelVolumes` + `mute` (streams, card-less nodes, channels).
pub(crate) fn props_volume_pod(v: &Volume) -> Vec<u8> {
    serialize(volume_props(v))
}

/// Card `Route` write (M5.1 outcome B): the hardware volume of the route feeding a device node.
pub(crate) fn route_volume_pod(route_index: i32, route_device: i32, v: &Volume) -> Vec<u8> {
    serialize(Object {
        type_: SpaTypes::ObjectParamRoute.as_raw(),
        id: ParamType::Route.as_raw(),
        properties: vec![
            prop(libspa::sys::SPA_PARAM_ROUTE_index, Value::Int(route_index)),
            prop(
                libspa::sys::SPA_PARAM_ROUTE_device,
                Value::Int(route_device),
            ),
            prop(
                libspa::sys::SPA_PARAM_ROUTE_props,
                Value::Object(volume_props(v)),
            ),
            prop(libspa::sys::SPA_PARAM_ROUTE_save, Value::Bool(true)),
        ],
    })
}

/// `(index, device)` of an active route from a card's `Route` param.
pub(crate) fn parse_route(bytes: &[u8]) -> Option<(i32, i32)> {
    let (_, value) = PodDeserializer::deserialize_any_from(bytes).ok()?;
    let Value::Object(obj) = value else {
        return None;
    };
    let mut index = None;
    let mut device = None;
    for p in obj.properties {
        match (p.key, p.value) {
            (libspa::sys::SPA_PARAM_ROUTE_index, Value::Int(i)) => index = Some(i),
            (libspa::sys::SPA_PARAM_ROUTE_device, Value::Int(d)) => device = Some(d),
            _ => {}
        }
    }
    Some((index?, device?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn volume_pods_round_trip() {
        let v = Volume {
            channels: vec![0.25, 0.5],
            muted: true,
        };
        assert_eq!(
            parse_props_volume(&props_volume_pod(&v)),
            Some(PropsVolume {
                channels: Some(vec![0.25, 0.5]),
                muted: Some(true)
            })
        );
        let route = route_volume_pod(3, 7, &v);
        assert_eq!(parse_route(&route), Some((3, 7)));
    }

    fn props_pod(properties: Vec<Property>) -> Vec<u8> {
        let obj = Value::Object(Object {
            type_: libspa::utils::SpaTypes::ObjectParamProps.as_raw(),
            id: libspa::param::ParamType::Props.as_raw(),
            properties,
        });
        PodSerializer::serialize(Cursor::new(Vec::new()), &obj)
            .expect("serialize")
            .0
            .into_inner()
    }

    #[test]
    fn parses_volume_and_mute() {
        let bytes = props_pod(vec![
            Property {
                key: libspa::sys::SPA_PROP_channelVolumes,
                flags: PropertyFlags::empty(),
                value: Value::ValueArray(ValueArray::Float(vec![0.5, 0.25])),
            },
            Property {
                key: libspa::sys::SPA_PROP_mute,
                flags: PropertyFlags::empty(),
                value: Value::Bool(true),
            },
        ]);
        assert_eq!(
            parse_props_volume(&bytes),
            Some(PropsVolume {
                channels: Some(vec![0.5, 0.25]),
                muted: Some(true)
            })
        );
    }

    #[test]
    fn missing_keys_stay_none() {
        let bytes = props_pod(vec![]);
        assert_eq!(parse_props_volume(&bytes), Some(PropsVolume::default()));
        assert_eq!(parse_props_volume(&[1, 2, 3]), None);
    }
}
