//! The fields of one tunnel: what it does, the port it listens on and where it goes.
use crate::theme::ActiveTheme as _;
use crate::widgets::button::{Button, ButtonStyle};
use crate::widgets::text_input::TextInput;
use anyhow::{Context as _, Result, bail};
use gpui::prelude::*;
use gpui::{Context, Entity, SharedString, Window, div, px};
use shcp_core::{ForwardKind, ForwardSpec};

const KINDS: [(ForwardKind, &str, &str); 3] = [
    (
        ForwardKind::Local,
        "Local",
        "Listens on this PC and goes through the server",
    ),
    (
        ForwardKind::Remote,
        "Remote",
        "Listens on the server and comes back to this PC",
    ),
    (
        ForwardKind::Dynamic,
        "SOCKS",
        "A proxy on this PC: each connection says where it goes",
    ),
];
const LOOPBACK: &str = "127.0.0.1";

pub struct ForwardFields {
    kind: ForwardKind,
    bind: Entity<TextInput>,
    target: Entity<TextInput>,
}

/// `port` or `address:port`.
fn parse_bind(text: &str) -> Result<(String, u16)> {
    let (address, port) = match text.rsplit_once(':') {
        Some((address, port)) => (address.trim_matches(['[', ']']), port),
        None => (LOOPBACK, text),
    };
    Ok((address.to_string(), parse_port(port)?))
}

/// `host:port`. A port alone is on this machine's loopback, and nothing at all is `port` there too.
fn parse_target(text: &str, port: u16) -> Result<(String, u16)> {
    if text.is_empty() {
        return Ok((LOOPBACK.to_string(), port));
    }
    match text.rsplit_once(':') {
        Some((host, port)) => Ok((host.trim_matches(['[', ']']).to_string(), parse_port(port)?)),
        None => Ok((
            LOOPBACK.to_string(),
            parse_port(text)
                .with_context(|| format!("Destination \"{text}\" must be host:port"))?,
        )),
    }
}

fn parse_port(text: &str) -> Result<u16> {
    match text.parse() {
        Ok(port) if port > 0 => Ok(port),
        _ => bail!("Port \"{text}\" must be a number from 1 to 65535"),
    }
}

impl ForwardFields {
    pub fn new(spec: Option<&ForwardSpec>, cx: &mut Context<Self>) -> Self {
        let bind_text = spec.map_or_else(String::new, |spec| {
            if spec.bind_addr == LOOPBACK {
                spec.bind_port.to_string()
            } else {
                format!("{}:{}", spec.bind_addr, spec.bind_port)
            }
        });
        let target_text = spec
            .filter(|spec| spec.kind != ForwardKind::Dynamic)
            .map_or_else(String::new, |spec| {
                format!("{}:{}", spec.remote_host, spec.remote_port)
            });
        let field = |placeholder: &str, text: &str, cx: &mut Context<Self>| {
            let (placeholder, text) = (placeholder.to_string(), text.to_string());
            cx.new(|cx| {
                let mut input = TextInput::new(&placeholder, false, cx);
                input.set_text(&text, cx);
                input
            })
        };
        Self {
            kind: spec.map_or(ForwardKind::Local, |spec| spec.kind),
            bind: field("Port", &bind_text, cx),
            target: field("Same port on 127.0.0.1", &target_text, cx),
        }
    }

    /// The two text fields, for whoever wants to know about Enter and Esc in them.
    pub fn inputs(&self) -> [&Entity<TextInput>; 2] {
        [&self.bind, &self.target]
    }

    /// Raw values, including incomplete ports, for detecting unsaved edits.
    pub fn draft(&self, cx: &gpui::App) -> [String; 3] {
        [
            format!("{:?}", self.kind),
            self.bind.read(cx).text().to_string(),
            self.target.read(cx).text().to_string(),
        ]
    }

    pub fn focus(&self, window: &mut Window, cx: &gpui::App) {
        self.bind.read(cx).focus(window);
    }

    pub fn clear(&mut self, cx: &mut Context<Self>) {
        self.bind.update(cx, |input, cx| input.set_text("", cx));
        self.target.update(cx, |input, cx| input.set_text("", cx));
    }

    /// Nothing was typed: a row left empty is no tunnel.
    pub fn is_blank(&self, cx: &gpui::App) -> bool {
        self.bind.read(cx).text().trim().is_empty() && self.target.read(cx).text().trim().is_empty()
    }

    pub fn spec(&self, cx: &gpui::App) -> Result<ForwardSpec> {
        let bind = self.bind.read(cx).text().trim().to_string();
        if bind.is_empty() {
            bail!("A tunnel needs the port it listens on");
        }
        let (bind_addr, bind_port) = parse_bind(&bind)?;
        if self.kind == ForwardKind::Dynamic {
            return Ok(ForwardSpec::socks(bind_addr, bind_port));
        }
        let target = self.target.read(cx).text().trim().to_string();
        let (remote_host, remote_port) = parse_target(&target, bind_port)?;
        Ok(ForwardSpec {
            bind_addr,
            bind_port,
            remote_host,
            remote_port,
            kind: self.kind,
        })
    }
}

impl Render for ForwardFields {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let kind = self.kind;
        let (listens, goes) = match kind {
            ForwardKind::Local => ("on this PC", "from the server"),
            ForwardKind::Remote => ("on the server", "from this PC"),
            ForwardKind::Dynamic => ("on this PC", ""),
        };
        let label = |text: SharedString| {
            div()
                .flex_none()
                .text_size(crate::theme::ui_px(12.))
                .text_color(theme.text_muted)
                .child(text)
        };
        div()
            .flex()
            .flex_1()
            .min_w_0()
            .items_center()
            .gap(px(6.))
            .child(
                div()
                    .flex()
                    .flex_none()
                    .gap(px(2.))
                    .children(
                        KINDS
                            .iter()
                            .enumerate()
                            .map(|(index, (option, name, help))| {
                                let option = *option;
                                Button::new(("forward-kind", index))
                                    .keyboard_focus()
                                    .label(*name)
                                    .tooltip(*help)
                                    .style(ButtonStyle::Ghost)
                                    .compact()
                                    .active(kind == option)
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.kind = option;
                                        cx.notify();
                                    }))
                            }),
                    ),
            )
            .child(div().w(px(84.)).flex_none().child(self.bind.clone()))
            .child(label(listens.into()))
            .when(kind != ForwardKind::Dynamic, |this| {
                this.child(label("→".into()))
                    .child(div().flex_1().min_w_0().child(self.target.clone()))
                    .child(label(goes.into()))
            })
    }
}
