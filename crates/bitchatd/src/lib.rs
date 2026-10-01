mod ble;
mod ipc;
mod mesh;
mod node;
mod power;
mod store;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use serde::Serialize;
use tokio::sync::broadcast;
use tokio::task::JoinHandle;

const RADIO_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(2);

pub use mesh::{ChatMessage, Me, PeerView, clean_message, sanitize_nickname};
pub use node::{MAX_TEXT_BYTES, RadioState, RadioStatus};
pub use power::Effective;
pub use store::{Mode, Settings};

#[derive(Clone, Debug, Serialize)]
pub struct Snapshot {
    pub me: Me,
    pub peers: Vec<PeerView>,
    pub messages: Vec<ChatMessage>,
    pub settings: Settings,
    pub radio: RadioStatus,
    pub version: &'static str,
}

pub struct MeshService {
    node: Arc<node::Node>,
    tasks: Vec<JoinHandle<()>>,
}

impl MeshService {
    pub fn start_at(data_dir: PathBuf, state_dir: PathBuf) -> Result<Self> {
        Self::start_with(store::Store::at(data_dir, state_dir))
    }

    fn start_with(store: store::Store) -> Result<Self> {
        let (identity, nickname) = store.load_identity()?;
        let settings = store.load_settings();
        let history = if settings.persist_history {
            store.load_history()
        } else {
            Vec::new()
        };
        let mesh = mesh::Mesh::with_pins(identity, nickname, history, store.load_pins());
        let (node, outgoing) = node::Node::new(mesh, store, settings);
        let tasks = vec![
            tokio::spawn(node::ticker(node.clone())),
            tokio::spawn(ble::supervise(node.clone(), outgoing)),
        ];
        Ok(Self { node, tasks })
    }

    pub fn snapshot(&self) -> Snapshot {
        self.node.snapshot(mesh::LOG_MAX)
    }

    pub fn changes(&self) -> broadcast::Receiver<serde_json::Value> {
        self.node.events()
    }

    pub fn send_text(&self, text: &str) -> Result<(), String> {
        self.node.send_text(text)
    }

    pub fn set_nickname(&self, nickname: &str) -> Result<(), String> {
        self.node.set_nickname(nickname)
    }

    pub fn set_mode(&self, mode: Mode) -> Result<(), String> {
        self.node.set_mode(mode)
    }

    pub fn clear_history(&self) -> Result<(), String> {
        self.node.clear_history()
    }

