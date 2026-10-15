//! StatusNotifierItem tray (desktop M11.D12): "Show Estuary" and "Quit"; left click shows the
//! window. Absent where the desktop has no SNI host (GNOME without an extension).

use ksni::TrayMethods;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayEvent {
    Show,
    Quit,
}

pub struct EstuaryTray {
    tx: async_channel::Sender<TrayEvent>,
    icon_name: String,
    icon_pixmaps: Vec<ksni::Icon>,
    label_show: String,
    label_quit: String,
    tooltip: String,
}

impl ksni::Tray for EstuaryTray {
    fn id(&self) -> String {
        crate::config_build::APP_ID.into()
    }
    fn title(&self) -> String {
        "Estuary".into()
    }
    fn icon_name(&self) -> String {
        self.icon_name.clone()
    }
    fn icon_pixmap(&self) -> Vec<ksni::Icon> {
        self.icon_pixmaps.clone()
    }
    fn category(&self) -> ksni::Category {
        ksni::Category::Hardware
    }
    fn tool_tip(&self) -> ksni::ToolTip {
        ksni::ToolTip {
            title: self.tooltip.clone(),
            ..Default::default()
        }
    }
    fn activate(&mut self, _x: i32, _y: i32) {
        let _ = self.tx.try_send(TrayEvent::Show);
    }
    fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
        use ksni::menu::StandardItem;
        vec![
            StandardItem {
                label: self.label_show.clone(),
                activate: Box::new(|t: &mut Self| {
                    let _ = t.tx.try_send(TrayEvent::Show);
                }),
                ..Default::default()
            }
            .into(),
            ksni::MenuItem::Separator,
            StandardItem {
                label: self.label_quit.clone(),
                icon_name: "application-exit-symbolic".into(),
                activate: Box::new(|t: &mut Self| {
                    let _ = t.tx.try_send(TrayEvent::Quit);
                }),
                ..Default::default()
            }
            .into(),
        ]
    }
    fn watcher_offline(&self, reason: ksni::OfflineReason) -> bool {
        tracing::info!(?reason, "tray watcher offline");
        true // keep the service: ksni re-registers when the watcher returns
    }
    fn watcher_online(&self) {
        tracing::info!("tray watcher online");
    }
}

/// The running tray (`ksni::Handle` isn't `Debug`).
pub struct TrayHandle(pub ksni::Handle<EstuaryTray>);

impl std::fmt::Debug for TrayHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("TrayHandle")
    }
}

/// Texts and icons prepared on the UI thread.
pub struct TraySpec {
    pub icon_name: String,
    pub icon_pixmaps: Vec<ksni::Icon>,
    pub label_show: String,
    pub label_quit: String,
    pub tooltip: String,
}

/// Spawns the tray on relm4's tokio runtime. `events` receives clicks; `ready` gets the handle,
/// or `None` when the desktop has no tray host.
pub fn spawn_tray(
    spec: TraySpec,
    sandboxed: bool,
    events: async_channel::Sender<TrayEvent>,
    ready: impl FnOnce(Option<TrayHandle>) + Send + 'static,
) {
    let tray = EstuaryTray {
        tx: events,
        icon_name: spec.icon_name,
        icon_pixmaps: spec.icon_pixmaps,
        label_show: spec.label_show,
        label_quit: spec.label_quit,
        tooltip: spec.tooltip,
    };
    relm4::spawn(async move {
        match tray.disable_dbus_name(sandboxed).spawn().await {
            Ok(handle) => ready(Some(TrayHandle(handle))),
            Err(e @ (ksni::Error::Watcher(_) | ksni::Error::WontShow)) => {
                tracing::info!("no StatusNotifierItem host; running without a tray ({e})");
                ready(None);
            }
            Err(e) => {
                tracing::warn!("tray: {e}");
                ready(None);
            }
        }
    });
}

/// The bundled symbolic icon, recoloured light grey (tray backgrounds are usually dark), as
/// ARGB32 pixmaps at 22, 32 and 48 px. Hosts that resolve `icon_name` use that instead.
pub fn render_tray_pixmaps() -> Vec<ksni::Icon> {
    const SVG: &str = include_str!(
        "../../../../data/icons/hicolor/symbolic/apps/io.github.abidibo.Estuary-symbolic.svg"
    );
    let svg = SVG.replace("#2e3436", "#dedede");
    [22, 32, 48]
        .into_iter()
        .filter_map(|size| match pixmap(&svg, size) {
            Ok(icon) => Some(icon),
            Err(e) => {
                tracing::warn!("tray icon: {e}");
                None
            }
        })
        .collect()
}

fn pixmap(svg: &str, size: i32) -> Result<ksni::Icon, relm4::gtk::glib::Error> {
    use relm4::gtk::{gdk_pixbuf::Pixbuf, gio, glib};
    let stream = gio::MemoryInputStream::from_bytes(&glib::Bytes::from(svg.as_bytes()));
    let pb = Pixbuf::from_stream_at_scale(&stream, size, size, true, gio::Cancellable::NONE)?;
    let pb = if pb.has_alpha() {
        pb
    } else {
        pb.add_alpha(false, 0, 0, 0)
            .map_err(|e| glib::Error::new(glib::FileError::Failed, &e.to_string()))?
    };
    let (w, h) = (pb.width(), pb.height());
    let stride = usize::try_from(pb.rowstride()).unwrap_or(0);
    let bytes = pb.read_pixel_bytes();
    let (wu, hu) = (
        usize::try_from(w).unwrap_or(0),
        usize::try_from(h).unwrap_or(0),
    );
    let mut argb = Vec::with_capacity(wu * hu * 4);
    for y in 0..hu {
        for x in 0..wu {
            let i = y * stride + x * 4; // RGBA, not premultiplied
            argb.extend_from_slice(&[bytes[i + 3], bytes[i], bytes[i + 1], bytes[i + 2]]);
        }
    }
    Ok(ksni::Icon {
        width: w,
        height: h,
        data: argb,
    })
}
