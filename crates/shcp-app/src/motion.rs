//! The app's motion: one curve, the timings that use it, and the system's animation switch.
//! gpui can transform only svgs, so an entrance is a fade plus a short rise, never a scale.
//! Nothing started from the keyboard animates: palette, tabs, sidebar and dock change at once.
//! What the mouse opens glides, if the setting allows it.
use gpui::prelude::*;
use gpui::{Animation, AnimationExt as _, ElementId, px, relative};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// When an element moves, for how long and how far.
#[derive(Clone, Copy)]
pub(crate) struct Beat {
    delay_ms: u64,
    duration_ms: u64,
    rise: f32,
}

impl Beat {
    pub(crate) const fn new(duration_ms: u64, rise: f32) -> Self {
        Self {
            delay_ms: 0,
            duration_ms,
            rise,
        }
    }

    pub(crate) const fn after(mut self, delay_ms: u64) -> Self {
        self.delay_ms = delay_ms;
        self
    }

    pub(crate) const fn total(self) -> Duration {
        Duration::from_millis(self.delay_ms + self.duration_ms)
    }

    pub(crate) fn animation(self) -> Animation {
        let delay = self.delay_ms as f32;
        let duration = self.duration_ms as f32;
        let total = delay + duration;
        Animation::new(self.total())
            .with_easing(move |t| ease_out(((t * total - delay) / duration).clamp(0., 1.)))
    }
}

pub(crate) const MENU: Beat = Beat::new(110, 0.);
pub(crate) const MODAL: Beat = Beat::new(200, 6.);
pub(crate) const NOTICE_IN: Beat = Beat::new(180, 6.);
pub(crate) const NOTICE_OUT: Beat = Beat::new(120, 4.);
/// The content of a tab when it shows another way.
pub(crate) const FACE: Beat = Beat::new(170, 0.);

/// How long a panel takes to open or close.
const REVEAL: Duration = Duration::from_millis(210);
/// How long rows of a list take to fold. Slower than a panel: a lone row is small, and at the end
/// of the list nothing under it moves to show that it went.
const FOLD: Duration = Duration::from_millis(260);

/// A panel opening and closing: how much of it shows, from 0 to 1. Asked per frame, and can change
/// its mind halfway, going back from where it is.
pub(crate) struct Reveal {
    open: bool,
    from: f32,
    started: Option<Instant>,
    armed: bool,
    duration: Duration,
    ease: fn(f32) -> f32,
}

impl Reveal {
    pub(crate) fn new(open: bool) -> Self {
        Self {
            open,
            from: if open { 1. } else { 0. },
            started: None,
            armed: false,
            duration: REVEAL,
            ease: ease_out,
        }
    }

    /// For rows of a list folding under what holds them.
    pub(crate) fn rows(open: bool) -> Self {
        Self {
            duration: FOLD,
            ease: ease_out_soft,
            ..Self::new(open)
        }
    }

    /// The next change came from the mouse, so it may glide.
    pub(crate) fn arm(&mut self) {
        self.armed = true;
    }

    /// Says what it should be now. Once per frame.
    pub(crate) fn set(&mut self, open: bool) {
        let armed = std::mem::take(&mut self.armed);
        if open == self.open {
            return;
        }
        let now = Instant::now();
        self.from = self.value_at(now);
        self.open = open;
        self.started = (armed && !animations_off()).then_some(now);
    }

    pub(crate) fn value(&self) -> f32 {
        self.value_at(Instant::now())
    }

    pub(crate) fn moving(&self) -> bool {
        self.started
            .is_some_and(|started| started.elapsed() < self.duration)
    }

    fn value_at(&self, now: Instant) -> f32 {
        let to = if self.open { 1. } else { 0. };
        let Some(started) = self.started else {
            return to;
        };
        let t = now.duration_since(started).as_secs_f32() / self.duration.as_secs_f32();
        self.from + (to - self.from) * (self.ease)(t)
    }
}

/// How long the tools of a card take to come and go with the pointer.
const FADE: Duration = Duration::from_millis(140);

