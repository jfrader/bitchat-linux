//! Public BitChat location channels (Nostr kind 20000). Private messages are not Nostr DMs.
//! Bundled relay directory: permissionlesstech/georelays, MIT license (2025).
use std::collections::{HashSet, VecDeque};
use std::fs::{self, OpenOptions};
use std::future::Future;
use std::io::{Read, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use futures::StreamExt;
use geoutils::Location;
use nostr_sdk::prelude::*;
use serde::Deserialize;
use tokio::sync::mpsc;

use crate::config::private_directory;
use crate::types::{self, Room, Update};

const KIND_CHAT: Kind = Kind::Custom(20_000);
const DIRECTORY: &str =
    "https://raw.githubusercontent.com/permissionlesstech/georelays/main/nostr_relays.csv";
const BUNDLED_DIRECTORY: &str = include_str!("../assets/nostr_relays.csv");
const TIMEOUT: Duration = Duration::from_secs(10);
const STATUS_INTERVAL: Duration = Duration::from_secs(5);
const CONNECT_POLL_INTERVAL: Duration = Duration::from_millis(50);
const STATUS_POLL_TIMEOUT: Duration = Duration::from_millis(200);
const MAX_DIRECTORY_BYTES: usize = 256 * 1024;
const RECENT_IDS: usize = types::HISTORY_LIMIT * 2;
const NEAREST_RELAYS: usize = 5;

pub struct InternetConfig {
    pub data_dir: PathBuf,
    pub relays: Vec<String>,
}

pub enum Command {
    Join(String),
    Send {
        geohash: String,
        text: String,
        nickname: String,
    },
    Shutdown,
}

#[derive(Deserialize)]
struct DirectoryRow {
    #[serde(rename = "Relay URL")]
    relay: String,
    #[serde(rename = "Latitude")]
    latitude: f64,
    #[serde(rename = "Longitude")]
    longitude: f64,
}

fn relay_url(raw: &str) -> Result<String> {
    let raw = raw.trim();
    let url = if raw.contains("://") {
        raw.to_owned()
    } else {
        format!("wss://{raw}")
    };
    if !(url.starts_with("ws://") || url.starts_with("wss://")) {
        bail!("unsupported relay scheme: {raw}");
    }
    let parsed = RelayUrl::parse(&url).context("invalid relay URL")?;
    Ok(parsed.to_string())
}

fn closest_relays(csv: &str, geohash: &str) -> Result<Vec<String>> {
    let (point, _, _) = geohash::decode(geohash).context("invalid geohash")?;
    let center = Location::new(point.y, point.x);
    let mut rows = Vec::new();
    let mut seen = HashSet::new();
    for row in csv::Reader::from_reader(csv.as_bytes()).deserialize::<DirectoryRow>() {
        let Ok(row) = row else { continue };
        if !row.latitude.is_finite()
            || !row.longitude.is_finite()
            || row.latitude.abs() > 90.0
            || row.longitude.abs() > 180.0
        {
            continue;
        }
        if let Ok(url) = relay_url(&row.relay)
            && seen.insert(url.clone())
        {
            rows.push((
                center
                    .haversine_distance_to(&Location::new(row.latitude, row.longitude))
                    .meters(),
                url,
            ));
        }
    }
    rows.sort_by(|a, b| a.0.total_cmp(&b.0));
    let selected: Vec<_> = rows
        .into_iter()
        .take(NEAREST_RELAYS)
        .map(|(_, url)| url)
        .collect();
    if selected.is_empty() {
        bail!("relay directory contains no usable relays")
    }
    Ok(selected)
}

async fn discover(geohash: &str) -> Result<(Vec<String>, bool)> {
    let http = reqwest::Client::builder().timeout(TIMEOUT).build()?;
    let fetched = async {
        let mut response = http.get(DIRECTORY).send().await?.error_for_status()?;
        if response
            .content_length()
            .is_some_and(|n| n > MAX_DIRECTORY_BYTES as u64)
        {
            bail!("relay directory too large")
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await? {
            if bytes.len() + chunk.len() > MAX_DIRECTORY_BYTES {
                bail!("relay directory too large")
            }
            bytes.extend_from_slice(&chunk);
        }
        closest_relays(std::str::from_utf8(&bytes)?, geohash)
    };
    match tokio::time::timeout(TIMEOUT, fetched).await {
        Ok(Ok(relays)) => Ok((relays, false)),
        _ => Ok((closest_relays(BUNDLED_DIRECTORY, geohash)?, true)),
    }
}

fn key_for(dir: &Path, geohash: &str) -> Result<Keys> {
    // XDG data directories can be absent on the first run. Refuse leaf links
    // and shared directories rather than storing private keys through them.
    private_directory(dir)?;
    let root = dir.join("nostr");
    private_directory(&root)?;
    let owner = unsafe { libc::geteuid() };
    let path = root.join(format!("{geohash}.key"));
    let read = || -> Result<Keys> {
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(&path)?;
        let meta = file.metadata()?;
        if !meta.is_file() || meta.uid() != owner || meta.mode() & 0o077 != 0 {
            bail!("nostr key must be a private regular file owned by this user")
        }
        let mut secret = String::new();
        file.take(129).read_to_string(&mut secret)?;
        if secret.len() != 65 || !secret.ends_with('\n') {
            bail!("corrupt nostr key")
        }
        Keys::parse(secret.trim_end()).context("corrupt nostr key")
    };
    match read() {
        Ok(keys) => Ok(keys),
        Err(e)
            if e.downcast_ref::<std::io::Error>()
                .is_some_and(|io| io.kind() == std::io::ErrorKind::NotFound) =>
        {
            let keys = Keys::generate();
            let mut temp = tempfile::NamedTempFile::new_in(&root)?;
            temp.write_all(format!("{}\n", keys.secret_key().to_secret_hex()).as_bytes())?;
            temp.as_file().sync_all()?;
            match temp.persist_noclobber(&path) {
                Ok(_) => {
                    fs::File::open(&root)?.sync_all()?;
                    Ok(keys)
                }
                Err(e) if e.error.kind() == std::io::ErrorKind::AlreadyExists => read(),
                Err(e) => Err(e.error.into()),
            }
        }
        Err(e) => Err(e).with_context(|| format!("cannot load identity for #{geohash}")),
    }
}

fn chat_filter(geohash: &str) -> Filter {
    Filter::new()
        .kind(KIND_CHAT)
        .custom_tag(SingleLetterTag::LOWERCASE_G, geohash)
        .limit(types::HISTORY_LIMIT)
}

fn signed_message(keys: &Keys, geohash: &str, nickname: &str, text: &str) -> Result<Event> {
    let mut builder = EventBuilder::new(KIND_CHAT, text).tag(Tag::parse(["g", geohash])?);
    if let Some(nick) = bitchatd::sanitize_nickname(nickname) {
        builder = builder.tag(Tag::parse(["n", nick.as_str()])?);
    }
    Ok(builder.finalize(keys)?)
}

fn unpack(event: &Event, geohash: &str, mine: bool) -> Option<types::Message> {
    if event.kind != KIND_CHAT
        || event.verify().is_err()
        || !event.tags.iter().any(|tag| {
            let slice = tag.as_slice();
            slice.len() == 2 && slice[0] == "g" && slice[1] == geohash
        })
    {
        return None;
    }
    let nickname = event
        .tags
        .iter()
        .filter_map(|tag| {
            let slice = tag.as_slice();
            (slice.len() == 2 && slice[0] == "n")
                .then(|| bitchatd::sanitize_nickname(&slice[1]))
                .flatten()
        })
        .next()
        .unwrap_or_else(|| "anon".to_string());
    Some(types::Message {
        id: event.id.to_hex(),
        room: Room::Internet(geohash.to_owned()),
        author: event.pubkey.to_hex(),
        nickname,
        text: bitchatd::clean_message(&event.content),
        timestamp_ms: event.created_at.as_secs().saturating_mul(1000),
        mine,
    })
}

async fn status(client: &Client, geohash: &str, updates: &mpsc::Sender<Update>) -> Result<()> {
    let relays = match tokio::time::timeout(STATUS_POLL_TIMEOUT, client.relays()).await {
        Ok(relays) => relays,
        Err(_) => {
            updates
                .send(Update::InternetStatus {
                    geohash: Some(geohash.into()),
                    detail: format!("#{geohash}: relay status unavailable"),
                    connected: 0,
                })
                .await?;
            return Ok(());
        }
    };
    let connected = relays
        .values()
        .filter(|relay| relay.status() == RelayStatus::Connected)
        .count();
    updates
        .send(Update::InternetStatus {
            geohash: Some(geohash.into()),
            detail: format!("#{geohash}: {connected}/{} relays connected", relays.len()),
            connected,
        })
        .await?;
    Ok(())
}

type Notifications = Pin<Box<dyn futures::Stream<Item = ClientNotification> + Send>>;
type Setup = Pin<Box<dyn Future<Output = Result<Session>> + Send>>;
type Publish = Pin<Box<dyn Future<Output = Result<(Event, Vec<String>)>> + Send>>;

struct Session {
    client: Client,
    keys: Keys,
    notifications: Notifications,
}

struct PendingPublish {
    id: EventId,
    geohash: String,
    text: String,
    future: Publish,
}

async fn connect(
    data_dir: PathBuf,
    relays: Vec<String>,
    geohash: String,
    updates: mpsc::Sender<Update>,
) -> Result<Session> {
    let keys = key_for(&data_dir, &geohash)?;
    let relay_urls = if relays.is_empty() {
        let (urls, fallback) = discover(&geohash).await?;
        if fallback {
            updates
                .send(Update::Notice(
                    "Using bundled relay directory (live directory unavailable)".into(),
                ))
                .await?;
        }
        urls
    } else {
        relays
            .iter()
            .map(|url| relay_url(url))
            .collect::<Result<Vec<_>>>()?
    };
    updates
        .send(Update::InternetStatus {
            geohash: Some(geohash.clone()),
            detail: format!("#{geohash}: connecting to {} relays", relay_urls.len()),
            connected: 0,
        })
        .await?;
    let client = Client::builder().connect_timeout(TIMEOUT).build();
    for url in relay_urls {
        match tokio::time::timeout(TIMEOUT, client.add_relay(&url)).await {
            Ok(Ok(_)) => {}
            other => {
                updates
                    .send(Update::Notice(bitchatd::clean_message(&format!(
                        "{url}: {other:?}"
                    ))))
                    .await?
            }
        }
    }
    // The notification receiver must exist before REQ, or cached events can be missed.
    let notifications = client.notifications();
    client.connect().await;
    tokio::time::timeout(TIMEOUT, async {
        loop {
            if client
                .relays()
                .await
                .values()
                .any(|relay| relay.status() == RelayStatus::Connected)
            {
                break;
            }
            tokio::time::sleep(CONNECT_POLL_INTERVAL).await;
        }
    })
    .await
    .context("no relay connected")?;
    match tokio::time::timeout(TIMEOUT, client.subscribe(chat_filter(&geohash))).await {
        Ok(Ok(result)) => {
            for (relay, err) in result.failed {
                updates
                    .send(Update::Notice(bitchatd::clean_message(&format!(
                        "{relay}: {err}"
                    ))))
                    .await?;
            }
        }
        other => {
            updates
                .send(Update::Notice(bitchatd::clean_message(&format!(
                    "Subscription failed: {other:?}"
                ))))
                .await?
        }
    }
    status(&client, &geohash, &updates).await?;
    Ok(Session {
        client,
        keys,
        notifications,
    })
}

async fn publish(client: Client, event: Event) -> Result<(Event, Vec<String>)> {
    let result = tokio::time::timeout(TIMEOUT, client.send_event(&event)).await??;
    if result.success.is_empty() {
        bail!(
            "No relay accepted the message: {}",
            bitchatd::clean_message(&format!("{:?}", result.failed))
        )
    }
    let failures = result
        .failed
        .into_iter()
        .map(|(relay, err)| bitchatd::clean_message(&format!("{relay}: {err}")))
        .collect();
    Ok((event, failures))
}

async fn next_notification(session: &mut Option<Session>) -> Option<ClientNotification> {
    match session {
        Some(session) => session.notifications.next().await,
        None => std::future::pending().await,
    }
}

async fn next_setup(setup: &mut Option<Setup>) -> Result<Session> {
    match setup {
        Some(setup) => setup.await,
        None => std::future::pending().await,
    }
}

async fn next_publish(pending: &mut Option<PendingPublish>) -> Result<(Event, Vec<String>)> {
    match pending {
        Some(pending) => pending.future.as_mut().await,
        None => std::future::pending().await,
    }
}

async fn cancel_publish(
    pending: &mut Option<PendingPublish>,
    seen: &mut HashSet<EventId>,
    order: &mut VecDeque<EventId>,
    updates: &mpsc::Sender<Update>,
    reason: &'static str,
) -> Result<()> {
    if let Some(PendingPublish {
        id,
        geohash,
        text,
        future,
    }) = pending.take()
    {
        drop(future);
        remember(seen, order, id);
        updates
            .send(Update::SendFailed {
                room: Room::Internet(geohash),
                text,
                reason: reason.into(),
            })
            .await?;
    }
    Ok(())
}

pub async fn run(
    config: InternetConfig,
    mut commands: mpsc::Receiver<Command>,
    updates: mpsc::Sender<Update>,
) -> Result<()> {
    let mut session: Option<Session> = None;
    let mut setup: Option<Setup> = None;
    let mut pending: Option<PendingPublish> = None;
    let mut room = String::new();
    let mut seen = HashSet::new();
    let mut order = VecDeque::new();
    let mut tick = tokio::time::interval(STATUS_INTERVAL);
    updates
        .send(Update::InternetStatus {
            geohash: None,
            detail: "Not joined".into(),
            connected: 0,
        })
        .await?;
    loop {
        tokio::select! {
            biased;
            command = commands.recv() => match command {
                None => break,
                Some(Command::Shutdown) => {
                    cancel_publish(&mut pending, &mut seen, &mut order, &updates, "Shutting down before relay acknowledged the message").await?;
                    break
                }
                Some(Command::Join(raw)) => {
                    let geohash = match types::parse_geohash(&raw) {
                        Ok(code) => code,
                        Err(e) => { updates.send(Update::Notice(e.to_string())).await?; continue }
                    };
                    drop(setup.take());
                    cancel_publish(&mut pending, &mut seen, &mut order, &updates, "Channel changed before relay acknowledged the message").await?;
                    session = None; // SDK shuts down relays when its last Client drops.
                    if room != geohash { seen.clear(); order.clear(); }
                    room = geohash.clone();
                    updates.send(Update::InternetStatus { geohash: Some(geohash.clone()), detail: format!("#{geohash}: finding relays"), connected: 0 }).await?;
                    setup = Some(Box::pin(connect(config.data_dir.clone(), config.relays.clone(), geohash, updates.clone())));
                }
                Some(Command::Send { geohash, text, nickname }) => {
                    let target = Room::Internet(geohash.clone());
                    let body = bitchatd::clean_message(&text);
                    let prepared = match (&session, &pending) {
                        _ if geohash != room => Err(anyhow!("Join #{geohash} before sending")),
                        (_, Some(_)) => Err(anyhow!("Wait for the previous message to finish")),
                        (None, _) => Err(anyhow!("#{geohash}: not connected")),
                        (Some(_), _) if body.trim().is_empty() || text.len() > types::MAX_TEXT_BYTES => Err(anyhow!("Message is empty or too long")),
                        (Some(active), _) => signed_message(&active.keys, &geohash, &nickname, &body)
                            .map(|event| (active.client.clone(), event)),
                    };
                    match prepared {
                        Ok((client, event)) => pending = Some(PendingPublish {
                            id: event.id, geohash, text: body, future: Box::pin(publish(client, event)),
                        }),
                        Err(e) => updates.send(Update::SendFailed { room: target, text: body, reason: bitchatd::clean_message(&e.to_string()) }).await?,
                    }
                }
            },
            result = next_setup(&mut setup) => {
                setup = None;
                match result {
                    Ok(connected) => session = Some(connected),
                    Err(e) => {
                        updates.send(Update::Notice(bitchatd::clean_message(&format!("{e:#}")))).await?;
                        updates.send(Update::InternetStatus { geohash: Some(room.clone()), detail: format!("#{room}: disconnected"), connected: 0 }).await?;
                    }
                }
            }
            result = next_publish(&mut pending) => {
                if let Some(publish) = pending.take() {
                    remember(&mut seen, &mut order, publish.id);
                    match result {
                        Ok((event, failures)) => {
                            if !failures.is_empty() {
                                updates.send(Update::Notice(format!("Some relays rejected the message: {}", failures.join("; ")))).await?;
                            }
                            if let Some(message) = unpack(&event, &publish.geohash, true) { updates.send(Update::Message(message)).await? }
                        }
                        Err(e) => updates.send(Update::SendFailed {
                            room: Room::Internet(publish.geohash), text: publish.text,
                            reason: bitchatd::clean_message(&e.to_string()),
                        }).await?,
                    }
                }
            }
            notification = next_notification(&mut session) => {
                if notification.is_none() {
                    cancel_publish(&mut pending, &mut seen, &mut order, &updates, "Connection closed before relay acknowledged the message").await?;
                    session = None;
                    updates.send(Update::InternetStatus { geohash: Some(room.clone()), detail: format!("#{room}: notification stream closed; join to retry"), connected: 0 }).await?;
                }
                if let Some(ClientNotification::Event { event, .. }) = notification
                    && !seen.contains(&event.id)
                    && pending.as_ref().is_none_or(|publish| publish.id != event.id)
                    && let Some(message) = unpack(&event, &room, session.as_ref().is_some_and(|active| active.keys.public_key() == event.pubkey))
                {
                    remember(&mut seen, &mut order, event.id);
                    updates.send(Update::Message(message)).await?;
                }
            }
            _ = tick.tick(), if session.is_some() => {
                if let Some(active) = session.as_ref() { status(&active.client, &room, &updates).await?; }
            }
        }
    }
    // Dropping futures first releases their Client clones; the final SDK client
    // drop shuts down relay connections even if the websocket handshake stalled.
    drop(pending);
    drop(setup);
    drop(session);
    Ok(())
}

fn remember(seen: &mut HashSet<EventId>, order: &mut VecDeque<EventId>, id: EventId) {
    if seen.insert(id) {
        order.push_back(id);
        if order.len() > RECENT_IDS
            && let Some(old) = order.pop_front()
        {
            seen.remove(&old);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::future::Future;
    use std::net::SocketAddr;
    use std::os::unix::fs::{PermissionsExt, symlink};
    use std::sync::Arc;
    use tokio::io::AsyncReadExt;

    #[derive(Debug)]
    struct RejectChat;

    #[derive(Debug)]
    struct SlowChat(Option<mpsc::Sender<()>>);

    impl WritePolicy for SlowChat {
        fn admit_event<'a>(
            &'a self,
            event: &'a Event,
            _addr: &'a SocketAddr,
        ) -> Pin<Box<dyn Future<Output = WritePolicyResult> + Send + 'a>> {
            Box::pin(async move {
                if event.kind == KIND_CHAT
                    && let Some(notify) = &self.0
                {
                    let _ = notify.try_send(());
                }
                tokio::time::sleep(TIMEOUT + Duration::from_secs(2)).await;
                WritePolicyResult::Accept
            })
        }
    }

    impl WritePolicy for RejectChat {
        fn admit_event<'a>(
            &'a self,
            event: &'a Event,
            _addr: &'a SocketAddr,
        ) -> Pin<Box<dyn Future<Output = WritePolicyResult> + Send + 'a>> {
            Box::pin(async move {
                if event.kind == KIND_CHAT {
                    WritePolicyResult::reject(MachineReadablePrefix::Blocked, "chat not accepted")
                } else {
                    WritePolicyResult::Accept
                }
            })
        }
    }

    fn free_port() -> u16 {
        let socket = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        socket.local_addr().unwrap().port()
    }

    async fn connected(updates: &mut mpsc::Receiver<Update>, room: &str) {
        tokio::time::timeout(Duration::from_secs(2), async {
            while let Some(update) = updates.recv().await {
                if let Update::InternetStatus {
                    detail,
                    connected: 1,
                    ..
                } = update
                    && detail.starts_with(&format!("#{room}:"))
                {
                    return;
                }
            }
            panic!("internet actor exited without connecting to #{room}");
        })
        .await
        .unwrap();
    }

    async fn slow_handshake() -> (
        String,
        tokio::sync::oneshot::Receiver<()>,
        tokio::task::JoinHandle<()>,
    ) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://{}", listener.local_addr().unwrap());
        let (tx, rx) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let _ = tx.send(());
            let mut request = [0; 1024];
            loop {
                match stream.read(&mut request).await {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {} // Never complete the websocket handshake.
                }
            }
        });
        (url, rx, task)
    }

    #[tokio::test]
    async fn slow_handshake_does_not_block_switching_rooms_or_sending() {
        let (slow_url, accepted, slow_task) = slow_handshake().await;
        let relay = LocalRelay::builder().port(free_port()).build();
        relay.run().await.unwrap();
        let live_url = relay.url().await.to_string();
        let temp = tempfile::tempdir().unwrap();
        let (tx, rx) = mpsc::channel(16);
        let (updates_tx, mut updates) = mpsc::channel(32);
        let actor = tokio::spawn(run(
            InternetConfig {
                data_dir: temp.path().join("new/data"),
                relays: vec![slow_url, live_url.clone()],
            },
            rx,
            updates_tx,
        ));
        tx.send(Command::Join("dr5rs".into())).await.unwrap();
        tokio::time::timeout(Duration::from_secs(2), accepted)
            .await
            .unwrap()
            .unwrap();
        tx.send(Command::Send {
            geohash: "dr5rs".into(),
            text: "during setup".into(),
            nickname: "me".into(),
        })
        .await
        .unwrap();
        tokio::time::timeout(Duration::from_secs(1), async {
            while let Some(update) = updates.recv().await {
                if let Update::SendFailed { room, text, .. } = update {
                    assert_eq!(room, Room::Internet("dr5rs".into()));
                    assert_eq!(text, "during setup");
                    break;
                }
            }
        })
        .await
        .unwrap();
        tx.send(Command::Join("dr5rt".into())).await.unwrap();
        connected(&mut updates, "dr5rt").await;
        let sender = Client::new();
        sender.add_relay(live_url).await.unwrap();
        sender.connect().await;
        sender
            .send_event(&signed_message(&Keys::generate(), "dr5rt", "other", "arrived").unwrap())
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(2), async {
            while let Some(update) = updates.recv().await {
                if let Update::Message(message) = update {
                    assert_eq!(message.text, "arrived");
                    break;
                }
            }
        })
        .await
        .unwrap();
        tokio::time::timeout(Duration::from_secs(1), slow_task)
            .await
            .unwrap()
            .unwrap();
        tx.send(Command::Shutdown).await.unwrap();
        tokio::time::timeout(Duration::from_secs(1), actor)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
    }

    #[tokio::test]
    async fn shutdown_during_slow_handshake_closes_connection_promptly() {
        let (url, accepted, slow_task) = slow_handshake().await;
        let temp = tempfile::tempdir().unwrap();
        let (tx, rx) = mpsc::channel(16);
        let (updates_tx, _updates) = mpsc::channel(16);
        let actor = tokio::spawn(run(
            InternetConfig {
                data_dir: temp.path().join("new/data"),
                relays: vec![url],
            },
            rx,
            updates_tx,
        ));
        tx.send(Command::Join("dr5rs".into())).await.unwrap();
        tokio::time::timeout(Duration::from_secs(2), accepted)
            .await
            .unwrap()
            .unwrap();
        tx.send(Command::Shutdown).await.unwrap();
        tokio::time::timeout(Duration::from_secs(1), actor)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        tokio::time::timeout(Duration::from_secs(1), slow_task)
            .await
            .unwrap()
            .unwrap();
    }

    #[tokio::test]
    async fn same_channel_join_retries_after_connection() {
        let relay = LocalRelay::builder().port(free_port()).build();
        relay.run().await.unwrap();
        let temp = tempfile::tempdir().unwrap();
        let (tx, rx) = mpsc::channel(16);
        let (updates_tx, mut updates) = mpsc::channel(32);
        let actor = tokio::spawn(run(
            InternetConfig {
                data_dir: temp.path().join("new/data"),
                relays: vec![relay.url().await.to_string()],
            },
            rx,
            updates_tx,
        ));
        tx.send(Command::Join("dr5rs".into())).await.unwrap();
        connected(&mut updates, "dr5rs").await;
        tx.send(Command::Join("dr5rs".into())).await.unwrap();
        tokio::time::timeout(Duration::from_secs(2), async {
            while let Some(update) = updates.recv().await {
                if matches!(update, Update::InternetStatus { detail, connected: 0, .. } if detail == "#dr5rs: finding relays") { break }
            }
        }).await.unwrap();
        connected(&mut updates, "dr5rs").await;
        tx.send(Command::Shutdown).await.unwrap();
        tokio::time::timeout(Duration::from_secs(1), actor)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
    }

    #[tokio::test]
    async fn room_switch_cancels_unacknowledged_publish_to_old_room() {
        let (notify, mut submitted) = mpsc::channel(2);
        let relay = LocalRelay::builder()
            .port(free_port())
            .write_policy(SlowChat(Some(notify)))
            .build();
        relay.run().await.unwrap();
        let temp = tempfile::tempdir().unwrap();
        let (tx, rx) = mpsc::channel(16);
        let (updates_tx, mut updates) = mpsc::channel(32);
        let actor = tokio::spawn(run(
            InternetConfig {
                data_dir: temp.path().join("new/data"),
                relays: vec![relay.url().await.to_string()],
            },
            rx,
            updates_tx,
        ));
        tx.send(Command::Join("dr5rs".into())).await.unwrap();
        connected(&mut updates, "dr5rs").await;
        tx.send(Command::Send {
            geohash: "dr5rs".into(),
            text: "still pending".into(),
            nickname: "me".into(),
        })
        .await
        .unwrap();
        tokio::time::timeout(Duration::from_secs(2), submitted.recv())
            .await
            .unwrap()
            .unwrap();
        tx.send(Command::Send {
            geohash: "dr5rs".into(),
            text: "second".into(),
            nickname: "me".into(),
        })
        .await
        .unwrap();
        tx.send(Command::Join("dr5rt".into())).await.unwrap();
        tokio::time::timeout(Duration::from_secs(2), async {
            let mut failed = Vec::new();
            while let Some(update) = updates.recv().await {
                match update {
                    Update::SendFailed { room, text, .. } => {
                        assert_eq!(room, Room::Internet("dr5rs".into()));
                        failed.push(text);
                    }
                    Update::Message(message) if message.mine => {
                        panic!("unacknowledged echo: {message:?}")
                    }
                    Update::InternetStatus {
                        detail,
                        connected: 1,
                        ..
                    } if detail.starts_with("#dr5rt:") => break,
                    _ => {}
                }
            }
            assert!(failed.contains(&"still pending".into()));
            assert!(failed.contains(&"second".into()));
        })
        .await
        .unwrap();
        tx.send(Command::Shutdown).await.unwrap();
        tokio::time::timeout(Duration::from_secs(1), actor)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
    }

    #[tokio::test]
    async fn shutdown_cancels_unacknowledged_publish_promptly() {
        let (notify, mut submitted) = mpsc::channel(1);
        let relay = LocalRelay::builder()
            .port(free_port())
            .write_policy(SlowChat(Some(notify)))
            .build();
        relay.run().await.unwrap();
        let temp = tempfile::tempdir().unwrap();
        let (tx, rx) = mpsc::channel(16);
        let (updates_tx, mut updates) = mpsc::channel(16);
        let actor = tokio::spawn(run(
            InternetConfig {
                data_dir: temp.path().join("new/data"),
                relays: vec![relay.url().await.to_string()],
            },
            rx,
            updates_tx,
        ));
        tx.send(Command::Join("dr5rs".into())).await.unwrap();
        connected(&mut updates, "dr5rs").await;
        tx.send(Command::Send {
            geohash: "dr5rs".into(),
            text: "pending".into(),
            nickname: "me".into(),
        })
        .await
        .unwrap();
        tokio::time::timeout(Duration::from_secs(2), submitted.recv())
            .await
            .unwrap()
            .unwrap();
        tx.send(Command::Shutdown).await.unwrap();
        tokio::time::timeout(Duration::from_secs(1), actor)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        let mut failed = false;
        while let Ok(update) = updates.try_recv() {
            assert!(!matches!(update, Update::Message(ref message) if message.mine));
            if let Update::SendFailed { room, text, .. } = update {
                assert_eq!(room, Room::Internet("dr5rs".into()));
                assert_eq!(text, "pending");
                failed = true;
            }
        }
        assert!(failed, "shutdown must report the canceled publish");
    }

    #[test]
    fn signed_public_channel_events_are_verified_and_scoped() {
        let keys = Keys::generate();
        let event = signed_message(&keys, "dr5rs", "alice", "hi\n🌍").unwrap();
        assert_eq!(event.kind, KIND_CHAT);
        assert!(unpack(&event, "dr5rs", false).is_some());
        assert!(unpack(&event, "dr5rt", false).is_none());
        let mut corrupted = event.clone();
        corrupted.content = "forged".into();
        assert!(unpack(&corrupted, "dr5rs", false).is_none());
        let note = EventBuilder::new(Kind::TextNote, "not chat")
            .tag(Tag::parse(["g", "dr5rs"]).unwrap())
            .finalize(&keys)
            .unwrap();
        assert!(unpack(&note, "dr5rs", false).is_none());
        assert!(chat_filter("dr5rs").match_event(&event, MatchEventOptions::default()));
        assert!(!chat_filter("dr5rt").match_event(&event, MatchEventOptions::default()));
        let malicious = signed_message(
            &keys,
            "dr5rs",
            "bad\u{1b}[31m",
            "line\n\u{1b}[31mمی\u{200d}🌍",
        )
        .unwrap();
        let clean = unpack(&malicious, "dr5rs", false).unwrap();
        assert_eq!(clean.nickname, "bad[31m");
        assert_eq!(clean.text, "line\n[31mمی\u{200d}🌍");
        let missing_tag = EventBuilder::new(KIND_CHAT, "not location chat")
            .finalize(&keys)
            .unwrap();
        assert!(unpack(&missing_tag, "dr5rs", false).is_none());
        let presence = EventBuilder::new(Kind::Custom(20_001), "presence")
            .tag(Tag::parse(["g", "dr5rs"]).unwrap())
            .finalize(&keys)
            .unwrap();
        assert!(unpack(&presence, "dr5rs", false).is_none());
        let mut seen = HashSet::new();
        let mut order = VecDeque::new();
        for _ in 0..RECENT_IDS + 1 {
            remember(
                &mut seen,
                &mut order,
                EventBuilder::new(KIND_CHAT, "repeat")
                    .finalize(&Keys::generate())
                    .unwrap()
                    .id,
            );
        }
        assert_eq!(seen.len(), RECENT_IDS);
        assert_eq!(order.len(), RECENT_IDS);
    }

    #[test]
    fn relay_selection_and_keys() {
        let csv =
            "Relay URL,Latitude,Longitude\nnear.example,40.7,-74\nfar.example,0,0\ninvalid,999,0\n";
        assert_eq!(
            closest_relays(csv, "dr5rs").unwrap()[0],
            "wss://near.example"
        );
        let temp = tempfile::tempdir().unwrap();
        fs::set_permissions(temp.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let first = key_for(temp.path(), "dr5rs").unwrap();
        assert_eq!(
            first.public_key(),
            key_for(temp.path(), "dr5rs").unwrap().public_key()
        );
        assert_ne!(
            first.public_key(),
            key_for(temp.path(), "dr5rt").unwrap().public_key()
        );
        fs::write(temp.path().join("nostr/dr5rs.key"), "bad").unwrap();
        assert!(key_for(temp.path(), "dr5rs").is_err());
        assert_eq!(
            fs::read_to_string(temp.path().join("nostr/dr5rs.key")).unwrap(),
            "bad"
        );
        let nested = temp.path().join("missing/data");
        key_for(&nested, "dr5rs").unwrap();
        assert_eq!(
            fs::metadata(&nested).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert_eq!(
            fs::metadata(nested.join("nostr/dr5rs.key"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        let open = temp.path().join("open");
        fs::create_dir(&open).unwrap();
        fs::set_permissions(&open, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(key_for(&open, "dr5rs").is_err());
        let link = temp.path().join("link");
        symlink(&nested, &link).unwrap();
        assert!(key_for(&link, "dr5rs").is_err());
        let linked_key = nested.join("nostr/dr5rt.key");
        symlink(nested.join("nostr/dr5rs.key"), &linked_key).unwrap();
        assert!(key_for(&nested, "dr5rt").is_err());
        let concurrent = Arc::new(tempfile::tempdir().unwrap());
        fs::set_permissions(concurrent.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let handles: Vec<_> = (0..8)
            .map(|_| {
                let concurrent = concurrent.clone();
                std::thread::spawn(move || {
                    key_for(concurrent.path(), "dr5rs").unwrap().public_key()
                })
            })
            .collect();
        let keys: Vec<_> = handles
            .into_iter()
            .map(|handle| handle.join().unwrap())
            .collect();
        assert!(keys.iter().all(|key| *key == keys[0]));
    }

    #[test]
    fn a_fifo_at_the_key_path_is_rejected_without_waiting_for_a_writer() {
        let temp = tempfile::tempdir().unwrap();
        let data = temp.path().join("data");
        key_for(&data, "dr5rs").unwrap();
        let path = data.join("nostr/dr5rt.key");
        let name = std::ffi::CString::new(path.as_os_str().as_encoded_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
        assert!(
            key_for(&data, "dr5rt")
                .unwrap_err()
                .to_string()
                .contains("cannot load identity")
        );
    }

    #[tokio::test]
    async fn publish_and_receive_over_local_relay() {
        let relay = LocalRelay::builder().port(free_port()).build();
        relay.run().await.unwrap();
        let url = relay.url().await.to_string();
        let temp = tempfile::tempdir().unwrap();
        let data_dir = temp.path().join("new/data");
        let (tx, rx) = mpsc::channel(16);
        let (updates_tx, mut updates_rx) = mpsc::channel(16);
        let actor = tokio::spawn(run(
            InternetConfig {
                data_dir: data_dir.clone(),
                relays: vec![url.clone()],
            },
            rx,
            updates_tx,
        ));
        tx.send(Command::Join("dr5rs".into())).await.unwrap();
        let connected = tokio::time::timeout(Duration::from_secs(15), async {
            while let Some(update) = updates_rx.recv().await {
                if matches!(update, Update::InternetStatus { connected: 1, .. }) {
                    break;
                }
            }
        })
        .await;
        connected.unwrap();
        let sender = Client::new();
        sender.add_relay(&url).await.unwrap();
        sender.connect().await;
        let external = signed_message(&Keys::generate(), "dr5rs", "other", "incoming").unwrap();
        sender.send_event(&external).await.unwrap();
        let incoming = tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                if let Some(Update::Message(message)) = updates_rx.recv().await {
                    break message;
                }
            }
        })
        .await
        .unwrap();
        assert_eq!(incoming.text, "incoming");
        let own_key = key_for(&data_dir, "dr5rs").unwrap();
        let historical = signed_message(&own_key, "dr5rs", "me", "own history").unwrap();
        sender.send_event(&historical).await.unwrap();
        let own_history = tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                if let Some(Update::Message(message)) = updates_rx.recv().await {
                    break message;
                }
            }
        })
        .await
        .unwrap();
        assert_eq!(own_history.text, "own history");
        assert!(own_history.mine);
        tx.send(Command::Send {
            geohash: "dr5rs".into(),
            text: "outgoing".into(),
            nickname: "me".into(),
        })
        .await
        .unwrap();
        let outgoing = tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                if let Some(Update::Message(message)) = updates_rx.recv().await
                    && message.mine
                {
                    break message;
                }
            }
        })
        .await
        .unwrap();
        assert_eq!(outgoing.text, "outgoing");
        tx.send(Command::Send {
            geohash: "dr5rt".into(),
            text: "wrong room".into(),
            nickname: "me".into(),
        })
        .await
        .unwrap();
        let failed = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let Some(Update::SendFailed { room, .. }) = updates_rx.recv().await {
                    break room;
                }
            }
        })
        .await
        .unwrap();
        assert_eq!(failed, Room::Internet("dr5rt".into()));
        tx.send(Command::Shutdown).await.unwrap();
        actor.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn rejected_publish_never_appears_as_own_message() {
        let relay = LocalRelay::builder()
            .port(free_port())
            .write_policy(RejectChat)
            .build();
        relay.run().await.unwrap();
        let temp = tempfile::tempdir().unwrap();
        let (tx, rx) = mpsc::channel(16);
        let (updates_tx, mut updates_rx) = mpsc::channel(16);
        let actor = tokio::spawn(run(
            InternetConfig {
                data_dir: temp.path().join("new/data"),
                relays: vec![relay.url().await.to_string()],
            },
            rx,
            updates_tx,
        ));
        tx.send(Command::Join("dr5rs".into())).await.unwrap();
        tokio::time::timeout(Duration::from_secs(15), async {
            while let Some(update) = updates_rx.recv().await {
                if matches!(update, Update::InternetStatus { connected: 1, .. }) {
                    break;
                }
            }
        })
        .await
        .unwrap();
        tx.send(Command::Send {
            geohash: "dr5rs".into(),
            text: "rejected".into(),
            nickname: "me".into(),
        })
        .await
        .unwrap();
        let failure = tokio::time::timeout(Duration::from_secs(15), async {
            loop {
                match updates_rx.recv().await {
                    Some(Update::Message(message)) if message.mine => {
                        panic!("rejected message shown: {message:?}")
                    }
                    Some(Update::SendFailed { reason, .. }) => break reason,
                    _ => {}
                }
            }
        })
        .await
        .unwrap();
        assert!(
            failure.contains("relay") || failure.contains("accepted"),
            "{failure}"
        );
        tx.send(Command::Shutdown).await.unwrap();
        actor.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn publish_timeout_never_appears_as_own_message() {
        let relay = LocalRelay::builder()
            .port(free_port())
            .write_policy(SlowChat(None))
            .build();
        relay.run().await.unwrap();
        let temp = tempfile::tempdir().unwrap();
        let (tx, rx) = mpsc::channel(16);
        let (updates_tx, mut updates_rx) = mpsc::channel(16);
        let actor = tokio::spawn(run(
            InternetConfig {
                data_dir: temp.path().join("new/data"),
                relays: vec![relay.url().await.to_string()],
            },
            rx,
            updates_tx,
        ));
        tx.send(Command::Join("dr5rs".into())).await.unwrap();
        tokio::time::timeout(Duration::from_secs(15), async {
            while let Some(update) = updates_rx.recv().await {
                if matches!(update, Update::InternetStatus { connected: 1, .. }) {
                    break;
                }
            }
        })
        .await
        .unwrap();
        tx.send(Command::Send {
            geohash: "dr5rs".into(),
            text: "timeout".into(),
            nickname: "me".into(),
        })
        .await
        .unwrap();
        tokio::time::timeout(Duration::from_secs(20), async {
            loop {
                match updates_rx.recv().await {
                    Some(Update::Message(message)) if message.mine => {
                        panic!("unacknowledged message shown: {message:?}")
                    }
                    Some(Update::SendFailed { reason, .. }) => {
                        assert!(
                            reason.contains("timeout") || reason.contains("elapsed"),
                            "{reason}"
                        );
                        break;
                    }
                    _ => {}
                }
            }
        })
        .await
        .unwrap();
        tx.send(Command::Shutdown).await.unwrap();
        actor.await.unwrap().unwrap();
    }
}
