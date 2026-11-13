//! Finds color and file icon themes on Open VSX and installs them. Packages come from strangers:
//! only their colors and pictures are read, nothing in them runs. Every call blocks on the network.
use super::icon_theme::{ICONS_DIR, THEME_FILE};
use super::vscode;
use crate::net::{self, Service};
use anyhow::{Context, Result, bail, ensure};
use serde_json::{Map, Value, json};
use std::{
    collections::HashMap,
    fmt::Write as _,
    io::{Cursor, Read},
    path::Path,
    thread,
    time::Duration,
};
use zip::ZipArchive;

/// What an extension contributes that shcp can use.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum ThemeKind {
    Color,
    Icon,
}

const API: &str = "https://open-vsx.org/api";
const TRUSTED_PREFIX: &str = "https://open-vsx.org/";
/// Key of an installed theme file holding where it came from.
pub(crate) const ORIGIN_KEY: &str = "shcpOrigin";

/// Licenses that let anyone use the theme, whoever publishes it.
const LICENSES: [&str; 9] = [
    "0BSD",
    "Apache-2.0",
    "BSD-2-Clause",
    "BSD-3-Clause",
    "CC0-1.0",
    "ISC",
    "MIT",
    "MPL-2.0",
    "Unlicense",
];
const SEARCH_SIZE: usize = 20;
const MAX_JSON_BYTES: u64 = 1024 * 1024;
const MAX_PACKAGE_BYTES: u64 = 20 * 1024 * 1024;
const MAX_THEME_BYTES: u64 = 1024 * 1024;
const MAX_ICON_THEME_BYTES: u64 = 4 * 1024 * 1024;
const MAX_ICON_BYTES: u64 = 256 * 1024;
/// Every picture is drawn at this size, whatever it declares, so that none can take much memory.
const ICON_SIZE: u32 = 32;
const MAX_ZIP_ENTRIES: usize = 5_000;
const MAX_THEMES_PER_EXTENSION: usize = 40;
const MAX_INCLUDE_DEPTH: usize = 8;
const MAX_COLOR_LENGTH: usize = 128;

#[derive(Clone)]
pub(crate) struct Extension {
    /// `publisher.name`.
    pub id: String,
    pub publisher: String,
    pub display_name: String,
    pub description: String,
    pub version: String,
    pub license: String,
    pub downloads: u64,
    pub theme_count: usize,
    vsix_url: String,
    sha256_url: String,
}

/// What an install left in the themes folder.
pub(crate) struct Installed {
    pub written: usize,
    pub skipped: usize,
}

const OPEN_VSX: Service = Service {
    name: "Open VSX",
    rate_limited: &[429],
    accept: None,
};

fn agent() -> ureq::Agent {
    net::agent(Duration::from_secs(60))
}

fn text<'a>(value: &'a Value, key: &str) -> &'a str {
    value.get(key).and_then(Value::as_str).unwrap_or("").trim()
}

