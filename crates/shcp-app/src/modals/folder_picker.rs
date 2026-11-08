//! Picks a folder of the server by walking through it, or by typing where it is.
use super::{button_row, card, title};
use crate::state::AppState;
use crate::theme::{ActiveTheme as _, row_px, ui_px};
use crate::widgets::button::{Button, ButtonStyle};
use crate::widgets::icon::{Icon, IconName};
use crate::widgets::scrollbar::{ScrollDrag, ScrollMetrics, scrollbar};
use crate::widgets::text_input::{TextInput, TextInputEvent};
use gpui::prelude::*;
use gpui::{
    App, Context, DismissEvent, Entity, EventEmitter, FocusHandle, Focusable, ScrollStrategy,
    Subscription, UniformListScrollHandle, Window, div, point, px, uniform_list,
};
use shcp_core::{EntryKind, Event, RemoteEntry, RequestId, SessionId};
use std::ops::Range;

type OnPick = Box<dyn FnOnce(String, &mut Window, &mut App)>;
/// Why a folder cannot be the one picked.
type Refusal = Box<dyn Fn(&str) -> Option<&'static str>>;

const ROW_H: f32 = 28.;
const LIST_H: f32 = 300.;

pub struct FolderPicker {
    state: Entity<AppState>,
    session: SessionId,
    title: String,
    confirm: &'static str,
    /// The folder on screen, as the server names it; empty until the first listing arrives.
    path: String,
    folders: Vec<RemoteEntry>,
    input: Entity<TextInput>,
    pending: Option<RequestId>,
    error: Option<String>,
    refusal: Refusal,
    on_pick: Option<OnPick>,
    scroll: UniformListScrollHandle,
    scroll_drag: ScrollDrag,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<DismissEvent> for FolderPicker {}
impl EventEmitter<super::CancelEvent> for FolderPicker {}

impl Focusable for FolderPicker {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.input.focus_handle(cx)
    }
}

impl FolderPicker {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        state: Entity<AppState>,
        session: SessionId,
        title: String,
        confirm: &'static str,
        start: String,
        refusal: impl Fn(&str) -> Option<&'static str> + 'static,
        on_pick: impl FnOnce(String, &mut Window, &mut App) + 'static,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let input = cx.new(|cx| TextInput::new("Path on the server", false, cx));
        let subscriptions = vec![
            cx.subscribe_in(
                &input,
                window,
                |this, input, event: &TextInputEvent, _, cx| match event {
                    TextInputEvent::Submit => {
                        let typed = input.read(cx).text().trim().to_string();
                        if !typed.is_empty() {
                            let target = this.typed_target(&typed);
                            this.go_to(target, cx);
                        }
                    }
                    TextInputEvent::Cancel => cx.emit(DismissEvent),
                    TextInputEvent::Changed => {}
                },
            ),
            cx.subscribe(&state, |this, _, event: &Event, cx| {
                this.on_event(event, cx)
            }),
        ];
        let mut this = Self {
            state,
            session,
            title,
            confirm,
            path: String::new(),
            folders: Vec::new(),
            input,
            pending: None,
            error: None,
            refusal: Box::new(refusal),
            on_pick: Some(Box::new(on_pick)),
            scroll: UniformListScrollHandle::new(),
            scroll_drag: ScrollDrag::default(),
            _subscriptions: subscriptions,
        };
        this.go_to(start, cx);
        this
    }

    /// From the folder on screen, or `~` for the home folder, which is where SFTP starts from.
    fn typed_target(&self, typed: &str) -> String {
        match typed.strip_prefix('~') {
            Some("") => ".".to_string(),
            Some(rest) => rest.trim_start_matches('/').to_string(),
            None if typed.starts_with('/') || self.path.is_empty() => typed.to_string(),
            None => crate::files_format::join(&self.path, typed),
        }
    }

    fn go_to(&mut self, path: String, cx: &mut Context<Self>) {
        self.error = None;
        self.pending = Some(self.state.read(cx).engine.list_dir(self.session, path));
        cx.notify();
    }

    fn go_up(&mut self, cx: &mut Context<Self>) {
        if !self.path.is_empty() && self.path != "/" {
            self.go_to(crate::files_format::parent(&self.path), cx);
        }
    }

    fn on_event(&mut self, event: &Event, cx: &mut Context<Self>) {
        let Event::DirListing { req, path, entries } = event else {
            return;
        };
        if self.pending != Some(*req) {
            return;
        }
        self.pending = None;
        match entries {
            Ok(entries) => {
                let show_hidden = self.state.read(cx).settings.show_hidden_files;
                self.folders = entries
                    .iter()
                    // What a link points to is found out on opening it.
                    .filter(|e| matches!(e.kind, EntryKind::Dir | EntryKind::Symlink))
                    .filter(|e| show_hidden || !e.name.starts_with('.'))
                    .cloned()
                    .collect();
                self.folders.sort_by_key(|entry| entry.name.to_lowercase());
                self.path = path.clone();
                self.input.update(cx, |input, cx| input.set_text(path, cx));
                self.scroll.scroll_to_item(0, ScrollStrategy::Top);
            }
            // What was on screen stays, so that there is still somewhere to go.
            Err(error) => {
                self.error = Some(format!("Could not open {path}: {error}"));
                let current = self.path.clone();
                self.input
                    .update(cx, |input, cx| input.set_text(&current, cx));
            }
        }
        cx.notify();
    }

    fn pick(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.pending.is_some() || self.path.is_empty() || (self.refusal)(&self.path).is_some() {
            return;
        }
        if let Some(on_pick) = self.on_pick.take() {
            on_pick(self.path.clone(), window, cx);
        }
        cx.emit(DismissEvent);
    }

    fn render_row(&self, index: usize, cx: &mut Context<Self>) -> gpui::AnyElement {
        let theme = cx.theme().clone();
        let entry = &self.folders[index];
        let (hover, press) = (theme.hover, theme.active);
        let target = entry.path.clone();
        let (icon, color) = if entry.kind == EntryKind::Symlink {
            (IconName::Link, theme.text_muted)
        } else {
            (IconName::Folder, theme.accent)
        };
        div()
            .id(("picker-folder", index))
            .w_full()
            .h(row_px(ROW_H))
            .px(px(8.))
            .flex()
            .items_center()
            .gap(px(8.))
            .rounded(px(6.))
            .cursor_pointer()
            .hover(move |s| s.bg(hover))
            .active(move |s| s.bg(press))
            .on_click(cx.listener(move |this, _, _, cx| this.go_to(target.clone(), cx)))
            .child(Icon::new(icon).color(color))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .child(entry.name.clone()),
            )
            .child(
                Icon::new(IconName::ChevronRight)
                    .size(px(12.))
                    .color(theme.text_disabled),
            )
            .into_any_element()
    }
}

