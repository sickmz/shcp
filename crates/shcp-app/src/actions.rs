//! Global actions and the keymap, which the user can change.
//!
//! A focused terminal owns every Ctrl+letter combination (Ctrl+B is the tmux/herdr prefix,
//! Ctrl+W/T/N/J are readline keys), so app shortcuts inside a terminal use Ctrl+Shift, like
//! Windows Terminal. Outside a terminal the plain Ctrl variants work too.
use gpui::{App, Global, KeyBinding, Keystroke, actions};
use shcp_core::Settings;
use std::collections::BTreeMap;

actions!(
    shcp,
    [
        NewTerminal,
        OpenFiles,
        CloseTab,
        NextTab,
        PrevTab,
        ToggleSidebar,
        ToggleTransfers,
        CycleFace,
        NewHost,
        OpenSettings,
        OpenPalette,
        IncreaseFont,
        DecreaseFont,
        ResetFont,
        CopySelection,
        PasteClipboard,
        FocusNext,
        FocusPrev,
        FindInTerminal,
        SplitRight,
        SplitDown,
        FocusPaneLeft,
        FocusPaneRight,
        FocusPaneUp,
        FocusPaneDown,
    ]
);

const ANYWHERE: &str = "Workspace && !Modal";
const NOT_IN_TERMINAL: &str = "Workspace && !Modal && !Terminal";
const TERMINAL: &str = "Terminal";

/// An action the user can give other keys.
pub struct Shortcut {
    /// The name saved in the settings: keep it stable.
    pub id: &'static str,
    pub label: &'static str,
    /// Keys and where they work. A key the user sets works in the first of these places.
    defaults: &'static [(&'static str, &'static str)],
    make: fn(&str, &str) -> KeyBinding,
}

macro_rules! shortcut {
    ($id:literal, $label:literal, $action:expr, $defaults:expr) => {
        Shortcut {
            id: $id,
            label: $label,
            defaults: $defaults,
            make: |keys, context| KeyBinding::new(keys, $action, Some(context)),
        }
    };
}

