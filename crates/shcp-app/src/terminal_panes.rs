//! The content of a terminal tab: one terminal, or several split side by side and on top of
//! each other. All of them belong to the tab's host.
use crate::actions::{
    FocusPaneDown, FocusPaneLeft, FocusPaneRight, FocusPaneUp, SplitDown, SplitRight,
};
use crate::motion::{HoverFade, Reveal};
use crate::state::AppState;
use crate::terminal_view::TerminalView;
use crate::theme::{ActiveTheme as _, RADIUS_CARD, RADIUS_SMALL, Theme, ui_px};
use crate::widgets::button::{Button, ButtonStyle};
use crate::widgets::context_menu::{ContextMenu, MenuEntry, OpenMenu, handler};
use crate::widgets::icon::{Icon, IconName};
use crate::widgets::tile::shell_tile;
use crate::widgets::tooltip::tooltip;
use gpui::prelude::*;
use gpui::{
    AnyElement, App, Bounds, ClickEvent, ClipboardItem, Context, CursorStyle, Div, DragMoveEvent,
    Entity, EntityId, EventEmitter, FocusHandle, Focusable, Hsla, MouseButton, MouseDownEvent,
    MouseMoveEvent, MouseUpEvent, Pixels, Point, Subscription, Window, canvas, div, point, px,
    relative, size,
};
use shcp_core::{HostConfig, HostId, SavedPane, SplitAxis, TerminalId};
use std::{
    cell::Cell,
    collections::{HashMap, HashSet},
    rc::Rc,
};

const MIN_RATIO: f32 = 0.1;

/// The tree of a tab's terminals. Generic over its leaves only for the tests.
enum Node<L = Entity<TerminalView>> {
    Leaf(L),
    Split {
        axis: SplitAxis,
        /// Share of the first child.
        ratio: f32,
        /// Where the split was painted, to turn a drag into a ratio.
        bounds: Rc<Cell<Option<Bounds<Pixels>>>>,
        first: Box<Node<L>>,
        second: Box<Node<L>>,
    },
}

/// Which child to take at each split, from the top.
type Path = Vec<bool>;

/// A terminal and where its card is.
type Slot = (Entity<TerminalView>, Bounds<Pixels>);

/// The side of a terminal another one is put on.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Side {
    Left,
    Right,
    Top,
    Bottom,
}

impl Side {
    /// Where a split puts its new terminal: after the one split.
    fn after(axis: SplitAxis) -> Self {
        match axis {
            SplitAxis::Row => Self::Right,
            SplitAxis::Column => Self::Bottom,
        }
    }

    /// The edge of `bounds` nearest to `point`, each distance taken as a share of the width or
    /// height across it: a wide card is not all left and right.
    fn nearest(bounds: Bounds<Pixels>, point: Point<Pixels>) -> Self {
        let width = f32::from(bounds.size.width).max(1.);
        let height = f32::from(bounds.size.height).max(1.);
        let x = f32::from(point.x - bounds.origin.x);
        let y = f32::from(point.y - bounds.origin.y);
        [
            (x / width, Self::Left),
            ((width - x) / width, Self::Right),
            (y / height, Self::Top),
            ((height - y) / height, Self::Bottom),
        ]
        .into_iter()
        .min_by(|a, b| a.0.total_cmp(&b.0))
        .map_or(Self::Right, |(_, side)| side)
    }

    fn axis(self) -> SplitAxis {
        match self {
            Self::Left | Self::Right => SplitAxis::Row,
            Self::Top | Self::Bottom => SplitAxis::Column,
        }
    }

    /// What is put on this side comes first in the split.
    fn first(self) -> bool {
        matches!(self, Self::Left | Self::Top)
    }
}

impl<L: Clone + PartialEq> Node<L> {
    fn split(axis: SplitAxis, ratio: f32, first: Node<L>, second: Node<L>) -> Self {
        Self::Split {
            axis,
            ratio,
            bounds: Rc::new(Cell::new(None)),
            first: Box::new(first),
            second: Box::new(second),
        }
    }

    fn leaves(&self, out: &mut Vec<L>) {
        match self {
            Self::Leaf(view) => out.push(view.clone()),
            Self::Split { first, second, .. } => {
                first.leaves(out);
                second.leaves(out);
            }
        }
    }

    fn contains(&self, target: &L) -> bool {
        match self {
            Self::Leaf(view) => view == target,
            Self::Split { first, second, .. } => first.contains(target) || second.contains(target),
        }
    }

    /// Puts `new` on `side` of the terminal `target`, the two sharing its room evenly.
    fn insert_beside(self, target: &L, side: Side, new: &L) -> Self {
        match self {
            Self::Leaf(view) if view == *target => {
                let (first, second) = if side.first() {
                    (new.clone(), view)
                } else {
                    (view, new.clone())
                };
                Self::split(side.axis(), 0.5, Self::Leaf(first), Self::Leaf(second))
            }
            Self::Split {
                axis,
                ratio,
                bounds,
                first,
                second,
            } => Self::Split {
                axis,
                ratio,
                bounds,
                first: Box::new(first.insert_beside(target, side, new)),
                second: Box::new(second.insert_beside(target, side, new)),
            },
            leaf => leaf,
        }
    }

    /// Takes the terminal `moved` out of where it is and puts it on `side` of `target`. Its old
    /// neighbour takes its room, as when it is closed; the rest keep their shares. Unchanged when
    /// the two are the same terminal or either is not in the tree.
    fn move_beside(self, moved: &L, target: &L, side: Side) -> Self {
        if moved == target || !self.contains(moved) || !self.contains(target) {
            return self;
        }
        // With `target` still there, something is left.
        match self.without(moved) {
            Some(rest) => rest.insert_beside(target, side, moved),
            None => Self::Leaf(moved.clone()),
        }
    }