fn is_identifier(value: &str) -> bool {
    !value.is_empty()
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

fn trusted_file(detail: &Value, key: &str) -> Option<String> {
    let url = detail.get("files")?.get(key)?.as_str()?;
    url.starts_with(TRUSTED_PREFIX).then(|| url.to_string())
}

/// The themes of this kind that the manifest contributes.
fn contributed(manifest: &Value, kind: ThemeKind) -> &[Value] {
    let key = match kind {
        ThemeKind::Color => "themes",
        ThemeKind::Icon => "iconThemes",
    };
    manifest
        .get("contributes")
        .and_then(|c| c.get(key))
        .and_then(Value::as_array)
        .map_or(&[], Vec::as_slice)
}

/// Where a contributed theme is in the package.
fn contribution_path(contribution: &Value) -> Result<String> {
    package_path(
        contribution
            .get("path")
            .and_then(Value::as_str)
            .context("The theme has no path")?,
        "extension",
    )
}

/// The contribution's label, unless it is missing or `%key%`, which points to a translation file.
fn contribution_label(contribution: &Value) -> Option<&str> {
    let label = text(contribution, "label");
    (!label.is_empty() && !label.starts_with('%')).then_some(label)
}

/// What an installed theme file keeps of where it came from, under `ORIGIN_KEY`.
fn origin_json(extension: &Extension) -> Value {
    json!({
        "extension": extension.id,
        "displayName": extension.display_name,
        "version": extension.version,
        "license": extension.license,
    })
}

/// `None` when it is not something shcp can install: the category also holds other kinds of
/// themes and other extensions, and many licenses are only a pointer to a file nobody has read.
fn usable_extension(
    agent: &ureq::Agent,
    namespace: &str,
    name: &str,
    kind: ThemeKind,
) -> Result<Option<Extension>> {
    let detail = OPEN_VSX.get_json(agent, &format!("{API}/{namespace}/{name}"), MAX_JSON_BYTES)?;
    let license = text(&detail, "license");
    if !LICENSES.contains(&license) {
        return Ok(None);
    }
    let (Some(manifest_url), Some(vsix_url), Some(sha256_url)) = (
        trusted_file(&detail, "manifest"),
        trusted_file(&detail, "download"),
        trusted_file(&detail, "sha256"),
    ) else {
        return Ok(None);
    };
    let manifest = OPEN_VSX.get_json(agent, &manifest_url, MAX_JSON_BYTES)?;
    let theme_count = contributed(&manifest, kind).len();
    if theme_count == 0
        || theme_count > MAX_THEMES_PER_EXTENSION
        || text(&manifest, "license") != license
    {
        return Ok(None);
    }
    let display_name = match text(&detail, "displayName") {
        "" => name,
        display_name => display_name,
    };
    Ok(Some(Extension {
        id: format!("{namespace}.{name}"),
        publisher: namespace.to_string(),
        display_name: display_name.to_string(),
        description: text(&detail, "description").to_string(),
        version: text(&detail, "version").to_string(),
        license: license.to_string(),
        downloads: detail
            .get("downloadCount")
            .and_then(Value::as_u64)
            .unwrap_or(0),
        theme_count,
        vsix_url,
        sha256_url,
    }))
}

/// Most downloaded first. An empty query lists the most popular themes.
pub(crate) fn search(query: &str, kind: ThemeKind) -> Result<Vec<Extension>> {
    let agent = agent();
    let size = SEARCH_SIZE.to_string();
    let mut request = agent
        .get(format!("{API}/-/search"))
        .query("category", "Themes")
        .query("sortBy", "downloadCount")
        .query("sortOrder", "desc")
        .query("size", &size);
    // The category has no way to ask for icon themes only, and color themes fill its top.
    let query = match (query.trim(), kind) {
        ("", ThemeKind::Icon) => "icon",
        (query, _) => query,
    };
    if !query.is_empty() {
        request = request.query("query", query);
    }
    let found = OPEN_VSX.fetch_json(request, MAX_JSON_BYTES)?;
    let identities: Vec<(String, String)> = found
        .get("extensions")
        .and_then(Value::as_array)
        .context("Open VSX sent an unreadable response")?
        .iter()
        .map(|e| {
            (
                text(e, "namespace").to_string(),
                text(e, "name").to_string(),
            )
        })
        .filter(|(namespace, name)| is_identifier(namespace) && is_identifier(name))
        .take(SEARCH_SIZE)
        .collect();
    let details: Vec<Result<Option<Extension>>> = thread::scope(|scope| {
        let handles: Vec<_> = identities
            .iter()
            .map(|(namespace, name)| {
                let agent = &agent;
                scope.spawn(move || usable_extension(agent, namespace, name, kind))
            })
            .collect();
        handles
            .into_iter()
            .map(|handle| {
                handle
                    .join()
                    .unwrap_or_else(|_| bail!("A theme lookup stopped unexpectedly"))
            })
            .collect()
    });
    let mut extensions = Vec::new();
    let mut first_error = None;
    let mut any_answer = false;
    for detail in details {
        match detail {
            Ok(extension) => {
                any_answer = true;
                extensions.extend(extension);
            }
            Err(error) => first_error = first_error.or(Some(error)),
        }
    }
    // One broken entry should not hide the others.
    match first_error {
        Some(error) if !any_answer => Err(error),
        _ => Ok(extensions),
    }
}

/// Downloads an extension and writes its themes of this kind to `dir`.
pub(crate) fn install(extension: &Extension, kind: ThemeKind, dir: &Path) -> Result<Installed> {
    let agent = agent();
    let package = OPEN_VSX.get(&agent, &extension.vsix_url, MAX_PACKAGE_BYTES)?;
    let checksum = OPEN_VSX.get(&agent, &extension.sha256_url, 256)?;
    verify_checksum(&package, &checksum)?;
    match kind {
        ThemeKind::Color => install_package(extension, &package, dir),
        ThemeKind::Icon => install_icon_package(extension, &package, dir),
    }
}

fn verify_checksum(package: &[u8], checksum: &[u8]) -> Result<()> {
    ensure!(
        net::sha256_matches(package, checksum),
        "The downloaded theme failed its integrity check"
    );
    Ok(())
}

/// Opens a package, checking that it is the extension that was selected.
fn open_package<'a>(
    extension: &Extension,
    package: &'a [u8],
) -> Result<(ZipArchive<Cursor<&'a [u8]>>, Value)> {
    let mut zip = ZipArchive::new(Cursor::new(package)).context("The package cannot be opened")?;
    ensure!(
        zip.len() <= MAX_ZIP_ENTRIES,
        "The package has too many files"
    );
    let manifest = read_json(&mut zip, "extension/package.json", MAX_THEME_BYTES)?;
    let id = format!(
        "{}.{}",
        text(&manifest, "publisher"),
        text(&manifest, "name")
    );
    ensure!(
        id.eq_ignore_ascii_case(&extension.id) && text(&manifest, "version") == extension.version,
        "The package is not the theme that was selected"
    );
    ensure!(
        text(&manifest, "license") == extension.license,
        "The package does not match its advertised license"
    );
    Ok((zip, manifest))
}

