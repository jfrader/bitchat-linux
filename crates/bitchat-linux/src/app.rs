use std::collections::{HashMap, VecDeque};

use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers};
use tui_input::{Input, InputRequest, backend::crossterm::EventHandler};

use crate::types::{
    Action, FailedSend, HISTORY_LIMIT, MAX_TEXT_BYTES, Message, Notice, Room, Update,
    parse_geohash, send_failure_notice,
};

const MAX_SAVED_DRAFTS: usize = 16;
const SCROLL_LINES: usize = 10;

pub struct App {
    pub nickname: String,
    pub room: Room,
    pub geohash: Option<String>,
    pub mesh_status: String,
    pub internet_status: String,
    pub internet_connected: usize,
    pub peers: Vec<bitchatd::PeerView>,
    pub input: Input,
    pub notice: Option<Notice>,
    pub help: bool,
    /// Number of lines above the bottom of the current conversation.
    pub scroll: usize,
    mesh_messages: VecDeque<Message>,
    internet_messages: VecDeque<Message>,
    failed_sends: VecDeque<FailedSend>,
    drafts: HashMap<Room, Input>,
    last_mesh_state: Option<bitchatd::RadioState>,
}

impl App {
    pub fn new(nickname: String, geohash: Option<String>) -> Self {
        let internet_status = if geohash.is_some() {
            "Connecting"
        } else {
            "not joined"
        };
        let room = geohash
            .as_ref()
            .map_or(Room::Mesh, |hash| Room::Internet(hash.clone()));
        Self {
            nickname,
            room,
            geohash,
            mesh_status: "Starting".into(),
            internet_status: internet_status.into(),
            internet_connected: 0,
            peers: Vec::new(),
            input: Input::default(),
            notice: None,
            help: false,
            scroll: 0,
            mesh_messages: VecDeque::new(),
            internet_messages: VecDeque::new(),
            failed_sends: VecDeque::new(),
            drafts: HashMap::new(),
            last_mesh_state: None,
        }
    }

    pub fn apply_mesh(&mut self, snapshot: bitchatd::Snapshot) {
        self.nickname = snapshot.me.nickname;
        self.peers = snapshot.peers;
        self.mesh_status = match snapshot.radio.state {
            bitchatd::RadioState::AdapterOff => {
                if self.last_mesh_state != Some(bitchatd::RadioState::AdapterOff)
                    && self.notice.is_none()
                {
                    self.notice = Some(
                        "Bluetooth adapter off: rfkill unblock bluetooth; bluetoothctl power on"
                            .into(),
                    );
                }
                "adapter off".into()
            }
            bitchatd::RadioState::NoAdapter => "no adapter".into(),
            bitchatd::RadioState::Off => "off".into(),
            bitchatd::RadioState::Starting => "starting".into(),
            bitchatd::RadioState::Error => snapshot
                .radio
                .detail
                .unwrap_or_else(|| "Radio error".into()),
            bitchatd::RadioState::Running => format!("{} links", snapshot.radio.links),
        };
        self.last_mesh_state = Some(snapshot.radio.state);
        self.mesh_messages.clear();
        for message in snapshot.messages {
            self.insert(Message {
                id: message.id,
                room: Room::Mesh,
                author: message.sender_id,
                nickname: message.nickname,
                text: message.text,
                timestamp_ms: message.timestamp,
                mine: message.mine,
            });
        }
    }

