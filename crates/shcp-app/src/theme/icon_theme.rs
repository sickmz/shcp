//! File icon themes installed from Open VSX: which picture a file or folder gets. Each one is a
//! folder in `<config_dir>/icon-themes` with its pictures and a `theme.json` written by shcp.
use super::ThemeOrigin;
use super::open_vsx::ORIGIN_KEY;
use anyhow::{Context, Result};
use gpui::{App, Global};
use serde_json::Value;
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Arc,
};

pub const THEME_FILE: &str = "theme.json";
pub const ICONS_DIR: &str = "icons";

/// What VS Code knows by itself: themes count on it to give an icon to a file by its language,
/// some of them for nearly every common type. `(language, extensions, exact file names)`.
const LANGUAGES: &[(&str, &[&str], &[&str])] = &[
    ("bat", &["bat", "cmd"], &[]),
    ("c", &["c", "h"], &[]),
    ("clojure", &["clj", "cljs", "cljc", "edn"], &[]),
    ("coffeescript", &["coffee"], &[]),
    ("cpp", &["cpp", "cc", "cxx", "hpp", "hh", "hxx"], &[]),
    ("csharp", &["cs"], &[]),
    ("css", &["css"], &[]),
    ("dart", &["dart"], &[]),
    ("diff", &["diff", "patch"], &[]),
    (
        "dockerfile",
        &["dockerfile"],
        &["dockerfile", "containerfile"],
    ),
    ("elixir", &["ex", "exs"], &[]),
    ("erlang", &["erl", "hrl"], &[]),
    ("fsharp", &["fs", "fsi", "fsx"], &[]),
    ("go", &["go"], &[]),
    ("groovy", &["groovy", "gradle"], &["jenkinsfile"]),
    ("haskell", &["hs"], &[]),
    ("html", &["html", "htm", "xhtml"], &[]),
    ("ini", &["ini"], &[]),
    ("java", &["java"], &[]),
    ("javascript", &["js", "mjs", "cjs"], &[]),
    ("javascriptreact", &["jsx"], &[]),
    ("json", &["json"], &[]),
    ("jsonc", &["jsonc"], &[]),
    ("julia", &["jl"], &[]),
    ("kotlin", &["kt", "kts"], &[]),
    ("less", &["less"], &[]),
    ("log", &["log"], &[]),
    ("lua", &["lua"], &[]),
    ("makefile", &["mk"], &["makefile", "gnumakefile"]),
    ("markdown", &["md", "markdown"], &[]),
    ("objective-c", &["m"], &[]),
    ("ocaml", &["ml", "mli"], &[]),
    ("perl", &["pl", "pm"], &[]),
    ("php", &["php"], &[]),
    ("plaintext", &["txt"], &[]),
    ("powershell", &["ps1", "psm1", "psd1"], &[]),
    ("properties", &["properties", "conf", "cfg"], &[]),
    ("python", &["py", "pyw", "pyi"], &[]),
    ("r", &["r"], &[]),
    ("ruby", &["rb", "rake", "gemspec"], &["gemfile", "rakefile"]),
    ("rust", &["rs"], &[]),
    ("scala", &["scala", "sc"], &[]),
    ("scss", &["scss"], &[]),
    ("shellscript", &["sh", "bash", "zsh", "ksh"], &[]),
    ("sql", &["sql"], &[]),
    ("swift", &["swift"], &[]),
    ("terraform", &["tf", "tfvars"], &[]),
    ("toml", &["toml"], &[]),
    ("typescript", &["ts", "mts", "cts"], &[]),
    ("typescriptreact", &["tsx"], &[]),
    ("vb", &["vb"], &[]),
    ("vue", &["vue"], &[]),
    ("xml", &["xml", "xsd", "xsl"], &[]),
    ("yaml", &["yaml", "yml"], &[]),
];

/// The extensions of a lowercase file name, the longest first: `test.js` before `js`.
fn extensions(name: &str) -> impl Iterator<Item = &str> {
    name.char_indices()
        .filter(|(_, c)| *c == '.')
        .map(|(index, _)| &name[index + 1..])
        .filter(|extension| !extension.is_empty())
}

