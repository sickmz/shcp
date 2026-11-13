//! The content of a terminal tab: one terminal, or several split side by side and on top of
//! each other. All of them belong to the tab's host.
use crate::actions::{
    FocusPaneDown, FocusPaneLeft, FocusPaneRight, FocusPaneUp, SplitDown, SplitRight,
};
use crate::motion::HoverFade;
use crate::state::AppState;
use crate::terminal_view::TerminalView;
use crate::theme::{ActiveTheme as _, RADIUS_SMALL, Theme};
use crate::widgets::button::{Button, ButtonStyle};
use crate::widgets::context_menu::{ContextMenu, MenuEntry, OpenMenu, handler};
use crate::widgets::icon::IconName;
use crate::widgets::tile::shell_tile;
use crate::widgets::tooltip::tooltip;
use gpui::prelude::*;
use gpui::{
    AnyElement, App, Bounds, ClickEvent, ClipboardItem, Context, CursorStyle, Entity, EntityId,
    EventEmitter, FocusHandle, Focusable, MouseButton, MouseDownEvent, MouseMoveEvent,
    MouseUpEvent, Pixels, Point, Subscription, Window, canvas, div, px, relative,
};
use shcp_core::{HostConfig, HostId, SavedPane, SplitAxis, TerminalId};
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

pub(crate) enum PanesEvent {
    /// Show the files next to the terminals.
    Browse,
    /// Show the files, or hide them if they show.
    ToggleFiles,
    /// The last terminal was closed from its card: the tab goes.
    CloseTab,
}

impl EventEmitter<PanesEvent> for TerminalPanes {}

pub(crate) struct TerminalPanes {
    state: Entity<AppState>,
    host: HostId,
    /// Only `None` while the tree is being rebuilt.
    root: Option<Node>,
    /// The terminal that shortcuts act on.
    active: Entity<TerminalView>,
    dragging: Option<Path>,
    menu: Option<OpenMenu>,
    /// Initial shells have finished starting; later splits use their own local status.
    had_session: bool,
    /// The tab shows its files beside the terminals. Set by the workspace, for the cards' button.
    pub files_shown: bool,
    /// The tools of each card, coming and going with the pointer.
    fades: HashMap<EntityId, HoverFade>,
    _state_subscription: Subscription,
}

impl Focusable for TerminalPanes {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.active.focus_handle(cx)
    }
}

