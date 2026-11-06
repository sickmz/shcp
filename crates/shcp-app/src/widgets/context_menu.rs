//! Right-click menu: a floating list anchored at the pointer, closed by clicking outside,
//! picking an item or pressing Escape.
use super::icon::{Icon, IconName};
use crate::theme::ActiveTheme as _;
use gpui::prelude::*;
use gpui::{
    App, Bounds, Context, Corner, Div, MouseButton, Pixels, Point, SharedString, WeakEntity,
    Window, anchored, deferred, div, point, px,
};
use std::{cell::Cell, rc::Rc};

pub type Handler = Rc<dyn Fn(&mut Window, &mut App)>;

/// Wraps a view method as a menu handler.
pub fn handler<V: 'static>(
    view: WeakEntity<V>,
    f: impl Fn(&mut V, &mut Window, &mut Context<V>) + 'static,
) -> Handler {
    Rc::new(move |window, cx| {
        view.update(cx, |view, cx| f(view, window, cx)).ok();
    })
}

#[derive(Clone)]
pub enum MenuEntry {
    Item {
        label: SharedString,
        icon: Option<IconName>,
        shortcut: Option<SharedString>,
        disabled: bool,
        danger: bool,
        handler: Handler,
    },
    Separator,
}

impl MenuEntry {
    pub fn item(label: impl Into<SharedString>, handler: Handler) -> Self {
        Self::Item {
            label: label.into(),
            icon: None,
            shortcut: None,
            disabled: false,
            danger: false,
            handler,
        }
    }
    pub fn icon(mut self, name: IconName) -> Self {
        if let Self::Item { icon, .. } = &mut self {
            *icon = Some(name);
        }
        self
    }
    pub fn shortcut(mut self, text: impl Into<SharedString>) -> Self {
        if let Self::Item { shortcut, .. } = &mut self {
            *shortcut = Some(text.into());
        }
        self
    }
    pub fn disabled(mut self, value: bool) -> Self {
        if let Self::Item { disabled, .. } = &mut self {
            *disabled = value;
        }
        self
    }
    pub fn danger(mut self) -> Self {
        if let Self::Item { danger, .. } = &mut self {
            *danger = true;
        }
        self
    }
}

/// A menu waiting to be drawn by its owner view.
#[derive(Clone)]
pub struct OpenMenu {
    position: Point<Pixels>,
    /// The corner of the menu that sits at `position`.
    corner: Corner,
    entries: Vec<MenuEntry>,
}

impl OpenMenu {
    /// At the pointer, going down and to the right.
    pub fn at(position: Point<Pixels>, entries: Vec<MenuEntry>) -> Self {
        Self {
            position,
            corner: Corner::TopLeft,
            entries,
        }
    }

    /// Under a button, lined up with its right edge.
    pub fn below(button: &MenuAnchor, entries: Vec<MenuEntry>) -> Self {
        let position = button.0.get().map_or(Point::default(), |b| {
            b.bottom_right() + point(px(0.), px(4.))
        });
        Self {
            position,
            corner: Corner::TopRight,
            entries,
        }
    }
}

/// Remembers where a button is, for the menu it opens. Keep one per button.
#[derive(Clone, Default)]
pub struct MenuAnchor(Rc<Cell<Option<Bounds<Pixels>>>>);

impl MenuAnchor {
    pub fn wrap(&self, button: impl IntoElement) -> Div {
        let bounds = self.0.clone();
        div()
            .flex_none()
            .on_children_prepainted(move |children, _, _| bounds.set(children.first().copied()))
            .child(button)
    }
}

#[derive(IntoElement)]
pub struct ContextMenu {
    menu_id: SharedString,
    position: Point<Pixels>,
    corner: Corner,
    entries: Vec<MenuEntry>,
    on_dismiss: Handler,
}

impl ContextMenu {
    pub fn new(id: impl Into<SharedString>, menu: OpenMenu, on_dismiss: Handler) -> Self {
        Self {
            menu_id: id.into(),
            position: menu.position,
            corner: menu.corner,
            entries: menu.entries,
            on_dismiss,
        }
    }
}

impl RenderOnce for ContextMenu {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme().clone();
        let dismiss = self.on_dismiss.clone();
        let rows = self
            .entries
            .into_iter()
            .enumerate()
            .map(|(index, entry)| match entry {
                MenuEntry::Separator => div()
                    .my(px(4.))
                    .h(px(1.))
                    .bg(theme.border)
                    .into_any_element(),
                MenuEntry::Item {
                    label,
                    icon,
                    shortcut,
                    disabled,
                    danger,
                    handler,
                } => {
                    let color = if disabled {
                        theme.text_disabled
                    } else if danger {
                        theme.error
                    } else {
                        theme.text
                    };
                    let (hover, press) = (theme.hover, theme.active);
                    let dismiss = self.on_dismiss.clone();
                    div()
                        .id(("menu-item", index))
                        .h(px(26.))
                        .px(px(8.))
                        .flex()
                        .items_center()
                        .gap(px(8.))
                        .rounded(px(5.))
                        .text_color(color)
                        .child(
                            div()
                                .w(px(16.))
                                .flex_none()
                                .children(icon.map(|i| Icon::new(i).color(color))),
                        )
                        .child(div().flex_1().whitespace_nowrap().child(label))
                        .children(shortcut.map(|s| {
                            let s = crate::actions::with_current_keys(&s, cx);
                            div()
                                .pl(px(16.))
                                .text_size(crate::theme::ui_px(12.))
                                .text_color(theme.text_disabled)
                                .child(s)
                        }))
                        .when(!disabled, |this| {
                            this.cursor_pointer()
                                .hover(move |s| s.bg(hover))
                                .active(move |s| s.bg(press))
                                .on_click(move |_, window, cx| {
                                    cx.stop_propagation();
                                    dismiss(window, cx);
                                    handler(window, cx);
                                })
                        })
                        .into_any_element()
                }
            });
        deferred(
            anchored()
                .anchor(self.corner)
                .position(self.position)
                .snap_to_window_with_margin(px(8.))
                .child(crate::motion::enter(
                    div()
                        .id(self.menu_id)
                        .occlude()
                        .min_w(px(200.))
                        .p(px(4.))
                        .flex()
                        .flex_col()
                        .rounded(px(10.))
                        .border_1()
                        .border_color(theme.border)
                        .bg(theme.elevated)
                        .shadow(crate::theme::float_shadow())
                        .text_size(crate::theme::ui_px(13.))
                        .on_mouse_down_out(move |_, window, cx| dismiss(window, cx))
                        .on_mouse_down(MouseButton::Right, |_, _, cx| cx.stop_propagation())
                        .children(rows),
                    "menu-enter",
                    crate::motion::MENU,
                )),
        )
        .with_priority(1)
    }
}
