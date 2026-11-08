//! The content of a terminal tab: one terminal, or several split side by side and on top of
//! each other. All of them belong to the tab's host.
use crate::actions::{
    FocusPaneDown, FocusPaneLeft, FocusPaneRight, FocusPaneUp, SplitDown, SplitRight,
};
use crate::login_prompt::present;
use crate::session_overlay::{self, Overlay, Surface};
use crate::state::AppState;
use crate::terminal_view::TerminalView;
use crate::theme::ActiveTheme as _;
use crate::widgets::button::{Button, ButtonStyle};
use crate::widgets::context_menu::{ContextMenu, MenuEntry, OpenMenu, handler};
use crate::widgets::icon::IconName;
use gpui::prelude::*;
use gpui::{
    AnyElement, App, Bounds, ClipboardItem, Context, CursorStyle, Entity, EntityId, EventEmitter,
    FocusHandle, Focusable, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels,
    Point, SharedString, Subscription, Window, canvas, div, px, relative,
};
use shcp_core::{HostId, SavedPane, SessionState, SplitAxis, TerminalId};
use std::{
    cell::Cell,
    collections::{HashMap, HashSet},
    rc::Rc,
};

const MIN_RATIO: f32 = 0.1;

enum Node {
    Leaf(Entity<TerminalView>),
    Split {
        axis: SplitAxis,
        /// Share of the first child.
        ratio: f32,
        /// Where the split was painted, to turn a drag into a ratio.
        bounds: Rc<Cell<Option<Bounds<Pixels>>>>,
        first: Box<Node>,
        second: Box<Node>,
    },
}

/// Which child to take at each split, from the top.
type Path = Vec<bool>;

impl Node {
    fn split(axis: SplitAxis, ratio: f32, first: Node, second: Node) -> Self {
        Self::Split {
            axis,
            ratio,
            bounds: Rc::new(Cell::new(None)),
            first: Box::new(first),
            second: Box::new(second),
        }
    }

    fn leaves(&self, out: &mut Vec<Entity<TerminalView>>) {
        match self {
            Self::Leaf(view) => out.push(view.clone()),
            Self::Split { first, second, .. } => {
                first.leaves(out);
                second.leaves(out);
            }
        }
    }

    /// Puts `new` next to the terminal `target`.
    fn split_at(self, target: EntityId, axis: SplitAxis, new: &Entity<TerminalView>) -> Self {
        match self {
            Self::Leaf(view) if view.entity_id() == target => {
                Self::split(axis, 0.5, Self::Leaf(view), Self::Leaf(new.clone()))
            }
            Self::Split {
                axis: own,
                ratio,
                bounds,
                first,
                second,
            } => Self::Split {
                axis: own,
                ratio,
                bounds,
                first: Box::new(first.split_at(target, axis, new)),
                second: Box::new(second.split_at(target, axis, new)),
            },
            leaf => leaf,
        }
    }

    /// Without the terminal `target`; its neighbour takes the room.
    fn without(self, target: EntityId) -> Option<Self> {
        match self {
            Self::Leaf(view) if view.entity_id() == target => None,
            Self::Split {
                axis,
                ratio,
                bounds,
                first,
                second,
            } => match (first.without(target), second.without(target)) {
                (Some(first), Some(second)) => Some(Self::Split {
                    axis,
                    ratio,
                    bounds,
                    first: Box::new(first),
                    second: Box::new(second),
                }),
                (Some(only), None) | (None, Some(only)) => Some(only),
                (None, None) => None,
            },
            leaf => Some(leaf),
        }
    }

    /// Exchanges the places of two terminals.
    fn swap(&mut self, a: &Entity<TerminalView>, b: &Entity<TerminalView>) {
        match self {
            Self::Leaf(view) if view == a => *view = b.clone(),
            Self::Leaf(view) if view == b => *view = a.clone(),
            Self::Leaf(_) => {}
            Self::Split { first, second, .. } => {
                first.swap(a, b);
                second.swap(a, b);
            }
        }
    }

