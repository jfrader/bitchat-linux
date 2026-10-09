use std::io::IsTerminal;
use std::time::Duration;

use anyhow::{Result, bail};
use clap::Parser;
use crossterm::event::{DisableBracketedPaste, EnableBracketedPaste, EventStream};
use futures::StreamExt;
use tokio::signal::unix::{SignalKind, signal};
use tokio::sync::{broadcast, mpsc};

use bitchat_linux::app::App;
use bitchat_linux::config::Config;
use bitchat_linux::internet::{self, Command, InternetConfig};
use bitchat_linux::types::{Action, CHANNEL_CAPACITY, Notice, Room, Update, send_failure_notice};
use bitchat_linux::ui;

const FRAME_INTERVAL: Duration = Duration::from_millis(100);
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(3);
const FALLBACK_NICKNAME: &str = "anon";

struct TerminalRestore;

impl Drop for TerminalRestore {
    fn drop(&mut self) {
        let _ = crossterm::execute!(std::io::stdout(), DisableBracketedPaste);
        ratatui::restore();
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let config = Config::parse();
    if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
        bail!("bitchat-linux requires an interactive terminal");
    }
    let paths = config.paths()?;
    paths.prepare()?;
    let mut app = App::new(
        config
            .nickname
            .clone()
            .unwrap_or_else(|| FALLBACK_NICKNAME.into()),
        config.geohash.clone(),
    );
    let mesh = if config.no_bluetooth {
        app.mesh_status = "disabled".into();
        None
    } else {
        match bitchatd::MeshService::start_at(paths.data.join("mesh"), paths.state.join("mesh")) {
            Ok(mesh) => {
                if let Some(nickname) = &config.nickname
                    && let Err(error) = mesh.set_nickname(nickname)
                {
                    app.apply(Update::Notice(error.into()));
                }
                Some(mesh)
            }
            Err(error) => {
                app.mesh_status = "unavailable".into();
                app.apply(Update::Notice(format!("Bluetooth: {error:#}").into()));
                None
            }
        }
    };
    let mut mesh_changes = mesh.as_ref().map(bitchatd::MeshService::changes);
    if let Some(mesh) = &mesh {
        app.apply_mesh(mesh.snapshot());
    }
    let (commands, command_rx) = mpsc::channel(CHANNEL_CAPACITY);
    let (updates, mut update_rx) = mpsc::channel(CHANNEL_CAPACITY);
    let network = tokio::spawn(internet::run(
        InternetConfig {
            data_dir: paths.data,
            relays: config.relay,
        },
        command_rx,
        updates,
    ));
    if let Some(geohash) = config.geohash {
        commands.try_send(Command::Join(geohash))?;
    }

    let outcome = run_ui(
        &mut app,
        mesh.as_ref(),
        &mut mesh_changes,
        &commands,
        &mut update_rx,
    )
    .await;
    let shutdown_notices = shutdown_network(network, &commands, &mut update_rx).await;
    for notice in shutdown_notices {
        eprintln!("{notice}");
    }
    if let Some(mesh) = mesh {
        mesh.shutdown().await;
    }
    outcome?;
    Ok(())
}

async fn shutdown_network(
    mut network: tokio::task::JoinHandle<Result<()>>,
    commands: &mpsc::Sender<Command>,
    updates: &mut mpsc::Receiver<Update>,
) -> Vec<String> {
    let mut notices = Vec::new();
    let mut updates_open = true;
    let mut requested = false;
    let request = commands.send(Command::Shutdown);
    tokio::pin!(request);
    let completed = tokio::time::timeout(SHUTDOWN_TIMEOUT, async {
        loop {
            tokio::select! {
                result = &mut network => break result,
                _ = &mut request, if !requested => requested = true,
                update = updates.recv(), if updates_open => match update {
                    Some(update) => collect_shutdown_notice(update, &mut notices),
                    None => updates_open = false,
                },
            }
        }
    })
    .await;
    match completed {
        Ok(Ok(Err(error))) => {
            notices.push(bitchatd::clean_message(&format!("Internet: {error:#}")))
        }
        Ok(Err(error)) => notices.push(bitchatd::clean_message(&format!("Internet task: {error}"))),
        Err(_) => {
            network.abort();
            let _ = network.await;
            notices.push("Internet shutdown timed out; pending messages are unconfirmed".into());
        }
        Ok(Ok(Ok(()))) => {}
    }
    while let Ok(update) = updates.try_recv() {
        collect_shutdown_notice(update, &mut notices);
    }
    notices
}