pub const SHORTCUTS: &[Shortcut] = &[
    shortcut!(
        "palette",
        "Command palette",
        OpenPalette,
        &[("ctrl-shift-k", ANYWHERE), ("ctrl-k", NOT_IN_TERMINAL)]
    ),
    shortcut!(
        "new_terminal",
        "New terminal",
        NewTerminal,
        &[("ctrl-shift-t", ANYWHERE), ("ctrl-t", NOT_IN_TERMINAL)]
    ),
    shortcut!(
        "browse_files",
        "Browse files",
        OpenFiles,
        &[("ctrl-shift-e", ANYWHERE)]
    ),
    shortcut!(
        "close_tab",
        "Close terminal or tab",
        CloseTab,
        &[
            ("ctrl-shift-w", ANYWHERE),
            ("ctrl-f4", ANYWHERE),
            ("ctrl-w", NOT_IN_TERMINAL)
        ]
    ),
    shortcut!(
        "next_tab",
        "Next tab",
        NextTab,
        &[("ctrl-tab", ANYWHERE), ("ctrl-pagedown", ANYWHERE)]
    ),
    shortcut!(
        "previous_tab",
        "Previous tab",
        PrevTab,
        &[("ctrl-shift-tab", ANYWHERE), ("ctrl-pageup", ANYWHERE)]
    ),
    shortcut!(
        "find",
        "Find in terminal",
        FindInTerminal,
        &[("ctrl-shift-f", TERMINAL)]
    ),
    shortcut!(
        "split_right",
        "Split right",
        SplitRight,
        &[("ctrl-shift-d", TERMINAL)]
    ),
    shortcut!(
        "split_down",
        "Split down",
        SplitDown,
        &[("ctrl-shift-s", TERMINAL)]
    ),
    shortcut!(
        "pane_left",
        "Move to the terminal on the left",
        FocusPaneLeft,
        &[("ctrl-alt-left", TERMINAL)]
    ),
    shortcut!(
        "pane_right",
        "Move to the terminal on the right",
        FocusPaneRight,
        &[("ctrl-alt-right", TERMINAL)]
    ),
    shortcut!(
        "pane_up",
        "Move to the terminal above",
        FocusPaneUp,
        &[("ctrl-alt-up", TERMINAL)]
    ),
    shortcut!(
        "pane_down",
        "Move to the terminal below",
        FocusPaneDown,
        &[("ctrl-alt-down", TERMINAL)]
    ),
    shortcut!(
        "sidebar",
        "Toggle sidebar",
        ToggleSidebar,
        &[("ctrl-shift-b", ANYWHERE), ("ctrl-b", NOT_IN_TERMINAL)]
    ),
    shortcut!(
        "transfers",
        "Toggle transfers",
        ToggleTransfers,
        &[("ctrl-shift-j", ANYWHERE), ("ctrl-j", NOT_IN_TERMINAL)]
    ),
    shortcut!(
        "cycle_face",
        "Terminal, files or both",
        CycleFace,
        &[("ctrl-`", ANYWHERE)]
    ),
    shortcut!(
        "new_host",
        "New host",
        NewHost,
        &[("ctrl-shift-n", ANYWHERE), ("ctrl-n", NOT_IN_TERMINAL)]
    ),
    shortcut!(
        "settings",
        "Settings",
        OpenSettings,
        &[("ctrl-,", ANYWHERE)]
    ),
    shortcut!(
        "font_bigger",
        "Terminal font bigger",
        IncreaseFont,
        &[("ctrl-=", ANYWHERE), ("ctrl-+", ANYWHERE)]
    ),
    shortcut!(
        "font_smaller",
        "Terminal font smaller",
        DecreaseFont,
        &[("ctrl--", ANYWHERE)]
    ),
    shortcut!(
        "font_reset",
        "Terminal font default size",
        ResetFont,
        &[("ctrl-0", ANYWHERE)]
    ),
    shortcut!(
        "copy",
        "Copy in terminal",
        CopySelection,
        &[("ctrl-shift-c", TERMINAL)]
    ),
    shortcut!(
        "paste",
        "Paste in terminal",
        PasteClipboard,
        &[
            ("ctrl-v", TERMINAL),
            ("ctrl-shift-v", TERMINAL),
            ("shift-insert", TERMINAL)
        ]
    ),
];

/// The default key `Ctrl+V` setting turns off.
const CTRL_V: &str = "ctrl-v";

impl Shortcut {
    /// The keys in force, with where each one works.
    fn keys(&self, settings: &Settings) -> Vec<(String, &'static str)> {
        if let Some(custom) = settings.keybindings.get(self.id) {
            return match custom.as_str() {
                "" => Vec::new(),
                keys => vec![(keys.to_string(), self.defaults[0].1)],
            };
        }
        self.defaults
            .iter()
            .filter(|(keys, _)| settings.ctrl_v_pastes || self.id != "paste" || *keys != CTRL_V)
            .map(|(keys, context)| (keys.to_string(), *context))
            .collect()
    }

    /// What the settings show: the keys in force, as people write them.
    pub fn label_of_keys(&self, settings: &Settings) -> String {
        self.keys(settings)
            .iter()
            .map(|(keys, _)| pretty(keys))
            .collect::<Vec<_>>()
            .join(" / ")
    }
}

/// `ctrl-shift-t` as `Ctrl+Shift+T`.
pub fn pretty(keys: &str) -> String {
    let Ok(stroke) = Keystroke::parse(keys) else {
        return keys.to_string();
    };
    let mut parts: Vec<String> = Vec::new();
    for (on, name) in [
        (stroke.modifiers.control, "Ctrl"),
        (stroke.modifiers.alt, "Alt"),
        (stroke.modifiers.shift, "Shift"),
        (stroke.modifiers.platform, "Win"),
    ] {
        if on {
            parts.push(name.to_string());
        }
    }
    parts.push(match stroke.key.as_str() {
        "pageup" => "PageUp".to_string(),
        "pagedown" => "PageDown".to_string(),
        key => {
            let mut letters = key.chars();
            match letters.next() {
                Some(first) => first.to_uppercase().chain(letters).collect(),
                None => String::new(),
            }
        }
    });
    parts.join("+")
}