fn install_package(extension: &Extension, package: &[u8], dir: &Path) -> Result<Installed> {
    let (mut zip, manifest) = open_package(extension, package)?;
    let contributions = contributed(&manifest, ThemeKind::Color);
    ensure!(
        !contributions.is_empty(),
        "The extension has no color themes"
    );
    ensure!(
        contributions.len() <= MAX_THEMES_PER_EXTENSION,
        "The extension has too many color themes"
    );
    std::fs::create_dir_all(dir).context("Create the themes folder")?;
    let mut names = Vec::new();
    let mut skipped = 0;
    for contribution in contributions {
        // Formats shcp cannot read, like .tmTheme files, should not cost the other variants.
        match build_theme_file(extension, contribution, &mut zip, &names) {
            Ok((name, file_name, content)) => {
                std::fs::write(dir.join(file_name), content).context("Write the theme file")?;
                names.push(name);
            }
            Err(_) => skipped += 1,
        }
    }
    ensure!(
        !names.is_empty(),
        "None of the extension's themes can be used"
    );
    Ok(Installed {
        written: names.len(),
        skipped,
    })
}

fn build_theme_file(
    extension: &Extension,
    contribution: &Value,
    zip: &mut ZipArchive<Cursor<&[u8]>>,
    taken: &[String],
) -> Result<(String, String, String)> {
    let path = contribution_path(contribution)?;
    let (theme_name, colors) = load_colors(zip, &path, MAX_INCLUDE_DEPTH)?;
    // Without it the theme relies on VS Code's defaults, which shcp does not know.
    ensure!(
        colors.contains_key("editor.background"),
        "The theme does not set its background"
    );
    // Without a label the theme's own name is good enough.
    let name = if let Some(label) = contribution_label(contribution) {
        label.to_string()
    } else if !theme_name.is_empty() {
        theme_name
    } else {
        extension.display_name.clone()
    };
    ensure!(!taken.contains(&name), "Another variant has this name");
    let kind = match text(contribution, "uiTheme") {
        "vs" => "light",
        "hc-light" => "hc-light",
        "hc-black" => "hc-black",
        _ => "dark",
    };
    let content = serde_json::to_string_pretty(&json!({
        "name": name,
        "type": kind,
        ORIGIN_KEY: origin_json(extension),
        "colors": colors,
    }))?;
    vscode::parse(&content, &name)?;
    let stem = path.rsplit('/').next().unwrap_or(&path);
    let stem = stem.strip_suffix(".json").unwrap_or(stem);
    Ok((name, file_name(&extension.id, stem), content))
}

fn install_icon_package(extension: &Extension, package: &[u8], dir: &Path) -> Result<Installed> {
    let (mut zip, manifest) = open_package(extension, package)?;
    let contributions = contributed(&manifest, ThemeKind::Icon);
    ensure!(
        !contributions.is_empty(),
        "The extension has no file icon themes"
    );
    ensure!(
        contributions.len() <= MAX_THEMES_PER_EXTENSION,
        "The extension has too many file icon themes"
    );
    std::fs::create_dir_all(dir).context("Create the icon themes folder")?;
    let mut names = Vec::new();
    let mut last_error = None;
    let mut skipped = 0;
    for contribution in contributions {
        match write_icon_theme(extension, contribution, &mut zip, dir, &names) {
            Ok(name) => names.push(name),
            Err(error) => {
                last_error = Some(error);
                skipped += 1;
            }
        }
    }
    match (names.len(), last_error) {
        // The reason says more than "cannot be used", as with a theme made of a font.
        (0, Some(error)) => Err(error),
        (written, _) => Ok(Installed { written, skipped }),
    }
}