fn collect_shutdown_notice(update: Update, notices: &mut Vec<String>) {
    if let Update::SendFailed { room, text, reason } = update {
        notices.push(send_failure_notice(&room, &text, &reason));
    }
}

async fn run_ui(
    app: &mut App,
    mesh: Option<&bitchatd::MeshService>,
    mesh_changes: &mut Option<broadcast::Receiver<serde_json::Value>>,
    commands: &mpsc::Sender<Command>,
    updates: &mut mpsc::Receiver<Update>,
) -> Result<()> {
    let mut terminal = ratatui::try_init()?;
    let _restore = TerminalRestore;
    crossterm::execute!(std::io::stdout(), EnableBracketedPaste)?;
    let mut events = EventStream::new();
    let mut redraw = tokio::time::interval(FRAME_INTERVAL);
    let mut terminate = signal(SignalKind::terminate())?;
    let mut interrupt = signal(SignalKind::interrupt())?;
    let mut hangup = signal(SignalKind::hangup())?;
    let mut internet_open = true;
    loop {
        terminal.draw(|frame| ui::render(frame, app))?;
        tokio::select! {
            event = events.next() => match event {
                Some(Ok(event)) => {
                    if let Some(action) = app.handle_event(event)
                        && execute(action, app, mesh, commands)
                    {
                        break;
                    }
                }
                Some(Err(error)) => return Err(error.into()),
                None => break,
            },
            update = updates.recv(), if internet_open => match update {
                Some(update) => app.apply(update),
                None => {
                    internet_open = false;
                    app.apply(Update::InternetStatus { geohash: app.geohash.clone(), detail: "stopped".into(), connected: 0 });
                }
            },
            changed = mesh_changed(mesh_changes) => {
                match changed {
                    Ok(()) | Err(broadcast::error::RecvError::Lagged(_)) => {
                        if let Some(mesh) = mesh {
                            app.apply_mesh(mesh.snapshot());
                        }
                    }
                    Err(broadcast::error::RecvError::Closed) => {
                        *mesh_changes = None;
                        app.mesh_status = "stopped".into();
                    }
                }
            },
            _ = redraw.tick() => {},
            _ = terminate.recv() => break,
            _ = interrupt.recv() => break,
            _ = hangup.recv() => break,
        }
    }
    Ok(())
}

async fn mesh_changed(
    changes: &mut Option<broadcast::Receiver<serde_json::Value>>,
) -> Result<(), broadcast::error::RecvError> {
    match changes {
        Some(changes) => changes.recv().await.map(|_| ()),
        None => std::future::pending().await,
    }
}

