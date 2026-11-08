//! What a terminal or file view shows while its session is not usable: a card with the steps of
//! the connection and whatever the session needs from the user.
use crate::login_prompt::LoginPrompt;
use crate::motion::{self, Beat};
use crate::state::SessionInfo;
use crate::theme::{PANEL_INNER_RADIUS, Theme, ui_font, ui_px};
use crate::widgets::button::{Button, ButtonStyle};
use crate::widgets::icon::{Icon, IconName};
use gpui::prelude::*;
use gpui::{
    AnyElement, App, ClickEvent, Div, Entity, EntityId, FocusHandle, FontWeight, Hsla, MouseButton,
    Window, div, px,
};
use shcp_core::{ConnectStep, HostConfig, SessionState};

/// Late enough for a connection that needs nothing to reach its shell without a flash.
const CARD: Beat = Beat::new(160, 4.).after(120);
const STEP_TEXT: Beat = Beat::new(140, 3.);
const CARD_WIDTH: f32 = 320.;
const PROMPT: Beat = Beat::new(180, 4.);
/// A step of the connection as a line of text.
#[derive(Clone)]
struct Row {
    now: String,
    detail: Option<String>,
}

/// Where the connection stands, ready to be drawn.
#[derive(Clone, Default)]
pub struct Steps {
    position: u32,
    /// The steps shown have reached the connection: what it asks of the user can appear.
    caught_up: bool,
    /// Empty until the session reports its first step: there is nothing to count yet.
    rows: Vec<Row>,
    text: String,
    detail: Option<String>,
}

pub enum Overlay {
    Progress {
        steps: Steps,
    },
    /// The connection is up, and the terminal is waiting for its shell.
    StartingShell {
        steps: Steps,
    },
    /// `own`: this view draws the prompt; the others say where it is.
    Login {
        steps: Steps,
        prompt: Entity<LoginPrompt>,
        own: bool,
    },
    Failed {
        steps: Steps,
        message: String,
        hint: Option<String>,
    },
    Closed(String),
    /// The connection was lost and another attempt is on its way.
    Reconnecting {
        attempt: u32,
    },
}

/// A lost connection being retried reads differently from one that needs the user.
pub fn with_reconnect(overlay: Option<Overlay>, attempt: Option<u32>) -> Option<Overlay> {
    match (overlay, attempt) {
        (Some(Overlay::Closed(_) | Overlay::Failed { .. }), Some(attempt)) => {
            Some(Overlay::Reconnecting { attempt })
        }
        (overlay, _) => overlay,
    }
}

/// What a step is called.
fn step_name(step: ConnectStep) -> &'static str {
    match step {
        ConnectStep::Resolving => "Resolving host",
        ConnectStep::Connecting => "Connecting",
        ConnectStep::StartingProxy => "Opening proxy tunnel",
        ConnectStep::Handshake => "Negotiating SSH",
        ConnectStep::VerifyingHostKey => "Verifying host key",
        ConnectStep::Authenticating => "Authenticating",
        ConnectStep::OpeningForwards => "Opening port forwards",
    }
}

/// The step the session reached last, among all of them.
pub fn steps_of(session: Option<&SessionInfo>) -> Steps {
    let Some((info, (_, progress))) =
        session.and_then(|info| info.step.as_ref().map(|step| (info, step)))
    else {
        return Steps {
            caught_up: true,
            text: "Connecting…".to_string(),
            ..Steps::default()
        };
    };
    // On a jump host, a step is not about the host named on the card.
    let named = |hop: &Option<String>, name: &str| match hop {
        Some(hop) => format!("{hop}: {name}"),
        None => name.to_string(),
    };
    let position = info.shown_step;
    let rows: Vec<Row> = progress
        .plan
        .iter()
        .enumerate()
        .map(|(index, planned)| Row {
            now: named(&planned.hop, step_name(planned.step)),
            detail: info.step_details.get(&(index as u32)).cloned(),
        })
        .collect();
    let current = rows.get(position as usize);
    let text = current.map_or_else(|| "Connecting…".to_string(), |row| format!("{}…", row.now));
    let detail = current.and_then(|row| row.detail.clone());
    Steps {
        position,
        caught_up: position >= progress.position,
        rows,
        text,
        detail,
    }
}