/// The keys of an icon theme that shcp reads. Their values name icon definitions.
const ICON_SINGLE_KEYS: [&str; 2] = ["file", "folder"];
const ICON_MAP_KEYS: [&str; 4] = ["fileExtensions", "fileNames", "folderNames", "languageIds"];

fn used_definitions<'a>(set: &'a Value, used: &mut Vec<&'a str>) {
    for key in ICON_SINGLE_KEYS {
        used.extend(set.get(key).and_then(Value::as_str));
    }
    for key in ICON_MAP_KEYS {
        if let Some(map) = set.get(key).and_then(Value::as_object) {
            used.extend(map.values().filter_map(Value::as_str));
        }
    }
}

/// The same associations, pointing at the numbers of the pictures that could be kept.
fn renumbered_set(set: &Value, numbers: &HashMap<&str, u32>) -> Value {
    let number = |value: &Value| value.as_str().and_then(|id| numbers.get(id)).copied();
    let mut out = Map::new();
    for key in ICON_SINGLE_KEYS {
        if let Some(found) = set.get(key).and_then(number) {
            out.insert(key.to_string(), found.into());
        }
    }
    for key in ICON_MAP_KEYS {
        if let Some(map) = set.get(key).and_then(Value::as_object) {
            let kept: Map<String, Value> = map
                .iter()
                .filter_map(|(name, value)| Some((name.clone(), number(value)?.into())))
                .collect();
            out.insert(key.to_string(), kept.into());
        }
    }
    out.into()
}

fn write_icon_theme(
    extension: &Extension,
    contribution: &Value,
    zip: &mut ZipArchive<Cursor<&[u8]>>,
    dir: &Path,
    taken: &[String],
) -> Result<String> {
    let path = contribution_path(contribution)?;
    let theme = read_json(zip, &path, MAX_ICON_THEME_BYTES)?;
    let base = path.rsplit_once('/').map_or("extension", |(base, _)| base);
    let empty = Value::Null;
    let light = theme.get("light").unwrap_or(&empty);
    let mut used = Vec::new();
    used_definitions(&theme, &mut used);
    used_definitions(light, &mut used);
    used.sort_unstable();
    used.dedup();

    let definitions = theme.get("iconDefinitions").unwrap_or(&empty);
    let mut pictures: Vec<String> = Vec::new();
    let mut numbers: HashMap<&str, u32> = HashMap::new();
    for id in used {
        let picture = definitions
            .get(id)
            .and_then(|definition| definition.get("iconPath"))
            .and_then(Value::as_str)
            .filter(|path| path.to_ascii_lowercase().ends_with(".svg"))
            .and_then(|path| package_path(path, base).ok())
            .and_then(|path| read_text_limited(zip, &path, MAX_ICON_BYTES).ok())
            .and_then(|svg| normalize_svg(&svg));
        if let Some(picture) = picture {
            numbers.insert(id, pictures.len() as u32);
            pictures.push(picture);
        }
    }
    ensure!(
        !pictures.is_empty(),
        "It draws its icons with a font, which shcp cannot use"
    );

    let name = contribution_label(contribution)
        .map_or_else(|| extension.display_name.clone(), str::to_string);
    ensure!(!taken.contains(&name), "Another variant has this name");
    let content = serde_json::to_string(&json!({
        "name": name,
        ORIGIN_KEY: origin_json(extension),
        "set": renumbered_set(&theme, &numbers),
        "light": renumbered_set(light, &numbers),
    }))?;

    // Built aside, so that a failure leaves what was installed before as it was.
    let stem = text(contribution, "id");
    let folder = file_name(&extension.id, stem);
    let folder = folder.strip_suffix(".json").unwrap_or(&folder);
    let target = dir.join(folder);
    let building = dir.join(format!("{folder}.building"));
    let _ = std::fs::remove_dir_all(&building);
    let icons = building.join(ICONS_DIR);
    std::fs::create_dir_all(&icons).context("Create the icon theme's folder")?;
    for (number, picture) in pictures.iter().enumerate() {
        std::fs::write(icons.join(format!("{number}.svg")), picture)
            .context("Write an icon of the theme")?;
    }
    std::fs::write(building.join(THEME_FILE), content).context("Write the icon theme")?;
    if target.exists() {
        std::fs::remove_dir_all(&target).context("Replace the installed icon theme")?;
    }
    std::fs::rename(&building, &target).context("Finish installing the icon theme")?;
    Ok(name)
}

