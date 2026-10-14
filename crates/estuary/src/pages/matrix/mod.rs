//! Matrix page. M3 shows a placeholder; the grid arrives in M9.

use relm4::{ComponentParts, ComponentSender, SimpleComponent, adw};

use crate::i18n::i18n;

#[derive(Debug)]
pub struct MatrixPage;

impl SimpleComponent for MatrixPage {
    type Init = ();
    type Input = ();
    type Output = ();
    type Root = adw::StatusPage;
    type Widgets = ();

    fn init_root() -> adw::StatusPage {
        adw::StatusPage::new()
    }

    fn init(_: (), root: adw::StatusPage, _sender: ComponentSender<Self>) -> ComponentParts<Self> {
        root.set_icon_name(Some("view-grid-symbolic"));
        root.set_title(&i18n("Matrix"));
        root.set_description(Some(&i18n(
            "Channels, virtual microphones and their sends will appear here.",
        )));
        ComponentParts {
            model: MatrixPage,
            widgets: (),
        }
    }
}
