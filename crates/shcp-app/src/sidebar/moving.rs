//! Moving a session, a host or a group to another place in the list with the pointer, the rest
//! making way as it goes. A host moves with its sessions, a group with its hosts and theirs.
use super::{Sidebar, SidebarEvent};
use crate::widgets::reorder::{Place, Reorder, Slot};
use gpui::prelude::*;
use gpui::{App, Bounds, Context, Pixels, Point, canvas, px};
use shcp_core::HostId;

/// A row of the list, as far as moving goes.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub(super) enum RowKey {
    /// The title of a part of the list: a group's header, or the one of the hosts without one.
    Title(Option<String>),
    Host(HostId),
    Session(u64),
}

/// What is on its way, and where the rest of the list was when it set off.
pub(super) enum Moving {
    /// A session among those of its host. The slots are the sessions, by tab id.
    Tab(Reorder),
    /// A host, with its sessions, among the hosts of every part of the list. The slots are
    /// `rows`, by place: the titles of the parts stay, and head them.
    Host { reorder: Reorder, rows: Vec<RowKey> },
    /// A group, with all it holds, among the groups. The slots are `groups`, by place.
    Group {
        reorder: Reorder,
        groups: Vec<String>,
    },
}

impl Moving {
    fn reorder(&self) -> &Reorder {
        match self {
            Moving::Tab(reorder) | Moving::Host { reorder, .. } | Moving::Group { reorder, .. } => {
                reorder
            }
        }
    }

    fn reorder_mut(&mut self) -> &mut Reorder {
        match self {
            Moving::Tab(reorder) | Moving::Host { reorder, .. } | Moving::Group { reorder, .. } => {
                reorder
            }
        }
    }

    /// The rows are on their way: draw again next frame.
    pub(super) fn gliding(&self) -> bool {
        self.reorder().moving()
    }
}

/// Where a row starts along the list, and how long it is.
fn span(area: &Bounds<Pixels>) -> (f32, f32) {
    (f32::from(area.origin.y), f32::from(area.size.height))
}

impl Sidebar {
    /// A row where it belongs while something moves, faint if it is what moves. At rest, it says
    /// where it is, for a drag to start from.
    pub(super) fn place_row<E: Styled + ParentElement>(&self, key: RowKey, row: E) -> E {
        let shift = self.shifts.get(&key).copied().unwrap_or(0.);
        let mut row = row;
        if shift != 0. {
            row = row.top(px(shift));
        }
        if self.lifted.contains(&key) {
            row = row.opacity(0.35);
        }
        let (bounds, at_rest) = (self.row_bounds.clone(), self.moving.is_none());
        row.child(
            canvas(
                move |area, _, _| {
                    if at_rest {
                        bounds.borrow_mut().insert(key, area);
                    }
                },
                |_, _, _, _| {},
            )
            .absolute()
            .size_full(),
        )
    }

    /// For this frame: how far each row is drawn from its place, and which rows move.
    pub(super) fn measure_moves(&mut self, cx: &App) {
        self.shifts.clear();
        self.lifted.clear();
        let Some(moving) = &self.moving else {
            return;
        };
        let reorder = moving.reorder();
        let mut rows: Vec<(RowKey, u64)> = Vec::new();
        let sessions_of = |host: &HostId| -> Vec<RowKey> {
            self.open
                .iter()
                .filter(|tab| tab.host == *host)
                .map(|tab| RowKey::Session(tab.id))
                .collect()
        };
        match moving {
            Moving::Tab(_) => {
                for tab in &self.open {
                    rows.push((RowKey::Session(tab.id), tab.id));
                }
            }
            Moving::Host { rows: keys, .. } => {
                for (slot, key) in keys.iter().enumerate() {
                    let slot = slot as u64;
                    if let RowKey::Host(host) = key {
                        rows.extend(sessions_of(host).into_iter().map(|row| (row, slot)));
                    }
                    rows.push((key.clone(), slot));
                }
            }
            Moving::Group { groups, .. } => {
                let sections = self.sections(cx);
                for (slot, group) in groups.iter().enumerate() {
                    let slot = slot as u64;
                    rows.push((RowKey::Title(Some(group.clone())), slot));
                    let hosts = sections
                        .iter()
                        .find(|(name, _)| name.as_ref() == Some(group))
                        .map(|(_, hosts)| hosts.clone())
                        .unwrap_or_default();
                    for host in hosts {
                        rows.extend(sessions_of(&host.id).into_iter().map(|row| (row, slot)));
                        rows.push((RowKey::Host(host.id), slot));
                    }
                }
            }
        }
        for (key, slot) in rows {
            if reorder.dragged() == slot {
                self.lifted.insert(key.clone());
            }
            self.shifts.insert(key, reorder.offset(slot));
        }
    }

