use chrono::{Local, TimeZone};
use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Borders, Clear, Paragraph, Wrap},
};

use crate::{
    app::App,
    types::{APP_NAME, FailedSend, Message, Room},
};

const ID_SUFFIX_LENGTH: usize = 6;
const DIM: Color = Color::DarkGray;
/// Width of the right-aligned nickname column in the log.
const NICK_WIDTH: usize = 9;
/// Text column start: `HH:MM ` (6) + nickname (NICK_WIDTH + 1) + `│ ` (2).
const TEXT_COLUMN: usize = 18;

const HELP: &[&str] = &[
    "Public chat (unencrypted)",
    "Enter       Send a message",
    "Tab         Switch between mesh and Internet",
    "PgUp/PgDn   Scroll chat history",
    "Esc         Clear draft / close help",
    "/join <geohash>    Join a location channel",
    "/mesh              Open Bluetooth mesh chat",
    "/internet          Open joined location channel",
    "#mesh, #<geohash>  Same as /mesh and /internet",
    "/nick <name>       Change display name",
    "/radio auto|balanced|saver|off",
    "/clear             Clear current room history",
    "/help              Show this help",
    "/quit              Exit (or Ctrl-C)",
    "//text             Send a message beginning with /",
    "Encrypted DMs are not implemented",
    "Adapter off? rfkill unblock bluetooth; bluetoothctl power on",
];

fn label(input: &str) -> String {
    bitchatd::clean_message(input).replace(['\n', '\t'], " ")
}

fn short_id(input: &str) -> String {
    let id = label(input);
    id.chars()
        .skip(id.chars().count().saturating_sub(ID_SUFFIX_LENGTH))
        .collect()
}

fn timestamp(ms: u64) -> String {
    i64::try_from(ms)
        .ok()
        .and_then(|value| Local.timestamp_millis_opt(value).single())
        .map_or_else(|| "--:--".into(), |time| time.format("%H:%M").to_string())
}

fn conversation<'a>(
    mut lines: Vec<Line<'a>>,
    width: u16,
    height: u16,
    from_bottom: usize,
) -> Paragraph<'a> {
    let wrap = Wrap { trim: false };
    let rows: Vec<_> = lines
        .iter()
        .map(|line| Paragraph::new(line.clone()).wrap(wrap).line_count(width))
        .collect();
    let total: usize = rows.iter().sum();
    let start = total
        .saturating_sub(height as usize)
        .saturating_sub(from_bottom);
    let mut removed_rows = 0;
    let mut removed_lines = 0;
    for count in rows {
        if removed_rows + count > start {
            break;
        }
        removed_rows += count;
        removed_lines += 1;
    }
    lines.drain(..removed_lines);
    Paragraph::new(lines)
        .wrap(wrap)
        .scroll(((start - removed_rows).min(u16::MAX as usize) as u16, 0))
}

fn nick_span(message: &Message, width: usize) -> Span<'static> {
    let name =
        bitchatd::sanitize_nickname(&message.nickname).unwrap_or_else(|| label(&message.nickname));
    let trimmed: String = name.chars().take(width).collect();
    Span::styled(
        format!("{trimmed:>width$} "),
        Style::default()
            .fg(if message.mine {
                Color::Cyan
            } else {
                Color::Yellow
            })
            .add_modifier(Modifier::BOLD),
    )
}

fn failed_header(send: &FailedSend) -> Line<'static> {
    Line::styled(
        format!(
            "{} Not sent · {}",
            timestamp(send.timestamp_ms),
            label(&send.reason)
        ),
        Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
    )
}

fn indented_lines(text: &str) -> Vec<Line<'static>> {
    bitchatd::clean_message(text)
        .split('\n')
        .map(|line| Line::from(format!("{}{line}", " ".repeat(TEXT_COLUMN))))
        .collect()
}

