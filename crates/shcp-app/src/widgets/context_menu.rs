//! Right-click menu: a floating list anchored at the pointer, closed by clicking outside,
//! picking an item or pressing Escape. A row can open a submenu beside it.
use super::icon::{Icon, IconName};
use crate::theme::{ActiveTheme as _, Theme};
use gpui::prelude::*;
use gpui::{
    AnyElement, App, Bounds, Context, Corner, Div, ElementId, Hsla, MouseButton, Pixels, Point,
    SharedString, Stateful, WeakEntity, Window, anchored, deferred, div, point, px,
};
use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    rc::Rc,
};

pub type Handler = Rc<dyn Fn(&mut Window, &mut App)>;
type HoverHandler = Rc<dyn Fn(&mut App)>;

/// How much space a submenu wants beside its menu before it goes to the other side.
const SUBMENU_ROOM: Pixels = px(220.);

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
    /// A row that opens more entries beside the menu. Those cannot be submenus in turn.
    Submenu {
        label: SharedString,
        icon: Option<IconName>,
        entries: Vec<MenuEntry>,
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
    pub fn submenu(label: impl Into<SharedString>, entries: Vec<MenuEntry>) -> Self {
        Self::Submenu {
            label: label.into(),
            icon: None,
            entries,
        }
    }
    pub fn icon(mut self, name: IconName) -> Self {
        if let Self::Item { icon, .. } | Self::Submenu { icon, .. } = &mut self {
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

/// Which submenu is open, and where things are for it. Lives as long as the menu is on screen.
#[derive(Default)]
struct SubmenuState {
    open: Option<usize>,
    /// Where the rows that open a submenu are, by their index in the menu.
    rows: Rc<RefCell<HashMap<usize, Bounds<Pixels>>>>,
    panel: MenuAnchor,
}

/// A row of a menu, with what every kind of row has.
fn base_row(
    id: impl Into<ElementId>,
    label: SharedString,
    icon: Option<IconName>,
    color: Hsla,
) -> Stateful<Div> {
    div()
        .id(id)
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
}

/// A separator or an item, in a menu or a submenu. `on_hover` runs when the pointer gets there.
fn entry_row(
    name: &'static str,
    index: usize,
    entry: MenuEntry,
    dismiss: Handler,
    on_hover: Option<HoverHandler>,
    cx: &App,
) -> AnyElement {
    let theme = cx.theme();
    match entry {
        // A submenu's row needs the menu around it: only the menu draws one, and only one deep.
        MenuEntry::Submenu { .. } => div().into_any_element(),
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
            base_row((name, index), label, icon, color)
                .children(shortcut.map(|s| {
                    let s = crate::actions::with_current_keys(&s, cx);
                    div()
                        .pl(px(16.))
                        .text_size(crate::theme::ui_px(12.))
                        .text_color(theme.text_disabled)
                        .child(s)
                }))
                .when_some(on_hover, |this, on_hover| {
                    this.on_hover(move |hovered, _, cx| {
                        if *hovered {
                            on_hover(cx);
                        }
                    })
                })
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
    }
}

/// What a menu floats on.
fn panel(id: impl Into<ElementId>, theme: &Theme) -> Stateful<Div> {
    div()
        .id(id)
        .occlude()
        .p(px(4.))
        .flex()
        .flex_col()
        .rounded(px(10.))
        .border_1()
        .border_color(theme.border)
        .bg(theme.elevated)
        .shadow(crate::theme::float_shadow())
        .text_size(crate::theme::ui_px(13.))
        .on_mouse_down(MouseButton::Right, |_, _, cx| cx.stop_propagation())
}

impl RenderOnce for ContextMenu {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme().clone();
        // Per opening: another menu at another spot starts with its submenus closed.
        let key = SharedString::from(format!("{}{:?}", self.menu_id, self.position));
        let state =
            window.use_keyed_state(ElementId::Name(key), cx, |_, _| SubmenuState::default());
        let (open, row_bounds, panel_anchor) = {
            let state = state.read(cx);
            (state.open, state.rows.clone(), state.panel.clone())
        };
        let open_entries = open.and_then(|index| {
            let row = row_bounds.borrow().get(&index).copied()?;
            match self.entries.get(index)? {
                MenuEntry::Submenu { entries, .. } => Some((entries.clone(), row)),
                _ => None,
            }
        });
        let submenu = open_entries.map(|(entries, row)| {
            // The rows line up, past the padding and the border of the menu.
            let (across, up) = (px(8.), px(5.));
            let fits = row.right() + across + SUBMENU_ROOM <= window.viewport_size().width;
            let (position, corner) = if fits {
                (row.top_right() + point(across, -up), Corner::TopLeft)
            } else {
                (row.origin + point(-across, -up), Corner::TopRight)
            };
            let rows: Vec<AnyElement> = entries
                .into_iter()
                .enumerate()
                .map(|(index, entry)| {
                    let dismiss = self.on_dismiss.clone();
                    entry_row("submenu-item", index, entry, dismiss, None, cx)
                })
                .collect();
            anchored()
                .anchor(corner)
                .position(position)
                .snap_to_window_with_margin(px(8.))
                .child(panel_anchor.wrap(crate::motion::enter(
                    panel("submenu", &theme).min_w(px(160.)).children(rows),
                    "submenu-enter",
                    crate::motion::MENU,
                )))
        });
        let rows: Vec<AnyElement> = self
            .entries
            .into_iter()
            .enumerate()
            .map(|(index, entry)| match entry {
                MenuEntry::Submenu { label, icon, .. } => {
                    let hover = theme.hover;
                    let (on_hover, on_click) = (state.clone(), state.clone());
                    let bounds = row_bounds.clone();
                    let row = base_row(("menu-item", index), label, icon, theme.text)
                        .child(
                            Icon::new(IconName::ChevronRight)
                                .size(px(12.))
                                .color(theme.text_disabled),
                        )
                        .cursor_pointer()
                        .when(open == Some(index), |this| this.bg(hover))
                        .hover(move |s| s.bg(hover))
                        .on_hover(move |hovered, _, cx| {
                            if *hovered {
                                on_hover.update(cx, |state, cx| {
                                    state.open = Some(index);
                                    cx.notify();
                                });
                            }
                        })
                        .on_click(move |_, _, cx| {
                            cx.stop_propagation();
                            on_click.update(cx, |state, cx| {
                                state.open = Some(index);
                                cx.notify();
                            });
                        });
                    div()
                        .flex_none()
                        .on_children_prepainted(move |children, _, _| {
                            if let Some(row) = children.first() {
                                bounds.borrow_mut().insert(index, *row);
                            }
                        })
                        .child(row)
                        .into_any_element()
                }
                entry => {
                    let state = state.clone();
                    let close: HoverHandler = Rc::new(move |cx| {
                        state.update(cx, |state, cx| {
                            if state.open.take().is_some() {
                                cx.notify();
                            }
                        });
                    });
                    let dismiss = self.on_dismiss.clone();
                    entry_row("menu-item", index, entry, dismiss, Some(close), cx)
                }
            })
            .collect();
        let dismiss = self.on_dismiss;
        let outside = state.clone();
        deferred(
            anchored()
                .anchor(self.corner)
                .position(self.position)
                .snap_to_window_with_margin(px(8.))
                .child(crate::motion::enter(
                    panel(self.menu_id, &theme)
                        .min_w(px(200.))
                        .on_mouse_down_out(move |event, window, cx| {
                            // The submenu sits outside this panel, and is part of the menu.
                            let state = outside.read(cx);
                            let in_submenu = state.open.is_some()
                                && state
                                    .panel
                                    .0
                                    .get()
                                    .is_some_and(|panel| panel.contains(&event.position));
                            if !in_submenu {
                                dismiss(window, cx);
                            }
                        })
                        .children(rows)
                        .children(submenu),
                    "menu-enter",
                    crate::motion::MENU,
                )),
        )
        .with_priority(1)
    }
}
