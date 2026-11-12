//! Bottom dock with two faces: transfers (progress, speed, ETA, cancel) and ports. The buttons of
//! the status row and the palette pick which one shows.
use crate::ports_panel::PortsPanel;
use crate::state::AppState;
use crate::theme::{ActiveTheme as _, Theme, row_px};
use crate::widgets::button::{Button, ButtonStyle};
use crate::widgets::icon::{Icon, IconName};
use crate::widgets::progress::ProgressBar;
use crate::widgets::status::badge;
use gpui::prelude::*;
use gpui::{Context, Entity, EventEmitter, Subscription, Window, div, px};
use shcp_core::{Direction, TransferInfo, TransferState};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum DockTab {
    Transfers,
    Ports,
}

/// The user pressed the panel's close button.
pub struct TransfersEvent;

pub struct TransfersPanel {
    state: Entity<AppState>,
    tab: DockTab,
    ports: Entity<PortsPanel>,
    _subscription: Subscription,
}

impl EventEmitter<TransfersEvent> for TransfersPanel {}

fn bytes(value: u64) -> String {
    let mut v = value as f64;
    for unit in ["B", "KB", "MB", "GB"] {
        if v < 1024. {
            return if unit == "B" {
                format!("{value} B")
            } else {
                format!("{v:.1} {unit}")
            };
        }
        v /= 1024.;
    }
    format!("{v:.1} TB")
}

fn eta(info: &TransferInfo) -> String {
    match info.bytes_total {
        Some(total) if info.bytes_per_sec > 0 && total > info.bytes_done => {
            let secs = (total - info.bytes_done) / info.bytes_per_sec;
            format!("{}:{:02}", secs / 60, secs % 60)
        }
        _ => String::new(),
    }
}

fn fraction(info: &TransferInfo) -> f32 {
    match (info.bytes_total, info.files_total) {
        (Some(total), _) if total > 0 => info.bytes_done as f32 / total as f32,
        (_, Some(files)) if files > 0 => info.files_done as f32 / files as f32,
        _ => matches!(info.state, TransferState::Done) as u8 as f32,
    }
}

impl TransfersPanel {
    pub fn new(state: Entity<AppState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let subscription = cx.observe(&state, |_, _, cx| cx.notify());
        let ports = cx.new(|cx| PortsPanel::new(state.clone(), window, cx));
        cx.observe(&ports, |_, _, cx| cx.notify()).detach();
        Self {
            state,
            tab: DockTab::Transfers,
            ports,
            _subscription: subscription,
        }
    }

    pub fn tab(&self) -> DockTab {
        self.tab
    }

    pub fn show(&mut self, tab: DockTab, cx: &mut Context<Self>) {
        self.tab = tab;
        cx.notify();
    }

    fn row(&self, info: &TransferInfo, theme: &Theme) -> impl IntoElement {
        let running = matches!(info.state, TransferState::Queued | TransferState::Running);
        let (status, status_color) = match &info.state {
            TransferState::Queued => ("Queued".to_string(), theme.text_muted),
            TransferState::Running => {
                (format!("{}/s", bytes(info.bytes_per_sec)), theme.text_muted)
            }
            TransferState::Done => (
                info.bytes_total.map_or("Done".to_string(), |total| {
                    format!("{}, done", bytes(total))
                }),
                theme.text_muted,
            ),
            TransferState::Failed(message) => (format!("Failed: {message}"), theme.error),
            TransferState::Cancelled => ("Cancelled".to_string(), theme.text_muted),
        };
        let failed = matches!(info.state, TransferState::Failed(_));
        let id = info.id;
        let state = self.state.clone();
        div()
            .h(row_px(36.))
            .mx(px(6.))
            .px(px(8.))
            .flex()
            .items_center()
            .gap(px(14.))
            .rounded(px(crate::theme::RADIUS_CONTROL))
            .child(
                Icon::new(match info.direction {
                    Direction::Upload => IconName::Upload,
                    Direction::Download => IconName::Download,
                })
                .color(theme.text_muted),
            )
            .child(
                div()
                    .flex_1()
                    .min_w(px(120.))
                    .truncate()
                    .child(info.label.clone()),
            )
            .child(
                div()
                    .flex_grow()
                    .flex_basis(px(180.))
                    .min_w(px(90.))
                    .child(ProgressBar::new(fraction(info)).failed(failed)),
            )
            .child(
                div()
                    .w(px(150.))
                    .flex_none()
                    .text_right()
                    .truncate()
                    .text_size(crate::theme::ui_px(13.))
                    .text_color(status_color)
                    .child(if running && !eta(info).is_empty() {
                        format!("{status} · {}", eta(info))
                    } else {
                        status
                    }),
            )
            .child(
                div()
                    .w(px(24.))
                    .flex_none()
                    .when(running, |this| {
                        let state = state.clone();
                        this.child(
                            Button::new(("cancel-transfer", id.0 as usize))
                                .icon(IconName::Close)
                                .style(ButtonStyle::Ghost)
                                .on_click(move |_, _, cx| {
                                    state.update(cx, |s, _| s.cancel_transfer(id))
                                }),
                        )
                    })
                    .when(failed, |this| {
                        this.child(
                            Button::new(("retry-transfer", id.0 as usize))
                                .icon(IconName::Refresh)
                                .tooltip("Try again, from what already arrived")
                                .style(ButtonStyle::Ghost)
                                .on_click(move |_, _, cx| {
                                    state.update(cx, |s, cx| s.retry_transfer(id, cx))
                                }),
                        )
                    }),
            )
    }
}

