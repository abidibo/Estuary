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
        "estuary".into()
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

/// A neutral light-grey 3×3 matrix glyph as ARGB32 pixmaps (22, 32, 48 px). Packaging (M12)
/// may replace it with the rendered app symbol; hosts that resolve `icon_name` use that.
pub fn render_tray_pixmaps() -> Vec<ksni::Icon> {
    [22, 32, 48]
        .into_iter()
        .filter_map(|size| {
            glyph(size)
                .map_err(|e| tracing::warn!("tray icon: {e}"))
                .ok()
        })
        .collect()
}

fn glyph(size: i32) -> Result<ksni::Icon, relm4::gtk::cairo::Error> {
    use relm4::gtk::cairo::{Context, Format, ImageSurface};
    let mut surface = ImageSurface::create(Format::ARgb32, size, size)?;
    {
        let cr = Context::new(&surface)?;
        cr.set_source_rgba(
            0xde as f64 / 255.0,
            0xde as f64 / 255.0,
            0xde as f64 / 255.0,
            1.0,
        );
        let s = f64::from(size);
        let cell = s / 4.0;
        let r = cell * 0.36;
        for row in 0..3 {
            for col in 0..3 {
                // A diagonal of "on" sends, the rest small.
                let on = row == col;
                let (x, y) = (cell * (f64::from(col) + 1.0), cell * (f64::from(row) + 1.0));
                cr.arc(
                    x,
                    y,
                    if on { r * 1.35 } else { r * 0.7 },
                    0.0,
                    std::f64::consts::TAU,
                );
                cr.fill()?;
            }
        }
    }
    surface.flush();
    let stride = usize::try_from(surface.stride()).unwrap_or(0);
    let side = usize::try_from(size).unwrap_or(0);
    let data = surface
        .data()
        .map_err(|_| relm4::gtk::cairo::Error::SurfaceFinished)?;
    let mut argb = Vec::with_capacity(side * side * 4);
    for y in 0..side {
        for x in 0..side {
            // cairo ARGB32 is native-endian u32 (premultiplied): little-endian bytes B, G, R, A.
            let i = y * stride + x * 4;
            argb.extend_from_slice(&[data[i + 3], data[i + 2], data[i + 1], data[i]]);
        }
    }
    Ok(ksni::Icon {
        width: size,
        height: size,
        data: argb,
    })
}