/// Whether keys can be a shortcut without getting in the way of typing.
pub fn acceptable(keys: &str) -> bool {
    Keystroke::parse(keys).is_ok_and(|stroke| {
        let m = stroke.modifiers;
        m.control
            || m.alt
            || m.platform
            || (stroke.key.len() > 1 && stroke.key.starts_with('f') && stroke.key != "f")
    })
}

/// The action already on these keys, other than `id`.
pub fn taken_by(keys: &str, id: &str, settings: &Settings) -> Option<&'static str> {
    SHORTCUTS
        .iter()
        .filter(|shortcut| shortcut.id != id)
        .find(|shortcut| shortcut.keys(settings).iter().any(|(k, _)| k == keys))
        .map(|shortcut| shortcut.label)
}

/// Keys named in tooltips and menus, with what they became: `Ctrl+Shift+T` to `Ctrl+Y`.
#[derive(Clone, Default)]
pub struct KeyHints(BTreeMap<String, String>);

impl Global for KeyHints {}

impl KeyHints {
    fn new(settings: &Settings) -> Self {
        let mut hints = BTreeMap::new();
        for shortcut in SHORTCUTS {
            let Some(custom) = settings.keybindings.get(shortcut.id) else {
                continue;
            };
            let now = if custom.is_empty() {
                "no shortcut".to_string()
            } else {
                pretty(custom)
            };
            for (keys, _) in shortcut.defaults {
                hints.insert(pretty(keys), now.clone());
            }
        }
        Self(hints)
    }

    /// The text with the keys in force. Longest names first: `Ctrl+Shift+T` before `Ctrl+T`.
    pub fn apply(&self, text: &str) -> String {
        if self.0.is_empty() {
            return text.to_string();
        }
        let mut names: Vec<_> = self.0.iter().collect();
        names.sort_by_key(|(name, _)| std::cmp::Reverse(name.len()));
        // One pass, so that a key that became another one is not replaced twice.
        let mut out = String::with_capacity(text.len());
        let mut rest = text;
        'text: while !rest.is_empty() {
            for (name, now) in &names {
                if let Some(after) = rest.strip_prefix(name.as_str()) {
                    out.push_str(now);
                    rest = after;
                    continue 'text;
                }
            }
            let mut chars = rest.chars();
            out.extend(chars.next());
            rest = chars.as_str();
        }
        out
    }
}

/// `text` with the keys in force, wherever it names a key.
pub fn with_current_keys(text: &str, cx: &App) -> String {
    match cx.try_global::<KeyHints>() {
        Some(hints) => hints.apply(text),
        None => text.to_string(),
    }
}