    /// Without the terminal `target`; its neighbour takes the room.
    fn without(self, target: &L) -> Option<Self> {
        match self {
            Self::Leaf(view) if view == *target => None,
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
    fn swap(&mut self, a: &L, b: &L) {
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

    /// A copy with the same shape and shares, painted nowhere yet.
    fn duplicate(&self) -> Self {
        match self {
            Self::Leaf(view) => Self::Leaf(view.clone()),
            Self::Split {
                axis,
                ratio,
                first,
                second,
                ..
            } => Self::split(*axis, *ratio, first.duplicate(), second.duplicate()),
        }
    }

    /// The layout while `moved` is dragged: put on its side of the target, or, with none, out of
    /// the way, the others taking its room. Leaves this tree as it is.
    fn preview(&self, moved: &L, target: Option<(&L, Side)>) -> Self {
        let copy = self.duplicate();
        match target.filter(|(target, _)| *target != moved && self.contains(target)) {
            Some((target, side)) => copy.move_beside(moved, target, side),
            None => copy.without(moved).unwrap_or_else(|| self.duplicate()),
        }
    }

    /// Where each terminal's card goes in `bounds`, as the splits lay them out: the first child
    /// takes its share of the whole, the second what is left past the `gap`.
    fn areas(&self, bounds: Bounds<Pixels>, gap: Pixels, out: &mut Vec<(L, Bounds<Pixels>)>) {
        match self {
            Self::Leaf(view) => out.push((view.clone(), bounds)),
            Self::Split {
                axis,
                ratio,
                first,
                second,
                ..
            } => {
                let (origin, whole) = (bounds.origin, bounds.size);
                let (first_area, second_area) = match axis {
                    SplitAxis::Row => {
                        let lead = whole.width * *ratio;
                        let rest = px((f32::from(whole.width - lead - gap)).max(0.));
                        (
                            Bounds::new(origin, size(lead, whole.height)),
                            Bounds::new(
                                point(origin.x + lead + gap, origin.y),
                                size(rest, whole.height),
                            ),
                        )
                    }
                    SplitAxis::Column => {
                        let lead = whole.height * *ratio;
                        let rest = px((f32::from(whole.height - lead - gap)).max(0.));
                        (
                            Bounds::new(origin, size(whole.width, lead)),
                            Bounds::new(
                                point(origin.x, origin.y + lead + gap),
                                size(whole.width, rest),
                            ),
                        )
                    }
                };
                first.areas(first_area, gap, out);
                second.areas(second_area, gap, out);
            }
        }
    }
}

/// What a dragged terminal is aimed at, with the pointer at `pointer`. `slots` are the cards of the
/// others with the dragged one out of the way, fixed for the whole drag: the layout moving under
/// the pointer cannot change what it points at. Over the placeholder of the `current` aim, or in
/// a gap between cards, the aim stays; out of `area`, there is none.
fn aim<L: Clone + PartialEq>(
    slots: &[(L, Bounds<Pixels>)],
    area: Bounds<Pixels>,
    placeholder: Option<Bounds<Pixels>>,
    current: Option<(L, Side)>,
    pointer: Point<Pixels>,
) -> Option<(L, Side)> {
    if !area.contains(&pointer) {
        return None;
    }
    if placeholder.is_some_and(|placeholder| placeholder.contains(&pointer)) {
        return current;
    }
    slots
        .iter()
        .find(|(_, slot)| slot.contains(&pointer))
        .map(|(view, slot)| (view.clone(), Side::nearest(*slot, pointer)))
        .or(current)
}

/// `t` of the way from `from` to `to`.
fn lerp_bounds(from: Bounds<Pixels>, to: Bounds<Pixels>, t: f32) -> Bounds<Pixels> {
    let mix = |a: Pixels, b: Pixels| a + (b - a) * t;
    Bounds::new(
        point(
            mix(from.origin.x, to.origin.x),
            mix(from.origin.y, to.origin.y),
        ),
        size(
            mix(from.size.width, to.size.width),
            mix(from.size.height, to.size.height),
        ),
    )
}

impl Node {
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

/// A terminal dragged by its header to another place in its tab, and the small card that follows
/// the pointer meanwhile.
#[derive(Clone)]
struct PaneDrag {
    view: Entity<TerminalView>,
    who: String,
    folder: Option<String>,
    color: Hsla,
    /// Where the header was grabbed, from its top left corner.
    grabbed: Point<Pixels>,
}

/// The size of the card that follows the pointer while a terminal is dragged.
const DRAG_CARD: (f32, f32) = (220., 124.);
/// How see-through that card is: the real one starts so as it lands.
const DRAG_CARD_OPACITY: f32 = 0.94;

/// How far from the corner gpui draws the drag at, which is where the header was `grabbed`, the
/// dragged card is. Along a wide header that corner can be far away: the card comes to the
/// pointer instead, the pointer on its header.
fn drag_card_inset(grabbed: Point<Pixels>) -> Point<Pixels> {
    point(
        px((f32::from(grabbed.x) - 24.).max(0.)),
        px((f32::from(grabbed.y) - 15.).max(0.)),
    )
}

/// Where the dragged card is with the pointer at `pointer`, the header `grabbed` where it was.
fn drag_card_bounds(pointer: Point<Pixels>, grabbed: Point<Pixels>) -> Bounds<Pixels> {
    let inset = drag_card_inset(grabbed);
    Bounds::new(
        point(
            pointer.x - grabbed.x + inset.x,
            pointer.y - grabbed.y + inset.y,
        ),
        size(px(DRAG_CARD.0), px(DRAG_CARD.1)),
    )
}

impl Render for PaneDrag {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let inset = drag_card_inset(self.grabbed);
        div().pl(inset.x).pt(inset.y).child(
            div()
                .w(px(DRAG_CARD.0))
                .h(px(DRAG_CARD.1))
                .flex()
                .flex_col()
                .gap(px(2.))
                .overflow_hidden()
                .rounded(px(RADIUS_CARD))
                .border_1()
                .border_color(theme.strong_border)
                .bg(theme.terminal_bg)
                .shadow(crate::theme::float_shadow())
                .opacity(DRAG_CARD_OPACITY)
                .font_family(crate::theme::ui_font())
                .child(
                    div()
                        .h(px(30.))
                        .flex()
                        .flex_none()
                        .items_center()
                        .gap(px(7.))
                        .px(px(9.))
                        .text_size(ui_px(12.))
                        .child(
                            Icon::new(IconName::Terminal)
                                .size(px(13.))
                                .color(self.color),
                        )
                        .child(
                            div()
                                .min_w_0()
                                .truncate()
                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                .text_color(theme.text)
                                .child(self.who.clone()),
                        ),
                )
                .children(self.folder.clone().map(|folder| {
                    div()
                        .px(px(9.))
                        .truncate()
                        .font_family(crate::theme::mono_font())
                        .text_size(ui_px(11.))
                        .text_color(theme.text_disabled)
                        .child(folder)
                })),
        )
    }
}

/// A terminal being dragged to another place in its tab. The tree itself stays as it was until
/// the drop: what shows meanwhile is its preview, and letting go elsewhere leaves it untouched.
struct Rearrange {
    moved: Entity<TerminalView>,
    /// Where its header was grabbed, for where the card under the pointer is.
    grabbed: Point<Pixels>,
    /// The terminal it would go next to, and on which side.
    aim: Option<(Entity<TerminalView>, Side)>,
    /// Where each card was drawn when the layout last changed course, and where it is going.
    from: HashMap<EntityId, Bounds<Pixels>>,
    to: HashMap<EntityId, Bounds<Pixels>>,
    glide: Reveal,
    animated: bool,
}

impl Rearrange {
    /// Where each card is drawn now, on its way.
    fn drawn(&self) -> HashMap<EntityId, Bounds<Pixels>> {
        let t = self.glide.value();
        self.to
            .iter()
            .map(|(id, to)| {
                let from = self.from.get(id).copied().unwrap_or(*to);
                (*id, lerp_bounds(from, *to, t))
            })
            .collect()
    }

    /// The cards set off again from where they are, towards the layout of the new aim.
    fn glide_from_here(&mut self) {
        self.from = self.drawn();
        self.glide = Reveal::new(false);
        if self.animated {
            self.glide.arm();
        }
        self.glide.set(true);
    }
}

/// A dragged terminal let go: it travels from the card under the pointer to its place, and the
/// others glide the rest of the way to theirs. Only drawn: the tree already is what it will be.
struct Landing {
    moved: Entity<TerminalView>,
    /// Where each card was drawn when let go, the dragged one as the card under the pointer.
    from: HashMap<EntityId, Bounds<Pixels>>,
    /// Where the placeholder was, fading out.
    placeholder: Option<Bounds<Pixels>>,
    flight: Reveal,
}

/// Something of the layout, drawn at `at` in the terminals' `area`.
fn placed(at: Bounds<Pixels>, area: Bounds<Pixels>, content: impl IntoElement) -> AnyElement {
    div()
        .absolute()
        .left(at.origin.x - area.origin.x)
        .top(at.origin.y - area.origin.y)
        .w(at.size.width)
        .h(at.size.height)
        .child(content)
        .into_any_element()
}

/// What every card of a layout goes by.
struct Cards<'a> {
    /// The terminal whose card reaches the top right corner of the layout.
    corner: &'a Entity<TerminalView>,
    /// There is more than one terminal.
    several: bool,
    /// How much of its tools each card shows.
    tools: &'a HashMap<EntityId, f32>,
}

/// The room a dragged terminal would take: a card of the accent, see-through.
fn placeholder(theme: &Theme) -> Div {
    let accent = theme.accent;
    div()
        .size_full()
        .rounded(px(RADIUS_CARD))
        .border_1()
        .border_color(accent)
        .bg(Hsla { a: 0.14, ..accent })
}

pub(crate) enum PanesEvent {
    /// Show the files next to the terminals.
    Browse,
    /// Show the files, or hide them if they show.
    ToggleFiles,
    /// The last terminal was closed from its card: the tab goes.
    CloseTab,
    /// The last terminal was closed beside the files, which stay: the keyboard goes to them.
    ShellClosed,
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
    /// A terminal is dragged by its header.
    rearrange: Option<Rearrange>,
    /// A dragged terminal let go, on its way to its place.
    landing: Option<Landing>,
    /// Where the terminals were laid out last, for a drag to lay them out itself.
    area: Rc<Cell<Option<Bounds<Pixels>>>>,
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
            rearrange: None,
            landing: None,
            area: Rc::new(Cell::new(None)),
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

    /// Where each shell is, by its name on the server.
    pub(crate) fn folders(&self, cx: &App) -> Vec<(String, String)> {
        self.leaves()
            .iter()
            .filter_map(|view| {
                let view = view.read(cx);
                Some((view.key()?, view.folder()?.to_string()))
            })
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

    /// A divider or a terminal is under the pointer, so the layout is on its way to something else.
    pub(crate) fn is_dragging(&self) -> bool {
        self.dragging.is_some()
            || self.rearrange.is_some()
            || self
                .landing
                .as_ref()
                .is_some_and(|landing| landing.flight.moving())
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
        self.landing = None;
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
        // The files sit beside one terminal.
        if self.files_shown {
            return;
        }
        self.landing = None;
        let new = Self::terminal(&self.state, &self.host, None, false, window, cx);
        let target = self.active.clone();
        self.root = self
            .root
            .take()
            .map(|root| root.insert_beside(&target, Side::after(axis), &new));
        self.active = new;
        window.focus(&self.active.focus_handle(cx));
        cx.notify();
    }

    /// Closes the active terminal. `false`: it was the last one, the tab has to close instead.
    pub(crate) fn close_active(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        self.take_pane(&self.active.clone(), true, window, cx)
    }

    /// The last terminal goes. Beside the files it ends its shell and leaves them the tab, as
    /// `exit` does; alone, the tab goes with it.
    pub(crate) fn close_last(&mut self, cx: &mut Context<Self>) {
        if self.files_shown && !self.shells_gone(cx) {
            self.active.read(cx).close(cx);
            cx.emit(PanesEvent::ShellClosed);
            cx.notify();
        } else {
            cx.emit(PanesEvent::CloseTab);
        }
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
        self.landing = None;
        let index = leaves.iter().position(|view| view == target).unwrap_or(0);
        // Closed from elsewhere, it must not pull the keyboard away from what it is on.
        let refocus = kill || target.read(cx).contains_focus(window, cx);
        if kill {
            target.read(cx).close(cx);
        } else {
            target.read(cx).detach(cx);
        }
        self.root = self.root.take().and_then(|root| root.without(target));
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
        self.landing = None;
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

    /// The cards of the others, with `moved` out of the way: what a drag aims at.
    fn slots(&self, moved: &Entity<TerminalView>, area: Bounds<Pixels>) -> Vec<Slot> {
        let mut slots = Vec::new();
        if let Some(root) = &self.root {
            root.preview(moved, None)
                .areas(area, px(crate::panel::CARD_GAP), &mut slots);
        }
        slots
    }

    /// A terminal starts to be dragged by its header: it leaves the layout, the others take its
    /// room.
    fn start_rearrange(
        &mut self,
        moved: Entity<TerminalView>,
        grabbed: Point<Pixels>,
        cx: &mut Context<Self>,
    ) {
        self.landing = None;
        let mut from = Vec::new();
        if let (Some(root), Some(area)) = (&self.root, self.area.get()) {
            root.areas(area, px(crate::panel::CARD_GAP), &mut from);
        }
        let to: HashMap<EntityId, Bounds<Pixels>> = from
            .into_iter()
            .map(|(view, bounds)| (view.entity_id(), bounds))
            .collect();
        let mut rearrange = Rearrange {
            moved,
            grabbed,
            aim: None,
            from: HashMap::new(),
            to,
            glide: Reveal::new(true),
            animated: self.state.read(cx).settings.animated_selection,
        };
        rearrange.glide_from_here();
        self.rearrange = Some(rearrange);
        cx.notify();
    }

    /// Follows the dragged terminal: what it is aimed at, and the layout that would make.
    fn track_rearrange(&mut self, event: &DragMoveEvent<PaneDrag>, cx: &mut Context<Self>) {
        let (Some(rearrange), Some(area)) = (&self.rearrange, self.area.get()) else {
            return;
        };
        let moved = rearrange.moved.clone();
        let current = rearrange.aim.clone();
        let slots = self.slots(&moved, area);
        // Where the placeholder of the current aim is, at the end of its way.
        let placeholder = current.as_ref().and_then(|(target, side)| {
            let mut areas = Vec::new();
            self.root
                .as_ref()?
                .preview(&moved, Some((target, *side)))
                .areas(area, px(crate::panel::CARD_GAP), &mut areas);
            areas
                .into_iter()
                .find(|(view, _)| *view == moved)
                .map(|(_, bounds)| bounds)
        });
        let aimed = aim(
            &slots,
            area,
            placeholder,
            current.clone(),
            event.event.position,
        );
        if aimed != current
            && let Some(rearrange) = &mut self.rearrange
        {
            rearrange.glide_from_here();
            rearrange.aim = aimed;
            cx.notify();
        }
    }

    /// The dragged terminal is let go at `pointer`: with `commit`, it takes the place it was
    /// aimed at, if any; otherwise the layout stays as it was. Either way, the terminal and what
    /// runs in it stay, and the cards glide from where they were drawn to where they belong.
    fn finish_rearrange(
        &mut self,
        pointer: Point<Pixels>,
        commit: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(rearrange) = self.rearrange.take() else {
            return;
        };
        cx.notify();
        let moved = rearrange.moved.clone();
        let mut from = rearrange.drawn();
        let placeholder = from.remove(&moved.entity_id());
        from.insert(
            moved.entity_id(),
            drag_card_bounds(pointer, rearrange.grabbed),
        );
        let mut flight = Reveal::new(false);
        if rearrange.animated {
            flight.arm();
        }
        flight.set(true);
        self.landing = flight.moving().then(|| Landing {
            moved: moved.clone(),
            from,
            placeholder,
            flight,
        });
        let inside = self.area.get().is_some_and(|area| area.contains(&pointer));
        let Some((target, side)) = rearrange.aim.filter(|_| commit && inside) else {
            return;
        };
        self.root = self
            .root
            .take()
            .map(|root| root.move_beside(&moved, &target, side));
        if self.root.as_ref().is_some_and(|root| root.contains(&moved)) {
            self.active = moved.clone();
            window.focus(&moved.focus_handle(cx));
        }
    }

    /// The cards of the layout as it now is, each gliding from where it was drawn when the
    /// dragged terminal was let go, that one above the rest. `None` once they are there.
    fn render_landing(
        &mut self,
        several: bool,
        tools: &HashMap<EntityId, f32>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Vec<AnyElement>> {
        let landing = self.landing.as_ref()?;
        window.request_animation_frame();
        let area = self.area.get()?;
        let t = landing.flight.value();
        let root = self.root.as_ref()?;
        let mut areas = Vec::new();
        root.areas(area, px(crate::panel::CARD_GAP), &mut areas);
        let cards = Cards {
            corner: Self::top_right(root),
            several,
            tools,
        };
        let theme = cx.theme().clone();
        let mut drawn: Vec<AnyElement> = landing
            .placeholder
            .map(|at| placed(at, area, placeholder(&theme).opacity(1. - t)))
            .into_iter()
            .collect();
        let mut flying = None;
        for (view, end) in areas {
            let from = landing.from.get(&view.entity_id()).copied().unwrap_or(end);
            let at = lerp_bounds(from, end, t);
            let card = self.render_card(&view, &cards, window, cx);
            if view == landing.moved {
                let look = DRAG_CARD_OPACITY + (1. - DRAG_CARD_OPACITY) * t;
                flying = Some(placed(
                    at,
                    area,
                    div()
                        .size_full()
                        .rounded(px(RADIUS_CARD))
                        .shadow(crate::theme::float_shadow())
                        .opacity(look)
                        .child(card),
                ));
            } else {
                drawn.push(placed(at, area, card));
            }
        }
        drawn.extend(flying);
        Some(drawn)
    }

    /// The cards where a drag puts them, gliding there: the dragged terminal is a placeholder at
    /// the place it would take. `None` before the terminals were ever laid out.
    fn render_rearranged(
        &mut self,
        several: bool,
        tools: &HashMap<EntityId, f32>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Vec<AnyElement>> {
        let area = self.area.get()?;
        let rearrange = self.rearrange.as_ref()?;
        let moved = rearrange.moved.clone();
        let aim = rearrange.aim.clone();
        let tree = self
            .root
            .as_ref()?
            .preview(&moved, aim.as_ref().map(|(target, side)| (target, *side)));
        let mut areas = Vec::new();
        tree.areas(area, px(crate::panel::CARD_GAP), &mut areas);
        let rearrange = self.rearrange.as_mut()?;
        rearrange.to = areas
            .iter()
            .map(|(view, bounds)| (view.entity_id(), *bounds))
            .collect();
        let drawn = rearrange.drawn();
        if rearrange.glide.moving() {
            window.request_animation_frame();
        }
        let corner = Self::top_right(&tree).clone();
        let theme = cx.theme().clone();
        let cards = areas
            .into_iter()
            .map(|(view, end)| {
                let at = drawn.get(&view.entity_id()).copied().unwrap_or(end);
                let content = if view == moved {
                    placeholder(&theme).into_any_element()
                } else {
                    let cards = Cards {
                        corner: &corner,
                        several,
                        tools,
                    };
                    self.render_card(&view, &cards, window, cx)
                };
                placed(at, area, content)
            })
            .collect();
        Some(cards)
    }

    /// The header of a terminal's card: its host and folder, and what acts on it.
    fn card_header(
        &self,
        view: &Entity<TerminalView>,
        corner: &Entity<TerminalView>,
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
        let folder = view.read(cx).cwd().map(|folder| {
            // A path's line is already centred on the tile: no lift.
            folder_crumbs(view, &folder, px(0.), &theme)
        });
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
        // The files sit beside one terminal: with them there is no splitting, and a split tab
        // gets no files until it is one terminal again.
        let mut tools = Vec::new();
        if !files_shown {
            tools.extend([
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
            ]);
        }
        tools.extend([
            Button::new(("pane-files", id))
                .icon(IconName::Folder)
                .tooltip(if files_shown {
                    "Hide the files"
                } else if several {
                    "The files go beside one terminal: close the others first"
                } else {
                    "Files beside the terminal (Ctrl+Shift+E)"
                })
                .style(ButtonStyle::Ghost)
                .active(files_shown)
                .disabled(several && !files_shown)
                .on_click(cx.listener(|_, _, _, cx| cx.emit(PanesEvent::ToggleFiles)))
                .into_any_element(),
            Button::new(("pane-close", id))
                .icon(IconName::Close)
                .tooltip(if several || files_shown {
                    "Close terminal (Ctrl+Shift+W)"
                } else {
                    "Close tab (Ctrl+Shift+W)"
                })
                .style(ButtonStyle::Ghost)
                .on_click(cx.listener(move |this, _, window, cx| {
                    if !this.take_pane(&closing, true, window, cx) {
                        this.close_last(cx);
                    }
                }))
                .into_any_element(),
        ]);
        let dragged = several.then(|| PaneDrag {
            view: view.clone(),
            who: who.clone(),
            folder: view.read(cx).cwd(),
            color,
            grabbed: Point::default(),
        });
        // The header moves the terminal, but a press on a button stays a press.
        let tools = if several {
            tools
                .into_iter()
                .map(|tool| {
                    div()
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .child(tool)
                        .into_any_element()
                })
                .collect()
        } else {
            tools
        };
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
                .relative()
                .top(-crate::widgets::tile::text_lift(
                    crate::theme::ui_font(),
                    ui_px(13.),
                    cx,
                ))
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(theme.text)
                .when_some(target, |this, target| this.tooltip(tooltip(target)))
                .child(who)
                .into_any_element(),
        ]
        .into_iter()
        .chain(folder)
        .collect();
        // The window's buttons lie over the top right corner of the cards: the card there keeps
        // its tools clear of them. Beside the files, that card is the files' own.
        let caption_room = crate::title_bar::caption_room();
        let cornered = !files_shown && caption_room > px(0.) && corner == view;
        crate::panel::header(tools_shown, lead, tools)
            .id(("pane-header", id))
            .when(cornered, |this| this.pr(caption_room))
            .when_some(dragged, |this, dragged| {
                let panes = cx.entity().downgrade();
                this.on_drag(dragged, move |dragged, grabbed, _, cx| {
                    panes
                        .update(cx, |panes, cx| {
                            panes.start_rearrange(dragged.view.clone(), grabbed, cx)
                        })
                        .ok();
                    cx.new(|_| PaneDrag {
                        grabbed,
                        ..dragged.clone()
                    })
                })
            })
            .into_any_element()
    }

    /// The terminal whose card reaches the top right corner.
    fn top_right(node: &Node) -> &Entity<TerminalView> {
        match node {
            Node::Leaf(view) => view,
            Node::Split {
                axis: SplitAxis::Row,
                second,
                ..
            } => Self::top_right(second),
            Node::Split { first, .. } => Self::top_right(first),
        }
    }

    /// The card of a terminal: its header over it.
    fn render_card(
        &self,
        view: &Entity<TerminalView>,
        cards: &Cards,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let id = view.entity_id().as_u64() as usize;
        let entity = view.entity_id();
        let target = view.clone();
        let several = cards.several;
        let shown = cards.tools.get(&entity).copied().unwrap_or(0.);
        let header = self.card_header(view, cards.corner, shown, several, cx);
        let theme = cx.theme().clone();
        // What a terminal contains is known from the frame before, where a new one was not
        // yet: it would show unmarked first. Its own focus is known at once. Alone, a card
        // has nothing to stand out from.
        let dimmed = (several || self.files_shown)
            && !(view.focus_handle(cx).is_focused(window)
                || view.read(cx).contains_focus(window, cx));
        crate::panel::frame(&theme, theme.terminal_bg)
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
            .capture_any_mouse_down(
                cx.listener(move |this, event: &MouseDownEvent, window, cx| {
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
                }),
            )
            .child(
                crate::panel::contents(dimmed).child(header).child(
                    div()
                        .relative()
                        .flex_1()
                        .min_h_0()
                        .w_full()
                        .child(view.clone()),
                ),
            )
            .into_any_element()
    }

    fn render_node(
        &self,
        node: &Node,
        path: Path,
        cards: &Cards,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        match node {
            Node::Leaf(view) => self.render_card(view, cards, window, cx),
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
                let first = self.render_node(first, first_path, cards, window, cx);
                let second = self.render_node(second, second_path, cards, window, cx);
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
/// `lift`: how far the path goes up to look centred on the tile at the start of the header.
fn folder_crumbs(
    view: &Entity<TerminalView>,
    folder: &str,
    lift: Pixels,
    theme: &Theme,
) -> AnyElement {
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
        .relative()
        .top(-lift)
        .flex()
        // As wide as the path while it fits; narrower, it loses its start, not its end.
        .min_w_0()
        .justify_end()
        .items_center()
        .gap(px(1.))
        .overflow_hidden()
        // Each folder is a button: a press on the path does not drag the terminal by its header.
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .font_family(crate::theme::mono_font())
        .text_size(crate::theme::ui_px(12.))
        .children(crumbs)
        .into_any_element()
}

impl Render for TerminalPanes {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // A drag let go anywhere ends here too, with or without a drop: the layout stays as it was.
        if !cx.has_active_drag() {
            self.rearrange = None;
        }
        // A landing is over, here or in a tab out of sight meanwhile. One more frame for what
        // waits for the layout to be still, like a shared layout.
        if self
            .landing
            .as_ref()
            .is_some_and(|landing| !landing.flight.moving())
        {
            self.landing = None;
            window.request_animation_frame();
        }
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
        // While a drag lays the cards out, the terminals keep their size: the servers hear of
        // the new one once, after the drop.
        let rearranging = self.rearrange.is_some();
        let landing = self.landing.is_some();
        for view in &leaves {
            view.update(cx, |view, _| {
                view.shared_startup = shared_startup;
                view.hold_size = rearranging || landing;
            });
        }
        let several = leaves.len() > 1;
        let rearranged = match self.render_rearranged(several, &tools, window, cx) {
            Some(cards) => Some(cards),
            None => self.render_landing(several, &tools, window, cx),
        };
        let landing = self.landing.is_some();
        let content = match rearranged {
            Some(cards) => cards,
            None => {
                let root = self.root.take();
                let content = root.as_ref().map(|root| {
                    let cards = Cards {
                        corner: Self::top_right(root),
                        several,
                        tools: &tools,
                    };
                    self.render_node(root, Vec::new(), &cards, window, cx)
                });
                self.root = root;
                content.into_iter().collect()
            }
        };
        // The dragged terminal is out of the layout but keeps the keyboard: Esc reaches this.
        let held_focus = self
            .rearrange
            .as_ref()
            .map(|rearrange| div().track_focus(&rearrange.moved.focus_handle(cx)));
        let dragging = self.dragging.is_some();
        let area = self.area.clone();
        div()
            .size_full()
            .relative()
            .child(
                canvas(move |bounds, _, _| area.set(Some(bounds)), |_, _, _, _| {})
                    .absolute()
                    .size_full(),
            )
            .on_drag_move(cx.listener(|this, event: &DragMoveEvent<PaneDrag>, _, cx| {
                this.track_rearrange(event, cx)
            }))
            // In the capture phase, which nothing under the pointer can stop. What covers a
            // terminal, like its connection card, hides this container from the pointer: a
            // release there counts as outside.
            .when(rearranging, |this| {
                this.capture_any_mouse_up(cx.listener(|this, event: &MouseUpEvent, window, cx| {
                    if event.button == MouseButton::Left {
                        this.finish_rearrange(event.position, true, window, cx)
                    }
                }))
                .on_mouse_up_out(
                    MouseButton::Left,
                    cx.listener(|this, event: &MouseUpEvent, window, cx| {
                        this.finish_rearrange(event.position, true, window, cx)
                    }),
                )
            })
            // A click while a terminal lands puts everything in its place at once.
            .when(landing, |this| {
                this.capture_any_mouse_down(cx.listener(|this, _: &MouseDownEvent, _, cx| {
                    this.landing = None;
                    cx.notify();
                }))
            })
            .capture_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, window, cx| {
                if this.rearrange.is_some() && event.keystroke.key == "escape" {
                    let pointer = window.mouse_position();
                    this.finish_rearrange(pointer, false, window, cx);
                    cx.stop_active_drag(window);
                    cx.stop_propagation();
                    cx.notify();
                    return;
                }
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
            .children(held_focus)
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

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{point, size};

    fn leaf(id: u32) -> Node<u32> {
        Node::Leaf(id)
    }

    fn row(ratio: f32, first: Node<u32>, second: Node<u32>) -> Node<u32> {
        Node::split(SplitAxis::Row, ratio, first, second)
    }

    fn col(ratio: f32, first: Node<u32>, second: Node<u32>) -> Node<u32> {
        Node::split(SplitAxis::Column, ratio, first, second)
    }

    /// The tree as text: `row` side by side, `col` one above the other, each with its ratio.
    fn shape(node: &Node<u32>) -> String {
        match node {
            Node::Leaf(id) => id.to_string(),
            Node::Split {
                axis,
                ratio,
                first,
                second,
                ..
            } => {
                let axis = match axis {
                    SplitAxis::Row => "row",
                    SplitAxis::Column => "col",
                };
                format!("{axis}{ratio}({}, {})", shape(first), shape(second))
            }
        }
    }

    fn bounds() -> Bounds<Pixels> {
        Bounds::new(point(px(100.), px(50.)), size(px(400.), px(100.)))
    }

    #[test]
    fn the_nearest_edge_is_taken_as_a_share_of_the_card() {
        let at = |x: f32, y: f32| Side::nearest(bounds(), point(px(x), px(y)));
        assert_eq!(at(110., 100.), Side::Left);
        assert_eq!(at(490., 100.), Side::Right);
        assert_eq!(at(300., 55.), Side::Top);
        assert_eq!(at(300., 145.), Side::Bottom);
        // 60 of 400 from the left is nearer than 25 of 100 from the top.
        assert_eq!(at(160., 75.), Side::Left);
        // 100 of 400 from the left is farther than 20 of 100 from the top.
        assert_eq!(at(200., 70.), Side::Top);
    }

    #[test]
    fn each_side_splits_along_its_axis_with_the_moved_one_in_place() {
        let cases = [
            (Side::Left, "row0.5(2, 1)"),
            (Side::Right, "row0.5(1, 2)"),
            (Side::Top, "col0.5(2, 1)"),
            (Side::Bottom, "col0.5(1, 2)"),
        ];
        for (side, expected) in cases {
            let moved = row(0.3, leaf(1), leaf(2)).move_beside(&2, &1, side);
            assert_eq!(shape(&moved), expected, "{side:?}");
        }
    }

    #[test]
    fn a_moved_terminal_leaves_its_split_to_its_neighbour() {
        let tree = row(0.3, leaf(1), col(0.7, leaf(2), leaf(3)));
        let moved = tree.move_beside(&3, &1, Side::Left);
        assert_eq!(shape(&moved), "row0.3(row0.5(3, 1), 2)");
    }

    #[test]
    fn the_other_splits_keep_their_shares() {
        let tree = col(
            0.25,
            row(0.6, leaf(1), leaf(2)),
            row(0.4, leaf(3), col(0.8, leaf(4), leaf(5))),
        );
        let moved = tree.move_beside(&2, &5, Side::Bottom);
        assert_eq!(
            shape(&moved),
            "col0.25(1, row0.4(3, col0.8(4, col0.5(5, 2))))"
        );
    }

    #[test]
    fn a_split_puts_the_new_terminal_after_the_one_split() {
        let tree = row(0.4, leaf(1), leaf(2));
        let split = tree.insert_beside(&2, Side::after(SplitAxis::Column), &3);
        assert_eq!(shape(&split), "row0.4(1, col0.5(2, 3))");
        let split = split.insert_beside(&1, Side::after(SplitAxis::Row), &4);
        assert_eq!(shape(&split), "row0.4(row0.5(1, 4), col0.5(2, 3))");
    }

    #[test]
    fn moving_onto_itself_or_with_a_stranger_changes_nothing() {
        let tree = || row(0.3, leaf(1), col(0.7, leaf(2), leaf(3)));
        let expected = shape(&tree());
        assert_eq!(shape(&tree().move_beside(&2, &2, Side::Left)), expected);
        assert_eq!(shape(&tree().move_beside(&9, &1, Side::Left)), expected);
        assert_eq!(shape(&tree().move_beside(&1, &9, Side::Left)), expected);
        assert_eq!(shape(&leaf(1).move_beside(&1, &1, Side::Top)), "1");
    }

    #[test]
    fn every_move_keeps_every_terminal_once() {
        let sides = [Side::Left, Side::Right, Side::Top, Side::Bottom];
        for moved in 1..=4 {
            for target in 1..=4 {
                for side in sides {
                    let tree = row(0.5, col(0.5, leaf(1), leaf(2)), col(0.5, leaf(3), leaf(4)));
                    let after = tree.move_beside(&moved, &target, side);
                    let mut leaves = Vec::new();
                    after.leaves(&mut leaves);
                    leaves.sort_unstable();
                    assert_eq!(leaves, [1, 2, 3, 4], "{moved} to {side:?} of {target}");
                }
            }
        }
    }

    fn rect(x: f32, y: f32, w: f32, h: f32) -> Bounds<Pixels> {
        Bounds::new(point(px(x), px(y)), size(px(w), px(h)))
    }

    #[test]
    fn a_preview_leaves_the_tree_as_it_was() {
        let tree = row(0.3, leaf(1), col(0.7, leaf(2), leaf(3)));
        let before = shape(&tree);
        let collapsed = tree.preview(&3, None);
        assert_eq!(shape(&collapsed), "row0.3(1, 2)");
        let aimed = tree.preview(&3, Some((&1, Side::Top)));
        assert_eq!(shape(&aimed), "row0.3(col0.5(3, 1), 2)");
        assert_eq!(shape(&tree), before);
    }

    #[test]
    fn a_preview_aimed_at_itself_or_a_stranger_is_the_collapsed_one() {
        let tree = row(0.3, leaf(1), col(0.7, leaf(2), leaf(3)));
        assert_eq!(
            shape(&tree.preview(&3, Some((&3, Side::Left)))),
            "row0.3(1, 2)"
        );
        assert_eq!(
            shape(&tree.preview(&3, Some((&9, Side::Left)))),
            "row0.3(1, 2)"
        );
        // Dropping what it shows gives the same tree.
        let committed = tree.duplicate().move_beside(&3, &1, Side::Right);
        assert_eq!(
            shape(&committed),
            shape(&tree.preview(&3, Some((&1, Side::Right))))
        );
    }

    #[test]
    fn areas_follow_the_shares_and_the_gap() {
        let tree = row(0.25, leaf(1), col(0.5, leaf(2), leaf(3)));
        let mut areas = Vec::new();
        tree.areas(rect(10., 20., 406., 206.), px(6.), &mut areas);
        assert_eq!(
            areas,
            [
                (1, rect(10., 20., 101.5, 206.)),
                (2, rect(117.5, 20., 298.5, 103.)),
                (3, rect(117.5, 129., 298.5, 97.)),
            ]
        );
    }

    #[test]
    fn the_aim_holds_over_its_placeholder_and_in_the_gaps() {
        let area = rect(0., 0., 406., 100.);
        let slots = [
            (1, rect(0., 0., 200., 100.)),
            (2, rect(206., 0., 200., 100.)),
        ];
        let at = |x: f32, y: f32| point(px(x), px(y));
        // Over a card: its nearest edge.
        assert_eq!(
            aim(&slots, area, None, None, at(390., 50.)),
            Some((2, Side::Right))
        );
        // Over the placeholder, which took the right half of 2, the aim stays even where the
        // card under it would say otherwise.
        let current = Some((2, Side::Right));
        let placeholder = Some(rect(309., 0., 97., 100.));
        assert_eq!(
            aim(&slots, area, placeholder, current, at(320., 5.)),
            current
        );
        // Off the placeholder, the card under the pointer decides again.
        assert_eq!(
            aim(&slots, area, placeholder, current, at(220., 50.)),
            Some((2, Side::Left))
        );
        // In the gap between two cards: as it was.
        assert_eq!(
            aim(&slots, area, placeholder, current, at(203., 50.)),
            current
        );
        // Out of the terminals: nowhere.
        assert_eq!(aim(&slots, area, placeholder, current, at(500., 50.)), None);
    }

    #[test]
    fn the_aim_does_not_move_with_the_layout() {
        // The slots are those of the collapsed layout: whatever the preview does, the same
        // pointer gets the same answer.
        let area = rect(0., 0., 406., 100.);
        let slots = [
            (1, rect(0., 0., 200., 100.)),
            (2, rect(206., 0., 200., 100.)),
        ];
        let pointer = point(px(150.), px(50.));
        let first = aim(&slots, area, None, None, pointer);
        let again = aim(&slots, area, None, first, pointer);
        assert_eq!(first, Some((1, Side::Right)));
        assert_eq!(again, first);
    }

    #[test]
    fn a_glide_goes_from_one_rectangle_to_the_other() {
        let (from, to) = (rect(0., 0., 100., 50.), rect(100., 20., 300., 150.));
        assert_eq!(lerp_bounds(from, to, 0.), from);
        assert_eq!(lerp_bounds(from, to, 1.), to);
        assert_eq!(lerp_bounds(from, to, 0.5), rect(50., 10., 200., 100.));
    }

    #[test]
    fn a_landing_starts_where_the_dragged_card_was() {
        let pointer = point(px(500.), px(400.));
        // Grabbed far along the header: the card sits just under the pointer.
        assert_eq!(
            drag_card_bounds(pointer, point(px(300.), px(20.))),
            rect(476., 385., DRAG_CARD.0, DRAG_CARD.1)
        );
        // Grabbed near the corner: the card keeps that corner where gpui puts it.
        assert_eq!(
            drag_card_bounds(pointer, point(px(10.), px(5.))),
            rect(490., 395., DRAG_CARD.0, DRAG_CARD.1)
        );
    }
}