/// The picture with a fixed size on its root element, or `None` when it is not one shcp should
/// draw: what could reach outside the picture, or what has no size to scale from.
fn normalize_svg(svg: &str) -> Option<String> {
    let lower = svg.to_ascii_lowercase();
    if ["<image", "<script", "<foreignobject", "<!entity"]
        .iter()
        .any(|tag| lower.contains(tag))
    {
        return None;
    }
    let start = lower.find("<svg")? + "<svg".len();
    let mut rest = &svg[start..];
    let mut attributes: Vec<(&str, &str)> = Vec::new();
    loop {
        let trimmed = rest.trim_start();
        if trimmed.starts_with('>') {
            rest = trimmed;
            break;
        }
        let equals = trimmed.find('=')?;
        let name = trimmed[..equals].trim();
        let value = trimmed[equals + 1..].trim_start();
        let quote = value.chars().next().filter(|c| matches!(c, '"' | '\''))?;
        let end = value[1..].find(quote)?;
        attributes.push((name, &value[1..=end]));
        rest = &value[end + 2..];
    }
    let attribute = |wanted: &str| {
        attributes
            .iter()
            .find(|(name, _)| *name == wanted)
            .map(|(_, value)| *value)
    };
    let view_box = match attribute("viewBox") {
        Some(view_box) => view_box.to_string(),
        None => {
            let side = |value: &str| value.trim_end_matches("px").parse::<f32>().ok();
            let width = side(attribute("width")?)?;
            let height = side(attribute("height")?)?;
            format!("0 0 {width} {height}")
        }
    };
    let mut out = svg[..start].to_string();
    for (name, value) in &attributes {
        if !matches!(*name, "width" | "height" | "viewBox") {
            let _ = write!(out, " {name}=\"{value}\"");
        }
    }
    let _ = write!(
        out,
        " viewBox=\"{view_box}\" width=\"{ICON_SIZE}\" height=\"{ICON_SIZE}\""
    );
    out.push_str(rest);
    Some(out)
}

fn file_name(extension: &str, stem: &str) -> String {
    let safe: String = format!("{extension}.{stem}")
        .chars()
        .map(|c| match c.to_ascii_lowercase() {
            c @ ('a'..='z' | '0'..='9' | '.' | '-' | '_') => c,
            _ => '-',
        })
        .collect();
    format!("{safe}.json")
}

/// Joins a path from a package to `base`, refusing anything that would leave the extension.
fn package_path(path: &str, base: &str) -> Result<String> {
    let path = path.replace('\\', "/");
    ensure!(
        !path.starts_with('/') && !path.contains(':') && !path.contains('\0'),
        "The theme path is not relative"
    );
    let mut segments: Vec<&str> = base.split('/').collect();
    for segment in path.split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                ensure!(segments.len() > 1, "The theme path leaves the package");
                segments.pop();
            }
            segment => segments.push(segment),
        }
    }
    Ok(segments.join("/"))
}

/// A theme's name and colors, with those of the themes it includes underneath.
fn load_colors(
    zip: &mut ZipArchive<Cursor<&[u8]>>,
    path: &str,
    depth: usize,
) -> Result<(String, Map<String, Value>)> {
    ensure!(depth > 0, "Theme includes are nested too deeply");
    let theme = read_json(zip, path, MAX_THEME_BYTES)?;
    let mut colors = Map::new();
    if let Some(include) = theme.get("include").and_then(Value::as_str) {
        let base = path.rsplit_once('/').map_or("extension", |(base, _)| base);
        let included = package_path(include, base)?;
        colors = load_colors(zip, &included, depth - 1)?.1;
    }
    if let Some(own) = theme.get("colors").and_then(Value::as_object) {
        for (key, value) in own {
            if value.as_str().is_some_and(|c| c.len() <= MAX_COLOR_LENGTH) {
                colors.insert(key.clone(), value.clone());
            }
        }
    }
    Ok((text(&theme, "name").to_string(), colors))
}