enum Entry<'a> {
    Message(&'a Message),
    Failed(&'a FailedSend),
}

fn chat_lines(app: &App) -> Vec<Line<'static>> {
    let mut entries: Vec<(u64, Entry<'_>)> = app
        .messages()
        .iter()
        .map(|message| (message.timestamp_ms, Entry::Message(message)))
        .chain(
            app.failed_sends()
                .map(|send| (send.timestamp_ms, Entry::Failed(send))),
        )
        .collect();
    entries.sort_by_key(|(timestamp_ms, _)| *timestamp_ms);

    let mut lines: Vec<Line<'static>> = Vec::new();
    for (_, entry) in entries {
        match entry {
            Entry::Message(message) => {
                let mut parts: Vec<String> = bitchatd::clean_message(&message.text)
                    .split('\n')
                    .map(str::to_owned)
                    .collect();
                if parts.is_empty() {
                    parts.push(String::new());
                }
                let first = parts.remove(0);
                lines.push(Line::from(vec![
                    Span::styled(
                        format!("{} ", timestamp(message.timestamp_ms)),
                        Style::default().fg(DIM),
                    ),
                    nick_span(message, NICK_WIDTH),
                    Span::styled("│ ", Style::default().fg(DIM)),
                    Span::raw(first),
                ]));
                lines.extend(
                    parts
                        .into_iter()
                        .map(|part| Line::from(format!("{}{part}", " ".repeat(TEXT_COLUMN)))),
                );
            }
            Entry::Failed(send) => {
                lines.push(failed_header(send));
                lines.extend(indented_lines(&send.text));
            }
        }
    }
    lines
}

pub fn render(frame: &mut Frame, app: &App) {
    let area = frame.area();
    if area.width == 0 || area.height == 0 {
        return;
    }
    let sections = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(4),
            Constraint::Min(0),
            Constraint::Length(2),
            Constraint::Length(1),
        ])
        .split(area);

    let header = Text::from(vec![
        Line::from(vec![
            Span::styled(APP_NAME, Style::default().add_modifier(Modifier::BOLD)),
            Span::styled(
                format!("  ·  {}  ·  public (unencrypted)", label(&app.nickname)),
                Style::default().fg(DIM),
            ),
        ]),
        Line::from(vec![
            Span::styled("Mesh: ", Style::default().fg(DIM)),
            Span::raw(label(&app.mesh_status)),
        ]),
        Line::from(vec![
            Span::styled("Internet: ", Style::default().fg(DIM)),
            Span::raw(label(&app.internet_status)),
        ]),
    ]);
    frame.render_widget(
        Paragraph::new(header).block(
            Block::default()
                .borders(Borders::BOTTOM)
                .border_style(Style::default().fg(DIM)),
        ),
        sections[0],
    );

    let sidebar_width = if sections[1].width >= 52 { 23 } else { 0 };
    let columns = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(sidebar_width), Constraint::Min(0)])
        .split(sections[1]);
    if sidebar_width > 0 {
        let mut lines = vec![
            Line::from(Span::styled("Rooms", Style::default().fg(DIM))),
            room_line("#mesh", app.room == Room::Mesh),
        ];
        if let Some(hash) = &app.geohash {
            lines.push(room_line(
                &format!("#{hash}"),
                app.room == Room::Internet(hash.clone()),
            ));
        } else {
            lines.push(Line::from(Span::styled(
                "  /join <geohash>",
                Style::default().fg(DIM),
            )));
        }
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled("Nearby", Style::default().fg(DIM))));
        if app.peers.is_empty() {
            lines.push(Line::from(Span::styled(
                "No peers",
                Style::default().fg(DIM),
            )));
        }
        for peer in &app.peers {
            lines.push(Line::from(vec![
                Span::raw(label(&peer.nickname)),
                Span::styled(
                    format!(" · {}", short_id(&peer.id)),
                    Style::default().fg(DIM),
                ),
            ]));
        }
        frame.render_widget(
            Paragraph::new(lines).block(
                Block::default()
                    .borders(Borders::RIGHT)
                    .border_style(Style::default().fg(DIM)),
            ),
            columns[0],
        );
    }

    let chat_area = columns[1];
    if chat_area.width > 0 && chat_area.height > 0 {
        frame.render_widget(
            conversation(
                chat_lines(app),
                chat_area.width,
                chat_area.height,
                app.scroll,
            ),
            chat_area,
        );
    }

    let composer_area = sections[2];
    if composer_area.width > 0 && composer_area.height > 0 {
        let rows =
            Layout::vertical([Constraint::Length(1), Constraint::Length(1)]).split(composer_area);
        frame.render_widget(
            Paragraph::new(Span::styled(
                "─".repeat(composer_area.width as usize),
                Style::default().fg(DIM),
            )),
            rows[0],
        );
        let room = label(&app.room.label());
        let prefix_width = room.chars().count() + 3;
        let input_area = rows[1];
        if (input_area.width as usize) > prefix_width {
            let columns =
                Layout::horizontal([Constraint::Length(prefix_width as u16), Constraint::Min(0)])
                    .split(input_area);
            frame.render_widget(
                Paragraph::new(Line::from(vec![
                    Span::styled(room, Style::default().add_modifier(Modifier::BOLD)),
                    Span::styled(" › ", Style::default().fg(DIM)),
                ])),
                columns[0],
            );
            let scroll = app.input.visual_scroll(columns[1].width as usize);
            frame.render_widget(
                Paragraph::new(label(app.input.value()))
                    .scroll((0, scroll.min(u16::MAX as usize) as u16)),
                columns[1],
            );
            if columns[1].width > 0 {
                let cursor = app
                    .input
                    .visual_cursor()
                    .saturating_sub(scroll)
                    .min(columns[1].width.saturating_sub(1) as usize);
                frame.set_cursor_position((columns[1].x + cursor as u16, columns[1].y));
            }
        }
    }

    let footer = if let Some(notice) = app.notice_text() {
        Line::from(vec![
            Span::styled(label(notice), Style::default().fg(Color::Yellow)),
            Span::styled(
                "  ·  F1 help  Tab rooms  Ctrl-C quit",
                Style::default().fg(DIM),
            ),
        ])
    } else {
        Line::from(Span::styled(
            "Public chat · Enter send  Tab rooms  PgUp/PgDn scroll  F1 help  Ctrl-C quit",
            Style::default().fg(DIM),
        ))
    };
    frame.render_widget(
        Paragraph::new(footer).wrap(Wrap { trim: false }),
        sections[3],
    );
    if app.help {
        let width = area.width.saturating_sub(4).min(72);
        let height = area.height.saturating_sub(2).min((HELP.len() + 2) as u16);
        let overlay = ratatui::layout::Rect::new(
            area.x + (area.width - width) / 2,
            area.y + (area.height - height) / 2,
            width,
            height,
        );
        frame.render_widget(Clear, overlay);
        frame.render_widget(
            Paragraph::new(HELP.iter().copied().map(Line::from).collect::<Vec<_>>()).block(
                Block::default()
                    .title("Help · F1 / Esc to close")
                    .borders(Borders::ALL),
            ),
            overlay,
        );
    }
}

