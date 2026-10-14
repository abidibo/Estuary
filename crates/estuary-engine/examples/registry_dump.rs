//! M2.1 experiment helper (read-only). Connects to PipeWire with Estuary's client properties
//! and reports how our own client can be identified in the registry:
//! (i) whether the custom `estuary.instance` property is visible in global and/or info props,
//! (ii) whether `application.process.id` matches our pid.
//!
//! Run: `cargo run -p estuary-engine --example registry_dump --features live-pw`
#![allow(clippy::print_stdout)] // an experiment tool: its output is the point

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use pipewire as pw;
use pw::client::{Client, ClientListener};
use pw::properties::properties;
use pw::types::ObjectType;

fn main() -> Result<(), pw::Error> {
    pw::init();
    let instance = uuid::Uuid::new_v4().simple().to_string();
    let pid = std::process::id().to_string();
    println!("our instance = {instance}, pid = {pid}");

    let main_loop = pw::main_loop::MainLoopRc::new(None)?;
    let context = pw::context::ContextRc::new(&main_loop, None)?;
    let core = context.connect_rc(Some(properties! {
        "application.name" => "Estuary",
        "application.id" => "io.github.abidibo.Estuary",
        "media.category" => "Manager",
        "estuary.instance" => instance.as_str(),
    }))?;
    let registry = core.get_registry_rc()?;
    let registry_weak = registry.downgrade();

    let bound: Rc<RefCell<HashMap<u32, (Client, ClientListener)>>> = Rc::default();
    let _reg_listener = registry
        .add_listener_local()
        .global({
            let bound = bound.clone();
            let instance = instance.clone();
            let pid = pid.clone();
            move |g| {
                if g.type_ != ObjectType::Client {
                    return;
                }
                let gp = g.props.as_ref();
                let g_inst = gp.and_then(|p| p.get("estuary.instance"));
                let g_pid = gp.and_then(|p| p.get("application.process.id"));
                let g_name = gp.and_then(|p| p.get("application.name"));
                if g_name == Some("Estuary") || g_pid == Some(pid.as_str()) {
                    println!(
                        "GLOBAL client {}: application.name={g_name:?} application.process.id={g_pid:?} estuary.instance={g_inst:?}",
                        g.id
                    );
                }
                let Some(registry) = registry_weak.upgrade() else { return };
                let Ok(client) = registry.bind::<Client, _>(g) else { return };
                let id = g.id;
                let instance = instance.clone();
                let pid = pid.clone();
                let listener = client
                    .add_listener_local()
                    .info(move |info| {
                        let Some(p) = info.props() else { return };
                        let inst = p.get("estuary.instance");
                        let ppid = p.get("application.process.id");
                        if inst.is_some() || ppid == Some(pid.as_str()) {
                            println!(
                                "INFO   client {id}: estuary.instance={inst:?} (ours: {}) application.process.id={ppid:?} (ours: {})",
                                inst == Some(instance.as_str()),
                                ppid == Some(pid.as_str())
                            );
                        }
                    })
                    .register();
                bound.borrow_mut().insert(id, (client, listener));
            }
        })
        .register();

    // Stop after two round-trips so all globals and their info events have arrived.
    let pending = core.sync(0)?;
    let ml_weak = main_loop.downgrade();
    let core_weak = core.downgrade();
    let second: Rc<RefCell<Option<pw::spa::utils::result::AsyncSeq>>> = Rc::default();
    let _core_listener = core
        .add_listener_local()
        .done(move |id, seq| {
            if id != pw::core::PW_ID_CORE {
                return;
            }
            if seq == pending {
                if let Some(core) = core_weak.upgrade() {
                    *second.borrow_mut() = core.sync(0).ok();
                }
            } else if Some(seq) == *second.borrow()
                && let Some(ml) = ml_weak.upgrade()
            {
                ml.quit();
            }
        })
        .register();

    main_loop.run();
    Ok(())
}