impl TerminalPanes {
    /// `known`: the terminals of `layout` are from the last run, see `TerminalView::new`.
    pub(crate) fn new(
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
            files_shown: false,
            fades: HashMap::new(),
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

    pub(crate) fn had_session(&self) -> bool {
        self.had_session
    }

    pub(crate) fn initial_shell_pending(&self, cx: &App) -> bool {
        !self.had_session
            && self
                .leaves()
                .iter()
                .any(|view| view.read(cx).starting_shell())
    }

    pub(crate) fn retry(&mut self, cx: &mut Context<Self>) {
        for view in self.leaves() {
            view.update(cx, TerminalView::retry);
        }
    }

    fn leaves(&self) -> Vec<Entity<TerminalView>> {
        let mut leaves = Vec::new();
        if let Some(root) = &self.root {
            root.leaves(&mut leaves);
        }
        leaves
    }

    pub(crate) fn active(&self) -> &Entity<TerminalView> {
        &self.active
    }

    /// More than one terminal side by side.
    pub(crate) fn is_split(&self) -> bool {
        matches!(self.root, Some(Node::Split { .. }))
    }

    pub(crate) fn title(&self, cx: &App) -> Option<String> {
        self.active.read(cx).title()
    }

    /// With the connection up, none of them has a shell.
    pub(crate) fn shells_gone(&self, cx: &App) -> bool {
        self.leaves()
            .iter()
            .all(|terminal| terminal.read(cx).shell_gone(cx))
    }

    pub(crate) fn saved(&self, cx: &App) -> SavedPane {
        self.root
            .as_ref()
            .map_or(SavedPane::Terminal { key: None }, |root| root.saved(cx))
    }

    /// Releases every backend terminal; call when the tab is closed.
    pub(crate) fn close(&self, cx: &App) {
        for view in self.leaves() {
            view.read(cx).close(cx);
        }
    }

    /// Lets go of every backend terminal, leaving the shells to whoever else has them.
    pub(crate) fn detach(&self, cx: &App) {
        for view in self.leaves() {
            view.read(cx).detach(cx);
        }
    }

    /// The names of its terminals on the server.
    pub(crate) fn keys(&self, cx: &App) -> Vec<String> {
        self.saved(cx).keys()
    }

    /// The names of the terminals that should be on a server in use but are not in `live`.
    pub(crate) fn missing_keys(&self, live: &HashSet<String>, cx: &App) -> Vec<String> {
        self.leaves()
            .iter()
            .map(|view| view.read(cx))
            .filter(|view| view.expects_shell())
            .filter_map(TerminalView::key)
            .filter(|key| !live.contains(key))
            .collect()
    }

    pub(crate) fn has_terminal(&self, id: TerminalId, cx: &App) -> bool {
        self.leaves()
            .iter()
            .any(|view| view.read(cx).terminal_id() == id)
    }

    /// A terminal that can still reach the server.
    pub(crate) fn live_terminal(&self, cx: &App) -> Option<TerminalId> {
        self.leaves()
            .iter()
            .map(|view| view.read(cx))
            .find(|view| view.is_alive())
            .map(TerminalView::terminal_id)
    }

    /// A divider is under the pointer, so the layout is on its way to something else.
    pub(crate) fn is_dragging(&self) -> bool {
        self.dragging.is_some()
    }

    /// Takes the layout another PC gave its terminals. The ones in `gone` were closed there; any
    /// other that the layout does not have is new here, so it stays, next to the rest.
    pub(crate) fn apply_layout(
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
    pub(crate) fn drop_keys(
        &mut self,
        keys: &HashSet<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        for view in self.leaves() {
            let closed = view.read(cx).key().is_some_and(|key| keys.contains(&key));
            if closed && !self.take_pane(&view, false, window, cx) {
                return false;
            }
        }
        true
    }

    /// Lets go of a terminal closed elsewhere. `false`: it was the last one, the tab goes instead.
    pub(crate) fn remove_terminal(
        &mut self,
        id: TerminalId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let target = self
            .leaves()
            .into_iter()
            .find(|view| view.read(cx).terminal_id() == id);
        target.is_none_or(|target| self.take_pane(&target, false, window, cx))
    }

    pub(crate) fn split(&mut self, axis: SplitAxis, window: &mut Window, cx: &mut Context<Self>) {
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
    pub(crate) fn close_active(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        self.take_pane(&self.active.clone(), true, window, cx)
    }

    /// Takes a terminal out of the tree, ending what runs in it if `kill`.
    fn take_pane(
        &mut self,
        target: &Entity<TerminalView>,
        kill: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let leaves = self.leaves();
        if leaves.len() < 2 {
            return false;
        }
        let index = leaves.iter().position(|view| view == target).unwrap_or(0);
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
        if self.active == *target {
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
        if let Some(view) = self.neighbour(&self.active, dx, dy, cx) {
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
            let target = view;
            entries.push(
                MenuEntry::item(
                    "Close terminal",
                    handler(this, move |panes, window, cx| {
                        panes.take_pane(&target, true, window, cx);
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

    /// The header of a terminal's card: its host and folder, and what acts on it.
    fn card_header(
        &self,
        view: &Entity<TerminalView>,
        tools_shown: f32,
        several: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme().clone();
        let state = self.state.read(cx);
        let host = state.host(&self.host);
        let who = host.map_or(self.host.0.clone(), |host| {
            format!("{}@{}", host.user, host.alias)
        });
        let target = host.map(HostConfig::target);
        let color = theme.host_color(state.host_color(&self.host));
        let user = host.map(|host| host.user.clone()).unwrap_or_default();
        let shell = state
            .host_session(&self.host)
            .and_then(|session| session.login_shell.clone());
        let folder = view
            .read(cx)
            .cwd()
            .map(|folder| folder_crumbs(view, &folder, &theme));
        let id = view.entity_id().as_u64() as usize;
        let split = |axis: SplitAxis| {
            let target = view.clone();
            cx.listener(move |this: &mut Self, _: &ClickEvent, window, cx| {
                this.active = target.clone();
                this.split(axis, window, cx);
            })
        };
        let closing = view.clone();
        let files_shown = self.files_shown;
        let tools = vec![
            Button::new(("pane-split-right", id))
                .icon(IconName::SplitRight)
                .tooltip("Split right (Ctrl+Shift+D)")
                .style(ButtonStyle::Ghost)
                .on_click(split(SplitAxis::Row))
                .into_any_element(),
            Button::new(("pane-split-down", id))
                .icon(IconName::SplitDown)
                .tooltip("Split down (Ctrl+Shift+S)")
                .style(ButtonStyle::Ghost)
                .on_click(split(SplitAxis::Column))
                .into_any_element(),
            Button::new(("pane-files", id))
                .icon(IconName::Folder)
                .tooltip(if files_shown {
                    "Hide the files"
                } else {
                    "Files beside the terminal (Ctrl+Shift+E)"
                })
                .style(ButtonStyle::Ghost)
                .active(files_shown)
                .on_click(cx.listener(|_, _, _, cx| cx.emit(PanesEvent::ToggleFiles)))
                .into_any_element(),
            Button::new(("pane-close", id))
                .icon(IconName::Close)
                .tooltip(if several {
                    "Close terminal (Ctrl+Shift+W)"
                } else {
                    "Close tab (Ctrl+Shift+W)"
                })
                .style(ButtonStyle::Ghost)
                .on_click(cx.listener(move |this, _, window, cx| {
                    if !this.take_pane(&closing, true, window, cx) {
                        cx.emit(PanesEvent::CloseTab);
                    }
                }))
                .into_any_element(),
        ];
        let lead = vec![
            shell_tile(
                ("pane-tile", id),
                shell.as_deref(),
                &user,
                color,
                None,
                &theme,
            ),
            div()
                .id(("pane-host", id))
                .flex_none()
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(theme.text)
                .when_some(target, |this, target| this.tooltip(tooltip(target)))
                .child(who)
                .into_any_element(),
        ]
        .into_iter()
        .chain(folder)
        .collect();
        crate::panel::header(tools_shown, lead, tools).into_any_element()
    }

    fn render_node(
        &self,
        node: &Node,
        path: Path,
        several: bool,
        tools: &HashMap<EntityId, f32>,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        match node {
            Node::Leaf(view) => {
                let id = view.entity_id().as_u64() as usize;
                let entity = view.entity_id();
                let target = view.clone();
                let shown = tools.get(&entity).copied().unwrap_or(0.);
                let header = self.card_header(view, shown, several, cx);
                let theme = cx.theme().clone();
                // What a terminal contains is known from the frame before, where a new one was not
                // yet: it would show unmarked first. Its own focus is known at once. Alone, a card
                // has nothing to stand out from.
                let focused = (several || self.files_shown)
                    && (view.focus_handle(cx).is_focused(window)
                        || view.read(cx).contains_focus(window, cx));
                crate::panel::frame(&theme, theme.terminal_bg, focused)
                    .id(("pane", id))
                    .on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
                        let animated = this.state.read(cx).settings.animated_selection;
                        this.fades
                            .entry(entity)
                            .or_default()
                            .hover(*hovered, animated);
                        cx.notify();
                    }))
                    .size_full()
                    .overflow_hidden()
                    // Before the terminal, which may keep the click for the program in it.
                    .capture_any_mouse_down(cx.listener(
                        move |this, event: &MouseDownEvent, window, cx| {
                            // The selection stays where it is: the menu offers to swap with it.
                            if event.button == MouseButton::Right && event.modifiers.control {
                                this.open_menu(target.clone(), event.position, cx);
                                cx.stop_propagation();
                            } else {
                                // The keyboard comes along, on the header too, whichever part of
                                // the card was clicked.
                                if !target.read(cx).contains_focus(window, cx) {
                                    window.focus(&target.focus_handle(cx));
                                }
                                this.set_active(target.clone(), cx);
                            }
                        },
                    ))
                    .child(header)
                    .child(
                        div()
                            .relative()
                            .flex_1()
                            .min_h_0()
                            .w_full()
                            .child(view.clone()),
                    )
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
                let first = self.render_node(first, first_path, several, tools, window, cx);
                let second = self.render_node(second, second_path, several, tools, window, cx);
                let cell = bounds.clone();
                let gap = px(crate::panel::CARD_GAP);
                div()
                    .size_full()
                    .relative()
                    .flex()
                    .when(!row, Styled::flex_col)
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
                        // The gap between the two cards is what to grab.
                        div()
                            .relative()
                            .flex_none()
                            .when(row, |this| this.w(gap).h_full())
                            .when(!row, |this| this.h(gap).w_full())
                            .child(
                                div()
                                    .id(gpui::SharedString::from(format!("divider-{path:?}")))
                                    .absolute()
                                    .occlude()
                                    .size_full()
                                    .cursor(if row {
                                        CursorStyle::ResizeLeftRight
                                    } else {
                                        CursorStyle::ResizeUpDown
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

/// The path of a terminal, each folder a way to go there with `cd` in that terminal. Always in
/// sight: a narrow card cuts it from the left, keeping where the terminal is.
fn folder_crumbs(view: &Entity<TerminalView>, folder: &str, theme: &Theme) -> AnyElement {
    let (hover, press) = (theme.track, theme.active);
    let from_home = folder.starts_with('~');
    let names: Vec<&str> = folder
        .split('/')
        .filter(|name| !name.is_empty() && *name != "~")
        .collect();
    let last = names.len().checked_sub(1);
    let crumb = |id: usize, label: String, current: bool, command: String| {
        let view = view.clone();
        div()
            .id(("terminal-crumb", id))
            .flex_none()
            .px(px(4.))
            .py(px(2.))
            .rounded(px(RADIUS_SMALL))
            .cursor_pointer()
            .text_color(if current {
                theme.text_muted
            } else {
                theme.text_disabled
            })
            .hover(move |style| style.bg(hover))
            .active(move |style| style.bg(press))
            .tooltip(tooltip(format!("Run: {command}")))
            .on_click(move |_, window, cx| {
                view.update(cx, |view, cx| view.run_command(&command, cx));
                window.focus(&view.focus_handle(cx));
            })
            .child(label)
    };
    let separator = || div().flex_none().text_color(theme.text_disabled).child("/");
    let mut crumbs: Vec<AnyElement> = Vec::new();
    crumbs.push(separator().into_any_element());
    if from_home {
        crumbs.push(
            crumb(0, "~".to_string(), names.is_empty(), "cd ~".to_string()).into_any_element(),
        );
    }
    let mut reached = Vec::new();
    for (index, name) in names.iter().enumerate() {
        reached.push(*name);
        let quoted = shcp_core::shell_quote(&reached.join("/"));
        let command = if from_home {
            format!("cd ~/{quoted}")
        } else {
            format!(
                "cd {}",
                shcp_core::shell_quote(&format!("/{}", reached.join("/")))
            )
        };
        if index > 0 || from_home {
            crumbs.push(separator().into_any_element());
        }
        crumbs.push(
            crumb(index + 1, (*name).to_string(), Some(index) == last, command).into_any_element(),
        );
    }
    div()
        .id("terminal-crumbs")
        .flex()
        // As wide as the path while it fits; narrower, it loses its start, not its end.
        .min_w_0()
        .justify_end()
        .items_center()
        .gap(px(1.))
        .overflow_hidden()
        .font_family(crate::theme::mono_font())
        .text_size(crate::theme::ui_px(12.))
        .children(crumbs)
        .into_any_element()
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
        // How much of its tools each card shows, with the pointer over it or on its way out.
        let live: HashSet<EntityId> = leaves.iter().map(Entity::entity_id).collect();
        self.fades.retain(|id, _| live.contains(id));
        let tools: HashMap<EntityId, f32> = live
            .iter()
            .map(|id| (*id, self.fades.entry(*id).or_default().value(false)))
            .collect();
        if self.fades.values().any(HoverFade::moving) {
            window.request_animation_frame();
        }
        self.had_session |= leaves.iter().any(|view| view.read(cx).shell_started())
            && !leaves.iter().any(|view| view.read(cx).starting_shell());
        let shared_startup = !self.had_session;
        for view in &leaves {
            view.update(cx, |view, _| view.shared_startup = shared_startup);
        }
        let several = leaves.len() > 1;
        let root = self.root.take();
        let content = root
            .as_ref()
            .map(|root| self.render_node(root, Vec::new(), several, &tools, window, cx));
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
            .children(self.menu.clone().map(|menu| {
                let this = cx.entity().downgrade();
                ContextMenu::new(
                    "pane-menu",
                    menu,
                    crate::widgets::context_menu::dismiss(this, |view| &mut view.menu),
                )
            }))
    }
}
