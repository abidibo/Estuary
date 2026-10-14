//! Quit support (engine M8.11, master C10): who is using our virtual devices.

use crate::appkey;
use crate::error::{UsageKind, VirtualDeviceUsage};
use crate::graph::{GraphMirror, NodeKind, OwnedRole};
use crate::setup::Setup;

/// Apps playing into an owned channel, and recorders linked from an owned virtual mic.
pub(crate) fn usages(m: &GraphMirror, setup: &Setup) -> Vec<VirtualDeviceUsage> {
    let mut out: Vec<VirtualDeviceUsage> = Vec::new();
    for l in m.links.values() {
        let (Some(from), Some(to)) = (m.nodes.get(&l.out_node), m.nodes.get(&l.in_node)) else {
            continue;
        };
        let usage = match (&from.kind, &to.kind) {
            (NodeKind::AppPlayback, NodeKind::Owned(OwnedRole::Channel(id))) => setup
                .channels
                .iter()
                .find(|c| c.id == *id)
                .map(|c| VirtualDeviceUsage {
                    app_display_name: appkey::display_name(&from.props),
                    device_name: c.name.clone(),
                    kind: UsageKind::PlayingToChannel,
                }),
            (NodeKind::Owned(OwnedRole::Vmic(id)), NodeKind::AppCapture) => setup
                .virtual_mics
                .iter()
                .find(|v| v.id == *id)
                .map(|v| VirtualDeviceUsage {
                    app_display_name: appkey::display_name(&to.props),
                    device_name: v.name.clone(),
                    kind: UsageKind::RecordingFromVirtualMic,
                }),
            _ => None,
        };
        if let Some(u) = usage
            && !out
                .iter()
                .any(|x| x.app_display_name == u.app_display_name && x.device_name == u.device_name)
        {
            out.push(u);
        }
    }
    out
}