/// The last step of a terminal, which the session knows nothing about: it comes after all of its.
pub fn shell_steps(session: Option<&SessionInfo>) -> Steps {
    let rows = steps_of(session).rows;
    Steps {
        position: rows.len() as u32,
        caught_up: true,
        rows,
        text: "Starting shell…".to_string(),
        detail: None,
    }
}

/// `None` when the session is connected.
pub fn overlay_for(session: Option<&SessionInfo>) -> Option<Overlay> {
    let Some(info) = session else {
        return Some(Overlay::Closed("Not connected".into()));
    };
    let steps = steps_of(Some(info));
    match &info.state {
        SessionState::Connected => None,
        SessionState::Connecting { .. }
        | SessionState::AwaitingHostKey { .. }
        | SessionState::AwaitingPassphrase { .. }
        | SessionState::AwaitingPassword { .. }
        | SessionState::AwaitingAnswer { .. } => Some(Overlay::Progress { steps }),
        SessionState::Failed { error } => Some(Overlay::Failed {
            steps,
            message: error.message.clone(),
            hint: error.hint.clone(),
        }),
        SessionState::Disconnected { reason } => Some(Overlay::Closed(
            reason.clone().unwrap_or_else(|| "Disconnected".into()),
        )),
    }
}

/// The two lines at the top of the card.
pub fn heading(host: &HostConfig) -> (String, String) {
    let port = if host.port == 22 {
        String::new()
    } else {
        format!(":{}", host.port)
    };
    (
        host.alias.clone(),
        format!("{}@{}{port}", host.user, host.hostname),
    )
}

#[derive(Clone, Copy, PartialEq)]
enum Activity {
    /// The current step is under way.
    Working,
    /// It is waiting for the user.
    Waiting,
    Failed,
}

/// One segment per step. `extra` is a step the session does not count, as the shell of a terminal.
fn segments(
    steps: &Steps,
    extra: u32,
    activity: Activity,
    owner: EntityId,
    theme: &Theme,
) -> Option<Div> {
    let total = steps.rows.len() as u32 + extra;
    if total == 0 {
        return None;
    }
    let current = steps.position;
    Some(div().flex().gap(px(3.)).children((0..total).map(|index| {
        let segment = div()
            .relative()
            .flex_1()
            .h(px(3.))
            .rounded(px(2.))
            .overflow_hidden();
        if index < current {
            return segment.bg(theme.text).into_any_element();
        }
        if index > current {
            return segment.bg(theme.border).into_any_element();
        }
        match activity {
            Activity::Failed => segment.bg(theme.error).into_any_element(),
            Activity::Waiting => segment.bg(theme.text_muted).into_any_element(),
            Activity::Working => segment
                .bg(theme.border)
                .child(motion::sweep(
                    div().absolute().top_0().bottom_0().bg(theme.text),
                    ("connect-sweep", element_key(owner, u64::from(index))),
                ))
                .into_any_element(),
        }
    })))
}

/// What the current step is called, sliding in as the step changes.
fn step_line(
    text: String,
    detail: Option<String>,
    key: u32,
    owner: EntityId,
    theme: &Theme,
) -> impl IntoElement {
    motion::enter(
        div()
            .flex()
            .items_baseline()
            .justify_between()
            .gap(px(12.))
            .child(
                div()
                    .min_w_0()
                    .truncate()
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(theme.text)
                    .child(text),
            )
            .children(detail.map(|detail| {
                div()
                    .flex_shrink_0()
                    .text_size(ui_px(11.))
                    .text_color(theme.text_muted)
                    .child(detail)
            })),
        ("connect-step", element_key(owner, u64::from(key))),
        STEP_TEXT,
    )
}

/// Views that share a session keep independent animation identities.
fn element_key(owner: EntityId, part: u64) -> u64 {
    owner.as_u64().wrapping_mul(4096).wrapping_add(part)
}