fn read_json(zip: &mut ZipArchive<Cursor<&[u8]>>, path: &str, limit: u64) -> Result<Value> {
    let text = read_text_limited(zip, path, limit)?;
    serde_json::from_str(&vscode::strip_jsonc(&text)).with_context(|| format!("{path} is invalid"))
}

fn read_text_limited(
    zip: &mut ZipArchive<Cursor<&[u8]>>,
    path: &str,
    limit: u64,
) -> Result<String> {
    let entry = zip
        .by_name(path)
        .with_context(|| format!("{path} is missing from the package"))?;
    ensure!(entry.size() <= limit, "{path} is too large");
    // The size above is only what the archive claims.
    let mut bytes = Vec::new();
    entry
        .take(limit + 1)
        .read_to_end(&mut bytes)
        .with_context(|| format!("{path} cannot be read"))?;
    ensure!(bytes.len() as u64 <= limit, "{path} is too large");
    let text = String::from_utf8_lossy(&bytes);
    Ok(text.strip_prefix('\u{feff}').unwrap_or(&text).to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::sync::atomic::{AtomicU32, Ordering};
    use zip::write::SimpleFileOptions;

    fn extension() -> Extension {
        Extension {
            id: "acme.night".into(),
            publisher: "acme".into(),
            display_name: "Night".into(),
            description: String::new(),
            version: "1.0.0".into(),
            license: "MIT".into(),
            downloads: 0,
            theme_count: 1,
            vsix_url: String::new(),
            sha256_url: String::new(),
        }
    }

    #[test]
    fn a_picture_gets_a_fixed_size_and_keeps_its_scale() {
        let svg = r#"<?xml version="1.0"?><svg xmlns="x" width="512px" height="256"><path/></svg>"#;
        let out = normalize_svg(svg).unwrap();
        assert!(out.contains(r#"viewBox="0 0 512 256""#));
        assert!(out.contains(r#"width="32" height="32""#));
        assert!(out.contains(r#"xmlns="x""#));
        assert!(!out.contains("512px"));
        assert!(out.ends_with("><path/></svg>"));
    }

    #[test]
    fn a_picture_keeps_its_view_box() {
        let out = normalize_svg(r#"<svg viewBox='0 0 24 24'></svg>"#).unwrap();
        assert!(out.contains(r#"viewBox="0 0 24 24""#));
    }

    #[test]
    fn a_picture_that_reaches_outside_or_has_no_size_is_refused() {
        assert!(normalize_svg(r#"<svg viewBox="0 0 1 1"><image href="a"/></svg>"#).is_none());
        assert!(normalize_svg(r#"<svg viewBox="0 0 1 1"><script/></svg>"#).is_none());
        assert!(normalize_svg(r#"<svg width="100%" height="100%"></svg>"#).is_none());
        assert!(normalize_svg("not a picture").is_none());
    }

    fn icon_package(theme: &str) -> Vec<u8> {
        package(&[
            (
                "extension/package.json",
                r#"{"publisher":"acme","name":"night","version":"1.0.0","license":"MIT",
                    "contributes":{"iconThemes":[
                        {"id":"glyphs","label":"Glyphs","path":"./dist/theme.json"}]}}"#,
            ),
            ("extension/dist/theme.json", theme),
            ("extension/icons/a.svg", r#"<svg viewBox="0 0 1 1"></svg>"#),
            ("extension/icons/b.svg", r#"<svg viewBox="0 0 2 2"></svg>"#),
        ])
    }

    #[test]
    fn an_icon_theme_keeps_only_the_pictures_it_uses() {
        let dir = std::env::temp_dir().join(format!("shcp-icons-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let package = icon_package(
            r#"{"iconDefinitions":{
                    "a":{"iconPath":"../icons/a.svg"},
                    "b":{"iconPath":"../icons/b.svg"},
                    "font":{"fontCharacter":"\\E001"}},
                "file":"a",
                "fileExtensions":{"rs":"font","txt":"a"}}"#,
        );
        let installed = install_icon_package(&extension(), &package, &dir).unwrap();
        assert_eq!((installed.written, installed.skipped), (1, 0));
        let folder = dir.join("acme.night.glyphs");
        let theme: Value =
            serde_json::from_str(&std::fs::read_to_string(folder.join(THEME_FILE)).unwrap())
                .unwrap();
        assert_eq!(theme["name"], "Glyphs");
        assert_eq!(theme["set"]["file"], 0);
        assert_eq!(theme["set"]["fileExtensions"]["txt"], 0);
        // What a font draws has no picture to keep.
        assert!(theme["set"]["fileExtensions"].get("rs").is_none());
        assert!(folder.join(ICONS_DIR).join("0.svg").exists());
        assert!(!folder.join(ICONS_DIR).join("1.svg").exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn an_icon_theme_made_of_a_font_says_so() {
        let dir = std::env::temp_dir().join(format!("shcp-font-icons-{}", std::process::id()));
        let package =
            icon_package(r#"{"iconDefinitions":{"a":{"fontCharacter":"\\E001"}},"file":"a"}"#);
        let error = install_icon_package(&extension(), &package, &dir)
            .err()
            .unwrap();
        assert!(error.to_string().contains("font"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn package(files: &[(&str, &str)]) -> Vec<u8> {
        let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
        for (name, content) in files {
            zip.start_file(*name, SimpleFileOptions::default()).unwrap();
            zip.write_all(content.as_bytes()).unwrap();
        }
        zip.finish().unwrap().into_inner()
    }

    fn manifest(themes: &str) -> String {
        format!(
            r#"{{"publisher":"acme","name":"night","version":"1.0.0","license":"MIT",
                "contributes":{{"themes":{themes}}}}}"#
        )
    }

    fn temp_dir() -> std::path::PathBuf {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "shcp-open-vsx-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn only_theme(dir: &Path) -> Value {
        let mut files = std::fs::read_dir(dir).unwrap();
        let file = files.next().unwrap().unwrap().path();
        assert!(files.next().is_none());
        serde_json::from_str(&std::fs::read_to_string(file).unwrap()).unwrap()
    }

    #[test]
    fn package_paths_stay_inside_the_extension() {
        assert_eq!(
            package_path("./themes/a.json", "extension").unwrap(),
            "extension/themes/a.json"
        );
        assert_eq!(
            package_path("../b.json", "extension/themes").unwrap(),
            "extension/b.json"
        );
        assert!(package_path("../../etc/passwd", "extension").is_err());
        assert!(package_path("/etc/passwd", "extension").is_err());
        assert!(package_path("C:\\Windows\\win.ini", "extension").is_err());
    }

    #[test]
    fn file_names_cannot_leave_the_themes_folder() {
        assert_eq!(
            file_name("acme.night", "../../My Theme"),
            "acme.night...-..-my-theme.json"
        );
    }

    #[test]
    fn includes_are_merged_and_only_colors_are_kept() {
        let dir = temp_dir();
        let bytes = package(&[
            (
                "extension/package.json",
                &manifest(
                    r#"[{"label":"Night","uiTheme":"vs-dark","path":"./themes/night.json"}]"#,
                ),
            ),
            (
                "extension/themes/base.json",
                r##"{"colors":{"editor.background":"#000000","editor.foreground":"#111111"}}"##,
            ),
            (
                "extension/themes/night.json",
                r##"{ // comment
                    "include":"./base.json",
                    "tokenColors":[{"scope":"x"}],
                    "colors":{"editor.foreground":"#222222",},
                }"##,
            ),
        ]);
        let installed = install_package(&extension(), &bytes, &dir).unwrap();
        assert_eq!((installed.written, installed.skipped), (1, 0));
        let theme = only_theme(&dir);
        assert_eq!(theme["name"], "Night");
        assert_eq!(theme["type"], "dark");
        assert_eq!(theme["colors"]["editor.background"], "#000000");
        assert_eq!(theme["colors"]["editor.foreground"], "#222222");
        assert!(theme.get("tokenColors").is_none());
        assert_eq!(theme[ORIGIN_KEY]["extension"], "acme.night");
        let parsed = vscode::parse(&theme.to_string(), "x").unwrap();
        assert_eq!(parsed.origin.unwrap().extension, "acme.night");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn unusable_variants_are_skipped_but_not_the_others() {
        let dir = temp_dir();
        let bytes = package(&[
            (
                "extension/package.json",
                &manifest(
                    r#"[{"label":"Old","uiTheme":"vs","path":"./old.tmTheme"},
                       {"label":"Bare","uiTheme":"vs","path":"./bare.json"},
                       {"label":"%label%","uiTheme":"vs","path":"./good.json"}]"#,
                ),
            ),
            ("extension/old.tmTheme", "<plist></plist>"),
            ("extension/bare.json", r#"{"colors":{}}"#),
            (
                "extension/good.json",
                r##"{"name":"Good","colors":{"editor.background":"#ffffff"}}"##,
            ),
        ]);
        let installed = install_package(&extension(), &bytes, &dir).unwrap();
        assert_eq!((installed.written, installed.skipped), (1, 2));
        let theme = only_theme(&dir);
        assert_eq!(theme["name"], "Good");
        assert_eq!(theme["type"], "light");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn includes_that_loop_are_refused() {
        let dir = temp_dir();
        let bytes = package(&[
            (
                "extension/package.json",
                &manifest(r#"[{"label":"Loop","path":"./a.json"}]"#),
            ),
            ("extension/a.json", r#"{"include":"./a.json"}"#),
        ]);
        assert!(install_package(&extension(), &bytes, &dir).is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_package_that_is_not_the_selected_one_is_refused() {
        let dir = temp_dir();
        let other = manifest("[]").replace("\"night\"", "\"other\"");
        let bytes = package(&[("extension/package.json", &other)]);
        let error = install_package(&extension(), &bytes, &dir).err().unwrap();
        assert!(error.to_string().contains("not the theme"));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_different_license_in_the_package_is_refused() {
        let dir = temp_dir();
        let other = manifest("[]").replace("\"MIT\"", "\"GPL-3.0\"");
        let bytes = package(&[("extension/package.json", &other)]);
        let error = install_package(&extension(), &bytes, &dir).err().unwrap();
        assert!(error.to_string().contains("license"));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn checksums_are_verified() {
        // SHA-256 of "abc".
        let good = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad  x.vsix\n";
        assert!(verify_checksum(b"abc", good.as_bytes()).is_ok());
        assert!(verify_checksum(b"abd", good.as_bytes()).is_err());
        assert!(verify_checksum(b"abc", b"").is_err());
    }

    /// Talks to the real registry: `cargo test -p shcp-app -- --ignored`.
    #[test]
    #[ignore]
    fn real_icon_themes_install() {
        let dir = temp_dir();
        let found = search("", ThemeKind::Icon).unwrap();
        for id in [
            "PKief.material-icon-theme",
            "vscode-icons-team.vscode-icons",
        ] {
            let extension = found
                .iter()
                .find(|e| e.id == id)
                .unwrap_or_else(|| panic!("{id} not found"));
            install(extension, ThemeKind::Icon, &dir).unwrap();
        }
        let (catalog, errors) = crate::theme::icon_theme::IconThemeCatalog::load(&dir);
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!(catalog.themes.len(), 2);
        for theme in &catalog.themes {
            for (name, is_dir) in [
                ("src", true),
                ("main.rs", false),
                ("app.py", false),
                ("Dockerfile", false),
                ("notes.unknown", false),
            ] {
                let path = theme.icon(name, is_dir, false).unwrap();
                println!("{}: {name} -> {}", theme.name, path.display());
                assert!(path.exists(), "{name} has no picture in {}", theme.name);
            }
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Talks to the real registry: `cargo test -p shcp-app -- --ignored`.
    #[test]
    #[ignore]
    fn real_themes_install() {
        let dir = temp_dir();
        for (query, id) in [
            ("tokyo night", "enkia.tokyo-night"),
            ("catppuccin", "Catppuccin.catppuccin-vsc"),
            ("one dark pro", "zhuangtongfa.material-theme"),
            ("github", "GitHub.github-vscode-theme"),
            ("dracula", "dracula-theme.theme-dracula"),
        ] {
            let found = search(query, ThemeKind::Color).unwrap();
            let listed: Vec<&str> = found.iter().map(|e| e.id.as_str()).collect();
            println!("{query}: {listed:?}");
            let extension = found
                .iter()
                .find(|e| e.id == id)
                .unwrap_or_else(|| panic!("{id} not found for {query}"));
            let installed = install(extension, ThemeKind::Color, &dir).unwrap();
            println!(
                "{id} v{} {}: {} written, {} skipped",
                extension.version, extension.license, installed.written, installed.skipped
            );
        }
        let (catalog, errors) = crate::theme::ThemeCatalog::load(&dir);
        assert!(errors.is_empty(), "{errors:?}");
        for theme in &catalog.themes {
            println!(
                "{:<36} light={} bg={:?}",
                theme.name.as_ref(),
                theme.is_light,
                theme.bg.to_rgb()
            );
        }
        std::fs::remove_dir_all(dir).unwrap();
    }
}
