//! Isolated native material preview: no engine, settings, credentials or network.
//! Three panes of one split, the first with the keyboard, over the reference's own colors.
//! Run with `cargo run -p shcp-app --example terminal_material --locked --offline`.
// The preview shows Prune's colors only: what recolors the material is not used here.
#[allow(dead_code)]
#[path = "../src/terminal_material.rs"]
mod terminal_material;

use gpui::{prelude::*, *};
use terminal_material::Material;

const GAP: f32 = 6.;

struct Preview;

impl Preview {
    fn pane(title: &'static str, active: bool) -> Div {
        let material = Material {
            radius: 12.,
            ..Material::smoked(rgb(0x22201e).into())
        };
        terminal_material::frame(material, active, active)
            .size_full()
            .child(
                div()
                    .h(px(38.))
                    .px(px(18.))
                    .flex()
                    .items_center()
                    .child(title),
            )
            .child(div().px(px(18.)).pt(px(8.)).child("$ material preview"))
    }
}

impl Render for Preview {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .relative()
            .child(terminal_material::backdrop(
                Material::smoked(rgb(0x22201e).into()),
                1.,
            ))
            .p(px(8.))
            .text_color(rgb(0xece3df))
            .font_family("Consolas")
            .text_size(px(13.))
            .child(
                div()
                    .size_full()
                    .relative()
                    .flex()
                    .gap(px(GAP))
                    .child(
                        div()
                            .w(relative(0.55))
                            .h_full()
                            .child(Self::pane("active", true)),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .h_full()
                            .flex()
                            .flex_col()
                            .gap(px(GAP))
                            .child(div().h(relative(0.4)).child(Self::pane("idle, top", false)))
                            .child(
                                div()
                                    .flex_1()
                                    .min_h_0()
                                    .child(Self::pane("idle, bottom", false)),
                            ),
                    ),
            )
    }
}

fn main() {
    Application::new().run(|cx: &mut App| {
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
                    None,
                    size(px(1024.), px(620.)),
                    cx,
                ))),
                titlebar: Some(TitlebarOptions {
                    title: Some("SHCP isolated material preview".into()),
                    ..Default::default()
                }),
                ..Default::default()
            },
            |_, cx| cx.new(|_| Preview),
        )
        .expect("open isolated preview");
        cx.activate(true);
    });
}
