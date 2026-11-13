//! Formatting helpers for the file browser (sizes, times, permissions, remote paths).
use chrono::{DateTime, Local, Utc};
use shcp_core::{EntryKind, RemoteEntry};
use std::time::SystemTime;

/// A size as a number and its unit: bytes whole, larger units with one decimal.
fn scaled(size: u64) -> (String, &'static str) {
    let mut value = size as f64;
    for unit in ["B", "KB", "MB", "GB"] {
        if value < 1024. {
            return if unit == "B" {
                (size.to_string(), unit)
            } else {
                (format!("{value:.1}"), unit)
            };
        }
        value /= 1024.;
    }
    (format!("{value:.1}"), "TB")
}

pub(crate) fn bytes(size: u64) -> String {
    let (number, unit) = scaled(size);
    format!("{number} {unit}")
}

/// A size in two parts, the number and its unit, for columns that set them apart.
pub(crate) fn size_parts(entry: &RemoteEntry) -> (String, &'static str) {
    if entry.kind == EntryKind::Dir {
        return (String::new(), "");
    }
    scaled(entry.size)
}

/// The short label a file wears in the list: its extension, or what a dot file is about.
pub(crate) fn badge(name: &str) -> String {
    let short = |text: &str| text.chars().take(4).collect::<String>().to_lowercase();
    match name.strip_prefix('.') {
        Some(rest) if !rest.contains('.') => short(rest),
        _ => match name.rsplit_once('.') {
            Some((stem, extension)) if !stem.is_empty() && !extension.is_empty() => {
                short(extension)
            }
            _ => "—".to_string(),
        },
    }
}

/// How long ago, for a list: minutes, hours, days, then the date.
pub(crate) fn relative(time: Option<SystemTime>, now: SystemTime) -> String {
    let Some(time) = time else {
        return String::new();
    };
    let Ok(ago) = now.duration_since(time) else {
        return "just now".to_string();
    };
    let minutes = ago.as_secs() / 60;
    match minutes {
        0..=1 => "just now".to_string(),
        2..=59 => format!("{minutes} min ago"),
        60..=1439 => format!("{} h ago", (minutes + 30) / 60),
        1440..=2879 => "yesterday".to_string(),
        2880..=43199 => format!("{} days ago", (minutes + 720) / 1440),
        _ => DateTime::<Local>::from(time)
            .format("%-d %b %Y")
            .to_string(),
    }
}

/// `YYYY-MM-DD HH:MM` in the time zone of this PC.
pub(crate) fn modified(time: Option<SystemTime>) -> String {
    time.map_or_else(String::new, |time| {
        DateTime::<Local>::from(time)
            .format("%Y-%m-%d %H:%M")
            .to_string()
    })
}

/// The same moment in UTC, for the tooltip.
pub(crate) fn modified_utc(time: Option<SystemTime>) -> String {
    time.map_or_else(String::new, |time| {
        DateTime::<Utc>::from(time)
            .format("%Y-%m-%d %H:%M UTC")
            .to_string()
    })
}

pub(crate) fn permissions(entry: &RemoteEntry) -> String {
    let Some(mode) = entry.permissions else {
        return String::new();
    };
    let kind = match entry.kind {
        EntryKind::Dir => 'd',
        EntryKind::Symlink => 'l',
        _ => '-',
    };
    let bits: String = "rwxrwxrwx"
        .chars()
        .enumerate()
        .map(|(i, c)| if mode & (1 << (8 - i)) != 0 { c } else { '-' })
        .collect();
    format!("{kind}{bits}")
}

pub(crate) fn join(dir: &str, name: &str) -> String {
    if dir.ends_with('/') {
        format!("{dir}{name}")
    } else {
        format!("{dir}/{name}")
    }
}

pub(crate) fn parent(path: &str) -> String {
    let trimmed = path.trim_end_matches('/');
    match trimmed.rfind('/') {
        Some(0) | None => "/".to_string(),
        Some(i) => trimmed[..i].to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn a_badge_is_the_extension_in_short() {
        assert_eq!(badge("nginx.conf"), "conf");
        assert_eq!(badge("backup-2026.tar.gz"), "gz");
        assert_eq!(badge("README.MD"), "md");
        assert_eq!(badge("package.jsonld"), "json");
        assert_eq!(badge(".env"), "env");
        assert_eq!(badge(".bash.old"), "old");
        assert_eq!(badge("Makefile"), "—");
    }

    #[test]
    fn times_read_as_how_long_ago() {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(10_000_000);
        let ago = |seconds: u64| relative(Some(now - Duration::from_secs(seconds)), now);
        assert_eq!(ago(30), "just now");
        assert_eq!(ago(600), "10 min ago");
        assert_eq!(ago(3 * 3600), "3 h ago");
        assert_eq!(ago(30 * 3600), "yesterday");
        assert_eq!(ago(5 * 86400), "5 days ago");
        assert_eq!(relative(None, now), "");
    }
}