    /// The pointer moved, with something on its way: what it would land among makes way.
    pub(super) fn follow_pointer(
        &mut self,
        pointer: Point<Pixels>,
        list: Bounds<Pixels>,
        cx: &mut Context<Self>,
    ) {
        let Some(moving) = &mut self.moving else {
            return;
        };
        let reorder = moving.reorder_mut();
        // Some way off the list still counts as over it.
        reorder.inside = list.dilate(px(24.)).contains(&pointer);
        if reorder.aim(f32::from(pointer.y)) {
            cx.notify();
        }
    }

    fn animated(&self, cx: &App) -> bool {
        self.state.read(cx).settings.animated_selection
    }

    /// A session sets off, among the sessions of its host.
    pub(super) fn start_tab_drag(&mut self, id: u64, cx: &mut Context<Self>) {
        let Some(host) = self
            .open
            .iter()
            .find(|tab| tab.id == id)
            .map(|tab| tab.host.clone())
        else {
            return;
        };
        let bounds = self.row_bounds.borrow();
        let slots: Option<Vec<Slot>> = self
            .open
            .iter()
            .filter(|tab| tab.host == host)
            .map(|tab| {
                let (start, len) = span(bounds.get(&RowKey::Session(tab.id))?);
                Some(Slot::item(tab.id, start, len))
            })
            .collect();
        drop(bounds);
        let animated = self.animated(cx);
        self.moving = slots
            .and_then(|slots| Reorder::start(id, slots, animated))
            .map(Moving::Tab);
        cx.notify();
    }

    /// A host sets off with its sessions, among the hosts of every part of the list.
    pub(super) fn start_host_drag(&mut self, host: &HostId, cx: &mut Context<Self>) {
        let sections = self.sections(cx);
        let bounds = self.row_bounds.borrow();
        let (mut rows, mut slots, mut dragged) = (Vec::new(), Vec::new(), None);
        for (part, (group, hosts)) in sections.iter().enumerate() {
            let title = RowKey::Title(group.clone());
            let Some((start, len)) = bounds.get(&title).map(span) else {
                return;
            };
            slots.push(Slot {
                id: rows.len() as u64,
                start,
                len,
                part,
                header: true,
            });
            rows.push(title);
            // A closed group shows only its title: what lands under it goes into it.
            if !self.shows_host(group.as_deref(), cx) {
                continue;
            }
            for config in hosts {
                let key = RowKey::Host(config.id.clone());
                let Some(area) = bounds.get(&key) else {
                    return;
                };
                let bottom = self
                    .open
                    .iter()
                    .filter(|tab| tab.host == config.id)
                    .filter_map(|tab| bounds.get(&RowKey::Session(tab.id)))
                    .map(|session| f32::from(session.bottom()))
                    .fold(f32::from(area.bottom()), f32::max);
                if config.id == *host {
                    dragged = Some(rows.len() as u64);
                }
                let start = f32::from(area.origin.y);
                slots.push(Slot {
                    id: rows.len() as u64,
                    start,
                    len: bottom - start,
                    part,
                    header: false,
                });
                rows.push(key);
            }
        }
        drop(bounds);
        let animated = self.animated(cx);
        self.moving = dragged
            .and_then(|dragged| Reorder::start(dragged, slots, animated))
            .map(|reorder| Moving::Host { reorder, rows });
        cx.notify();
    }