impl Render for TransfersPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let state = self.state.clone();
        let transfers = self.state.read(cx).transfers.clone();
        let active = self.state.read(cx).active_transfers();
        let can_add = self.ports.read(cx).target(cx).is_some();
        div()
            .flex()
            .flex_col()
            .flex_none()
            // As tall as its content, up to this; then its list scrolls.
            .max_h(px(280.))
            .text_size(crate::theme::ui_px(13.))
            .child(
                div()
                    .flex_none()
                    .py(px(6.))
                    .pl(px(12.))
                    .pr(px(8.))
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    // Which of the two it is showing: the status row's buttons pick it.
                    .child(
                        Icon::new(match self.tab {
                            DockTab::Transfers => IconName::Swap,
                            DockTab::Ports => IconName::Link,
                        })
                        .size(px(13.))
                        .color(theme.text_muted),
                    )
                    .child(
                        div()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .child(match self.tab {
                                DockTab::Transfers => "Transfers",
                                DockTab::Ports => "Ports",
                            }),
                    )
                    .when(self.tab == DockTab::Transfers && active > 0, |this| {
                        this.child(badge(format!("{active} active"), theme.accent))
                    })
                    .child(div().flex_1())
                    .when(self.tab == DockTab::Transfers, |this| {
                        this.child(
                            Button::new("clear-completed")
                                .label("Clear completed")
                                .on_click(move |_, _, cx| {
                                    state.update(cx, |s, cx| s.clear_completed_transfers(cx))
                                }),
                        )
                    })
                    .when(self.tab == DockTab::Ports, |this| {
                        this.child(
                            Button::new("add-port")
                                .icon(IconName::Plus)
                                .label("Add port")
                                .disabled(!can_add)
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.ports
                                        .update(cx, |ports, cx| ports.start_adding(window, cx));
                                    cx.notify();
                                })),
                        )
                    })
                    .child(
                        Button::new("close-transfers")
                            .icon(IconName::Close)
                            .tooltip("Close (Ctrl+Shift+J). Transfers and tunnels keep running")
                            .style(ButtonStyle::Ghost)
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(TransfersEvent))),
                    ),
            )
            .when(self.tab == DockTab::Ports, |this| {
                this.child(self.ports.clone())
            })
            .when(self.tab == DockTab::Transfers, |this| {
                this.child(
                    div()
                        .id("transfer-list")
                        .min_h_0()
                        .pb(px(4.))
                        .overflow_y_scroll()
                        .children(transfers.iter().rev().map(|t| self.row(t, &theme)))
                        .when(transfers.is_empty(), |this| {
                            this.child(
                                div().p(px(10.)).text_color(theme.text_muted).child(
                                    "No transfers yet. Drop files on a Files tab to upload.",
                                ),
                            )
                        }),
                )
            })
    }
}