fn language_of(name: &str) -> Option<&'static str> {
    let by_name = LANGUAGES.iter().find(|(_, _, names)| names.contains(&name));
    by_name
        .or_else(|| {
            extensions(name).find_map(|extension| {
                LANGUAGES
                    .iter()
                    .find(|(_, extensions, _)| extensions.contains(&extension))
            })
        })
        .map(|(language, _, _)| *language)
}

/// One set of associations. Values are the numbers of the pictures in the theme's folder.
#[derive(Default)]
struct IconSet {
    file: Option<u32>,
    folder: Option<u32>,
    file_extensions: HashMap<String, u32>,
    file_names: HashMap<String, u32>,
    folder_names: HashMap<String, u32>,
    language_ids: HashMap<String, u32>,
}

fn number(value: Option<&Value>) -> Option<u32> {
    value.and_then(Value::as_u64)?.try_into().ok()
}

fn lowercase_map(set: &Value, key: &str) -> HashMap<String, u32> {
    set.get(key)
        .and_then(Value::as_object)
        .map(|map| {
            map.iter()
                .filter_map(|(name, value)| Some((name.to_lowercase(), number(Some(value))?)))
                .collect()
        })
        .unwrap_or_default()
}

impl IconSet {
    fn parse(set: &Value) -> Self {
        Self {
            file: number(set.get("file")),
            folder: number(set.get("folder")),
            file_extensions: lowercase_map(set, "fileExtensions"),
            file_names: lowercase_map(set, "fileNames"),
            folder_names: lowercase_map(set, "folderNames"),
            language_ids: lowercase_map(set, "languageIds"),
        }
    }
}

pub struct IconTheme {
    pub name: String,
    pub origin: ThemeOrigin,
    /// The folder it lives in, where nothing else is kept.
    pub dir: PathBuf,
    set: IconSet,
    /// What changes on a light theme; the rest comes from `set`.
    light: IconSet,
}

impl IconTheme {
    fn parse(text: &str, dir: PathBuf) -> Result<Self> {
        let theme: Value = serde_json::from_str(text).context("It cannot be read")?;
        let origin = theme.get(ORIGIN_KEY).context("It has no origin")?;
        let field = |value: &Value, key: &str| {
            value
                .get(key)
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string()
        };
        Ok(Self {
            name: field(&theme, "name"),
            origin: ThemeOrigin {
                extension: field(origin, "extension").into(),
                extension_name: field(origin, "displayName").into(),
            },
            dir,
            set: IconSet::parse(theme.get("set").unwrap_or(&Value::Null)),
            light: IconSet::parse(theme.get("light").unwrap_or(&Value::Null)),
        })
    }

    /// The picture of a file or folder: its exact name, then its extension, then its language, and
    /// finally the theme's default.
    pub fn icon(&self, name: &str, is_dir: bool, is_light: bool) -> Option<PathBuf> {
        let name = name.to_lowercase();
        let sets: &[&IconSet] = if is_light {
            &[&self.light, &self.set]
        } else {
            &[&self.set]
        };
        let first = |pick: &dyn Fn(&IconSet) -> Option<u32>| sets.iter().find_map(|set| pick(set));
        let number = if is_dir {
            first(&|set| set.folder_names.get(&name).copied()).or_else(|| first(&|set| set.folder))
        } else {
            first(&|set| set.file_names.get(&name).copied())
                // The longest extension wins, whichever set knows it: `test.js` before `js`.
                .or_else(|| {
                    extensions(&name).find_map(|extension| {
                        first(&|set| set.file_extensions.get(extension).copied())
                    })
                })
                .or_else(|| {
                    let language = language_of(&name)?;
                    first(&|set| set.language_ids.get(language).copied())
                })
                .or_else(|| first(&|set| set.file))
        }?;
        Some(self.dir.join(ICONS_DIR).join(format!("{number}.svg")))
    }
}

/// All the installed icon themes.
pub struct IconThemeCatalog {
    pub themes: Vec<Arc<IconTheme>>,
}

impl Global for IconThemeCatalog {}

