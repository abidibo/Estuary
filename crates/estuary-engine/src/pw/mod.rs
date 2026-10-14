//! The imperative shell (engine M2.6): owns every PipeWire object, turns PipeWire callbacks into
//! `GraphChange`s for the core, and executes the core's `Effects`.
//!
//! Re-entrancy rule: callbacks only enqueue inputs; a single `drain` processes the queue. No
//! `RefCell` borrow is held while the core runs or while effects are executed.

mod meter_stream;
mod params;
pub(crate) mod thread;
mod timers;

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet, VecDeque};
use std::rc::{Rc, Weak};
use std::sync::Arc;

use pipewire::client::{Client, ClientChangeMask, ClientListener};
use pipewire::context::ContextRc;
use pipewire::core::{CoreRc, PW_ID_CORE};
use pipewire::main_loop::MainLoopRc;
use pipewire::metadata::{Metadata, MetadataListener};
use pipewire::node::{Node, NodeChangeMask, NodeListener};
use pipewire::properties::properties;
use pipewire::proxy::ProxyT;
use pipewire::registry::{GlobalObject, RegistryRc};
use pipewire::spa::param::ParamType;
use pipewire::spa::utils::dict::DictRef;
use pipewire::types::ObjectType;

use self::meter_stream::{MeterSpec, MeterStream, create_meter};
use self::thread::{Control, guarded};
use self::timers::Timers;
use crate::core::EngineCore;
use crate::core::effects::{Effects, OwnedKey, PwOp, TimerKind};
use crate::graph::{
    ClientEntry, CoreInfo, GraphChange, LinkEntry, PortDir, PortEntry, Props, parse_u32,
};
use crate::meter::{MeterBank, MeterSlotId};
use crate::{Command, EngineConfig, Event};

/// Linux `EPIPE`: the core reports a lost connection as `error(PW_ID_CORE, .., -EPIPE, ..)`.
const EPIPE: i32 = 32;

#[derive(Debug)]
enum Input {
    Graph(GraphChange),
    Cmd(Command),
    Timer(TimerKind),
}

/// Everything tied to one connection. Field order is drop order: proxies and their listeners
/// first, then the registry, then the core.
struct Conn {
    meters: HashMap<u16, MeterStream>,
    nodes: HashMap<u32, (Node, NodeListener)>,
    clients: HashMap<u32, (Client, ClientListener)>,
    default_meta: Option<DefaultMeta>,
    ports: HashSet<u32>,
    links: HashSet<u32>,
    metadata: HashMap<u32, String>,
    _registry_listener: pipewire::registry::Listener,
    _core_listener: pipewire::core::Listener,
    registry: RegistryRc,
    core: CoreRc,
}

struct DefaultMeta {
    global_id: u32,
    proxy_id: u32,
    _listener: MetadataListener,
    proxy: Metadata,
}

struct State {
    core: EngineCore,
    conn: Option<Conn>,
    /// Connections retired inside a callback; dropped from the control channel (`DropGraveyard`).
    graveyard: Vec<Conn>,
    /// Same for meter streams destroyed while one of their own callbacks may be running.
    meter_graveyard: Vec<MeterStream>,
}

struct Inner {
    ml: MainLoopRc,
    ctx: ContextRc,
    cfg: EngineConfig,
    ev_tx: async_channel::Sender<Event>,
    timers: Timers,
    ctl_tx: pipewire::channel::Sender<Control>,
    meters: Arc<MeterBank>,
    state: RefCell<State>,
    queue: RefCell<VecDeque<Input>>,
    draining: Cell<bool>,
}

#[derive(Clone)]
pub(crate) struct Shell(Rc<Inner>);