    /// A group sets off with all it holds, among the groups.
    pub(super) fn start_group_drag(&mut self, name: &str, cx: &mut Context<Self>) {
        let sections = self.sections(cx);
        let bounds = self.row_bounds.borrow();
        let (mut groups, mut slots) = (Vec::new(), Vec::new());
        for (group, hosts) in &sections {
            let Some(group) = group else {
                continue;
            };
            let Some(title) = bounds.get(&RowKey::Title(Some(group.clone()))) else {
                return;
            };
            let mut bottom = f32::from(title.bottom());
            if self.shows_host(Some(group), cx) {
                for config in hosts {
                    let rows = std::iter::once(RowKey::Host(config.id.clone())).chain(
                        self.open
                            .iter()
                            .filter(|tab| tab.host == config.id)
                            .map(|tab| RowKey::Session(tab.id)),
                    );
                    for row in rows {
                        if let Some(area) = bounds.get(&row) {
                            bottom = bottom.max(f32::from(area.bottom()));
                        }
                    }
                }
            }
            let start = f32::from(title.origin.y);
            slots.push(Slot::item(groups.len() as u64, start, bottom - start));
            groups.push(group.clone());
        }
        drop(bounds);
        let animated = self.animated(cx);
        self.moving = groups
            .iter()
            .position(|group| group == name)
            .and_then(|dragged| Reorder::start(dragged as u64, slots, animated))
            .map(|reorder| Moving::Group { reorder, groups });
        cx.notify();
    }

    /// Let go: over the list, what moved takes its new place.
    pub(super) fn finish_moving(&mut self, cx: &mut Context<Self>) {
        let Some(moving) = self.moving.take() else {
            return;
        };
        match moving {
            Moving::Tab(reorder) => {
                if let Some(place) = reorder.moved_to() {
                    cx.emit(SidebarEvent::ReorderTab(reorder.dragged(), place.index));
                }
            }
            Moving::Host { reorder, rows } => {
                if let (Some(place), Some(RowKey::Host(host))) =
                    (reorder.moved_to(), rows.get(reorder.dragged() as usize))
                {
                    self.put_host(host.clone(), place, cx);
                }
            }
            Moving::Group { reorder, groups } => {
                if let (Some(place), Some(group)) =
                    (reorder.moved_to(), groups.get(reorder.dragged() as usize))
                {
                    self.put_group(group.clone(), place.index, cx);
                }
            }
        }
        cx.notify();
    }

    /// A host goes to a place in a part of the list: next to the host there, in its group.
    fn put_host(&mut self, host: HostId, place: Place, cx: &mut Context<Self>) {
        let sections = self.sections(cx);
        let Some((group, members)) = sections.get(place.part) else {
            return;
        };
        let others: Vec<&HostId> = members
            .iter()
            .map(|member| &member.id)
            .filter(|id| **id != host)
            .collect();
        let target = match others.get(place.index) {
            Some(next) => Some(((*next).clone(), false)),
            None => others.last().map(|last| ((*last).clone(), true)),
        };
        let group = group.clone();
        self.state.update(cx, |state, cx| match target {
            Some((target, after)) => state.place_host(&host, &target, after, cx),
            None => state.set_host_group(&host, group, cx),
        });
        self.keep_selection_listed(cx);
    }

    /// A group goes to a place among the others.
    fn put_group(&mut self, group: String, index: usize, cx: &mut Context<Self>) {
        let others: Vec<String> = self
            .sections(cx)
            .into_iter()
            .filter_map(|(name, _)| name)
            .filter(|name| *name != group)
            .collect();
        let (target, after) = match (others.get(index), others.last()) {
            (Some(next), _) => (next.clone(), false),
            (None, Some(last)) => (last.clone(), true),
            (None, None) => return,
        };
        self.state.update(cx, |state, cx| {
            state.place_group(&group, &target, after, cx)
        });
    }

    /// Escape: what moved goes back to its place. Whether something was moving.
    pub(crate) fn cancel_reorder(&mut self, cx: &mut Context<Self>) -> bool {
        let moving = self.moving.take().is_some();
        if moving {
            cx.notify();
        }
        moving
    }
}