/// All the steps, ticked as the connection moves on. `waiting` replaces the text of the current one.
fn step_list(
    steps: &Steps,
    waiting: Option<String>,
    extra: u32,
    activity: Activity,
    theme: &Theme,
) -> Div {
    let current = steps.position;
    let shell = (extra > 0).then(|| Row {
        now: "Starting shell".to_string(),
        detail: None,
    });
    let rows = steps.rows.iter().cloned().chain(shell).enumerate();
    div()
        .flex()
        .flex_col()
        .gap(px(2.))
        .children(rows.map(|(index, row)| {
            let index = index as u32;
            let is_current = index == current;
            let mark = if index < current {
                Icon::new(IconName::Check)
                    .size(px(13.))
                    .color(theme.accent)
                    .into_any_element()
            } else if !is_current {
                div()
                    .size(px(4.))
                    .rounded_full()
                    .bg(theme.text_disabled)
                    .into_any_element()
            } else {
                match activity {
                    Activity::Working => Icon::new(IconName::Spinner)
                        .size(px(12.))
                        .color(theme.accent)
                        .spin()
                        .into_any_element(),
                    Activity::Waiting => div()
                        .size(px(7.))
                        .rounded_full()
                        .bg(theme.warning)
                        .into_any_element(),
                    Activity::Failed => Icon::new(IconName::Alert)
                        .size(px(13.))
                        .color(theme.error)
                        .into_any_element(),
                }
            };
            let text = if is_current {
                waiting.clone().unwrap_or_else(|| row.now.clone())
            } else {
                row.now.clone()
            };
            let color = if !is_current {
                if index < current {
                    theme.text_muted
                } else {
                    theme.text_disabled
                }
            } else if activity == Activity::Failed {
                theme.error
            } else {
                theme.text
            };
            let detail = if index > current { None } else { row.detail };
            div()
                .flex()
                .items_center()
                .gap(px(10.))
                .h(px(27.))
                .text_size(ui_px(13.))
                .text_color(color)
                .child(
                    div()
                        .size(px(14.))
                        .flex()
                        .flex_none()
                        .items_center()
                        .justify_center()
                        .child(mark),
                )
                .child(div().flex_1().min_w_0().truncate().child(text))
                .children(detail.map(|detail| {
                    div()
                        .flex_shrink_0()
                        .text_size(ui_px(11.))
                        .text_color(theme.text_muted)
                        .child(detail)
                }))
        }))
}

/// How far the connection went, in the design chosen.
fn progress_view(
    steps: Steps,
    waiting: Option<String>,
    extra: u32,
    activity: Activity,
    docked: bool,
    owner: EntityId,
    theme: &Theme,
) -> Div {
    if !docked && !steps.rows.is_empty() {
        return step_list(&steps, waiting, extra, activity, theme);
    }
    let bar = segments(&steps, extra, activity, owner, theme);
    let view = div().flex().flex_col().gap(px(10.)).children(bar);
    if activity == Activity::Failed {
        return view;
    }
    let (text, detail) = match waiting {
        Some(text) => (text, None),
        None => (steps.text, steps.detail),
    };
    view.child(step_line(text, detail, steps.position, owner, theme))
}

/// `docked`: the view has something worth reading, so the card becomes a strip along the bottom of
/// it and leaves the rest visible. Otherwise it sits in the middle of an empty view.
pub struct Surface {
    pub heading: Option<(String, String)>,
    pub docked: bool,
    pub background: Hsla,
    /// A click on it gives the view the focus, which moves on to the prompt.
    pub focus: FocusHandle,
    /// A step after those of the session: the terminal's shell.
    pub extra_steps: u32,
    pub owner: EntityId,
}

