//! Moving one item of a list to another place with the pointer: the others make way as it goes,
//! as the icons of a phone do. The list keeps its layout during the drag; each item is drawn
//! shifted to where it would be, and only a release over the list changes the order.
use super::icon::{Icon, IconName};
use crate::motion::Reveal;
use crate::theme::ActiveTheme as _;
use gpui::prelude::*;
use gpui::{
    App, Context, DispatchPhase, Hsla, MouseButton, MouseUpEvent, Pixels, Point, SharedString,
    Window, canvas, div, px,
};
use std::collections::HashMap;

/// Where an item was before the drag, along the list: where it starts, and how long it is. Items
/// can be of any length. A list can be made of parts, each starting with a header that is not
/// moved itself: an item goes into the part of what it lands after.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Slot {
    pub id: u64,
    pub start: f32,
    pub len: f32,
    /// The part of the list it is in.
    pub part: usize,
    /// It heads its part.
    pub header: bool,
}

impl Slot {
    /// An item of a list of one part.
    pub(crate) fn item(id: u64, start: f32, len: f32) -> Self {
        Self {
            id,
            start,
            len,
            part: 0,
            header: false,
        }
    }
}

/// Where an item goes: a part of the list, and its place among the items of that part.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Place {
    pub part: usize,
    pub index: usize,
}

/// Where the dragged item would go among the others: before every one whose middle, as it was
/// before the drag, is past the pointer. Measured against places that do not move, it cannot
/// flicker between two. Never before the header of the first part.
fn target_for(slots: &[Slot], dragged: u64, along: f32) -> usize {
    let mut others = slots.iter().filter(|slot| slot.id != dragged).peekable();
    let first_is_header = others.peek().is_some_and(|slot| slot.header);
    let before = others
        .filter(|slot| slot.start + slot.len / 2. < along)
        .count();
    before.max(usize::from(first_is_header))
}

/// The part, and the place in it, of the dragged item put at `target` among the others.
fn place_of(slots: &[Slot], dragged: u64, target: usize) -> Option<Place> {
    let moved = slots.iter().find(|slot| slot.id == dragged)?;
    let others: Vec<&Slot> = slots.iter().filter(|slot| slot.id != dragged).collect();
    let part = target
        .checked_sub(1)
        .and_then(|before| others.get(before))
        .map_or(moved.part, |slot| slot.part);
    let index = others[..target.min(others.len())]
        .iter()
        .filter(|slot| slot.part == part && !slot.header)
        .count();
    Some(Place { part, index })
}

/// How far each item is from its own place with the dragged one at `target` among the others.
/// What follows the dragged item closes up by the room it took, the gap after it included; what
/// is at or past its new place makes that room again there.
fn offsets(slots: &[Slot], dragged: u64, target: usize) -> HashMap<u64, f32> {
    let Some(from) = slots.iter().position(|slot| slot.id == dragged) else {
        return HashMap::new();
    };
    let moved = slots[from];
    let room = match (slots.get(from + 1), from.checked_sub(1).map(|i| slots[i])) {
        (Some(next), _) => next.start - moved.start,
        (None, Some(previous)) => moved.start + moved.len - previous.start - previous.len,
        (None, None) => moved.len,
    };
    let others: Vec<(usize, &Slot)> = slots
        .iter()
        .enumerate()
        .filter(|(index, _)| *index != from)
        .collect();
    // Where an item starts once the dragged one is out of the list.
    let closed = |index: usize, slot: &Slot| slot.start - if index > from { room } else { 0. };
    let mut offsets: HashMap<u64, f32> = others
        .iter()
        .enumerate()
        .map(|(place, (index, slot))| {
            let opening = if place >= target { room } else { 0. };
            (slot.id, closed(*index, slot) - slot.start + opening)
        })
        .collect();
    let to = match others.get(target) {
        Some((index, slot)) => closed(*index, slot),
        None => others.last().map_or(moved.start, |(index, slot)| {
            closed(*index, slot) + slot.len + (room - moved.len)
        }),
    };
    offsets.insert(dragged, to - moved.start);
    offsets
}

