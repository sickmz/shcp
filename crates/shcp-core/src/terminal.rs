use crate::{
    Event, NoticeLevel, TerminalHandle,
    session::{Context, OpenTerminal, Ssh},
    session_log::SessionLog,
    term::TerminalCommand,
    tmux,
};
use anyhow::{Result, bail};
use russh::ChannelMsg;
use std::{path::PathBuf, sync::Arc};
use tokio_util::sync::CancellationToken;

pub(crate) async fn run(
    ctx: Arc<Context>,
    ssh: Arc<Ssh>,
    command: OpenTerminal,
    cancel: CancellationToken,
) {
    let result = channel(&ctx, &ssh, &command, &cancel).await;
    let terminal = &command.terminal;
    terminal.finish();
    let end = match result {
        Ok(end) => end,
        Err(error) => {
            ctx.notice(NoticeLevel::Error, format!("Terminal: {error:#}"));
            End::Exited(None)
        }
    };
    ctx.emit(match end {
        End::Exited(status) => Event::TerminalExited {
            id: terminal.id(),
            status,
        },
        End::Closed => Event::TerminalClosed { id: terminal.id() },
    });
}
enum End {
    Exited(Option<u32>),
    /// Closed from elsewhere, so it has no place here any more.
    Closed,
}
pub(crate) async fn accepted(channel: &mut russh::Channel<russh::client::Msg>) -> Result<()> {
    loop {
        match channel.wait().await {
            Some(ChannelMsg::Success) => return Ok(()),
            Some(ChannelMsg::Failure | ChannelMsg::Close) | None => {
                bail!("Server rejected the terminal request")
            }
            _ => {}
        }
    }
}
/// Opens the log of a terminal. Not being able to write it must not cost the terminal.
pub(crate) fn open_log(ctx: &Context, dir: Option<PathBuf>) -> Option<SessionLog> {
    match SessionLog::create(&dir?, &ctx.host.alias, terminal_stamp()) {
        Ok(log) => Some(log),
        Err(error) => {
            ctx.notice(
                NoticeLevel::Important,
                format!("Could not start the session log: {error}"),
            );
            None
        }
    }
}
fn terminal_stamp() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs())
}
/// Puts output on the terminal and in its log.
pub(crate) fn show(
    ctx: &Context,
    terminal: &TerminalHandle,
    log: &mut Option<SessionLog>,
    data: &[u8],
) {
    terminal.feed(data);
    if !data.is_empty() {
        terminal.reveal();
    }
    if let Some(file) = log.as_mut()
        && let Err(error) = file.write(data)
    {
        *log = None;
        ctx.notice(
            NoticeLevel::Important,
            format!("The session log stopped: {error}"),
        );
    }
}
async fn channel(
    ctx: &Context,
    ssh: &Ssh,
    command: &OpenTerminal,
    cancel: &CancellationToken,
) -> Result<End> {
    let OpenTerminal {
        terminal,
        input,
        log_dir,
        persist,
        startup,
        ..
    } = command;
    if !terminal.is_alive() {
        return Ok(End::Exited(None));
    }
    if let Some(persist) = persist {
        match tmux::run(ctx, ssh, command, persist, cancel).await? {
            // tmux has the shell, so what it ended with is not known here.
            tmux::Outcome::Ended => return Ok(End::Exited(None)),
            tmux::Outcome::Closed => return Ok(End::Closed),
            tmux::Outcome::NoTmux => ctx.emit(Event::TmuxMissing {
                host: ctx.host.id.clone(),
            }),
            tmux::Outcome::Unavailable => tmux::tell_unavailable(ctx),
        }
    }
    let plain = async {
        terminal.set_managed(false);
        let mut channel = ssh.channel_open_session().await?;
        let (cols, rows) = terminal.dimensions();
        channel
            .request_pty(true, "xterm-256color", cols as u32, rows as u32, 0, 0, &[])
            .await?;
        accepted(&mut channel).await?;
        if ctx.host.forward_agent {
            // A server that refuses it can still give a shell: no answer is waited for, like OpenSSH.
            channel.agent_forward(false).await?;
        }
        channel.request_shell(true).await?;
        accepted(&mut channel).await?;
        terminal.mark_shell_started();
        let (mut reader, writer) = channel.split();
        if let Some(startup) = startup {
            writer
                .data_bytes(format!("{startup}\r").into_bytes())
                .await?;
        }
        // A separate writer lets incoming channel data keep draining under SSH flow control.
        let write_task = async {
            while let Ok(command) = input.recv().await {
                match command {
                    TerminalCommand::Write(bytes) => writer.data_bytes(bytes).await?,
                    // Asked for while tmux had the terminal, which is gone now.
                    TerminalCommand::Paste(text) => {
                        writer
                            .data_bytes(text.replace("\r\n", "\r").replace('\n', "\r").into_bytes())
                            .await?
                    }
                    TerminalCommand::Resize(cols, rows) => {
                        writer.window_change(cols as u32, rows as u32, 0, 0).await?
                    }
                    TerminalCommand::Publish(_) => {}
                    TerminalCommand::Close => break,
                }
            }
            let _ = writer.eof().await;
            let _ = writer.close().await;
            Ok::<(), russh::Error>(())
        };
        let mut log = open_log(ctx, log_dir.clone());
        let read_task = async {
            let mut status = None;
            while let Some(message) = reader.wait().await {
                match message {
                    ChannelMsg::Data { data } | ChannelMsg::ExtendedData { data, .. } => {
                        show(ctx, terminal, &mut log, &data);
                    }
                    ChannelMsg::ExitStatus { exit_status } => status = Some(exit_status),
                    ChannelMsg::Close => break,
                    // EOF is half-close; wait for exit-status and close.
                    _ => {}
                }
            }
            status
        };
        tokio::select! {
            result = write_task => { result?; Ok(End::Exited(None)) },
            status = read_task => { tracing::debug!(session = ctx.id.0, "Terminal channel closed"); Ok(End::Exited(status)) },
        }
    };
    tokio::select! { biased;
        _ = cancel.cancelled() => Ok(End::Exited(None)),
        result = plain => result,
    }
}