impl Shell {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        ml: MainLoopRc,
        ctx: ContextRc,
        cfg: EngineConfig,
        instance: String,
        ev_tx: async_channel::Sender<Event>,
        timers: Timers,
        ctl_tx: pipewire::channel::Sender<Control>,
        meters: Arc<MeterBank>,
    ) -> Self {
        let core = EngineCore::new(instance, cfg.reconnect_backoff);
        Self(Rc::new(Inner {
            ml,
            ctx,
            cfg,
            ev_tx,
            timers,
            ctl_tx,
            meters,
            state: RefCell::new(State {
                core,
                conn: None,
                graveyard: Vec::new(),
                meter_graveyard: Vec::new(),
            }),
            queue: RefCell::new(VecDeque::new()),
            draining: Cell::new(false),
        }))
    }

    pub fn start(&self) {
        let fx = self.0.state.borrow_mut().core.start();
        self.execute(fx);
    }

    pub fn on_control(&self, c: Control) {
        match c {
            Control::Cmd(cmd) => self.push(Input::Cmd(cmd)),
            Control::Timer(kind) => self.push(Input::Timer(kind)),
            Control::DropGraveyard => {
                let mut st = self.0.state.borrow_mut();
                st.graveyard.clear();
                st.meter_graveyard.clear();
            }
            Control::Shutdown => self.0.ml.quit(),
        }
    }

    /// Drops every PipeWire object. Called after the loop stopped.
    pub fn finish(&self) {
        let mut st = self.0.state.borrow_mut();
        st.meter_graveyard.clear();
        st.graveyard.clear();
        st.conn = None;
        self.0.queue.borrow_mut().clear();
    }

    fn push(&self, input: Input) {
        self.0.queue.borrow_mut().push_back(input);
        self.drain();
    }

    fn drain(&self) {
        if self.0.draining.replace(true) {
            return; // an outer drain will pick it up
        }
        loop {
            let Some(input) = self.0.queue.borrow_mut().pop_front() else {
                break;
            };
            let fx = {
                let mut st = self.0.state.borrow_mut();
                match input {
                    Input::Graph(ch) => st.core.on_graph(&ch),
                    Input::Cmd(cmd) => st.core.on_command(cmd),
                    Input::Timer(kind) => st.core.on_timer(kind),
                }
            };
            self.execute(fx);
        }
        self.0.draining.set(false);
    }

    fn execute(&self, fx: Effects) {
        for kind in &fx.cancel_timers {
            self.0.timers.cancel(*kind);
        }
        if fx.reset_connection {
            let mut st = self.0.state.borrow_mut();
            if let Some(conn) = st.conn.take() {
                st.graveyard.push(conn);
                let _ = self.0.ctl_tx.send(Control::DropGraveyard);
            }
        }
        for op in fx.ops {
            if let Some(change) = self.run_op(op) {
                self.0.queue.borrow_mut().push_back(Input::Graph(change));
            }
        }
        for ev in fx.events {
            // The UI may already be gone during shutdown; nothing to do then.
            let _ = self.0.ev_tx.try_send(ev);
        }
        for (kind, after) in fx.timers {
            self.0.timers.arm(kind, after);
        }
        if fx.connect_now {
            let ok = self.connect();
            let fx = self.0.state.borrow_mut().core.on_connect_result(ok);
            self.execute(fx);
        }
    }

    /// Executes one op. A synchronous failure is returned as a change for the core.
    fn run_op(&self, op: PwOp) -> Option<GraphChange> {
        let mut st = self.0.state.borrow_mut();
        let st = &mut *st;
        let Some(conn) = st.conn.as_mut() else {
            tracing::debug!(?op, "no connection; op dropped");
            return None;
        };
        match op {
            PwOp::MetaWrite {
                subject,
                key,
                type_,
                value,
            } => match &conn.default_meta {
                Some(m) => m.proxy.set_property(subject, key, type_, value.as_deref()),
                None => tracing::warn!(key, "default metadata not bound; write dropped"),
            },
            PwOp::Sync { purpose } => match conn.core.sync(0) {
                Ok(seq) => st.core.on_sync_issued(purpose, seq.seq()),
                Err(e) => tracing::warn!(?purpose, "core.sync failed: {e}"),
            },
            PwOp::CreateMeter {
                slot,
                target_serial,
                capture_sink,
                autoconnect,
            } => {
                let spec = MeterSpec {
                    slot,
                    target_serial,
                    capture_sink,
                    autoconnect,
                    instance: st.core.instance(),
                };
                let weak = Rc::downgrade(&self.0);
                let on_error = move |slot: MeterSlotId, message: String| {
                    Self::callback(&weak, |s| {
                        s.push(Input::Graph(GraphChange::MeterStreamFailed {
                            slot,
                            message,
                        }));
                    });
                };
                match create_meter(&conn.core, self.0.meters.clone(), &spec, on_error) {
                    Ok(meter) => {
                        conn.meters.insert(slot.0, meter);
                    }
                    Err(e) => {
                        return Some(GraphChange::MeterStreamFailed {
                            slot,
                            message: e.to_string(),
                        });
                    }
                }
            }
            PwOp::DestroyMeter { slot } => {
                if let Some(meter) = conn.meters.remove(&slot.0) {
                    st.meter_graveyard.push(meter);
                    let _ = self.0.ctl_tx.send(Control::DropGraveyard);
                }
                self.0.meters.reset(slot);
            }
        }
        None
    }

    /// Wraps a callback: upgrade the weak handle, contain panics.
    fn callback(weak: &Weak<Inner>, f: impl FnOnce(&Shell)) {
        if let Some(inner) = weak.upgrade() {
            let shell = Shell(inner);
            guarded(&shell.0.ml, || f(&shell));
        }
    }

    fn connect(&self) -> bool {
        let instance = self.0.state.borrow().core.instance().to_owned();
        let props = properties! {
            "application.name" => self.0.cfg.app_name,
            "application.id" => self.0.cfg.app_id,
            "media.category" => "Manager",
            "estuary.instance" => instance.as_str(),
        };
        let core = match self.0.ctx.connect_rc(Some(props)) {
            Ok(c) => c,
            Err(e) => {
                tracing::info!("cannot connect to PipeWire: {e}");
                return false;
            }
        };
        let registry = match core.get_registry_rc() {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!("cannot get the registry: {e}");
                return false;
            }
        };
        let weak = Rc::downgrade(&self.0);
        let core_listener = core
            .add_listener_local()
            .info({
                let w = weak.clone();
                move |info| {
                    let version = info.version().to_owned();
                    Self::callback(&w, |s| {
                        s.push(Input::Graph(GraphChange::CoreInfo(CoreInfo { version })));
                    });
                }
            })
            .done({
                let w = weak.clone();
                move |id, seq| {
                    if id == PW_ID_CORE {
                        let seq = seq.seq();
                        Self::callback(&w, |s| s.push(Input::Graph(GraphChange::SyncDone { seq })));
                    }
                }
            })
            .error({
                let w = weak.clone();
                move |id, _seq, res, message| {
                    let message = message.to_owned();
                    Self::callback(&w, |s| s.on_core_error(id, res, message));
                }
            })
            .register();
        let registry_listener = registry
            .add_listener_local()
            .global({
                let w = weak.clone();
                move |g| Self::callback(&w, |s| s.on_global(g))
            })
            .global_remove({
                let w = weak;
                move |id| Self::callback(&w, |s| s.on_global_remove(id))
            })
            .register();
        self.0.state.borrow_mut().conn = Some(Conn {
            meters: HashMap::new(),
            nodes: HashMap::new(),
            clients: HashMap::new(),
            default_meta: None,
            ports: HashSet::new(),
            links: HashSet::new(),
            metadata: HashMap::new(),
            _registry_listener: registry_listener,
            _core_listener: core_listener,
            registry,
            core,
        });
        true
    }

    fn on_core_error(&self, id: u32, res: i32, message: String) {
        if id == PW_ID_CORE && res == -EPIPE {
            tracing::warn!(%message, "connection to PipeWire lost");
            self.push(Input::Graph(GraphChange::Disconnected));
            return;
        }
        let is_meta = self
            .0
            .state
            .borrow()
            .conn
            .as_ref()
            .and_then(|c| c.default_meta.as_ref())
            .is_some_and(|m| m.proxy_id == id);
        if is_meta {
            self.push(Input::Graph(GraphChange::ProxyError {
                key: OwnedKey::ProbeMeta,
                res,
                message,
            }));
        } else {
            tracing::warn!(id, res, %message, "PipeWire error");
        }
    }

    fn on_global(&self, g: &GlobalObject<&DictRef>) {
        let props = dict_to_props(g.props);
        let mut changes = Vec::new();
        {
            let mut st = self.0.state.borrow_mut();
            let Some(conn) = st.conn.as_mut() else { return };
            match g.type_ {
                ObjectType::Node => {
                    changes.push(GraphChange::NodeAdded {
                        id: g.id,
                        global_props: props,
                    });
                    match conn.registry.bind::<Node, _>(g) {
                        Ok(node) => {
                            let listener = self.node_listener(&node, g.id);
                            node.subscribe_params(&[ParamType::Props]);
                            conn.nodes.insert(g.id, (node, listener));
                        }
                        Err(e) => tracing::warn!(id = g.id, "cannot bind node: {e}"),
                    }
                }
                ObjectType::Port => {
                    if let Some(port) = port_entry(g.id, &props) {
                        conn.ports.insert(g.id);
                        changes.push(GraphChange::PortAdded(port));
                    }
                }
                ObjectType::Link => {
                    if let Some(link) = link_entry(g.id, &props) {
                        conn.links.insert(g.id);
                        changes.push(GraphChange::LinkAdded(link));
                    }
                }
                ObjectType::Client => match conn.registry.bind::<Client, _>(g) {
                    Ok(client) => {
                        let listener = self.client_listener(&client, g.id);
                        conn.clients.insert(g.id, (client, listener));
                    }
                    Err(e) => tracing::warn!(id = g.id, "cannot bind client: {e}"),
                },
                ObjectType::Metadata => {
                    let name = props.get("metadata.name").cloned().unwrap_or_default();
                    conn.metadata.insert(g.id, name.clone());
                    if name == "default" {
                        match conn.registry.bind::<Metadata, _>(g) {
                            Ok(proxy) => {
                                let listener = self.metadata_listener(&proxy);
                                conn.default_meta = Some(DefaultMeta {
                                    global_id: g.id,
                                    proxy_id: proxy.upcast_ref().id(),
                                    _listener: listener,
                                    proxy,
                                });
                            }
                            Err(e) => tracing::warn!("cannot bind the default metadata: {e}"),
                        }
                    }
                    changes.push(GraphChange::MetadataAdded(name));
                }
                _ => {}
            }
        }
        for ch in changes {
            self.push(Input::Graph(ch));
        }
    }

    fn on_global_remove(&self, id: u32) {
        let change = {
            let mut st = self.0.state.borrow_mut();
            let Some(conn) = st.conn.as_mut() else { return };
            if conn.nodes.remove(&id).is_some() {
                Some(GraphChange::NodeRemoved(id))
            } else if conn.ports.remove(&id) {
                Some(GraphChange::PortRemoved(id))
            } else if conn.links.remove(&id) {
                Some(GraphChange::LinkRemoved(id))
            } else if conn.clients.remove(&id).is_some() {
                Some(GraphChange::ClientRemoved(id))
            } else if let Some(name) = conn.metadata.remove(&id) {
                if conn
                    .default_meta
                    .as_ref()
                    .is_some_and(|m| m.global_id == id)
                {
                    conn.default_meta = None;
                }
                Some(GraphChange::MetadataRemoved(name))
            } else {
                None
            }
        };
        if let Some(ch) = change {
            self.push(Input::Graph(ch));
        }
    }

    fn node_listener(&self, node: &Node, id: u32) -> NodeListener {
        let weak = Rc::downgrade(&self.0);
        node.add_listener_local()
            .info({
                let w = weak.clone();
                move |info| {
                    // State-only updates (idle ↔ running) carry no props: applying them would
                    // wipe the node's identity (found at M4).
                    if !info.change_mask().contains(NodeChangeMask::PROPS) {
                        return;
                    }
                    let props = dict_to_props(info.props());
                    Self::callback(&w, |s| {
                        s.push(Input::Graph(GraphChange::NodeInfo { id, props }));
                    });
                }
            })
            .param(move |_seq, param_type, _index, _next, pod| {
                if param_type != ParamType::Props {
                    return;
                }
                let Some(v) = pod.and_then(|p| params::parse_props_volume(p.as_bytes())) else {
                    return;
                };
                if v.channels.is_none() && v.muted.is_none() {
                    return;
                }
                Self::callback(&weak, |s| {
                    s.push(Input::Graph(GraphChange::NodeVolume {
                        id,
                        channels: v.channels,
                        muted: v.muted,
                    }));
                });
            })
            .register()
    }

    fn client_listener(&self, client: &Client, id: u32) -> ClientListener {
        let weak = Rc::downgrade(&self.0);
        client
            .add_listener_local()
            .info(move |info| {
                if !info.change_mask().contains(ClientChangeMask::PROPS) {
                    return;
                }
                let props = dict_to_props(info.props());
                Self::callback(&weak, |s| {
                    s.push(Input::Graph(GraphChange::ClientAdded(ClientEntry {
                        id,
                        props,
                    })));
                });
            })
            .register()
    }

    fn metadata_listener(&self, proxy: &Metadata) -> MetadataListener {
        let weak = Rc::downgrade(&self.0);
        proxy
            .add_listener_local()
            .property(move |subject, key, type_, value| {
                let change = match key {
                    Some(key) => GraphChange::DefaultMetaProperty {
                        subject,
                        key: key.to_owned(),
                        type_: type_.map(str::to_owned),
                        value: value.map(str::to_owned),
                    },
                    // PW_ID_ANY (u32::MAX) clears every subject.
                    None => GraphChange::DefaultMetaCleared {
                        subject: (subject != u32::MAX).then_some(subject),
                    },
                };
                Self::callback(&weak, |s| s.push(Input::Graph(change)));
                0
            })
            .register()
    }
}