/// What shows while the pointer is over something, like the tools of a card: it fades in and out,
/// and a pointer going back halfway turns it around from where it is.
pub(crate) struct HoverFade {
    hovered: bool,
    reveal: Reveal,
}

impl Default for HoverFade {
    fn default() -> Self {
        Self {
            hovered: false,
            reveal: Reveal {
                duration: FADE,
                ..Reveal::new(false)
            },
        }
    }
}

impl HoverFade {
    /// The pointer came or went. It fades if the setting allows it, otherwise it changes at once.
    pub(crate) fn hover(&mut self, hovered: bool, animated: bool) {
        if hovered != self.hovered && animated {
            self.reveal.arm();
        }
        self.hovered = hovered;
    }

    /// How much of it shows now, from 0 to 1. `hold` keeps it in sight without the pointer. Once
    /// per frame; while `moving`, ask for another.
    pub(crate) fn value(&mut self, hold: bool) -> f32 {
        self.reveal.set(self.hovered || hold);
        self.reveal.value()
    }

    pub(crate) fn moving(&self) -> bool {
        self.reveal.moving()
    }
}

/// A panel is on its way: terminals keep their size meanwhile, and take the new one once at the end.
/// Otherwise each frame would reflow them and send the server one more resize.
static LAYOUT_MOVING: AtomicBool = AtomicBool::new(false);

pub(crate) fn set_layout_moving(moving: bool) {
    LAYOUT_MOVING.store(moving, Ordering::Relaxed);
}

pub(crate) fn layout_moving() -> bool {
    LAYOUT_MOVING.load(Ordering::Relaxed)
}

/// Already at its end: for what shares an element with something animated but must not move.
pub(crate) fn settled() -> Animation {
    Animation::new(Duration::from_millis(1)).with_easing(|_| 1.)
}

pub(crate) fn enter<E: Styled + IntoElement + 'static>(
    element: E,
    id: impl Into<ElementId>,
    beat: Beat,
) -> impl IntoElement {
    slide(element, id, beat, true)
}

pub(crate) fn exit<E: Styled + IntoElement + 'static>(
    element: E,
    id: impl Into<ElementId>,
    beat: Beat,
) -> impl IntoElement {
    slide(element, id, beat, false)
}

/// Fades and rises in, or the other way out.
fn slide<E: Styled + IntoElement + 'static>(
    element: E,
    id: impl Into<ElementId>,
    beat: Beat,
    entering: bool,
) -> impl IntoElement {
    let rise = travel(beat);
    element.with_animation(id, beat.animation(), move |element, delta| {
        let (shown, away) = if entering {
            (delta, 1. - delta)
        } else {
            (1. - delta, delta)
        };
        offset(element.opacity(shown), rise, away)
    })
}

/// A bar running along its parent, for as long as something is under way and nobody can say how
/// much is left. With animations off it stays put and fills the parent.
pub(crate) fn sweep<E: Styled + IntoElement + 'static>(
    element: E,
    id: impl Into<ElementId>,
) -> impl IntoElement {
    const WIDTH: f32 = 0.45;
    let still = animations_off();
    element.with_animation(
        id,
        Animation::new(Duration::from_millis(1100)).repeat(),
        move |element, delta| {
            if still {
                return element.left_0().right_0();
            }
            element
                .left(relative(delta * (1. + WIDTH) - WIDTH))
                .w(relative(WIDTH))
        },
    )
}

/// How firmly the edge leading a pill's move, and the one following it, are pulled to their place.
/// The difference is what stretches the pill along its way.
const PILL_LEAD: f32 = 52.;
const PILL_TRAIL: f32 = 32.;
/// Longest it gets on its way, for its size: a far move stretches no more than a near one.
const PILL_MAX_STRETCH: f32 = 1.7;
/// Under 1 it passes its place by a little, and comes back.
const PILL_DAMPING: f32 = 0.72;
/// Longest step of one calculation, for a stable result on a slow frame.
const PILL_STEP: f32 = 1. / 240.;
/// Longest time one frame counts for: after a stall, the pill goes on from where it was.
const PILL_MAX_FRAME: f32 = 0.05;

