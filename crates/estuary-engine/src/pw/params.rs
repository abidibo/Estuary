//! SPA Props pod parsing (engine M2.6). Building Props pods for volume writes arrives in M5.7.

use libspa::pod::deserialize::PodDeserializer;
use libspa::pod::{Value, ValueArray};

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

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use libspa::pod::serialize::PodSerializer;
    use libspa::pod::{Object, Property, PropertyFlags};

    use super::*;

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