fn execute(
    action: Action,
    app: &mut App,
    mesh: Option<&bitchatd::MeshService>,
    commands: &mpsc::Sender<Command>,
) -> bool {
    match action {
        Action::Quit => return true,
        Action::Send {
            room: Room::Mesh,
            text,
            ..
        } => {
            let result = mesh
                .ok_or_else(|| "Bluetooth unavailable".to_owned())
                .and_then(|mesh| mesh.send_text(&text));
            match result {
                Err(reason) => app.apply(Update::SendFailed {
                    room: Room::Mesh,
                    text,
                    reason,
                }),
                Ok(outcome) => {
                    if let Some(mesh) = mesh {
                        let snapshot = mesh.snapshot();
                        let no_peers = snapshot.radio.links == 0;
                        let persistent = snapshot.settings.persist_history;
                        app.apply_mesh(snapshot);
                        if let Some(error) = outcome.history_error {
                            app.apply(Update::Notice(Notice::in_room(
                                Room::Mesh,
                                format!("Mesh history error: {error}"),
                            )));
                        } else if no_peers {
                            app.apply(Update::Notice(Notice::in_room(
                                Room::Mesh,
                                if persistent {
                                    "Saved locally · no connected peers"
                                } else {
                                    "Kept in memory · no connected peers"
                                },
                            )));
                        }
                    }
                }
            }
        }
        Action::Send {
            room: Room::Internet(geohash),
            text,
            nickname,
        } => {
            if let Err(error) = commands.try_send(Command::Send {
                geohash: geohash.clone(),
                text: text.clone(),
                nickname,
            }) {
                app.apply(Update::SendFailed {
                    room: Room::Internet(geohash),
                    text,
                    reason: format!("Internet: {error}"),
                });
            }
        }
        Action::Join(geohash) => match commands.try_reserve() {
            Ok(permit) => {
                if app.join_channel(geohash.clone()) {
                    permit.send(Command::Join(geohash));
                }
            }
            Err(error) => app.apply(Update::Notice(format!("Internet: {error}").into())),
        },
        Action::Nickname(nickname) => {
            if let Some(nickname) = bitchatd::sanitize_nickname(&nickname) {
                let result = mesh.map_or(Ok(()), |mesh| mesh.set_nickname(&nickname));
                match result {
                    Ok(()) => app.nickname = nickname,
                    Err(error) => app.apply(Update::Notice(error.into())),
                }
            } else {
                app.apply(Update::Notice(
                    "Nickname must contain visible characters".into(),
                ));
            }
        }
        Action::Radio(mode) => match mesh {
            Some(mesh) => {
                if let Err(error) = mesh.set_mode(mode) {
                    app.apply(Update::Notice(error.into()));
                }
                app.apply_mesh(mesh.snapshot());
            }
            None => app.apply(Update::Notice("Bluetooth unavailable".into())),
        },
        Action::Clear(Room::Mesh) => {
            if let Some(mesh) = mesh
                && let Err(error) = mesh.clear_history()
            {
                app.apply(Update::Notice(error.into()));
            }
        }
        Action::Clear(Room::Internet(_)) => {}
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn routes_internet_send_to_the_requested_channel() {
        let mut app = App::new("test".into(), Some("dr5rs".into()));
        let (commands, mut receiver) = mpsc::channel(CHANNEL_CAPACITY);
        execute(
            Action::Send {
                room: Room::Internet("u4pru".into()),
                text: "hi".into(),
                nickname: "test".into(),
            },
            &mut app,
            None,
            &commands,
        );
        assert!(
            matches!(receiver.try_recv().unwrap(), Command::Send { geohash, .. } if geohash == "u4pru")
        );
    }

    #[test]
    fn unavailable_bluetooth_does_not_discard_the_message() {
        let mut app = App::new("test".into(), None);
        let (commands, _) = mpsc::channel(CHANNEL_CAPACITY);
        execute(
            Action::Send {
                room: Room::Mesh,
                text: "unsent".into(),
                nickname: "test".into(),
            },
            &mut app,
            None,
            &commands,
        );
        assert_eq!(app.input.value(), "unsent");
        assert!(app.notice_text().unwrap().contains("Bluetooth unavailable"));
    }

    #[test]
    fn a_full_command_queue_does_not_switch_the_selected_room() {
        let mut app = App::new("test".into(), None);
        let (commands, mut receiver) = mpsc::channel(1);
        commands.try_send(Command::Join("dr5rs".into())).unwrap();
        execute(Action::Join("u4pru".into()), &mut app, None, &commands);
        assert_eq!(app.room, Room::Mesh);
        assert_eq!(app.geohash, None);
        assert!(matches!(receiver.try_recv().unwrap(), Command::Join(hash) if hash == "dr5rs"));
    }

    #[tokio::test]
    async fn shutdown_keeps_last_unacknowledged_message_visible_after_restoring_terminal() {
        let (commands, mut receiver) = mpsc::channel(1);
        let (updates, mut update_rx) = mpsc::channel(1);
        updates
            .send(Update::Notice("queued before shutdown".into()))
            .await
            .unwrap();
        let network = tokio::spawn(async move {
            assert!(matches!(receiver.recv().await, Some(Command::Shutdown)));
            updates
                .send(Update::SendFailed {
                    room: Room::Internet("dr5rs".into()),
                    text: "unsent 🦀\u{1b}[31m\u{202e}".into(),
                    reason: "Shutting down before relay acknowledged the message".into(),
                })
                .await
                .unwrap();
            Ok(())
        });
        let notices = shutdown_network(network, &commands, &mut update_rx).await;
        assert_eq!(notices.len(), 1);
        assert!(notices[0].contains("#dr5rs") && notices[0].contains("unsent 🦀"));
        assert!(notices[0].contains("before relay acknowledged"));
        assert!(!notices[0].contains(['\u{1b}', '\u{202e}']));
    }
}