    pub fn apply(&mut self, update: Update) {
        match update {
            Update::Message(message) => {
                if matches!(&message.room, Room::Internet(hash) if self.geohash.as_ref() != Some(hash))
                {
                    return;
                }
                self.insert(message);
            }
            Update::InternetStatus {
                geohash,
                detail,
                connected,
            } => {
                if geohash != self.geohash {
                    return;
                }
                self.internet_status = detail;
                self.internet_connected = connected;
            }
            Update::Notice(notice) => self.notice = Some(notice),
            Update::SendFailed { room, text, reason } => {
                let mut restored = bitchatd::clean_message(&text).replace(['\n', '\t'], " ");
                while restored.len() > MAX_TEXT_BYTES {
                    restored.pop();
                }
                if !restored.is_empty() {
                    if self.room == room {
                        if self.input.value().is_empty() {
                            self.input = Input::new(restored);
                        }
                    } else if !self.drafts.contains_key(&room)
                        && self.drafts.len() < MAX_SAVED_DRAFTS
                    {
                        self.drafts.insert(room.clone(), Input::new(restored));
                    }
                }
                self.notice = Some(Notice::in_room(
                    room.clone(),
                    send_failure_notice(&room, &text, &reason),
                ));
                self.failed_sends.push_back(FailedSend {
                    room,
                    text,
                    reason,
                    timestamp_ms: chrono::Utc::now()
                        .timestamp_millis()
                        .try_into()
                        .unwrap_or_default(),
                });
                if self.failed_sends.len() > HISTORY_LIMIT {
                    self.failed_sends.pop_front();
                }
            }
        }
    }

    fn insert(&mut self, message: Message) {
        let messages = match message.room {
            Room::Mesh => &mut self.mesh_messages,
            Room::Internet(_) => &mut self.internet_messages,
        };
        if messages.iter().any(|old| old.id == message.id) {
            return;
        }
        let index = messages.partition_point(|old| old.timestamp_ms <= message.timestamp_ms);
        messages.insert(index, message);
        if messages.len() > HISTORY_LIMIT {
            messages.pop_front();
        }
    }

    fn switch_room(&mut self, room: Room) -> bool {
        if self.room == room {
            return true;
        }
        if !self.input.value().is_empty()
            && self.drafts.len() >= MAX_SAVED_DRAFTS
            && !self.drafts.contains_key(&room)
        {
            self.notice =
                Some("Draft limit reached. Send or clear this draft before switching rooms".into());
            return false;
        }
        let destination = self.drafts.remove(&room).unwrap_or_default();
        let previous = std::mem::replace(&mut self.input, destination);
        if !previous.value().is_empty() {
            self.drafts.insert(self.room.clone(), previous);
        }
        self.room = room;
        self.scroll = 0;
        true
    }

    pub fn join_channel(&mut self, geohash: String) -> bool {
        if !self.switch_room(Room::Internet(geohash.clone())) {
            return false;
        }
        if self.geohash.as_deref() != Some(&geohash) {
            self.internet_messages.clear();
        }
        self.geohash = Some(geohash);
        self.internet_status = "connecting".into();
        self.internet_connected = 0;
        true
    }

    pub fn messages(&self) -> &VecDeque<Message> {
        match self.room {
            Room::Mesh => &self.mesh_messages,
            Room::Internet(_) => &self.internet_messages,
        }
    }

    pub fn failed_sends(&self) -> impl Iterator<Item = &FailedSend> {
        self.failed_sends
            .iter()
            .filter(|send| send.room == self.room)
    }

    pub fn notice_text(&self) -> Option<&str> {
        self.notice.as_ref().and_then(|notice| {
            (notice.room.as_ref().is_none_or(|room| room == &self.room))
                .then_some(notice.text.as_str())
        })
    }