impl IconThemeCatalog {
    /// Reads every theme in `dir`. Returns the catalog and one message per unreadable theme.
    pub fn load(dir: &Path) -> (Self, Vec<String>) {
        let mut themes = Vec::new();
        let mut errors = Vec::new();
        let mut folders: Vec<_> = std::fs::read_dir(dir)
            .into_iter()
            .flatten()
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| path.join(THEME_FILE).is_file())
            .collect();
        folders.sort();
        for folder in folders {
            let read = std::fs::read_to_string(folder.join(THEME_FILE))
                .map_err(anyhow::Error::from)
                .and_then(|text| IconTheme::parse(&text, folder.clone()));
            match read {
                Ok(theme) => themes.push(Arc::new(theme)),
                Err(error) => errors.push(format!("Icon theme {}: {error:#}", folder.display())),
            }
        }
        (Self { themes }, errors)
    }
}

/// The theme the settings ask for. `None`: the app's own icons.
pub struct ActiveIconTheme(pub Option<Arc<IconTheme>>);

impl Global for ActiveIconTheme {}

/// The picture for a file or folder under the active themes, if an icon theme is in use.
pub fn file_icon(name: &str, is_dir: bool, cx: &App) -> Option<PathBuf> {
    let theme = cx.global::<ActiveIconTheme>().0.as_ref()?;
    theme.icon(name, is_dir, super::ActiveTheme::theme(cx).is_light)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn theme() -> IconTheme {
        let text = format!(
            r#"{{
                "name": "Test",
                "{ORIGIN_KEY}": {{ "extension": "a.b", "displayName": "B" }},
                "set": {{
                    "file": 1,
                    "folder": 2,
                    "fileExtensions": {{ "test.js": 3, "js": 4, "PNG": 5 }},
                    "fileNames": {{ "dockerfile": 6, "special.js": 7 }},
                    "folderNames": {{ "src": 8 }},
                    "languageIds": {{ "python": 9, "dockerfile": 10 }}
                }},
                "light": {{ "fileExtensions": {{ "js": 11 }} }}
            }}"#
        );
        IconTheme::parse(&text, PathBuf::from("root")).unwrap()
    }

    fn picture(number: u32) -> Option<PathBuf> {
        Some(
            PathBuf::from("root")
                .join(ICONS_DIR)
                .join(format!("{number}.svg")),
        )
    }

    #[test]
    fn the_exact_name_wins_over_the_extension() {
        assert_eq!(theme().icon("Special.JS", false, false), picture(7));
    }

    #[test]
    fn the_longest_extension_wins() {
        assert_eq!(theme().icon("a.test.js", false, false), picture(3));
        assert_eq!(theme().icon("a.js", false, false), picture(4));
    }

    #[test]
    fn extensions_ignore_case() {
        assert_eq!(theme().icon("PHOTO.png", false, false), picture(5));
    }

    #[test]
    fn a_theme_may_know_a_file_only_by_its_language() {
        assert_eq!(theme().icon("main.py", false, false), picture(9));
    }

    #[test]
    fn a_file_with_no_match_gets_the_default() {
        assert_eq!(theme().icon("notes.unknown", false, false), picture(1));
        assert_eq!(theme().icon("noextension", false, false), picture(1));
    }

    #[test]
    fn folders_have_their_own_names_and_default() {
        assert_eq!(theme().icon("SRC", true, false), picture(8));
        assert_eq!(theme().icon("other", true, false), picture(2));
        // A folder named like a file type stays a folder.
        assert_eq!(theme().icon("dockerfile", true, false), picture(2));
    }

    #[test]
    fn a_light_theme_overrides_only_what_it_names() {
        assert_eq!(theme().icon("a.js", false, true), picture(11));
        assert_eq!(theme().icon("a.test.js", false, true), picture(3));
    }

    #[test]
    fn languages_also_come_from_exact_names() {
        assert_eq!(language_of("dockerfile"), Some("dockerfile"));
        assert_eq!(language_of("a.rs"), Some("rust"));
        assert_eq!(language_of("nothing"), None);
    }
}
