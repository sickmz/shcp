use super::icon::{Icon, IconName};
use crate::theme::ActiveTheme as _;
use gpui::prelude::*;
use gpui::{App, ClickEvent, ElementId, SharedString, Window, div, px, transparent_black};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ButtonStyle {
    Ghost,
    /// Ghost, for the one action that destroys something.
    GhostDanger,
    Filled,
    Primary,
    Danger,
}

/// How a button takes the room it has.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Width {
    Own,
    Share,
    Whole,
}

type ClickHandler = Box<dyn Fn(&ClickEvent, &mut Window, &mut App) + 'static>;

/// Compact button: optional icon and label.
#[derive(IntoElement)]
pub struct Button {
    id: ElementId,
    icon: Option<IconName>,
    label: Option<SharedString>,
    style: ButtonStyle,
    disabled: bool,
    active: bool,
    compact: bool,
    keyboard_focus: bool,
    width: Width,
    tooltip: Option<SharedString>,
    on_click: Option<ClickHandler>,
}

impl Button {
    pub fn new(id: impl Into<ElementId>) -> Self {
        Self {
            id: id.into(),
            icon: None,
            label: None,
            style: ButtonStyle::Filled,
            disabled: false,
            active: false,
            compact: false,
            keyboard_focus: false,
            width: Width::Own,
            tooltip: None,
            on_click: None,
        }
    }
    pub fn keyboard_focus(mut self) -> Self {
        self.keyboard_focus = true;
        self
    }

    pub fn tooltip(mut self, text: impl Into<SharedString>) -> Self {
        self.tooltip = Some(text.into());
        self
    }

    pub fn icon(mut self, icon: IconName) -> Self {
        self.icon = Some(icon);
        self
    }
    pub fn label(mut self, label: impl Into<SharedString>) -> Self {
        self.label = Some(label.into());
        self
    }
    pub fn style(mut self, style: ButtonStyle) -> Self {
        self.style = style;
        self
    }
    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }
    /// Ghost button that shows something currently open.
    pub fn active(mut self, active: bool) -> Self {
        self.active = active;
        self
    }
    /// Shares the row evenly with others that do.
    pub fn share(mut self) -> Self {
        self.width = Width::Share;
        self
    }
    /// A row of its own.
    pub fn whole(mut self) -> Self {
        self.width = Width::Whole;
        self
    }
    /// Smaller, for tight headers.
    pub fn compact(mut self) -> Self {
        self.compact = true;
        self
    }
    pub fn on_click(
        mut self,
        handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_click = Some(Box::new(handler));
        self
    }
}

impl RenderOnce for Button {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme().clone();
        let (bg, fg) = match self.style {
            ButtonStyle::Ghost if self.active => (Some(theme.active), theme.text),
            ButtonStyle::Ghost => (None, theme.text_muted),
            ButtonStyle::GhostDanger => (None, theme.error),
            ButtonStyle::Filled => (None, theme.text),
            ButtonStyle::Primary => (Some(theme.primary), theme.on_primary),
            ButtonStyle::Danger => (Some(theme.error), theme.on_primary),
        };
        let solid = matches!(self.style, ButtonStyle::Primary | ButtonStyle::Danger);
        // A solid button keeps the text meant for its fill, and fades as a whole.
        let fg = if self.disabled && !solid {
            theme.text_disabled
        } else {
            fg
        };
        let height = if self.compact { 24. } else { 28. };
        // Only an icon: a square, the same target wherever it is.
        let square = self.label.is_none();
        let mut button = div()
            .id(self.id)
            .h(px(height))
            .when(square, |this| this.w(px(height)))
            .when(!square, |this| {
                this.px(px(if self.compact { 8. } else { 10. }))
            })
            .flex()
            .map(|this| match self.width {
                Width::Own => this.flex_none(),
                Width::Share => this.flex_1().min_w_0(),
                Width::Whole => this.w_full(),
            })
            .items_center()
            .justify_center()
            .gap(px(5.))
            .rounded(px(7.))
            .text_size(crate::theme::ui_px(if self.compact { 12. } else { 13. }))
            .font_weight(if square || self.style == ButtonStyle::Ghost {
                gpui::FontWeight::NORMAL
            } else {
                gpui::FontWeight::MEDIUM
            })
            .text_color(fg)
            .when_some(bg, |this, bg| this.bg(bg))
            .when(self.style == ButtonStyle::Filled, |this| {
                this.border_1().border_color(theme.strong_border)
            })
            // Always there, so the focus ring changes a color and never the button's size.
            .when(
                self.keyboard_focus && self.style != ButtonStyle::Filled,
                |this| this.border_1().border_color(transparent_black()),
            )
            .when_some(self.tooltip, |this, text| {
                this.tooltip(crate::widgets::tooltip::tooltip(text))
            });
        if self.disabled {
            return button
                .when(solid, |this| this.opacity(0.5))
                .children(self.icon.map(|i| Icon::new(i).color(fg)))
                .children(self.label);
        }
        let (hover, press, ring) = (theme.hover, theme.active, theme.text);
        button = button
            .when(self.keyboard_focus, |b| {
                b.focusable()
                    .tab_stop(true)
                    .focus(move |style| style.border_color(ring))
            })
            .on_key_down(|event, _, cx| {
                // Enter/Space belong to the focused button, whose click fires on key-up.
                if matches!(event.keystroke.key.as_str(), "enter" | "space") {
                    cx.stop_propagation();
                }
            })
            .cursor_pointer()
            // A translucent fill would wash out the label of a solid button.
            .when(solid, |b| {
                b.hover(|style| style.opacity(0.85))
                    .active(|style| style.opacity(0.7))
            })
            .when(!solid, |b| {
                b.hover(move |style| style.bg(hover))
                    .active(move |style| style.bg(press))
            })
            .children(self.icon.map(|i| Icon::new(i).color(fg)))
            .children(self.label);
        match self.on_click {
            Some(handler) => button.on_click(handler),
            None => button,
        }
    }
}
