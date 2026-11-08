//! What a host runs: the command every new terminal starts with, and the saved commands the
//! palette offers. A modal of its own, because `HostEditor` never opens for the read-only
//! `~/.ssh/config` hosts.
use super::{button_row, card, title};
use crate::state::AppState;
use crate::theme::{ActiveTheme as _, ui_px};
use crate::widgets::button::{Button, ButtonStyle};
use crate::widgets::icon::IconName;
use crate::widgets::text_input::{TextInput, TextInputEvent};
use gpui::prelude::*;
use gpui::{
    App, Context, DismissEvent, Entity, EventEmitter, FocusHandle, Focusable, Subscription, Window,
    div, px,
};
use shcp_core::{HostId, Snippet};

struct Row {
    id: u64,
    name: Entity<TextInput>,
    command: Entity<TextInput>,
    everywhere: bool,
}

pub struct HostCommandsModal {
    state: Entity<AppState>,
    host: HostId,
    alias: String,
    startup: Entity<TextInput>,
    rows: Vec<Row>,
    next_row: u64,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<DismissEvent> for HostCommandsModal {}
impl EventEmitter<super::CancelEvent> for HostCommandsModal {}

impl Focusable for HostCommandsModal {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.startup.focus_handle(cx)
    }
}

impl HostCommandsModal {
    pub fn new(
        state: Entity<AppState>,
        host: HostId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let (alias, startup_text, snippets) = {
            let state = state.read(cx);
            (
                state
                    .host(&host)
                    .map_or(host.0.clone(), |h| h.alias.clone()),
                state
                    .settings
                    .startup_commands
                    .get(&host.0)
                    .cloned()
                    .unwrap_or_default(),
                state
                    .settings
                    .snippets_for(&host)
                    .cloned()
                    .collect::<Vec<_>>(),
            )
        };
        let startup = cx.new(|cx| {
            let mut input = TextInput::new("cd /app", false, cx);
            input.set_text(&startup_text, cx);
            input
        });
        let subscriptions = vec![Self::watch(&startup, window, cx)];
        let mut this = Self {
            state,
            host,
            alias,
            startup,
            rows: Vec::new(),
            next_row: 0,
            _subscriptions: subscriptions,
        };
        for snippet in snippets {
            this.add_row(&snippet, window, cx);
        }
        this
    }

    fn watch(
        input: &Entity<TextInput>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Subscription {
        cx.subscribe_in(
            input,
            window,
            |this, _, event: &TextInputEvent, _, cx| match event {
                TextInputEvent::Submit => this.save(cx),
                TextInputEvent::Cancel => cx.emit(super::CancelEvent),
                TextInputEvent::Changed => {}
            },
        )
    }

    fn add_row(&mut self, snippet: &Snippet, window: &mut Window, cx: &mut Context<Self>) {
        let field = |placeholder: &str, text: &str, cx: &mut Context<Self>| {
            cx.new(|cx| {
                let mut input = TextInput::new(placeholder, false, cx);
                input.set_text(text, cx);
                input
            })
        };
        let name = field("Name", &snippet.name, cx);
        let command = field("tail -f /var/log/app.log", &snippet.command, cx);
        self._subscriptions.push(Self::watch(&name, window, cx));
        self._subscriptions.push(Self::watch(&command, window, cx));
        self.rows.push(Row {
            id: self.next_row,
            name,
            command,
            everywhere: snippet.host.is_none(),
        });
        self.next_row += 1;
        cx.notify();
    }

    fn save(&mut self, cx: &mut Context<Self>) {
        let startup = self.startup.read(cx).text().trim().to_string();
        // A row without a command says nothing; one without a name is called by its command.
        let snippets: Vec<_> = self
            .rows
            .iter()
            .filter_map(|row| {
                let command = row.command.read(cx).text().trim().to_string();
                if command.is_empty() {
                    return None;
                }
                let name = row.name.read(cx).text().trim().to_string();
                Some(Snippet {
                    name: if name.is_empty() {
                        command.clone()
                    } else {
                        name
                    },
                    command,
                    host: (!row.everywhere).then(|| self.host.0.clone()),
                })
            })
            .collect();
        let host = self.host.clone();
        self.state.update(cx, |state, cx| {
            state.set_host_commands(&host, startup, snippets, cx)
        });
        cx.emit(DismissEvent);
    }
}

impl Render for HostCommandsModal {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let label = |text: &'static str| {
            div()
                .text_size(ui_px(12.))
                .text_color(theme.text_muted)
                .child(text)
        };
        let rows: Vec<_> = self
            .rows
            .iter()
            .map(|row| {
                let id = row.id;
                let everywhere = row.everywhere;
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .child(div().w(px(140.)).flex_none().child(row.name.clone()))
                    .child(div().flex_1().min_w_0().child(row.command.clone()))
                    .child(
                        Button::new(("command-everywhere", id as usize))
                            .keyboard_focus()
                            .icon(IconName::Globe)
                            .tooltip(if everywhere {
                                "Offered on every host. Click for this host only"
                            } else {
                                "Offered on this host only. Click for every host"
                            })
                            .style(ButtonStyle::Ghost)
                            .active(everywhere)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                if let Some(row) = this.rows.iter_mut().find(|r| r.id == id) {
                                    row.everywhere = !row.everywhere;
                                }
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new(("command-remove", id as usize))
                            .keyboard_focus()
                            .icon(IconName::Trash)
                            .tooltip("Remove")
                            .style(ButtonStyle::Ghost)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.rows.retain(|r| r.id != id);
                                cx.notify();
                            })),
                    )
            })
            .collect();
        let empty = rows.is_empty();
        card(&theme, 620.)
            .child(title(&theme, format!("Commands of {}", self.alias)))
            .child(label("Run in every new terminal"))
            .child(div().child(self.startup.clone()))
            .child(label("Saved commands. Ctrl+Shift+K finds and runs them"))
            .child(
                div()
                    .id("command-rows")
                    .max_h(px(280.))
                    .overflow_y_scroll()
                    .flex()
                    .flex_col()
                    .gap(px(6.))
                    .children(rows)
                    .when(empty, |this| {
                        this.child(
                            div()
                                .text_color(theme.text_muted)
                                .child("No saved commands yet."),
                        )
                    }),
            )
            .child(
                div().child(
                    Button::new("command-add")
                        .keyboard_focus()
                        .icon(IconName::Plus)
                        .label("Add command")
                        .style(ButtonStyle::Ghost)
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.add_row(&Snippet::default_for_host(&this.host), window, cx);
                            if let Some(row) = this.rows.last() {
                                row.name.read(cx).focus(window);
                            }
                        })),
                ),
            )
            .child(
                button_row()
                    .child(
                        Button::new("cancel")
                            .keyboard_focus()
                            .label("Cancel")
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(super::CancelEvent))),
                    )
                    .child(
                        Button::new("save")
                            .keyboard_focus()
                            .label("Save")
                            .style(ButtonStyle::Primary)
                            .on_click(cx.listener(|this, _, _, cx| this.save(cx))),
                    ),
            )
    }
}

impl super::ModalView for HostCommandsModal {
    fn draft(&self, cx: &App) -> Vec<String> {
        let mut values = vec![self.startup.read(cx).text().to_string()];
        for row in &self.rows {
            values.extend([
                row.name.read(cx).text().to_string(),
                row.command.read(cx).text().to_string(),
                row.everywhere.to_string(),
            ]);
        }
        values
    }
}