/// An item on its way to another place in its list.
pub(crate) struct Reorder {
    dragged: u64,
    slots: Vec<Slot>,
    /// Where it would go among the others.
    target: usize,
    /// The pointer is over the list: a release there puts the item in its new place.
    pub(crate) inside: bool,
    from: HashMap<u64, f32>,
    to: HashMap<u64, f32>,
    glide: Reveal,
    animated: bool,
}

impl Reorder {
    /// `slots`: the items of the list where they were drawn, in their order.
    pub(crate) fn start(dragged: u64, slots: Vec<Slot>, animated: bool) -> Option<Self> {
        let target = slots.iter().position(|slot| slot.id == dragged)?;
        Some(Self {
            dragged,
            slots,
            target,
            inside: true,
            from: HashMap::new(),
            to: HashMap::new(),
            glide: Reveal::new(true),
            animated,
        })
    }

    pub(crate) fn dragged(&self) -> u64 {
        self.dragged
    }

    /// Follows the pointer, at `along` on the line of the list. Whether the others moved.
    pub(crate) fn aim(&mut self, along: f32) -> bool {
        let target = target_for(&self.slots, self.dragged, along);
        if target == self.target {
            return false;
        }
        // From where they are drawn now: a change of course mid-way does not jump.
        self.from = self
            .slots
            .iter()
            .map(|slot| (slot.id, self.offset(slot.id)))
            .collect();
        self.target = target;
        self.to = offsets(&self.slots, self.dragged, target);
        self.glide = Reveal::new(false);
        if self.animated {
            self.glide.arm();
        }
        self.glide.set(true);
        true
    }

    /// How far from its own place the item is drawn now.
    pub(crate) fn offset(&self, id: u64) -> f32 {
        let t = self.glide.value();
        let from = self.from.get(&id).copied().unwrap_or(0.);
        let to = self.to.get(&id).copied().unwrap_or(0.);
        from + (to - from) * t
    }

    /// The items are on their way: draw again next frame.
    pub(crate) fn moving(&self) -> bool {
        self.glide.moving()
    }

    /// The new place of the item, if a release now changes it.
    pub(crate) fn moved_to(&self) -> Option<Place> {
        let from = self.slots.iter().position(|slot| slot.id == self.dragged)?;
        let was = place_of(&self.slots, self.dragged, from)?;
        let now = place_of(&self.slots, self.dragged, self.target)?;
        (self.inside && now != was).then_some(now)
    }
}

/// Calls `release` when the left button is let go, wherever the pointer is: nothing over it can
/// keep the drop from its list. For a frame in which a drag is going on.
pub(crate) fn on_release(release: impl Fn(&mut Window, &mut App) + 'static) -> impl IntoElement {
    canvas(
        |_, _, _| {},
        move |_, _, window, _| {
            window.on_mouse_event(move |event: &MouseUpEvent, phase, window, cx| {
                if phase == DispatchPhase::Capture && event.button == MouseButton::Left {
                    release(window, cx);
                }
            });
        },
    )
    .absolute()
    .size_0()
}

/// What follows the pointer while a tab is dragged: its icon in the host's color, and its title.
#[derive(Clone)]
pub(crate) struct TabChip {
    /// The tab.
    pub id: u64,
    pub icon: IconName,
    pub color: Hsla,
    pub title: SharedString,
    /// Where the row was grabbed, from its top left corner.
    pub grabbed: Point<Pixels>,
}