    pub fn handle_event(&mut self, event: Event) -> Option<Action> {
        if let Event::Paste(text) = &event {
            if !self.help {
                for c in bitchatd::clean_message(text).chars() {
                    let c = if c == '\n' || c == '\t' { ' ' } else { c };
                    if self.input.value().len() + c.len_utf8() > MAX_TEXT_BYTES {
                        break;
                    }
                    self.input.handle(InputRequest::InsertChar(c));
                }
            }
            return None;
        }
        let Event::Key(key) = event else { return None };
        if !matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) {
            return None;
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            return Some(Action::Quit);
        }
        match key.code {
            KeyCode::F(1) => self.help = !self.help,
            KeyCode::Esc if self.help => self.help = false,
            KeyCode::Esc => self.input.reset(),
            KeyCode::Tab if !self.help => {
                let room = match &self.room {
                    Room::Mesh => self
                        .geohash
                        .as_ref()
                        .map_or(Room::Mesh, |hash| Room::Internet(hash.clone())),
                    Room::Internet(_) => Room::Mesh,
                };
                self.switch_room(room);
            }
            KeyCode::PageUp if !self.help => self.scroll = self.scroll.saturating_add(SCROLL_LINES),
            KeyCode::PageDown if !self.help => {
                self.scroll = self.scroll.saturating_sub(SCROLL_LINES)
            }
            KeyCode::Enter if !self.help => return self.submit(),
            _ if !self.help => {
                if let KeyCode::Char(character) = key.code
                    && bitchatd::clean_message(&character.to_string()).is_empty()
                {
                    return None;
                }
                let mut input = self.input.clone();
                input.handle_event(&Event::Key(key));
                if input.value().len() <= MAX_TEXT_BYTES {
                    self.input = input;
                }
            }
            _ => {}
        }
        None
    }

    fn submit(&mut self) -> Option<Action> {
        let text = self.input.value().trim().to_owned();
        if text.is_empty() {
            self.input.reset();
            return None;
        }
        if let Some(body) = text.strip_prefix("//") {
            self.input.reset();
            return Some(Action::Send {
                room: self.room.clone(),
                text: format!("/{body}"),
                nickname: self.nickname.clone(),
            });
        }
        if !text.starts_with('/') {
            self.input.reset();
            return Some(Action::Send {
                room: self.room.clone(),
                text,
                nickname: self.nickname.clone(),
            });
        }
        if let Some(name) = text.strip_prefix("/nick ") {
            let action = match bitchatd::sanitize_nickname(name) {
                Some(name) => Some(Action::Nickname(name)),
                None => {
                    self.notice = Some("Invalid nickname".into());
                    None
                }
            };
            self.input.reset();
            return action;
        }
        let mut parts = text.split_whitespace();
        let command = parts.next().unwrap_or_default();
        let argument = parts.next();
        let no_extra = parts.next().is_none();
        self.input.reset();
        match (command, argument, no_extra) {
            ("/join", Some(hash), true) => match parse_geohash(hash) {
                Ok(hash) => Some(Action::Join(hash)),
                Err(error) => {
                    self.notice = Some(error.to_string().into());
                    None
                }
            },
            ("/mesh", None, true) => {
                self.switch_room(Room::Mesh);
                None
            }
            ("/internet", None, true) => {
                if let Some(hash) = &self.geohash {
                    self.switch_room(Room::Internet(hash.clone()));
                } else {
                    self.notice = Some("Join a location first: /join <geohash>".into());
                }
                None
            }
            ("/radio", Some(mode), true) => match bitchatd::Mode::parse(mode) {
                Some(mode) => Some(Action::Radio(mode)),
                None => {
                    self.notice = Some("Use /radio auto|balanced|saver|off".into());
                    None
                }
            },
            ("/clear", None, true) => {
                match self.room {
                    Room::Mesh => self.mesh_messages.clear(),
                    Room::Internet(_) => self.internet_messages.clear(),
                }
                self.failed_sends.retain(|send| send.room != self.room);
                if self
                    .notice
                    .as_ref()
                    .is_some_and(|notice| notice.room.as_ref() == Some(&self.room))
                {
                    self.notice = None;
                }
                self.scroll = 0;
                Some(Action::Clear(self.room.clone()))
            }
            ("/help", None, true) => {
                self.help = true;
                None
            }
            ("/quit", None, true) => Some(Action::Quit),
            ("/dm" | "/msg", _, _) => {
                self.notice = Some("Encrypted DMs are not implemented".into());
                None
            }
            _ => {
                self.notice = Some("Unknown command. F1 for help".into());
                None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyEvent, KeyModifiers};

    fn key(code: KeyCode) -> Event {
        Event::Key(KeyEvent::new(code, KeyModifiers::NONE))
    }
    fn submit(app: &mut App, text: &str) -> Option<Action> {
        app.input = Input::new(text.into());
        app.handle_event(key(KeyCode::Enter))
    }
    fn message(id: &str, room: Room, time: u64) -> Message {
        Message {
            id: id.into(),
            room,
            author: "id".into(),
            nickname: "name".into(),
            text: "text".into(),
            timestamp_ms: time,
            mine: false,
        }
    }

    #[test]
    fn commands_and_dm_safety() {
        let mut app = App::new("me".into(), None);
        assert_eq!(submit(&mut app, "/dm alice secret"), None);
        assert_eq!(app.notice_text(), Some("Encrypted DMs are not implemented"));
        assert_eq!(
            submit(&mut app, "//dm secret"),
            Some(Action::Send {
                room: Room::Mesh,
                text: "/dm secret".into(),
                nickname: "me".into()
            })
        );
        assert_eq!(
            submit(&mut app, "/join #DR5RS"),
            Some(Action::Join("dr5rs".into()))
        );
        assert_eq!(app.room, Room::Mesh);
        assert_eq!(app.geohash, None);
        app.join_channel("dr5rs".into());
        assert_eq!(
            submit(&mut app, "/radio saver"),
            Some(Action::Radio(bitchatd::Mode::Saver))
        );
        assert_eq!(
            submit(&mut app, "/nick alice"),
            Some(Action::Nickname("alice".into()))
        );
        assert_eq!(
            submit(&mut app, "/clear"),
            Some(Action::Clear(Room::Internet("dr5rs".into())))
        );
    }

    #[test]
    fn drafts_and_unicode_byte_limit() {
        let mut app = App::new("me".into(), None);
        for _ in 0..MAX_TEXT_BYTES {
            app.handle_event(key(KeyCode::Char('é')));
        }
        assert!(app.input.value().len() <= MAX_TEXT_BYTES);
        assert!(app.input.value().ends_with('é'));
        app.input = Input::new("new draft".into());
        app.apply(Update::SendFailed {
            room: Room::Mesh,
            text: "old".into(),
            reason: "failed".into(),
        });
        assert_eq!(app.input.value(), "new draft");
        app.input.reset();
        app.apply(Update::SendFailed {
            room: Room::Mesh,
            text: "old".into(),
            reason: "failed".into(),
        });
        assert_eq!(app.input.value(), "old");
        app.handle_event(key(KeyCode::Char('🦀')));
        assert_eq!(app.input.value(), "old🦀");
        app.handle_event(key(KeyCode::Backspace));
        assert_eq!(app.input.value(), "old");
    }

    #[test]
    fn stale_channels_order_dedupe_and_limit() {
        let mut app = App::new("me".into(), Some("dr5rs".into()));
        app.apply(Update::Message(message(
            "old",
            Room::Internet("u4pr".into()),
            0,
        )));
        assert!(app.messages().is_empty());
        for i in (0..HISTORY_LIMIT + 3).rev() {
            app.apply(Update::Message(message(
                &format!("{i}"),
                app.room.clone(),
                i as u64,
            )));
        }
        app.apply(Update::Message(message("4", app.room.clone(), 4)));
        assert_eq!(app.messages().len(), HISTORY_LIMIT);
        assert_eq!(app.messages().front().unwrap().id, "3");
        app.join_channel("u4pr".into());
        assert!(app.messages().is_empty());
    }

    #[test]
    fn snapshot_uses_persisted_identity_and_short_radio_status() {
        let mut app = App::new("anon".into(), None);
        assert_eq!(app.internet_status, "not joined");
        let radio = bitchatd::RadioStatus {
            state: bitchatd::RadioState::Running,
            links: 0,
            ..Default::default()
        };
        app.apply_mesh(bitchatd::Snapshot {
            me: bitchatd::Me {
                peer_id: "id".into(),
                nickname: "persisted".into(),
                fingerprint: "fp".into(),
            },
            peers: Vec::new(),
            messages: Vec::new(),
            settings: bitchatd::Settings::default(),
            radio,
            version: "test",
        });
        assert_eq!(app.nickname, "persisted");
        assert_eq!(app.mesh_status, "0 links");
    }

    #[test]
    fn paste_uses_unicode_cursor_editing_without_executing_commands() {
        let mut app = App::new("me".into(), None);
        app.input = Input::new("ab".into()).with_cursor(1);
        assert_eq!(
            app.handle_event(Event::Paste("é\n\t\u{1b}[31m\u{202e}中".into())),
            None
        );
        assert_eq!(app.input.value(), "aé  [31m中b");
        assert_eq!(app.input.cursor(), "aé  [31m中".chars().count());
        app.input.reset();
        app.handle_event(Event::Paste("🦀".repeat(MAX_TEXT_BYTES)));
        assert_eq!(app.input.value().len(), MAX_TEXT_BYTES);
        assert!(app.input.value().chars().all(|c| c == '🦀'));
        app.handle_event(Event::Paste("no more".into()));
        assert_eq!(app.input.value().len(), MAX_TEXT_BYTES);
    }

    #[test]
    fn failed_text_stays_visible_without_overwriting_a_new_draft_or_room() {
        let mut app = App::new("me".into(), Some("dr5rs".into()));
        app.input = Input::new("new draft".into());
        app.apply(Update::SendFailed {
            room: Room::Internet("dr5rs".into()),
            text: "older text".into(),
            reason: "offline".into(),
        });
        assert_eq!(app.input.value(), "new draft");
        let notice = app.notice_text().unwrap();
        assert!(
            notice.contains("#dr5rs")
                && notice.contains("older text")
                && notice.contains("offline")
        );
        app.handle_event(key(KeyCode::Esc));
        app.handle_event(key(KeyCode::Tab));
        assert!(app.notice_text().is_none());
        app.apply(Update::SendFailed {
            room: Room::Internet("dr5rs".into()),
            text: "lost text".into(),
            reason: "timeout".into(),
        });
        assert_eq!(app.input.value(), "");
        assert!(app.notice_text().is_none());
        assert_eq!(app.failed_sends().count(), 0);
        app.handle_event(key(KeyCode::Tab));
        assert_eq!(app.input.value(), "lost text");
        assert!(app.notice_text().unwrap().contains("lost text"));
    }

    #[test]
    fn tab_keeps_per_room_text_and_cursor_until_each_room_submits() {
        let mut app = App::new("me".into(), Some("dr5rs".into()));
        app.handle_event(Event::Paste("mesh".into())); // Starts in Internet, not mesh.
        assert_eq!(app.room, Room::Internet("dr5rs".into()));
        app.handle_event(key(KeyCode::Left));
        let internet_cursor = app.input.cursor();
        app.handle_event(key(KeyCode::Tab));
        assert_eq!(app.room, Room::Mesh);
        assert_eq!(app.input.value(), "");
        app.handle_event(Event::Paste("bluetooth".into()));
        app.handle_event(key(KeyCode::Tab));
        assert_eq!(app.input.value(), "mesh");
        assert_eq!(app.input.cursor(), internet_cursor);
        // Restored Internet draft submits only to Internet.
        app.handle_event(Event::Paste("!".into()));
        assert_eq!(
            app.handle_event(key(KeyCode::Enter)),
            Some(Action::Send {
                room: Room::Internet("dr5rs".into()),
                text: "mes!h".into(),
                nickname: "me".into()
            })
        );
        app.handle_event(key(KeyCode::Tab));
        assert_eq!(app.input.value(), "bluetooth");
        assert_eq!(
            app.handle_event(key(KeyCode::Enter)),
            Some(Action::Send {
                room: Room::Mesh,
                text: "bluetooth".into(),
                nickname: "me".into()
            })
        );
    }

    #[test]
    fn commands_restore_destination_drafts_and_join_only_after_acceptance() {
        let mut app = App::new("me".into(), Some("dr5rs".into()));
        app.handle_event(key(KeyCode::Tab));
        app.handle_event(Event::Paste("mesh draft".into()));
        app.handle_event(key(KeyCode::Tab));
        app.handle_event(Event::Paste("internet draft".into()));
        assert_eq!(submit(&mut app, "/mesh"), None);
        assert_eq!(app.input.value(), "mesh draft");
        assert_eq!(submit(&mut app, "/internet"), None);
        assert_eq!(app.input.value(), ""); // The command replaced this room's draft.
        app.handle_event(Event::Paste("location one".into()));
        assert_eq!(
            submit(&mut app, "/join u4pru"),
            Some(Action::Join("u4pru".into()))
        );
        assert_eq!(app.room, Room::Internet("dr5rs".into()));
        assert_eq!(app.geohash.as_deref(), Some("dr5rs"));
        app.join_channel("u4pru".into());
        assert_eq!(app.input.value(), "");
        app.handle_event(Event::Paste("location two".into()));
        app.join_channel("dr5rs".into());
        assert_eq!(app.input.value(), "");
        app.join_channel("u4pru".into());
        assert_eq!(app.input.value(), "location two");
        assert_eq!(
            app.handle_event(key(KeyCode::Enter)),
            Some(Action::Send {
                room: Room::Internet("u4pru".into()),
                text: "location two".into(),
                nickname: "me".into()
            })
        );
        assert_eq!(submit(&mut app, "/msg alice private"), None);
        assert_eq!(app.notice_text(), Some("Encrypted DMs are not implemented"));
    }

    #[test]
    fn internet_command_restores_destination_draft() {
        let mut app = App::new("me".into(), Some("dr5rs".into()));
        app.handle_event(Event::Paste("internet draft".into()));
        app.handle_event(key(KeyCode::Tab));
        assert_eq!(app.room, Room::Mesh);
        assert_eq!(submit(&mut app, "/internet"), None);
        assert_eq!(app.room, Room::Internet("dr5rs".into()));
        assert_eq!(app.input.value(), "internet draft");
    }

    #[test]
    fn draft_limit_refuses_switch_without_losing_unsent_text() {
        let mut app = App::new("me".into(), None);
        for i in 0..MAX_SAVED_DRAFTS {
            app.handle_event(Event::Paste(format!("unsent {i}")));
            app.join_channel(format!("u{i}"));
        }
        assert_eq!(app.drafts.len(), MAX_SAVED_DRAFTS);
        app.handle_event(Event::Paste("latest unsent".into()));
        app.join_channel("new".into());
        assert_eq!(app.input.value(), "latest unsent");
        assert_eq!(app.geohash.as_deref(), Some("u15"));
        assert!(app.notice_text().unwrap().contains("Draft limit"));
        // A switch to an already saved room frees its slot first.
        app.join_channel("u0".into());
        assert_eq!(app.input.value(), "unsent 1");
        assert_eq!(app.drafts.len(), MAX_SAVED_DRAFTS);
    }

    #[test]
    fn failed_send_restores_inactive_room_without_overwriting_existing_draft() {
        let mut app = App::new("me".into(), Some("dr5rs".into()));
        app.handle_event(key(KeyCode::Tab));
        app.apply(Update::SendFailed {
            room: Room::Internet("dr5rs".into()),
            text: "retry".into(),
            reason: "offline".into(),
        });
        assert_eq!(app.input.value(), "");
        assert!(app.notice_text().is_none());
        app.handle_event(key(KeyCode::Tab));
        assert_eq!(app.input.value(), "retry");
        assert!(app.notice_text().unwrap().contains("retry"));
        app.handle_event(key(KeyCode::Tab));
        app.apply(Update::SendFailed {
            room: Room::Internet("dr5rs".into()),
            text: "older".into(),
            reason: "offline".into(),
        });
        assert!(app.notice_text().is_none());
        app.handle_event(key(KeyCode::Tab));
        assert_eq!(app.input.value(), "retry");
        assert!(app.notice_text().unwrap().contains("older"));
        assert_eq!(app.failed_sends().count(), 2);
    }

    #[test]
    fn statuses_from_a_previous_geohash_do_not_replace_current_connection_state() {
        let mut app = App::new("me".into(), Some("dr5rs".into()));
        app.join_channel("u4pru".into());
        app.apply(Update::InternetStatus {
            geohash: Some("u4pru".into()),
            detail: "new connected".into(),
            connected: 1,
        });
        app.apply(Update::InternetStatus {
            geohash: Some("dr5rs".into()),
            detail: "old offline".into(),
            connected: 0,
        });
        app.apply(Update::InternetStatus {
            geohash: None,
            detail: "not joined".into(),
            connected: 0,
        });
        assert_eq!(app.internet_status, "new connected");
        assert_eq!(app.internet_connected, 1);
    }

    #[test]
    fn repeated_blocked_snapshots_do_not_hide_send_failures() {
        let mut app = App::new("me".into(), None);
        let snapshot = bitchatd::Snapshot {
            me: bitchatd::Me {
                peer_id: "id".into(),
                nickname: "me".into(),
                fingerprint: "fp".into(),
            },
            peers: Vec::new(),
            messages: Vec::new(),
            settings: bitchatd::Settings::default(),
            radio: bitchatd::RadioStatus {
                state: bitchatd::RadioState::AdapterOff,
                ..Default::default()
            },
            version: "test",
        };
        app.apply_mesh(snapshot.clone());
        app.apply(Update::SendFailed {
            room: Room::Mesh,
            text: "unsent".into(),
            reason: "disk full".into(),
        });
        app.apply_mesh(snapshot);
        assert!(app.notice_text().unwrap().contains("disk full"));
        app.handle_event(key(KeyCode::Char('\u{202e}')));
        assert_eq!(app.input.value(), "unsent");
    }

    #[test]
    fn failed_send_history_is_bounded_and_cleared_per_room() {
        let mut app = App::new("me".into(), Some("dr5rs".into()));
        for i in 0..HISTORY_LIMIT + 2 {
            app.apply(Update::SendFailed {
                room: app.room.clone(),
                text: format!("unsent {i}"),
                reason: "offline".into(),
            });
        }
        assert_eq!(app.failed_sends.len(), HISTORY_LIMIT);
        assert_eq!(app.failed_sends.front().unwrap().text, "unsent 2");
        app.handle_event(key(KeyCode::Tab));
        app.apply(Update::SendFailed {
            room: Room::Mesh,
            text: "mesh unsent".into(),
            reason: "unavailable".into(),
        });
        assert_eq!(app.failed_sends().count(), 1);
        submit(&mut app, "/clear");
        assert_eq!(app.failed_sends().count(), 0);
        app.handle_event(key(KeyCode::Tab));
        assert_eq!(app.failed_sends().count(), HISTORY_LIMIT - 1);
    }

    #[test]
    fn clearing_a_room_removes_its_notice_but_keeps_global_notices() {
        let mut app = App::new("me".into(), None);
        app.apply(Update::SendFailed {
            room: Room::Mesh,
            text: "unsent".into(),
            reason: "offline".into(),
        });
        assert!(app.notice_text().unwrap().contains("unsent"));
        submit(&mut app, "/clear");
        assert!(app.notice_text().is_none());
        assert_eq!(app.failed_sends().count(), 0);

        app.apply(Update::Notice("Bluetooth unavailable".into()));
        submit(&mut app, "/clear");
        assert_eq!(app.notice_text(), Some("Bluetooth unavailable"));
    }
}