    pub async fn shutdown(mut self) {
        self.node
            .send_raw(self.node.leave_packet(), mesh::Target::All);
        tokio::time::sleep(Duration::from_millis(400)).await;
        self.node.stop_radio();
        let _ = tokio::time::timeout(RADIO_SHUTDOWN_TIMEOUT, async {
            while self.node.radio_status().state != RadioState::Off {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await;
        for task in &self.tasks {
            task.abort();
        }
        for task in std::mem::take(&mut self.tasks) {
            let _ = task.await;
        }
    }
}

impl Drop for MeshService {
    fn drop(&mut self) {
        for task in &self.tasks {
            task.abort();
        }
    }
}

pub async fn run_daemon() -> Result<()> {
    use tokio::signal::unix::{SignalKind, signal};

    let mesh = MeshService::start_with(store::Store::from_env()?)?;
    let socket = ipc::socket_path()?;
    let bound = ipc::bind(&socket).await?;
    let (listener, owner) = bound.split();
    let server = tokio::spawn(ipc::serve(mesh.node.clone(), listener));
    let mut term = signal(SignalKind::terminate())?;
    let mut int = signal(SignalKind::interrupt())?;
    tokio::select! {
        _ = term.recv() => {}
        _ = int.recv() => {}
        result = server => tracing::error!("control socket stopped: {result:?}"),
    }
    mesh.shutdown().await;
    owner.remove();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn embedded_mesh_uses_the_same_signed_history_without_a_control_socket() {
        let temp = tempfile::tempdir().unwrap();
        let store = store::Store::at(temp.path().join("data"), temp.path().join("state"));
        store
            .save_settings(&Settings {
                mode: Mode::Off,
                persist_history: true,
            })
            .unwrap();
        let service = MeshService::start_with(store).unwrap();
        let mut changes = service.changes();
        service.set_nickname("linux").unwrap();
        service.send_text("hello 🦀").unwrap();
        let snapshot = service.snapshot();
        assert_eq!(snapshot.me.nickname, "linux");
        assert_eq!(snapshot.messages.len(), 1);
        assert_eq!(snapshot.messages[0].text, "hello 🦀");
        assert!(snapshot.messages[0].mine);
        assert!(changes.try_recv().is_ok());
        assert_eq!(snapshot.settings.mode, Mode::Off);
        service.clear_history().unwrap();
        assert!(service.snapshot().messages.is_empty());
        service.shutdown().await;
    }

    #[tokio::test]
    async fn corrupt_mesh_identity_is_not_replaced() {
        let temp = tempfile::tempdir().unwrap();
        let data = temp.path().join("data");
        std::fs::create_dir(&data).unwrap();
        let identity = data.join("identity.json");
        std::fs::write(&identity, "broken").unwrap();
        assert!(MeshService::start_at(data, temp.path().join("state")).is_err());
        assert_eq!(std::fs::read_to_string(identity).unwrap(), "broken");
    }

    #[tokio::test]
    async fn embedded_snapshot_keeps_full_history_while_ipc_stays_bounded() {
        let temp = tempfile::tempdir().unwrap();
        let store = store::Store::at(temp.path().join("data"), temp.path().join("state"));
        store
            .save_settings(&Settings {
                mode: Mode::Off,
                persist_history: false,
            })
            .unwrap();
        let service = MeshService::start_with(store).unwrap();
        for index in 0..mesh::LOG_MAX + 1 {
            service.send_text(&format!("message {index}")).unwrap();
        }
        assert_eq!(service.snapshot().messages.len(), mesh::LOG_MAX);
        assert_eq!(service.snapshot().messages[0].text, "message 1");
        assert_eq!(
            service
                .node
                .snapshot(node::SNAPSHOT_MESSAGES)
                .messages
                .len(),
            node::SNAPSHOT_MESSAGES
        );
        service.shutdown().await;
    }

    #[tokio::test]
    async fn shutdown_releases_background_owners_without_changing_persisted_radio_mode() {
        let temp = tempfile::tempdir().unwrap();
        let store = store::Store::at(temp.path().join("data"), temp.path().join("state"));
        store
            .save_settings(&Settings {
                mode: Mode::Off,
                persist_history: false,
            })
            .unwrap();
        let service = MeshService::start_with(store).unwrap();
        let node = Arc::downgrade(&service.node);
        service.shutdown().await;
        tokio::time::timeout(Duration::from_secs(1), async {
            while node.upgrade().is_some() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("the service must not retain detached tasks");
        let store = store::Store::at(temp.path().join("data"), temp.path().join("state"));
        assert_eq!(store.load_settings().mode, Mode::Off);
    }

    #[test]
    fn radio_stop_does_not_save_off_as_the_next_startup_preference() {
        let temp = tempfile::tempdir().unwrap();
        let store = store::Store::at(temp.path().join("data"), temp.path().join("state"));
        store.save_settings(&Settings::default()).unwrap();
        let mesh = mesh::Mesh::new(
            bitchat_proto::Identity::generate(),
            "test".into(),
            Vec::new(),
        );
        let (node, _) = node::Node::new(mesh, store, Settings::default());
        node.stop_radio();
        assert_eq!(*node.mode().borrow(), Mode::Off);
        assert_eq!(node.snapshot(0).settings.mode, Mode::Auto);
        let store = store::Store::at(temp.path().join("data"), temp.path().join("state"));
        assert_eq!(store.load_settings().mode, Mode::Auto);
    }
}