impl Render for TabChip {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        // gpui draws the drag where the row was grabbed, which can be far along a wide row: the
        // chip comes to the pointer instead.
        let left = (f32::from(self.grabbed.x) - 20.).max(0.);
        let top = (f32::from(self.grabbed.y) - 14.).max(0.);
        div().pl(px(left)).pt(px(top)).child(
            div()
                .h(px(28.))
                .max_w(px(220.))
                .px(px(10.))
                .flex()
                .items_center()
                .gap(px(7.))
                .rounded(px(crate::theme::RADIUS_CONTROL))
                .map(|this| crate::panel::floating(this, theme, theme.strong_border))
                .shadow(crate::theme::float_shadow())
                .font_family(crate::theme::ui_font())
                .text_size(crate::theme::ui_px(13.))
                .text_color(theme.text)
                .child(Icon::new(self.icon).size(px(14.)).color(self.color))
                .child(div().min_w_0().truncate().child(self.title.clone())),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Three rows of 30 with gaps of 2, from 100.
    fn rows() -> Vec<Slot> {
        [1, 2, 3]
            .into_iter()
            .enumerate()
            .map(|(index, id)| Slot::item(id, 100. + index as f32 * 32., 30.))
            .collect()
    }

    /// Two parts: a header of 20 over a block of 30 and one of 62, then a header of 20 over a
    /// block of 30. Gaps of 2.
    fn parts() -> Vec<Slot> {
        let slot = |id, start, len, part, header| Slot {
            id,
            start,
            len,
            part,
            header,
        };
        vec![
            slot(10, 0., 20., 0, true),
            slot(1, 22., 30., 0, false),
            slot(2, 54., 62., 0, false),
            slot(20, 118., 20., 1, true),
            slot(3, 140., 30., 1, false),
        ]
    }

    #[test]
    fn the_place_is_counted_against_where_the_others_were() {
        let rows = rows();
        assert_eq!(target_for(&rows, 1, 100.), 0);
        // Past the middle of the second row, the first one goes after it.
        assert_eq!(target_for(&rows, 1, 148.), 1);
        assert_eq!(target_for(&rows, 1, 400.), 2);
        assert_eq!(target_for(&rows, 3, 0.), 0);
    }

    #[test]
    fn the_others_make_way_and_keep_their_gaps() {
        let rows = rows();
        let moved = offsets(&rows, 1, 2);
        assert_eq!(moved[&2], -32.);
        assert_eq!(moved[&3], -32.);
        assert_eq!(moved[&1], 64.);
        let still = offsets(&rows, 2, 1);
        assert!(still.values().all(|offset| *offset == 0.));
        let up = offsets(&rows, 3, 0);
        assert_eq!((up[&1], up[&2], up[&3]), (32., 32., -64.));
    }

    #[test]
    fn a_tall_item_moves_the_others_by_its_whole_height() {
        let parts = parts();
        // The block of 62 goes to the end, into the second part.
        let target = target_for(&parts, 2, 500.);
        assert_eq!(target, 4);
        let moved = offsets(&parts, 2, target);
        assert_eq!(moved[&20], -64.);
        assert_eq!(moved[&3], -64.);
        assert_eq!(moved[&1], 0.);
        // Under the last block, with the same gap.
        assert_eq!(moved[&2], 140. - 64. + 30. + 2. - 54.);
        assert_eq!(
            place_of(&parts, 2, target),
            Some(Place { part: 1, index: 1 })
        );
    }

    #[test]
    fn an_item_goes_into_the_part_it_lands_in() {
        let parts = parts();
        // Under the second header: first in the second part.
        assert_eq!(target_for(&parts, 1, 130.), 3);
        assert_eq!(place_of(&parts, 1, 3), Some(Place { part: 1, index: 0 }));
        // Above the second header: last in the first part.
        assert_eq!(place_of(&parts, 3, 3), Some(Place { part: 0, index: 2 }));
        // Never above the first header.
        assert_eq!(target_for(&parts, 3, -50.), 1);
        assert_eq!(place_of(&parts, 3, 1), Some(Place { part: 0, index: 0 }));
    }

    #[test]
    fn a_release_where_it_was_or_outside_changes_nothing() {
        let mut reorder = Reorder::start(1, rows(), false).unwrap();
        assert_eq!(reorder.moved_to(), None);
        assert!(reorder.aim(148.));
        assert_eq!(reorder.moved_to(), Some(Place { part: 0, index: 1 }));
        assert!(!reorder.aim(150.));
        reorder.inside = false;
        assert_eq!(reorder.moved_to(), None);
        assert!(Reorder::start(9, rows(), false).is_none());
        // The first of the second part, over the middle of its header, leaves its part; under
        // it, it is where it was.
        let mut first = Reorder::start(3, parts(), false).unwrap();
        assert!(first.aim(127.));
        assert_eq!(first.moved_to(), Some(Place { part: 0, index: 2 }));
        assert!(first.aim(129.));
        assert_eq!(first.moved_to(), None);
    }
}
