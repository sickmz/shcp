//! Which shell a user logs in with on a host, for the app to show it. Asked once per connection,
//! on a channel of its own: the terminals never see it. This is the login shell, not whatever the
//! user starts in a terminal later.
use crate::{
    Event,
    session::{Context, Ssh},
    tmux,
};
use std::{sync::Arc, time::Duration};
use tokio::time::timeout;

/// Long enough for a slow login shell, short enough not to linger past a disconnection.
const ASK_TIMEOUT: Duration = Duration::from_secs(10);
/// What starts the answer: a shell's startup files may print anything before it.
const MARKER: &str = "shcp-login-shell:";

/// Written for sh, whatever the login shell is: `$SHELL`, or the user's passwd entry without it.
/// A server without sh, Windows for one, just gives no answer.
fn command() -> String {
    format!(
        "sh -c 'S=\"$SHELL\"; [ -n \"$S\" ] || \
         S=$(getent passwd \"$(id -un)\" 2>/dev/null | cut -d: -f7); echo \"{MARKER}$S\"'"
    )
}

/// Asks, and says what it learned. Says nothing when the server would not tell: it stays unknown.
pub(crate) async fn learn(ctx: Arc<Context>, ssh: Arc<Ssh>) {
    let output = tokio::select! { biased;
        _ = ctx.cancel.cancelled() => return,
        output = timeout(ASK_TIMEOUT, tmux::read_all(&ssh, command())) => output,
    };
    let Some(shell) = output.ok().and_then(|output| parse(&output)) else {
        tracing::debug!(session = ctx.id.0, "Login shell not known");
        return;
    };
    ctx.emit(Event::LoginShell {
        session: ctx.id,
        shell,
    });
}

/// The shell named on the line that starts with the marker, if any.
fn parse(output: &[u8]) -> Option<String> {
    let text = String::from_utf8_lossy(output);
    text.lines()
        .find_map(|line| line.trim().strip_prefix(MARKER))
        .and_then(shell_name)
}

/// The name of a shell from its path: `/usr/bin/zsh` is `zsh`, a Windows one loses its `.exe`.
/// Anything that does not look like the name of a program is no name.
fn shell_name(path: &str) -> Option<String> {
    let path = path.trim();
    let name = path.rsplit(['/', '\\']).next()?;
    let name = match name.len().checked_sub(4) {
        Some(stem)
            if name
                .get(stem..)
                .is_some_and(|end| end.eq_ignore_ascii_case(".exe")) =>
        {
            &name[..stem]
        }
        _ => name,
    };
    let valid = !name.is_empty()
        && name.len() <= 32
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '+'));
    valid.then(|| name.to_ascii_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_path_gives_the_name_of_its_shell() {
        assert_eq!(shell_name("/bin/bash").as_deref(), Some("bash"));
        assert_eq!(shell_name("/usr/bin/zsh\n").as_deref(), Some("zsh"));
        assert_eq!(shell_name("/usr/bin/zsh").as_deref(), Some("zsh"));
        assert_eq!(shell_name("fish").as_deref(), Some("fish"));
        assert_eq!(
            shell_name("/opt/microsoft/powershell/7/pwsh").as_deref(),
            Some("pwsh")
        );
        assert_eq!(
            shell_name(r"C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe").as_deref(),
            Some("powershell")
        );
        assert_eq!(
            shell_name("powershell.exe\r\n").as_deref(),
            Some("powershell")
        );
    }

    #[test]
    fn what_is_not_a_shell_has_no_name() {
        assert_eq!(shell_name(""), None);
        assert_eq!(shell_name("\n"), None);
        assert_eq!(shell_name("/bin/"), None);
        assert_eq!(shell_name("Last login: today from somewhere"), None);
        assert_eq!(shell_name("\u{1b}[0mzsh"), None);
        assert_eq!(shell_name(".exe"), None);
    }

    #[test]
    fn only_the_marked_line_counts() {
        let output = format!("Welcome!\n/bin/zsh\n{MARKER}/usr/bin/fish\n");
        assert_eq!(parse(output.as_bytes()).as_deref(), Some("fish"));
        assert_eq!(
            parse(format!("{MARKER}/bin/bash").as_bytes()).as_deref(),
            Some("bash")
        );
        assert_eq!(parse(format!("{MARKER}\n").as_bytes()), None);
        assert_eq!(parse(b"/bin/bash\n"), None);
        assert_eq!(parse(b""), None);
    }
}