/// Puts the keymap in place, replacing the one there. A key that makes no sense, say from an edited
/// file, is left out: it could not be bound anyway.
pub fn bind_keys(settings: &Settings, cx: &mut App) {
    cx.clear_key_bindings();
    for shortcut in SHORTCUTS {
        for (keys, context) in shortcut.keys(settings) {
            if Keystroke::parse(&keys).is_ok() {
                cx.bind_keys([(shortcut.make)(&keys, context)]);
            }
        }
    }
    cx.bind_keys([
        KeyBinding::new("tab", FocusNext, Some("Modal")),
        KeyBinding::new("shift-tab", FocusPrev, Some("Modal")),
    ]);
    cx.set_global(KeyHints::new(settings));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shortcut(id: &str) -> &'static Shortcut {
        SHORTCUTS.iter().find(|s| s.id == id).unwrap()
    }

    fn with(id: &str, keys: &str) -> Settings {
        let mut settings = Settings::default();
        settings.keybindings.insert(id.into(), keys.into());
        settings
    }

    #[test]
    fn every_default_key_can_be_bound_and_ids_are_unique() {
        let mut ids = std::collections::HashSet::new();
        for shortcut in SHORTCUTS {
            assert!(ids.insert(shortcut.id), "{}", shortcut.id);
            assert!(!shortcut.defaults.is_empty());
            for (keys, _) in shortcut.defaults {
                assert!(Keystroke::parse(keys).is_ok(), "{keys}");
            }
        }
    }

    #[test]
    fn keys_read_the_way_people_write_them() {
        assert_eq!(pretty("ctrl-shift-t"), "Ctrl+Shift+T");
        assert_eq!(pretty("ctrl-,"), "Ctrl+,");
        assert_eq!(pretty("ctrl--"), "Ctrl+-");
        assert_eq!(pretty("ctrl-pageup"), "Ctrl+PageUp");
        assert_eq!(pretty("shift-insert"), "Shift+Insert");
        assert_eq!(pretty("f4"), "F4");
    }

    #[test]
    fn a_custom_key_replaces_the_defaults_and_empty_removes_them() {
        let settings = with("new_terminal", "ctrl-y");
        let keys = shortcut("new_terminal").keys(&settings);
        assert_eq!(keys, [("ctrl-y".to_string(), ANYWHERE)]);
        assert!(
            shortcut("new_terminal")
                .keys(&with("new_terminal", ""))
                .is_empty()
        );
        assert_eq!(shortcut("new_terminal").keys(&Settings::default()).len(), 2);
    }

    #[test]
    fn ctrl_v_can_be_left_to_the_program() {
        let paste = shortcut("paste");
        let has = |settings: &Settings| paste.keys(settings).iter().any(|(k, _)| k == CTRL_V);
        assert!(has(&Settings::default()));
        let off = Settings {
            ctrl_v_pastes: false,
            ..Default::default()
        };
        assert!(!has(&off));
        assert_eq!(paste.keys(&off).len(), 2);
        // Asked for by name, it is what the user wants.
        let mut chosen = with("paste", CTRL_V);
        chosen.ctrl_v_pastes = false;
        assert!(has(&chosen));
    }

    #[test]
    fn bare_letters_are_refused_but_function_keys_are_fine() {
        assert!(acceptable("ctrl-y"));
        assert!(acceptable("alt-1"));
        assert!(acceptable("f5"));
        assert!(!acceptable("y"));
        assert!(!acceptable("f"));
        assert!(!acceptable("enter"));
        assert!(!acceptable("shift-y"));
    }

    #[test]
    fn a_taken_key_names_who_has_it() {
        let settings = Settings::default();
        assert_eq!(
            taken_by("ctrl-shift-t", "palette", &settings),
            Some("New terminal")
        );
        assert_eq!(taken_by("ctrl-shift-t", "new_terminal", &settings), None);
        assert_eq!(taken_by("ctrl-y", "palette", &settings), None);
    }

    #[test]
    fn hints_follow_the_keys_in_force() {
        let hints = KeyHints::new(&with("new_terminal", "ctrl-y"));
        assert_eq!(
            hints.apply("New terminal (Ctrl+Shift+T)"),
            "New terminal (Ctrl+Y)"
        );
        assert_eq!(hints.apply("Close (Ctrl+Shift+W)"), "Close (Ctrl+Shift+W)");
        let none = KeyHints::new(&Settings::default());
        assert_eq!(none.apply("Ctrl+Shift+T"), "Ctrl+Shift+T");
        let removed = KeyHints::new(&with("transfers", ""));
        assert_eq!(removed.apply("Hide (Ctrl+Shift+J)"), "Hide (no shortcut)");
    }

    #[test]
    fn swapped_keys_are_not_replaced_twice() {
        let mut settings = with("new_terminal", "ctrl-shift-k");
        settings
            .keybindings
            .insert("palette".into(), "ctrl-shift-t".into());
        let hints = KeyHints::new(&settings);
        assert_eq!(hints.apply("Ctrl+Shift+T"), "Ctrl+Shift+K");
        assert_eq!(hints.apply("Ctrl+Shift+K"), "Ctrl+Shift+T");
    }
}
