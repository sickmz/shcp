use crate::{
    Event, NoticeLevel, TerminalHandle,
    session::{Context, OpenTerminal, Ssh},
    session_log::SessionLog,
    term::TerminalCommand,
};
use anyhow::{Result, bail};
use async_channel::Receiver;
use russh::ChannelMsg;
use std::{path::PathBuf, sync::Arc};
use tokio_util::sync::CancellationToken;

pub(crate) async fn run(
    ctx: Arc<Context>,
    ssh: Arc<Ssh>,
    command: OpenTerminal,
    cancel: CancellationToken,
) {
    let OpenTerminal {
        terminal,
        input,
        log_dir,
    } = command;
    let result = tokio::select! { _ = cancel.cancelled() => Ok(None), result = channel(&ctx, &ssh, &terminal, input, log_dir) => result };
    terminal.finish();
    let status = match result {
        Ok(status) => status,
        Err(error) => {
            ctx.notice(NoticeLevel::Error, format!("Terminal: {error:#}"));
            None
        }
    };
    ctx.emit(Event::TerminalExited {
        id: terminal.id(),
        status,
    });
}
async fn accepted(channel: &mut russh::Channel<russh::client::Msg>) -> Result<()> {
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
fn open_log(ctx: &Context, dir: Option<PathBuf>) -> Option<SessionLog> {
    match SessionLog::create(&dir?, &ctx.host.alias, terminal_stamp()) {
        Ok(log) => Some(log),
        Err(error) => {
            ctx.notice(
                NoticeLevel::Warning,
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
async fn channel(
    ctx: &Context,
    ssh: &Ssh,
    terminal: &TerminalHandle,
    input: Receiver<TerminalCommand>,
    log_dir: Option<PathBuf>,
) -> Result<Option<u32>> {
    if !terminal.is_alive() {
        return Ok(None);
    }
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
    // A separate writer lets incoming channel data keep draining under SSH flow control.
    let write_task = async {
        while let Ok(command) = input.recv().await {
            match command {
                TerminalCommand::Write(bytes) => writer.data_bytes(bytes).await?,
                TerminalCommand::Resize(cols, rows) => {
                    writer.window_change(cols as u32, rows as u32, 0, 0).await?
                }
                TerminalCommand::Close => break,
            }
        }
        let _ = writer.eof().await;
        let _ = writer.close().await;
        Ok::<(), russh::Error>(())
    };
    let mut log = open_log(ctx, log_dir);
    let read_task = async {
        let mut status = None;
        while let Some(message) = reader.wait().await {
            match message {
                ChannelMsg::Data { data } | ChannelMsg::ExtendedData { data, .. } => {
                    terminal.feed(&data);
                    if let Some(file) = &mut log
                        && let Err(error) = file.write(&data)
                    {
                        log = None;
                        ctx.notice(
                            NoticeLevel::Warning,
                            format!("The session log stopped: {error}"),
                        );
                    }
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
        result = write_task => { result?; Ok(None) },
        status = read_task => { tracing::debug!(session = ctx.id.0, "Terminal channel closed"); Ok(status) },
    }
}