    fn at_mut(&mut self, path: &[bool]) -> Option<&mut Self> {
        match (path.split_first(), self) {
            (None, node) => Some(node),
            (Some((second_child, rest)), Self::Split { first, second, .. }) => {
                if *second_child { second } else { first }.at_mut(rest)
            }
            _ => None,
        }
    }

    fn saved(&self, cx: &App) -> SavedPane {
        match self {
            Self::Leaf(view) => SavedPane::Terminal {
                key: view.read(cx).key(),
            },
            Self::Split {
                axis,
                ratio,
                first,
                second,
                ..
            } => SavedPane::Split {
                axis: *axis,
                ratio: *ratio,
                first: Box::new(first.saved(cx)),
                second: Box::new(second.saved(cx)),
            },
        }
    }
}

pub enum PanesEvent {
    /// Show the files next to the terminals.
    Browse,
}

impl EventEmitter<PanesEvent> for TerminalPanes {}

pub struct TerminalPanes {
    state: Entity<AppState>,
    host: HostId,
    /// Only `None` while the tree is being rebuilt.
    root: Option<Node>,
    /// The terminal that shortcuts act on.
    active: Entity<TerminalView>,
    dragging: Option<Path>,
    menu: Option<OpenMenu>,
    /// The shared status becomes a strip after this tab has shown a shell.
    had_session: bool,
    _state_subscription: Subscription,
}

impl Focusable for TerminalPanes {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.active.focus_handle(cx)
    }
}

impl TerminalPanes {
    /// `known`: the terminals of `layout` are from the last run, see `TerminalView::new`.
    pub fn new(
        state: Entity<AppState>,
        host: HostId,
        layout: &SavedPane,
        known: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let root = Self::build(
            &state,
            &host,
            layout,
            known,
            &mut HashMap::new(),
            window,
            cx,
        );
        let mut leaves = Vec::new();
        root.leaves(&mut leaves);
        let active = leaves.remove(0);
        let subscription = cx.observe(&state, |_, _, cx| cx.notify());
        Self {
            state,
            host,
            root: Some(root),
            active,
            dragging: None,
            menu: None,
            had_session: false,
            _state_subscription: subscription,
        }
    }