fn dict_to_props(d: Option<&DictRef>) -> Props {
    d.map(|d| {
        d.iter()
            .map(|(k, v)| (k.to_owned(), v.to_owned()))
            .collect()
    })
    .unwrap_or_default()
}

/// Ports are built from global props (M2.1: they carry everything we need; never bound).
fn port_entry(id: u32, p: &Props) -> Option<PortEntry> {
    Some(PortEntry {
        id,
        node_id: parse_u32(p.get("node.id"))?,
        dir: match p.get("port.direction").map(String::as_str) {
            Some("in") => PortDir::In,
            Some("out") => PortDir::Out,
            _ => return None,
        },
        name: p.get("port.name").cloned().unwrap_or_default(),
        channel: p.get("audio.channel").cloned().unwrap_or_default(),
        monitor: p.get("port.monitor").map(String::as_str) == Some("true"),
    })
}

/// Links are built from global props (M2.1).
fn link_entry(id: u32, p: &Props) -> Option<LinkEntry> {
    Some(LinkEntry {
        id,
        out_node: parse_u32(p.get("link.output.node"))?,
        out_port: parse_u32(p.get("link.output.port"))?,
        in_node: parse_u32(p.get("link.input.node"))?,
        in_port: parse_u32(p.get("link.input.port"))?,
        client_id: parse_u32(p.get("client.id")),
    })
}
