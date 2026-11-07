//! Embedded SVG icons (Hugeicons, free set, stroke rounded) drawn with the current text color.
use gpui::prelude::*;
use gpui::{
    Animation, AnimationExt as _, App, Hsla, Pixels, Transformation, Window, percentage, px, svg,
};
use std::time::Duration;

macro_rules! icons {
    ($($variant:ident => $file:literal),* $(,)?) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub enum IconName { $($variant),* }
        impl IconName {
            pub const ALL: &'static [IconName] = &[$(IconName::$variant),*];
            pub fn path(self) -> &'static str {
                match self { $(IconName::$variant => concat!("icons/", $file, ".svg")),* }
            }
            pub fn bytes(self) -> &'static [u8] {
                match self { $(IconName::$variant => include_bytes!(concat!("../../assets/icons/", $file, ".svg"))),* }
            }
        }
    };
}

icons! {
    Logo => "logo",
    Terminal => "terminal",
    Folder => "folder",
    FolderPlus => "folder_plus",
    FolderUp => "folder_up",
    File => "file",
    FilePlus => "file_plus",
    Link => "link",
    Plus => "plus",
    Close => "close",
    ChevronRight => "chevron_right",
    ChevronLeft => "chevron_left",
    ArrowUp => "arrow_up",
    Refresh => "refresh",
    Upload => "upload",
    Download => "download",
    Trash => "trash",
    Pencil => "pencil",
    Power => "power",
    Forward => "forward",
    Spinner => "spinner",
    PanelLeft => "panel_left",
    Check => "check",
    Copy => "copy",
    Home => "home",
    Settings => "settings",
    More => "more",
    Alert => "alert",
    Warning => "warning",
    Search => "search",
    ArrowLeft => "arrow_left",
    ChevronDown => "chevron_down",
    ChevronUp => "chevron_up",
    Minus => "minus",
    Square => "square",
    Restore => "restore",
    Palette => "palette",
    Keyboard => "keyboard",
    Plug => "plug",
    Info => "info",
    PanelBottom => "panel_bottom",
    Eye => "eye",
    EyeOff => "eye_off",
    ExternalLink => "external_link",
    Play => "play",
    Server => "server",
    Database => "database",
    Cloud => "cloud",
    Globe => "globe",
    Shield => "shield",
    Lock => "lock",
    Container => "box",
    Cpu => "cpu",
    Router => "router",
    Network => "network",
    HardDrive => "hard_drive",
    Archive => "archive",
    Flask => "flask",
    GitBranch => "git_branch",
    Monitor => "monitor",
    Briefcase => "briefcase",
    Wrench => "wrench",
    Mail => "mail",
    Zap => "zap",
    Star => "star",
    SplitRight => "split_right",
    SplitDown => "split_down",
    Swap => "swap",
    Sun => "sun",
    Moon => "moon",
}

#[derive(IntoElement)]
pub struct Icon {
    name: IconName,
    size: Pixels,
    color: Option<Hsla>,
    spin: bool,
}

impl Icon {
    pub fn new(name: IconName) -> Self {
        Self {
            name,
            size: px(14.),
            color: None,
            spin: false,
        }
    }
    pub fn size(mut self, size: Pixels) -> Self {
        self.size = size;
        self
    }
    pub fn color(mut self, color: Hsla) -> Self {
        self.color = Some(color);
        self
    }
    pub fn spin(mut self) -> Self {
        self.spin = true;
        self
    }
}

impl RenderOnce for Icon {
    fn render(self, window: &mut Window, _cx: &mut App) -> impl IntoElement {
        // gpui's svg paints only with its *own* text color (it does not inherit), so an icon
        // without an explicit color would be invisible: take the surrounding text color.
        let color = self.color.unwrap_or_else(|| window.text_style().color);
        let icon = svg()
            .path(self.name.path())
            .size(self.size)
            .flex_none()
            .text_color(color);
        if self.spin {
            icon.with_animation(
                "spin",
                Animation::new(Duration::from_millis(700)).repeat(),
                |icon, delta| icon.with_transformation(Transformation::rotate(percentage(delta))),
            )
            .into_any_element()
        } else {
            icon.into_any_element()
        }
    }
}