    /// Makes the tree of `layout`, taking the terminals it can out of `existing`.
    fn build(
        state: &Entity<AppState>,
        host: &HostId,
        layout: &SavedPane,
        known: bool,
        existing: &mut HashMap<String, Entity<TerminalView>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Node {
        match layout {
            SavedPane::Terminal { key } => {
                match key.as_ref().and_then(|key| existing.remove(key)) {
                    Some(view) => Node::Leaf(view),
                    None => Node::Leaf(Self::terminal(state, host, key.clone(), known, window, cx)),
                }
            }
            SavedPane::Split {
                axis,
                ratio,
                first,
                second,
            } => {
                let first = Self::build(state, host, first, known, existing, window, cx);
                let second = Self::build(state, host, second, known, existing, window, cx);
                let ratio = if ratio.is_finite() {
                    ratio.clamp(MIN_RATIO, 1. - MIN_RATIO)
                } else {
                    0.5
                };
                Node::split(*axis, ratio, first, second)
            }
        }
    }

    fn terminal(
        state: &Entity<AppState>,
        host: &HostId,
        key: Option<String>,
        known: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<TerminalView> {
        let (state, host) = (state.clone(), host.clone());
        let view = cx.new(|cx| TerminalView::new(state, host, key, known, window, cx));
        // The tab row and the palette show the active terminal's title and session.
        cx.observe(&view, |_, _, cx| cx.notify()).detach();
        view
    }

    fn leaves(&self) -> Vec<Entity<TerminalView>> {
        let mut leaves = Vec::new();
        if let Some(root) = &self.root {
            root.leaves(&mut leaves);
        }
        leaves
    }

    pub fn active(&self) -> &Entity<TerminalView> {
        &self.active
    }

    pub fn title(&self, cx: &App) -> Option<String> {
        self.active.read(cx).title()
    }

    /// With the connection up, none of them has a shell.
    pub fn shells_gone(&self, cx: &App) -> bool {
        self.leaves()
            .iter()
            .all(|terminal| terminal.read(cx).shell_gone(cx))
    }

    pub fn saved(&self, cx: &App) -> SavedPane {
        self.root
            .as_ref()
            .map_or(SavedPane::Terminal { key: None }, |root| root.saved(cx))
    }

    /// Releases every backend terminal; call when the tab is closed.
    pub fn close(&self, cx: &App) {
        for view in self.leaves() {
            view.read(cx).close(cx);
        }
    }

    /// Lets go of every backend terminal, leaving the shells to whoever else has them.
    pub fn detach(&self, cx: &App) {
        for view in self.leaves() {
            view.read(cx).detach(cx);
        }
    }

    /// The names of its terminals on the server.
    pub fn keys(&self, cx: &App) -> Vec<String> {
        self.saved(cx).keys()
    }

    /// The names of the terminals that should be on a server in use but are not in `live`.
    pub fn missing_keys(&self, live: &HashSet<String>, cx: &App) -> Vec<String> {
        self.leaves()
            .iter()
            .map(|view| view.read(cx))
            .filter(|view| view.expects_shell())
            .filter_map(|view| view.key())
            .filter(|key| !live.contains(key))
            .collect()
    }

    pub fn has_terminal(&self, id: TerminalId, cx: &App) -> bool {
        self.leaves()
            .iter()
            .any(|view| view.read(cx).terminal_id() == id)
    }

    /// A terminal that can still reach the server.
    pub fn live_terminal(&self, cx: &App) -> Option<TerminalId> {
        self.leaves()
            .iter()
            .map(|view| view.read(cx))
            .find(|view| view.is_alive())
            .map(TerminalView::terminal_id)
    }

    /// A divider is under the pointer, so the layout is on its way to something else.
    pub fn is_dragging(&self) -> bool {
        self.dragging.is_some()
    }

    /// Takes the layout another PC gave its terminals. The ones in `gone` were closed there; any
    /// other that the layout does not have is new here, so it stays, next to the rest.
    pub fn apply_layout(
        &mut self,
        layout: &SavedPane,
        gone: &HashSet<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.saved(cx) == *layout {
            return;
        }
        let focused = self.active.read(cx).contains_focus(window, cx);
        let mut existing = HashMap::new();
        let mut unnamed = Vec::new();
        for view in self.leaves() {
            match view.read(cx).key() {
                Some(key) => {
                    existing.insert(key, view);
                }
                None => unnamed.push(view),
            }
        }
        let (state, host) = (self.state.clone(), self.host.clone());
        let mut root = Self::build(&state, &host, layout, false, &mut existing, window, cx);
        for (key, view) in existing {
            if gone.contains(&key) {
                view.read(cx).detach(cx);
            } else {
                root = Node::split(SplitAxis::Row, 0.5, root, Node::Leaf(view));
            }
        }
        for view in unnamed {
            root = Node::split(SplitAxis::Row, 0.5, root, Node::Leaf(view));
        }
        let mut leaves = Vec::new();
        root.leaves(&mut leaves);
        if !leaves.contains(&self.active) {
            self.active = leaves[0].clone();
        }
        self.root = Some(root);
        if focused {
            window.focus(&self.active.focus_handle(cx));
        }
        cx.notify();
    }

    /// Lets go of the terminals with these names. `false`: the tab has none left.
    pub fn drop_keys(
        &mut self,
        keys: &HashSet<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        for view in self.leaves() {
            let closed = view.read(cx).key().is_some_and(|key| keys.contains(&key));
            if closed && !self.take_pane(view, false, window, cx) {
                return false;
            }
        }
        true
    }

    /// Lets go of a terminal closed elsewhere. `false`: it was the last one, the tab goes instead.
    pub fn remove_terminal(
        &mut self,
        id: TerminalId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let target = self
            .leaves()
            .into_iter()
            .find(|view| view.read(cx).terminal_id() == id);
        target.is_none_or(|target| self.take_pane(target, false, window, cx))
    }

    pub fn split(&mut self, axis: SplitAxis, window: &mut Window, cx: &mut Context<Self>) {
        let new = Self::terminal(&self.state, &self.host, None, false, window, cx);
        let target = self.active.entity_id();
        self.root = self
            .root
            .take()
            .map(|root| root.split_at(target, axis, &new));
        self.active = new;
        window.focus(&self.active.focus_handle(cx));
        cx.notify();
    }

    /// Closes the active terminal. `false`: it was the last one, the tab has to close instead.
    pub fn close_active(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        self.close_pane(self.active.clone(), window, cx)
    }

    fn close_pane(
        &mut self,
        target: Entity<TerminalView>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        self.take_pane(target, true, window, cx)
    }

    /// Takes a terminal out of the tree, ending what runs in it if `kill`.
    fn take_pane(
        &mut self,
        target: Entity<TerminalView>,
        kill: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let leaves = self.leaves();
        if leaves.len() < 2 {
            return false;
        }
        let index = leaves.iter().position(|view| *view == target).unwrap_or(0);
        // Closed from elsewhere, it must not pull the keyboard away from what it is on.
        let refocus = kill || target.read(cx).contains_focus(window, cx);
        if kill {
            target.read(cx).close(cx);
        } else {
            target.read(cx).detach(cx);
        }
        self.root = self
            .root
            .take()
            .and_then(|root| root.without(target.entity_id()));
        if self.active == target {
            self.active = leaves[if index == 0 { 1 } else { index - 1 }].clone();
        }
        if refocus {
            window.focus(&self.active.focus_handle(cx));
        }
        cx.notify();
        true
    }

    fn set_active(&mut self, view: Entity<TerminalView>, cx: &mut Context<Self>) {
        if self.active != view {
            self.active = view;
            cx.notify();
        }
    }

    /// The nearest terminal in a direction; `dx` and `dy` say which.
    fn neighbour(
        &self,
        from: &Entity<TerminalView>,
        dx: f32,
        dy: f32,
        cx: &App,
    ) -> Option<Entity<TerminalView>> {
        let center = |b: Bounds<Pixels>| {
            (
                f32::from(b.origin.x) + f32::from(b.size.width) / 2.,
                f32::from(b.origin.y) + f32::from(b.size.height) / 2.,
            )
        };
        let (x, y) = center(from.read(cx).area()?);
        self.leaves()
            .into_iter()
            .filter(|view| view != from)
            .filter_map(|view| {
                let (cx2, cy2) = center(view.read(cx).area()?);
                let along = (cx2 - x) * dx + (cy2 - y) * dy;
                let across = (cx2 - x) * dy + (cy2 - y) * dx;
                (along > 0.).then_some((along + across.abs(), view))
            })
            .min_by(|a, b| a.0.total_cmp(&b.0))
            .map(|(_, view)| view)
    }

    fn focus_towards(&mut self, dx: f32, dy: f32, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(view) = self.neighbour(&self.active.clone(), dx, dy, cx) {
            window.focus(&view.focus_handle(cx));
            self.set_active(view, cx);
        }
    }

    fn swap_panes(
        &mut self,
        a: &Entity<TerminalView>,
        b: &Entity<TerminalView>,
        cx: &mut Context<Self>,
    ) {
        if let Some(root) = &mut self.root {
            root.swap(a, b);
        }
        cx.notify();
    }

    /// The menu of a terminal. Plain right click stays paste, as in other Windows terminals.
    fn open_menu(
        &mut self,
        view: Entity<TerminalView>,
        position: Point<Pixels>,
        cx: &mut Context<Self>,
    ) {
        let this = cx.entity().downgrade();
        let leaves = self.leaves();
        let several = leaves.len() > 1;
        let selected = self.active.clone();
        // The terminal to exchange with: the one selected, or with two the only other one.
        let swap = if view != selected {
            Some((selected, "Swap with selected terminal"))
        } else if leaves.len() == 2 {
            leaves
                .into_iter()
                .find(|other| *other != view)
                .map(|other| (other, "Swap with the other terminal"))
        } else {
            None
        };
        let (copied, pasted) = (view.clone(), view.clone());
        let (split_right, split_down) = (view.clone(), view.clone());
        let mut entries = Vec::new();
        if let Some((other, label)) = swap {
            let view = view.clone();
            entries.push(
                MenuEntry::item(
                    label,
                    handler(this.clone(), move |panes, _, cx| {
                        panes.swap_panes(&view, &other, cx)
                    }),
                )
                .icon(IconName::Swap),
            );
            entries.push(MenuEntry::Separator);
        }
        entries.extend([
            MenuEntry::item(
                "Copy",
                handler(this.clone(), move |_, _, cx| {
                    if let Some(text) = copied.read(cx).selected_text() {
                        cx.write_to_clipboard(ClipboardItem::new_string(text));
                    }
                }),
            )
            .icon(IconName::Copy)
            .shortcut("Ctrl+Shift+C")
            .disabled(view.read(cx).selected_text().is_none()),
            MenuEntry::item(
                "Paste",
                handler(this.clone(), move |_, _, cx| {
                    pasted.update(cx, |view, cx| {
                        view.paste_clipboard(cx);
                    });
                }),
            )
            .shortcut("Ctrl+Shift+V"),
            MenuEntry::item(
                "Open folder",
                handler(this.clone(), move |_, _, cx: &mut Context<Self>| {
                    cx.emit(PanesEvent::Browse)
                }),
            )
            .icon(IconName::Folder),
            MenuEntry::Separator,
            MenuEntry::item(
                "Split right",
                handler(this.clone(), move |panes, window, cx| {
                    panes.active = split_right.clone();
                    panes.split(SplitAxis::Row, window, cx)
                }),
            )
            .icon(IconName::SplitRight)
            .shortcut("Ctrl+Shift+D"),
            MenuEntry::item(
                "Split down",
                handler(this.clone(), move |panes, window, cx| {
                    panes.active = split_down.clone();
                    panes.split(SplitAxis::Column, window, cx)
                }),
            )
            .icon(IconName::SplitDown)
            .shortcut("Ctrl+Shift+S"),
        ]);
        if several {
            entries.push(MenuEntry::Separator);
            let target = view.clone();
            entries.push(
                MenuEntry::item(
                    "Close terminal",
                    handler(this, move |panes, window, cx| {
                        panes.close_pane(target.clone(), window, cx);
                    }),
                )
                .icon(IconName::Close)
                .shortcut("Ctrl+Shift+W"),
            );
        }
        self.menu = Some(OpenMenu::at(position, entries));
        cx.notify();
    }

    fn drag_to(&mut self, event: &MouseMoveEvent, cx: &mut Context<Self>) {
        if event.pressed_button != Some(MouseButton::Left) {
            self.dragging = None;
            cx.notify();
            return;
        }
        let Some(path) = self.dragging.clone() else {
            return;
        };
        let Some(Node::Split {
            axis,
            ratio,
            bounds,
            ..
        }) = self.root.as_mut().and_then(|root| root.at_mut(&path))
        else {
            return;
        };
        let Some(area) = bounds.get() else {
            return;
        };
        let value = match axis {
            SplitAxis::Row => {
                f32::from(event.position.x - area.origin.x) / f32::from(area.size.width)
            }
            SplitAxis::Column => {
                f32::from(event.position.y - area.origin.y) / f32::from(area.size.height)
            }
        };
        if value.is_finite() {
            *ratio = value.clamp(MIN_RATIO, 1. - MIN_RATIO);
            cx.notify();
        }
    }

    fn render_node(
        &self,
        node: &Node,
        path: Path,
        several: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        match node {
            Node::Leaf(view) => {
                let id = view.entity_id().as_u64() as usize;
                let group = SharedString::from(format!("pane-{id}"));
                let target = view.clone();
                let closing = view.clone();
                let can_close = several && !view.read(cx).search_is_open();
                // The cursor tells the terminal in use: outlined in the others.
                let pane = div().relative().size_full().child(view.clone());
                div()
                    .id(("pane", id))
                    .group(group.clone())
                    .size_full()
                    .relative()
                    // Before the terminal, which may keep the click for the program in it.
                    .capture_any_mouse_down(cx.listener(
                        move |this, event: &MouseDownEvent, _, cx| {
                            // The selection stays where it is: the menu offers to swap with it.
                            if event.button == MouseButton::Right && event.modifiers.control {
                                this.open_menu(target.clone(), event.position, cx);
                                cx.stop_propagation();
                            } else {
                                this.set_active(target.clone(), cx);
                            }
                        },
                    ))
                    .child(pane)
                    .when(can_close, |this| {
                        this.child(
                            div()
                                .id(("pane-close-area", id))
                                // Not occluding: the pointer must stay "on the pane" here, or
                                // the button would hide itself. So it keeps its own clicks.
                                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                                .on_mouse_down(MouseButton::Right, |_, _, cx| cx.stop_propagation())
                                .on_mouse_down(MouseButton::Middle, |_, _, cx| {
                                    cx.stop_propagation()
                                })
                                .absolute()
                                .top(px(4.))
                                .right(px(4.))
                                .invisible()
                                .group_hover(group, |style| style.visible())
                                .child(
                                    Button::new(("pane-close", id))
                                        .icon(IconName::Close)
                                        .tooltip("Close terminal (Ctrl+Shift+W)")
                                        .style(ButtonStyle::Ghost)
                                        .compact()
                                        .on_click(cx.listener(move |this, _, window, cx| {
                                            this.close_pane(closing.clone(), window, cx);
                                        })),
                                ),
                        )
                    })
                    .into_any_element()
            }
            Node::Split {
                axis,
                ratio,
                bounds,
                first,
                second,
            } => {
                let row = *axis == SplitAxis::Row;
                let (mut first_path, mut second_path) = (path.clone(), path.clone());
                first_path.push(false);
                second_path.push(true);
                let first = self.render_node(first, first_path, several, cx);
                let second = self.render_node(second, second_path, several, cx);
                let cell = bounds.clone();
                let line = cx.theme().border;
                div()
                    .size_full()
                    .relative()
                    .flex()
                    .when(!row, |this| this.flex_col())
                    .child(
                        canvas(move |area, _, _| cell.set(Some(area)), |_, _, _, _| {})
                            .absolute()
                            .size_full(),
                    )
                    .child(
                        div()
                            .flex_none()
                            .overflow_hidden()
                            .when(row, |this| this.w(relative(*ratio)).h_full())
                            .when(!row, |this| this.h(relative(*ratio)).w_full())
                            .child(first),
                    )
                    .child(
                        // A hairline, with more room than that to grab it by.
                        div()
                            .relative()
                            .flex_none()
                            .bg(line)
                            .when(row, |this| this.w(px(1.)).h_full())
                            .when(!row, |this| this.h(px(1.)).w_full())
                            .child(
                                div()
                                    .id(gpui::SharedString::from(format!("divider-{path:?}")))
                                    .absolute()
                                    .occlude()
                                    .when(row, |this| {
                                        this.top_0()
                                            .bottom_0()
                                            .left(px(-3.))
                                            .w(px(7.))
                                            .cursor(CursorStyle::ResizeLeftRight)
                                    })
                                    .when(!row, |this| {
                                        this.left_0()
                                            .right_0()
                                            .top(px(-3.))
                                            .h(px(7.))
                                            .cursor(CursorStyle::ResizeUpDown)
                                    })
                                    .on_mouse_down(
                                        MouseButton::Left,
                                        cx.listener(move |this, _, _, cx| {
                                            this.dragging = Some(path.clone());
                                            cx.stop_propagation();
                                            cx.notify();
                                        }),
                                    ),
                            ),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .min_h_0()
                            .overflow_hidden()
                            .child(second),
                    )
                    .into_any_element()
            }
        }
    }
}

impl Render for TerminalPanes {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let leaves = self.leaves();
        // Focus can also arrive from outside, like a modal giving it back.
        if let Some(focused) = leaves
            .iter()
            .find(|view| view.read(cx).contains_focus(window, cx))
        {
            self.active = focused.clone();
        }
        let theme = cx.theme().clone();
        let focus = self.active.focus_handle(cx);
        let state = self.state.read(cx);
        let session = state.host_session(&self.host);
        self.had_session |= matches!(
            session.map(|session| &session.state),
            Some(SessionState::Connected)
        ) && leaves.iter().any(|view| view.read(cx).shell_started());
        let heading = state.host(&self.host).map(session_overlay::heading);
        let steps = session_overlay::steps_of(session);
        let shell_steps = session_overlay::shell_steps(session);
        let prompt = state.login_prompt(&self.host, cx);
        let overlay = session_overlay::with_reconnect(
            session_overlay::overlay_for(session),
            state.reconnect_attempt(&self.host),
        );
        let overlay = match prompt {
            Some(prompt) => {
                let own = present(&prompt, cx.entity_id(), &focus, window, cx);
                Some(Overlay::Login { steps, prompt, own })
            }
            None => overlay,
        };
        let overlay = overlay.or_else(|| {
            (!self.had_session && leaves.iter().any(|view| view.read(cx).starting_shell()))
                .then_some(Overlay::StartingShell { steps: shell_steps })
        });
        let overlay = overlay.map(|overlay| {
            session_overlay::render(
                overlay,
                Surface {
                    heading,
                    docked: self.had_session,
                    background: theme.terminal_bg,
                    focus,
                    extra_steps: 1,
                    owner: cx.entity_id(),
                },
                &theme,
                window,
                cx,
                cx.listener(|this, _, window, cx| {
                    for view in this.leaves() {
                        view.update(cx, |view, cx| view.retry(cx));
                    }
                    window.focus(&this.active.focus_handle(cx));
                }),
            )
        });
        let several = leaves.len() > 1;
        let root = self.root.take();
        let content = root
            .as_ref()
            .map(|root| self.render_node(root, Vec::new(), several, cx));
        self.root = root;
        let dragging = self.dragging.is_some();
        div()
            .size_full()
            .relative()
            .capture_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, _, cx| {
                if this.menu.is_some() && event.keystroke.key == "escape" {
                    this.menu = None;
                    cx.stop_propagation();
                    cx.notify();
                }
            }))
            .on_action(cx.listener(|this, _: &SplitRight, window, cx| {
                this.split(SplitAxis::Row, window, cx)
            }))
            .on_action(cx.listener(|this, _: &SplitDown, window, cx| {
                this.split(SplitAxis::Column, window, cx)
            }))
            .on_action(cx.listener(|this, _: &FocusPaneLeft, window, cx| {
                this.focus_towards(-1., 0., window, cx)
            }))
            .on_action(cx.listener(|this, _: &FocusPaneRight, window, cx| {
                this.focus_towards(1., 0., window, cx)
            }))
            .on_action(cx.listener(|this, _: &FocusPaneUp, window, cx| {
                this.focus_towards(0., -1., window, cx)
            }))
            .on_action(cx.listener(|this, _: &FocusPaneDown, window, cx| {
                this.focus_towards(0., 1., window, cx)
            }))
            .when(dragging, |this| {
                this.on_mouse_move(
                    cx.listener(|this, event: &MouseMoveEvent, _, cx| this.drag_to(event, cx)),
                )
                .on_mouse_up(
                    MouseButton::Left,
                    cx.listener(|this, _: &MouseUpEvent, _, cx| {
                        this.dragging = None;
                        cx.notify();
                    }),
                )
                // The divider occludes this container, so its releases count as "out" too.
                .on_mouse_up_out(
                    MouseButton::Left,
                    cx.listener(|this, _: &MouseUpEvent, _, cx| {
                        this.dragging = None;
                        cx.notify();
                    }),
                )
            })
            .children(content)
            .children(overlay)
            .children(self.menu.clone().map(|menu| {
                let this = cx.entity().downgrade();
                ContextMenu::new(
                    "pane-menu",
                    menu,
                    Rc::new(move |_, cx| {
                        this.update(cx, |this, cx| {
                            this.menu = None;
                            cx.notify();
                        })
                        .ok();
                    }),
                )
            }))
    }
}