#[derive(Clone, Copy, Default)]
struct Spring {
    position: f32,
    velocity: f32,
}

/// Where a pill is, and where it is going. Its edges move on their own: left, top, right, bottom.
/// Going somewhere else halfway keeps its speed, so a move can be interrupted at any time.
#[derive(Default)]
pub(crate) struct PillMotion {
    armed: bool,
    target: Option<[f32; 4]>,
    springs: [Spring; 4],
    firmness: [f32; 4],
    moving: bool,
    last: Option<Instant>,
}

impl PillMotion {
    pub(crate) fn arm(&mut self) {
        self.armed = true;
    }

    /// Takes in where the current item is. Once per frame: being armed lasts for one.
    pub(crate) fn aim(&mut self, target: Option<[f32; 4]>) {
        self.aim_with(target, animations_off());
    }

    /// `still`: the system asks for no motion, so the pill goes where it belongs at once.
    fn aim_with(&mut self, target: Option<[f32; 4]>, still: bool) {
        let armed = std::mem::take(&mut self.armed);
        if target == self.target {
            return;
        }
        let previous = self.target.is_some();
        self.target = target;
        let Some(target) = target else {
            self.moving = false;
            return;
        };
        if !armed || !previous || still {
            self.springs = target.map(|position| Spring {
                position,
                velocity: 0.,
            });
            self.moving = false;
            return;
        }
        // On each axis, the edge on the side it is heading to leads.
        let middle = |low: usize, high: usize, edges: [f32; 2]| {
            edges[0] + edges[1] - self.springs[low].position - self.springs[high].position
        };
        let (across, along) = (
            middle(0, 2, [target[0], target[2]]),
            middle(1, 3, [target[1], target[3]]),
        );
        let firm = |leads: bool| if leads { PILL_LEAD } else { PILL_TRAIL };
        self.firmness = [
            firm(across < 0.),
            firm(along < 0.),
            firm(across > 0.),
            firm(along > 0.),
        ];
        self.moving = true;
        self.last = None;
    }

    /// Where to paint it now, if anywhere, and whether another frame is needed.
    pub(crate) fn frame(&mut self, now: Instant) -> (Option<[f32; 4]>, bool) {
        if self.moving {
            let elapsed = self
                .last
                .map_or(0., |last| now.duration_since(last).as_secs_f32());
            self.last = Some(now);
            self.advance(elapsed.min(PILL_MAX_FRAME));
        }
        let edges = self.springs.map(|spring| spring.position);
        (self.target.map(|_| edges), self.moving)
    }

    fn advance(&mut self, mut elapsed: f32) {
        let Some(target) = self.target else {
            return;
        };
        while elapsed > 0. {
            let step = elapsed.min(PILL_STEP);
            elapsed -= step;
            for (index, spring) in self.springs.iter_mut().enumerate() {
                let firmness = self.firmness[index];
                let pull = firmness * firmness * (target[index] - spring.position);
                let drag = 2. * PILL_DAMPING * firmness * spring.velocity;
                spring.velocity += (pull - drag) * step;
                spring.position += spring.velocity * step;
            }
            for (low, high) in [(0, 2), (1, 3)] {
                let longest = (target[high] - target[low]) * PILL_MAX_STRETCH;
                let excess = self.springs[high].position - self.springs[low].position - longest;
                if excess <= 0. {
                    continue;
                }
                // The edge that follows gets dragged along.
                if self.firmness[high] > self.firmness[low] {
                    self.springs[low].position += excess;
                    self.springs[low].velocity = self.springs[high].velocity;
                } else {
                    self.springs[high].position -= excess;
                    self.springs[high].velocity = self.springs[low].velocity;
                }
            }
        }
        let arrived =
            self.springs.iter().zip(target).all(|(spring, to)| {
                (spring.position - to).abs() < 0.25 && spring.velocity.abs() < 6.
            });
        if arrived {
            self.springs = target.map(|position| Spring {
                position,
                velocity: 0.,
            });
            self.moving = false;
        }
    }
}