fn room_line(name: &str, active: bool) -> Line<'static> {
    Line::from(Span::styled(
        format!("{}{}", if active { "> " } else { "  " }, label(name)),
        if active {
            Style::default().add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(DIM)
        },
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Update;
    use ratatui::{Terminal, backend::TestBackend};

    #[test]
    fn tiny_and_narrow_layouts_render_without_panicking() {
        let mut app = App::new("alice\n\u{1b}[31m".into(), Some("dr5rs".into()));
        app.notice = Some("\u{1b}[2J\u{202e}bad".into());
        for (width, height) in [(1, 1), (10, 3), (30, 8), (80, 24)] {
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            terminal.draw(|frame| render(frame, &app)).unwrap();
            let output = format!("{:?}", terminal.backend().buffer());
            assert!(!output.contains("\u{1b}"));
            assert!(!output.contains("\u{202e}"));
        }
    }

    #[test]
    fn wrapped_messages_scroll_from_bottom() {
        let mut app = App::new("alice".into(), None);
        for i in 0..40 {
            app.apply(Update::Message(Message {
                id: i.to_string(),
                room: Room::Mesh,
                author: "sender".into(),
                nickname: "bob".into(),
                text: format!("line {i}\ncontinued"),
                timestamp_ms: 1_750_000_000_000,
                mine: false,
            }));
        }
        let mut terminal = Terminal::new(TestBackend::new(25, 12)).unwrap();
        terminal.draw(|frame| render(frame, &app)).unwrap();
        let bottom = format!("{:?}", terminal.backend().buffer());
        assert!(bottom.contains("line 39"));
        app.scroll = 60;
        terminal.draw(|frame| render(frame, &app)).unwrap();
        let earlier = format!("{:?}", terminal.backend().buffer());
        assert!(!earlier.contains("line 39"));
    }

    #[test]
    fn bottom_scroll_accounts_for_word_wrapping() {
        let mut app = App::new("alice".into(), None);
        for i in 0..40 {
            app.apply(Update::Message(Message {
                id: i.to_string(),
                room: Room::Mesh,
                author: "sender".into(),
                nickname: "bob".into(),
                text: format!("abcdefghijklmn abcdefghijklmn abcdefghijklmn\nlast {i}"),
                timestamp_ms: 1_750_000_000_000,
                mine: false,
            }));
        }
        let mut terminal = Terminal::new(TestBackend::new(25, 12)).unwrap();
        terminal.draw(|frame| render(frame, &app)).unwrap();
        assert!(format!("{:?}", terminal.backend().buffer()).contains("last 39"));
    }

    #[test]
    fn bottom_view_survives_more_than_u16_rows_and_unicode_whitespace() {
        let mut app = App::new("alice".into(), None);
        for i in 0..crate::types::HISTORY_LIMIT {
            app.apply(Update::Message(Message {
                id: i.to_string(),
                room: Room::Mesh,
                author: "sender".into(),
                nickname: "bob".into(),
                text: format!("{}\nlast {i}", "  中 🦀   \n".repeat(140)),
                timestamp_ms: i as u64,
                mine: false,
            }));
        }
        let mut terminal = Terminal::new(TestBackend::new(60, 12)).unwrap();
        terminal.draw(|frame| render(frame, &app)).unwrap();
        assert!(format!("{:?}", terminal.backend().buffer()).contains("last 499"));
    }

    #[test]
    fn help_overlay_and_independent_transport_rows_fit_80x24() {
        let mut app = App::new("alice".into(), None);
        app.mesh_status = "adapter off".into();
        app.internet_status = "connected".into();
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal.draw(|frame| render(frame, &app)).unwrap();
        let normal = format!("{:?}", terminal.backend().buffer());
        assert!(normal.contains("/join <geohash>"));
        assert!(normal.contains("Mesh: adapter off"));
        assert!(normal.contains("Internet: connected"));
        app.help = true;
        terminal.draw(|frame| render(frame, &app)).unwrap();
        let help = format!("{:?}", terminal.backend().buffer());
        for command in [
            "/join <geohash>",
            "/radio auto|balanced|saver|off",
            "/clear",
            "/quit",
            "rfkill unblock bluetooth; bluetoothctl power on",
        ] {
            assert!(help.contains(command), "missing help: {command}");
        }
    }

    #[test]
    fn every_message_keeps_its_own_line() {
        let mut app = App::new("alice".into(), None);
        for i in 0..3u64 {
            app.apply(Update::Message(Message {
                id: i.to_string(),
                room: Room::Mesh,
                author: "sender".into(),
                nickname: "bob".into(),
                text: format!("line {i}"),
                timestamp_ms: 1_750_000_000_000 + i * 1_000,
                mine: false,
            }));
        }
        let mut terminal = Terminal::new(TestBackend::new(60, 14)).unwrap();
        terminal.draw(|frame| render(frame, &app)).unwrap();
        let output = format!("{:?}", terminal.backend().buffer());
        assert_eq!(output.matches("bob").count(), 3);
        assert!(output.contains("line 2"));
    }

    #[test]
    fn failed_sends_remain_visible_while_a_new_draft_is_being_written() {
        let mut app = App::new("alice".into(), Some("dr5rs".into()));
        app.input = tui_input::Input::new("new draft".into());
        for text in ["first lost message", "second lost message"] {
            app.apply(Update::SendFailed {
                room: Room::Internet("dr5rs".into()),
                text: text.into(),
                reason: "relay rejected".into(),
            });
        }
        let mut terminal = Terminal::new(TestBackend::new(120, 36)).unwrap();
        terminal.draw(|frame| render(frame, &app)).unwrap();
        let output = format!("{:?}", terminal.backend().buffer());
        assert!(output.contains("first lost message"));
        assert!(output.contains("second lost message"));
        assert_eq!(app.input.value(), "new draft");
    }

    #[test]
    fn failed_sends_survive_leaving_and_rejoining_their_room() {
        let mut app = App::new("alice".into(), Some("dr5rs".into()));
        app.input = tui_input::Input::new("new draft".into());
        app.apply(Update::SendFailed {
            room: Room::Internet("dr5rs".into()),
            text: "room-specific unsent message".into(),
            reason: "offline".into(),
        });
        app.join_channel("u4pru".into());
        let mut terminal = Terminal::new(TestBackend::new(120, 36)).unwrap();
        terminal.draw(|frame| render(frame, &app)).unwrap();
        assert!(
            !format!("{:?}", terminal.backend().buffer()).contains("room-specific unsent message")
        );
        app.join_channel("dr5rs".into());
        terminal.draw(|frame| render(frame, &app)).unwrap();
        assert!(
            format!("{:?}", terminal.backend().buffer()).contains("room-specific unsent message")
        );
    }

    #[test]
    fn failed_sends_do_not_hide_later_received_messages() {
        let mut app = App::new("alice".into(), Some("dr5rs".into()));
        for i in 0..40 {
            app.apply(Update::SendFailed {
                room: app.room.clone(),
                text: format!("unsent {i}"),
                reason: "offline".into(),
            });
        }
        app.apply(Update::Message(Message {
            id: "newest".into(),
            room: app.room.clone(),
            author: "peer".into(),
            nickname: "bob".into(),
            text: "latest received message".into(),
            timestamp_ms: u64::MAX,
            mine: false,
        }));
        let mut terminal = Terminal::new(TestBackend::new(120, 36)).unwrap();
        terminal.draw(|frame| render(frame, &app)).unwrap();
        assert!(format!("{:?}", terminal.backend().buffer()).contains("latest received message"));
    }

    #[test]
    fn clearing_history_removes_failed_send_notice() {
        let mut app = App::new("alice".into(), Some("dr5rs".into()));
        app.apply(Update::SendFailed {
            room: app.room.clone(),
            text: "cleared failed message".into(),
            reason: "rejected".into(),
        });
        app.input = tui_input::Input::new("/clear".into());
        app.handle_event(crossterm::event::Event::Key(
            crossterm::event::KeyEvent::new(
                crossterm::event::KeyCode::Enter,
                crossterm::event::KeyModifiers::NONE,
            ),
        ));
        let mut terminal = Terminal::new(TestBackend::new(120, 36)).unwrap();
        terminal.draw(|frame| render(frame, &app)).unwrap();
        assert!(!format!("{:?}", terminal.backend().buffer()).contains("cleared failed message"));
    }
}
