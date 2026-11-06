use crate::{ConnectError, ConnectErrorKind, HostConfig, config::ssh::expand};
use parking_lot::Mutex;
use std::{
    collections::VecDeque,
    io,
    pin::Pin,
    process::Stdio,
    sync::Arc,
    task::{Context, Poll},
};
use tokio::{
    io::{AsyncRead, AsyncWrite, BufReader, ReadBuf},
    process::{Child, ChildStdin, ChildStdout, Command},
};

pub(crate) struct Proxy {
    child: Child,
    /// The command as configured, which errors look into.
    command: String,
    pub stderr: Arc<Mutex<VecDeque<String>>>,
    reader: tokio::task::JoinHandle<()>,
}
impl Drop for Proxy {
    fn drop(&mut self) {
        let _ = self.child.start_kill();
        self.reader.abort();
    }
}
impl Proxy {
    pub async fn settle_stderr(&mut self) {
        // An exited proxy normally closes stderr immediately; allow its reader to drain.
        let _ = tokio::time::timeout(std::time::Duration::from_millis(200), &mut self.reader).await;
    }
    pub fn error(&self, error: &str) -> ConnectError {
        let stderr = self
            .stderr
            .lock()
            .iter()
            .cloned()
            .collect::<Vec<_>>()
            .join("\n");
        classify(&self.command, error, &stderr, false)
    }
}
pub(crate) struct StdioStream {
    input: ChildStdin,
    output: ChildStdout,
    first_byte: Option<Box<dyn FnOnce() + Send>>,
}
impl StdioStream {
    /// `event` runs when the proxy first says something: the way to the server is open, however
    /// long that took. A tunnel that has to be set up, as with AWS SSM, spends its time here.
    pub fn on_first_byte(&mut self, event: impl FnOnce() + Send + 'static) {
        self.first_byte = Some(Box::new(event));
    }
}
impl AsyncRead for StdioStream {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let before = buf.filled().len();
        let result = Pin::new(&mut self.output).poll_read(cx, buf);
        if buf.filled().len() > before
            && let Some(event) = self.first_byte.take()
        {
            event();
        }
        result
    }
}
impl AsyncWrite for StdioStream {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.input).poll_write(cx, buf)
    }
    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.input).poll_flush(cx)
    }
    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.input).poll_shutdown(cx)
    }
}
fn command(program: &str, args: &[String]) -> Command {
    let mut cmd = Command::new(program);
    cmd.args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(windows)]
    {
        cmd.creation_flags(0x0800_0000);
    }
    cmd
}
pub(crate) fn spawn(host: &HostConfig) -> anyhow::Result<(Proxy, StdioStream)> {
    let port = host.port.to_string();
    let raw = expand(
        host.proxy_command.as_deref().unwrap_or_default(),
        &[
            ('h', &host.hostname),
            ('p', &port),
            ('r', &host.user),
            ('n', &host.alias),
        ],
    );
    // shell-words is POSIX; protect literal Windows path separators before splitting.
    let escaped = if cfg!(windows) {
        raw.replace('\\', "\\\\")
    } else {
        raw.clone()
    };
    let args = shell_words::split(&escaped)?;
    let (program, args) = args
        .split_first()
        .ok_or_else(|| anyhow::anyhow!("ProxyCommand is empty"))?;
    let result = command(program, args).spawn();
    #[cfg(windows)]
    let result = match result {
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            command("cmd", &["/C".into(), raw.clone()]).spawn()
        }
        other => other,
    };
    let mut child = result?;
    let input = child
        .stdin
        .take()
        .ok_or_else(|| anyhow::anyhow!("Proxy stdin unavailable"))?;
    let output = child
        .stdout
        .take()
        .ok_or_else(|| anyhow::anyhow!("Proxy stdout unavailable"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| anyhow::anyhow!("Proxy stderr unavailable"))?;
    let buffer = Arc::new(Mutex::new(VecDeque::new()));
    let log = buffer.clone();
    let reader = tokio::spawn(async move {
        let mut reader = BufReader::new(stderr);
        // Fixed-size reads keep a malicious or noisy proxy from buffering an unlimited line.
        let mut bytes = vec![0; 4096];
        use tokio::io::AsyncReadExt;
        let mut pending = String::new();
        loop {
            match reader.read(&mut bytes).await {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    pending.push_str(&String::from_utf8_lossy(&bytes[..n]));
                    while let Some(end) = pending.find('\n') {
                        let line: String = pending.drain(..=end).collect();
                        record(&log, line.trim_end().into());
                    }
                    if pending.len() > 8192 {
                        record(&log, std::mem::take(&mut pending));
                    }
                }
            }
        }
        if !pending.is_empty() {
            record(&log, pending);
        }
    });
    Ok((
        Proxy {
            child,
            command: host.proxy_command.clone().unwrap_or_default(),
            stderr: buffer,
            reader,
        },
        StdioStream {
            input,
            output,
            first_byte: None,
        },
    ))
}
fn record(buffer: &Mutex<VecDeque<String>>, line: String) {
    tracing::info!(message = %line, "SSH proxy stderr");
    let mut buffer = buffer.lock();
    if buffer.len() == 50 {
        buffer.pop_front();
    }
    buffer.push_back(line);
}
pub(crate) fn classify(command: &str, error: &str, stderr: &str, not_found: bool) -> ConnectError {
    let lower = stderr.to_ascii_lowercase();
    let (kind, hint) =
        if not_found || lower.contains("is not recognized as an internal or external command") {
            (
                ConnectErrorKind::ProxyNotFound,
                Some("Install AWS CLI v2 and make sure its executable is in PATH.".into()),
            )
        } else if lower.contains("sessionmanagerplugin is not found") {
            (
                ConnectErrorKind::SsmPluginMissing,
                Some("Install the AWS Session Manager plugin and add it to PATH.".into()),
            )
        } else if [
            "expiredtoken",
            "token has expired",
            "aws sso login",
            "unable to locate credentials",
        ]
        .iter()
        .any(|s| lower.contains(s))
        {
            let args = shell_words::split(command).unwrap_or_default();
            let profile = args
                .iter()
                .enumerate()
                .find_map(|(i, a)| {
                    a.strip_prefix("--profile=").map(String::from).or_else(|| {
                        if a == "--profile" {
                            args.get(i + 1).cloned()
                        } else {
                            None
                        }
                    })
                })
                .unwrap_or_else(|| "default".into());
            (
                ConnectErrorKind::AwsCredentials,
                Some(format!(
                    "Refresh AWS credentials: aws sso login --profile {profile}"
                )),
            )
        } else if lower.contains("targetnotconnected") {
            (
                ConnectErrorKind::InstanceUnreachable,
                Some("Check that the instance is running and its SSM agent is connected.".into()),
            )
        } else {
            (ConnectErrorKind::Transport, None)
        };
    ConnectError {
        kind,
        message: if stderr.is_empty() {
            error.into()
        } else {
            format!("{error}\nProxy stderr:\n{stderr}")
        },
        hint,
    }
}