fn travel(beat: Beat) -> f32 {
    if animations_off() { 0. } else { beat.rise }
}

/// Without a rise the element keeps its own position: it may be absolute.
fn offset<E: Styled>(element: E, rise: f32, progress: f32) -> E {
    if rise > 0. {
        element.relative().top(px(rise * progress))
    } else {
        element
    }
}

/// cubic-bezier(0.23, 1, 0.32, 1): starts at once, so the interface answers immediately.
fn ease_out(t: f32) -> f32 {
    if t <= 0. {
        return 0.;
    }
    if t >= 1. {
        return 1.;
    }
    let bezier = |p1: f32, p2: f32, s: f32| {
        let r = 1. - s;
        3. * r * r * s * p1 + 3. * r * s * s * p2 + s * s * s
    };
    let (mut low, mut high) = (0f32, 1f32);
    for _ in 0..16 {
        let middle = (low + high) / 2.;
        if bezier(0.23, 0.32, middle) < t {
            low = middle;
        } else {
            high = middle;
        }
    }
    bezier(1., 1., (low + high) / 2.).clamp(0., 1.)
}

/// Also starts at once, but keeps something of the way for later: what is small stays in sight.
fn ease_out_soft(t: f32) -> f32 {
    let left = 1. - t.clamp(0., 1.);
    1. - left * left
}