impl Render for FolderPicker {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let loading = self.pending.is_some();
        let refused = (!self.path.is_empty())
            .then(|| (self.refusal)(&self.path))
            .flatten();
        let can_pick = !loading && !self.path.is_empty() && refused.is_none();
        let empty = self.folders.is_empty();
        let bar = {
            let (metrics, moved) = (self.scroll.clone(), self.scroll.clone());
            let this = cx.entity().downgrade();
            scrollbar(
                self.scroll_drag.clone(),
                theme.text_muted,
                move |_| {
                    let base = metrics.0.borrow().base_handle.clone();
                    let max = f32::from(base.max_offset().height);
                    let height = f32::from(base.bounds().size.height);
                    (max > 0.).then(|| ScrollMetrics {
                        position: -f32::from(base.offset().y) / max,
                        visible: height / (height + max),
                    })
                },
                move |position, _, cx| {
                    let base = moved.0.borrow().base_handle.clone();
                    let max = f32::from(base.max_offset().height);
                    base.set_offset(point(px(0.), px(-position * max)));
                    this.update(cx, |_, cx| cx.notify()).ok();
                },
            )
        };
        card(&theme, 560.)
            .child(title(&theme, self.title.clone()))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .child(
                        Button::new("picker-up")
                            .keyboard_focus()
                            .icon(IconName::ArrowUp)
                            .tooltip("Parent folder")
                            .style(ButtonStyle::Ghost)
                            .disabled(self.path.is_empty() || self.path == "/")
                            .on_click(cx.listener(|this, _, _, cx| this.go_up(cx))),
                    )
                    .child(div().flex_1().min_w_0().child(self.input.clone())),
            )
            .child(
                div()
                    .relative()
                    .w_full()
                    .h(px(LIST_H))
                    .flex_none()
                    .rounded(px(8.))
                    .border_1()
                    .border_color(theme.border)
                    .p(px(4.))
                    .child(
                        uniform_list(
                            "picker-folders",
                            self.folders.len(),
                            cx.processor(|this, range: Range<usize>, _window, cx| {
                                range.map(|i| this.render_row(i, cx)).collect::<Vec<_>>()
                            }),
                        )
                        .track_scroll(self.scroll.clone())
                        .size_full(),
                    )
                    .child(bar)
                    .when(empty, |this| {
                        this.child(
                            div()
                                .absolute()
                                .top(px(16.))
                                .w_full()
                                .flex()
                                .justify_center()
                                .text_color(theme.text_muted)
                                .child(if loading {
                                    "Loading…"
                                } else {
                                    "No folders here"
                                }),
                        )
                    }),
            )
            .children(
                self.error
                    .clone()
                    .map(|error| div().text_color(theme.error).child(error)),
            )
            .children(refused.map(|reason| {
                div()
                    .text_size(ui_px(12.))
                    .text_color(theme.text_muted)
                    .child(reason)
            }))
            .child(
                button_row()
                    .child(
                        Button::new("picker-cancel")
                            .keyboard_focus()
                            .label("Cancel")
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(DismissEvent))),
                    )
                    .child(
                        Button::new("picker-confirm")
                            .keyboard_focus()
                            .label(self.confirm)
                            .style(ButtonStyle::Primary)
                            .disabled(!can_pick)
                            .on_click(cx.listener(|this, _, window, cx| this.pick(window, cx))),
                    ),
            )
    }
}

impl super::ModalView for FolderPicker {}