pub fn render(
    overlay: Overlay,
    surface: Surface,
    theme: &Theme,
    window: &mut Window,
    cx: &App,
    on_retry: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> AnyElement {
    // The steps move on by themselves here: not every view hears of it.
    if let Overlay::Progress { steps } | Overlay::Login { steps, .. } = &overlay
        && !steps.caught_up
    {
        window.request_animation_frame();
    }
    let Surface {
        heading,
        docked,
        background,
        focus,
        extra_steps,
        owner,
    } = surface;
    // Keep the status and its action together, above the output retained by the terminal.
    let banner = match &overlay {
        Overlay::Reconnecting { attempt } => {
            Some(("Reconnecting…", format!("Attempt {attempt}"), true))
        }
        Overlay::Closed(reason) if docked => Some((
            "Disconnected",
            if reason == "Disconnected" {
                "Reconnect to resume this session.".to_string()
            } else {
                reason.clone()
            },
            false,
        )),
        _ => None,
    };
    if let Some((title, detail, reconnecting)) = banner {
        return div()
            .absolute()
            .top_0()
            .bottom_0()
            .left_0()
            .right_0()
            .occlude()
            .bg(theme.scrim)
            .flex()
            .items_center()
            .justify_center()
            .p(px(12.))
            .font_family(ui_font())
            .child(
                div()
                    .id("reconnect-banner")
                    .occlude()
                    .w(px(420.))
                    .min_w_0()
                    .max_w_full()
                    .flex()
                    .items_center()
                    .gap(px(12.))
                    .p(px(12.))
                    .rounded(px(11.))
                    .bg(theme.elevated)
                    .border_1()
                    .border_color(theme.strong_border)
                    .shadow(crate::theme::float_shadow())
                    .text_size(ui_px(13.))
                    .text_color(theme.text)
                    .child(
                        div().flex_none().child(
                            Icon::new(if reconnecting {
                                IconName::Refresh
                            } else {
                                IconName::Warning
                            })
                            .size(px(18.))
                            .color(theme.warning)
                            .when(reconnecting, |icon| icon.spin()),
                        ),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(
                                div()
                                    .truncate()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(title),
                            )
                            .child(
                                div()
                                    .text_size(ui_px(12.))
                                    .text_color(theme.text_muted)
                                    .child(detail),
                            ),
                    )
                    .child(
                        Button::new("retry")
                            .label(if reconnecting { "Try now" } else { "Reconnect" })
                            .style(ButtonStyle::Primary)
                            .on_click(on_retry),
                    ),
            )
            .into_any_element();
    }
    let retry = |label: &str| {
        div().flex().justify_end().child(
            Button::new("retry")
                .label(label.to_string())
                .on_click(on_retry),
        )
    };
    let view = |steps, waiting: Option<&str>, activity| {
        let waiting = waiting.map(str::to_string);
        progress_view(steps, waiting, extra_steps, activity, docked, owner, theme)
    };
    let content = div().flex().flex_col().gap(px(10.));
    let content = match overlay {
        Overlay::Progress { steps } | Overlay::StartingShell { steps } => {
            content.child(view(steps, None, Activity::Working))
        }
        Overlay::Login { steps, prompt, own } => {
            let waiting = if own {
                prompt.read(cx).waiting_text()
            } else {
                "Waiting in another pane. Click to answer here."
            };
            if !steps.caught_up {
                // Already there, without taking any room: it holds the focus and what is typed.
                content
                    .child(view(steps, None, Activity::Working))
                    .when(own, |this| {
                        this.child(div().h_0().overflow_hidden().child(prompt))
                    })
            } else {
                content
                    .child(view(steps, Some(waiting), Activity::Waiting))
                    .when(own, |this| {
                        this.child(motion::enter(
                            div().child(prompt),
                            ("connect-prompt", owner.as_u64()),
                            PROMPT,
                        ))
                    })
            }
        }
        Overlay::Failed {
            steps,
            message,
            hint,
        } => content
            .child(view(steps, None, Activity::Failed))
            .child(div().text_color(theme.error).child(message))
            .children(hint.map(|hint| div().text_color(theme.text_muted).child(hint)))
            .child(retry("Retry")),
        Overlay::Closed(text) => content
            .child(div().text_color(theme.text_muted).child(text))
            .child(retry("Reconnect")),
        Overlay::Reconnecting { .. } => content,
    };
    let root = div()
        .absolute()
        .bottom_0()
        .left_0()
        .right_0()
        .occlude()
        .rounded_b(px(PANEL_INNER_RADIUS))
        .bg(background)
        .font_family(ui_font())
        .text_size(ui_px(12.))
        .on_mouse_down(MouseButton::Left, move |_, window, _| window.focus(&focus));
    if docked {
        return root
            .border_t_1()
            .border_color(theme.border)
            .px(px(12.))
            .py(px(10.))
            .child(content)
            .into_any_element();
    }
    let card = div()
        .w(px(CARD_WIDTH))
        .max_w_full()
        .flex()
        .flex_col()
        .gap(px(14.))
        .children(heading.map(|(alias, login)| {
            div()
                .child(
                    div()
                        .flex()
                        .gap(px(5.))
                        .text_size(ui_px(19.))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(theme.text)
                        .child("Connecting to")
                        .child(div().text_color(theme.accent).child(alias)),
                )
                .child(
                    div()
                        .text_size(ui_px(12.))
                        .text_color(theme.text_muted)
                        .child(login),
                )
        }))
        .child(content);
    root.top_0()
        .p(px(8.))
        .flex()
        .items_center()
        .justify_center()
        .child(motion::enter(card, ("connect-card", owner.as_u64()), CARD))
        .into_any_element()
}