/// "Animation effects" is off in Windows: elements fade, but stay where they are.
fn animations_off() -> bool {
    use std::ffi::c_void;

    const SPI_GETCLIENTAREAANIMATION: u32 = 0x1042;

    #[link(name = "user32")]
    unsafe extern "system" {
        fn SystemParametersInfoW(action: u32, param: u32, value: *mut c_void, ini: u32) -> i32;
    }

    let mut enabled: i32 = 1;
    // SAFETY: this action writes one BOOL through the pointer, which outlives the call.
    let read = unsafe {
        SystemParametersInfoW(SPI_GETCLIENTAREAANIMATION, 0, (&raw mut enabled).cast(), 0)
    };
    read != 0 && enabled == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_reveal_glides_only_when_armed() {
        let mut reveal = Reveal::new(false);
        reveal.set(true);
        assert_eq!(reveal.value(), 1.);
        assert!(!reveal.moving());
        reveal.arm();
        reveal.set(false);
        if !animations_off() {
            assert!(reveal.moving());
            let started = reveal.started.unwrap();
            assert!(reveal.value_at(started) > 0.99);
            assert_eq!(reveal.value_at(started + REVEAL), 0.);
        }
    }

    #[test]
    fn folding_rows_take_longer_than_a_panel_and_still_arrive() {
        let mut rows = Reveal::rows(true);
        let mut panel = Reveal::new(true);
        let now = Instant::now();
        rows.from = rows.value_at(now);
        rows.open = false;
        rows.started = Some(now);
        panel.from = panel.value_at(now);
        panel.open = false;
        panel.started = Some(now);
        let early = now + Duration::from_millis(50);
        assert!(rows.value_at(early) > panel.value_at(early));
        assert_eq!(rows.value_at(now + FOLD), 0.);
    }

    const FROM: [f32; 4] = [0., 0., 100., 28.];
    const TO: [f32; 4] = [300., 0., 400., 28.];
    const FRAME: f32 = 1. / 60.;

    fn pill_at_from() -> PillMotion {
        let mut pill = PillMotion::default();
        pill.aim_with(Some(FROM), false);
        pill
    }

    fn step(pill: &mut PillMotion) -> [f32; 4] {
        pill.advance(FRAME);
        pill.springs.map(|spring| spring.position)
    }

    #[test]
    fn a_pill_first_appears_in_place() {
        let mut pill = PillMotion::default();
        pill.arm();
        pill.aim_with(Some(FROM), false);
        assert_eq!(pill.frame(Instant::now()), (Some(FROM), false));
    }

    #[test]
    fn without_the_mouse_a_pill_is_at_once_where_it_belongs() {
        let mut pill = pill_at_from();
        pill.aim_with(Some(TO), false);
        assert_eq!(pill.frame(Instant::now()), (Some(TO), false));
    }

    #[test]
    fn being_armed_lasts_one_frame() {
        let mut pill = pill_at_from();
        pill.arm();
        pill.aim_with(Some(FROM), false);
        pill.aim_with(Some(TO), false);
        assert!(!pill.moving);
    }

    #[test]
    fn a_moving_pill_stretches_arrives_and_stops() {
        let mut pill = pill_at_from();
        pill.arm();
        pill.aim_with(Some(TO), false);
        assert!(pill.moving);
        let (mut widest, mut frames) = (0f32, 0);
        while pill.moving {
            let [left, _, right, _] = step(&mut pill);
            widest = widest.max(right - left);
            frames += 1;
            assert!(frames < 60, "it should settle within a second");
        }
        assert!(widest > 130., "only {widest} wide on its way");
        assert!(frames < 24, "took {frames} frames");
        assert_eq!(pill.frame(Instant::now()), (Some(TO), false));
    }

    #[test]
    fn a_moving_pill_passes_its_place_by_little() {
        let mut pill = pill_at_from();
        pill.arm();
        pill.aim_with(Some(TO), false);
        let mut furthest = 0f32;
        while pill.moving {
            furthest = furthest.max(step(&mut pill)[2]);
        }
        assert!(furthest > 400.);
        assert!(furthest < 415., "went up to {furthest}");
    }

    #[test]
    fn a_pill_can_turn_back_halfway_without_jumping() {
        let mut pill = pill_at_from();
        pill.arm();
        pill.aim_with(Some(TO), false);
        for _ in 0..4 {
            step(&mut pill);
        }
        let before = pill.springs[0];
        assert!(before.velocity > 0.);
        pill.arm();
        pill.aim_with(Some(FROM), false);
        assert_eq!(pill.springs[0].position, before.position);
        assert_eq!(pill.springs[0].velocity, before.velocity);
        while pill.moving {
            step(&mut pill);
        }
        assert_eq!(pill.frame(Instant::now()).0, Some(FROM));
    }

    #[test]
    fn a_far_move_stretches_a_pill_no_more_than_allowed() {
        for to in [[0., 900., 100., 928.], [0., -900., 100., -872.]] {
            let mut pill = pill_at_from();
            pill.arm();
            pill.aim_with(Some(to), false);
            let mut tallest = 0f32;
            while pill.moving {
                let [_, top, _, bottom] = step(&mut pill);
                tallest = tallest.max(bottom - top);
            }
            assert!(tallest <= 28. * PILL_MAX_STRETCH + 0.01, "{tallest} tall");
            assert_eq!(pill.frame(Instant::now()).0, Some(to));
        }
    }

    #[test]
    fn a_long_stall_does_not_throw_a_pill_off() {
        let mut pill = pill_at_from();
        pill.arm();
        pill.aim_with(Some(TO), false);
        pill.advance(PILL_MAX_FRAME);
        assert!(pill.springs[2].position < 400.);
    }

    #[test]
    fn ease_out_keeps_its_ends_and_only_moves_forward() {
        assert_eq!(ease_out(0.), 0.);
        assert_eq!(ease_out(1.), 1.);
        let steps: Vec<f32> = (0..=100).map(|i| ease_out(i as f32 / 100.)).collect();
        assert!(steps.windows(2).all(|pair| pair[0] <= pair[1]));
    }

    #[test]
    fn ease_out_covers_most_of_the_way_early() {
        assert!(ease_out(0.2) > 0.5);
    }

    #[test]
    fn a_delayed_beat_waits_and_then_arrives() {
        let beat = Beat::new(100, 0.).after(100);
        let easing = beat.animation().easing;
        assert_eq!(easing(0.25), 0.);
        assert_eq!(easing(0.5), 0.);
        assert!(easing(0.75) > 0.);
        assert_eq!(easing(1.), 1.);
    }
}
