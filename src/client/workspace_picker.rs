//! Initial workspace selection uses the same viewer interaction and ratatui presentation.
use super::view::Overlay;
use super::{ClientTerminal, WorkspaceTerminal, interaction::Interaction};
use crate::state::WorkspaceState;
use tokio::signal::unix::{SignalKind, signal};

pub async fn pick_workspace(names: Vec<String>) -> anyhow::Result<Option<(String, Vec<u8>)>> {
    anyhow::ensure!(
        names.len() <= crate::daemon::MAX_WORKSPACES,
        "too many workspaces"
    );
    for name in &names {
        crate::daemon::validate_workspace_name(name)?;
    }
    let mut interaction = Interaction::default();
    interaction.loading_workspaces();
    interaction.workspaces_loaded(Ok(names));
    if !interaction.active() {
        return Ok(None);
    }
    let mut interrupt = signal(SignalKind::interrupt())?;
    let mut terminate = signal(SignalKind::terminate())?;
    let mut hangup = signal(SignalKind::hangup())?;
    let (sender, receiver) = tokio::sync::watch::channel(interaction.panel());
    let mut terminal = WorkspaceTerminal::enter_default(false, None)?.with_hints(receiver);
    let (mut channels, tasks) = super::io::spawn_client_io()?;
    let state = WorkspaceState::default();
    let result = async {
        let mut escape_deadline = None;
        loop {
            sender.send_replace(interaction.panel());
            terminal.render(&state, &Overlay::empty(), None)?;
            tokio::select! {
                _ = interrupt.recv() => return Ok(None),
                _ = terminate.recv() => return Ok(None),
                _ = hangup.recv() => return Ok(None),
                chunk = channels.input_rx.recv() => {
                    let Some(chunk) = chunk else { return Ok(None); };
                    for (offset, byte) in chunk.iter().copied().enumerate() {
                        let _ = interaction.feed(byte, &state);
                        if let Some(name) = interaction.take_workspace() {
                            return Ok(Some((name, chunk.get(offset + 1..).unwrap_or_default().to_vec())));
                        }
                    }
                    escape_deadline = interaction.escape_pending().then(|| tokio::time::Instant::now() + std::time::Duration::from_millis(35));
                }
                _ = async {
                    if let Some(deadline) = escape_deadline { tokio::time::sleep_until(deadline).await; }
                    else { std::future::pending::<()>().await; }
                } => {
                    interaction.resolve_escape(); escape_deadline = None;
                    if interaction.take_back() { return Ok(None); }
                }
                Some(()) = channels.resize_rx.recv() => {},
            }
        }
    }.await;
    drop(terminal);
    tasks.shutdown().await?;
    result
}
