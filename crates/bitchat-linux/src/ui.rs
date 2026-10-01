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
    types::{APP_NAME, Room},
};

const HELP: &[&str] = &[
    "Public chat (unencrypted)",
    "Enter       Send a message",
    "Tab         Switch between mesh and Internet",
    "PgUp/PgDn   Scroll chat history",
    "Esc         Clear draft / close help",
    "/join <geohash>    Join a location channel",
    "/mesh              Open Bluetooth mesh chat",
    "/internet          Open joined location channel",
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
            Constraint::Length(3),
            Constraint::Length(2),
        ])
        .split(area);

    let header = Text::from(vec![
        Line::from(vec![
            Span::styled(APP_NAME, Style::default().add_modifier(Modifier::BOLD)),
            Span::raw(format!(
                "  ·  {}  ·  public (unencrypted)",
                label(&app.nickname)
            )),
        ]),
        Line::from(format!("Mesh: {}", label(&app.mesh_status))),
        Line::from(format!("Internet: {}", label(&app.internet_status))),
    ]);
    frame.render_widget(
        Paragraph::new(header).block(Block::default().borders(Borders::BOTTOM)),
        sections[0],
    );

    let sidebar_width = if sections[1].width >= 52 { 23 } else { 0 };
    let columns = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(sidebar_width), Constraint::Min(0)])
        .split(sections[1]);
    if sidebar_width > 0 {
        let mut lines = vec![
            Line::from("Rooms"),
            room_line("#mesh", app.room == Room::Mesh),
        ];
        if let Some(hash) = &app.geohash {
            lines.push(room_line(
                &format!("#{hash}"),
                app.room == Room::Internet(hash.clone()),
            ));
        } else {
            lines.push(room_line("/join <geohash>", false));
        }
        lines.push(Line::from(""));
        lines.push(Line::from("Nearby"));
        if app.peers.is_empty() {
            lines.push(Line::from("No peers"));
        }
        for peer in &app.peers {
            let id = label(&peer.id);
            let suffix: String = id
                .chars()
                .rev()
                .take(6)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect();
            lines.push(Line::from(format!("{} · {suffix}", label(&peer.nickname))));
        }
        frame.render_widget(
            Paragraph::new(lines).block(Block::default().borders(Borders::RIGHT)),
            columns[0],
        );
    }

    let chat_area = columns[1];
    if chat_area.width > 0 && chat_area.height > 0 {
        let mut lines = Vec::new();
        for message in app.messages() {
            let author = label(&message.author);
            let suffix: String = author
                .chars()
                .rev()
                .take(6)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect();
            let name = bitchatd::sanitize_nickname(&message.nickname)
                .unwrap_or_else(|| label(&message.nickname));
            lines.push(Line::from(vec![
                Span::styled(
                    timestamp(message.timestamp_ms),
                    Style::default().fg(Color::DarkGray),
                ),
                Span::styled(
                    format!(" {name} · {suffix}"),
                    Style::default()
                        .fg(if message.mine {
                            Color::Cyan
                        } else {
                            Color::Yellow
                        })
                        .add_modifier(Modifier::BOLD),
                ),
            ]));
            for line in bitchatd::clean_message(&message.text).split('\n') {
                lines.push(Line::from(format!("  {line}")));
            }
        }
        frame.render_widget(
            conversation(lines, chat_area.width, chat_area.height, app.scroll),
            chat_area,
        );
    }

    let composer = Paragraph::new(label(app.input.value()))
        .scroll((
            0,
            app.input
                .visual_scroll(sections[2].width.saturating_sub(4) as usize)
                .min(u16::MAX as usize) as u16,
        ))
        .block(
            Block::default()
                .title(format!("{} · message / command", label(&app.room.label())))
                .borders(Borders::ALL),
        );
    frame.render_widget(composer, sections[2]);
    if sections[2].width > 2 && sections[2].height > 2 {
        let cursor = app.input.visual_cursor().saturating_sub(
            app.input
                .visual_scroll(sections[2].width.saturating_sub(4) as usize),
        );
        frame.set_cursor_position((
            sections[2]
                .x
                .saturating_add(1)
                .saturating_add(cursor.min(sections[2].width.saturating_sub(2) as usize) as u16),
            sections[2].y.saturating_add(1),
        ));
    }

    let footer = if let Some(notice) = &app.notice {
        format!("{}  ·  F1 help  Tab rooms  Ctrl-C quit", label(notice))
    } else {
        "Public chat · Enter send  Tab rooms  PgUp/PgDn scroll  F1 help  Ctrl-C quit".to_owned()
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
        Style::default().add_modifier(if active {
            Modifier::BOLD
        } else {
            Modifier::empty()
        }),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{Message, Update};
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
        let mut terminal = Terminal::new(TestBackend::new(25, 12)).unwrap();
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
}
