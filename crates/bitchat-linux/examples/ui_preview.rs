//! Development tool: renders real Ratatui UI states into one HTML gallery.
//!
//!     cargo run -p bitchat-linux --example ui_preview -- [output-dir]
//!
//! Default output directory: /tmp/opencode/bitchat-ui-preview

use std::fmt::Write as _;
use std::fs;
use std::path::PathBuf;

use bitchat_linux::app::App;
use bitchat_linux::types::{Message, Notice, Room, Update};
use bitchat_linux::ui;
use bitchatd::PeerView;
use chrono::{Local, TimeZone};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::style::{Color, Modifier};
use tui_input::Input;

const ME: &str = "f65148aee0859c8f";
const PHONE: &str = "f098bef8336569d6";
const OTHER: &str = "9c21ab77e34a10bd";

fn ts_on(day: u32, hour: u32, minute: u32) -> u64 {
    Local
        .with_ymd_and_hms(2026, 10, day, hour, minute, 0)
        .single()
        .expect("preview time is unambiguous")
        .timestamp_millis() as u64
}

fn ts(hour: u32, minute: u32) -> u64 {
    ts_on(8, hour, minute)
}

fn message(room: &Room, author: &str, nickname: &str, text: &str, at: u64, mine: bool) -> Message {
    Message {
        id: format!("{author}:{at}:{text}"),
        room: room.clone(),
        author: author.into(),
        nickname: nickname.into(),
        text: text.into(),
        timestamp_ms: at,
        mine,
    }
}

fn peer(id: &str, nickname: &str, fingerprint: &str, direct: bool, last_seen: u64) -> PeerView {
    PeerView {
        id: id.into(),
        nickname: nickname.into(),
        fingerprint: fingerprint.into(),
        direct,
        last_seen,
    }
}

struct Frame {
    name: &'static str,
    caption: &'static str,
    width: u16,
    height: u16,
    app: App,
}

fn his_history(app: &mut App) {
    let room = Room::Mesh;
    for (author, nickname, text, at, mine) in [
        (ME, "anonf651", "hi", ts_on(1, 11, 41), true),
        (PHONE, "j", "hoii", ts_on(1, 11, 41), false),
        (ME, "anonf651", "#mesh", ts_on(1, 11, 41), true),
        (ME, "anonf651", "helo", ts_on(1, 13, 14), true),
        (ME, "anonf651", "#mesh", ts(17, 38), true),
    ] {
        app.apply(Update::Message(message(
            &room, author, nickname, text, at, mine,
        )));
    }
}

fn scenarios() -> Vec<Frame> {
    let mut frames = Vec::new();

    let mut app = App::new("anonf651".into(), None);
    app.mesh_status = "searching".into();
    frames.push(Frame {
        name: "mesh · fresh start",
        caption: "Fresh start with no history.",
        width: 110,
        height: 30,
        app,
    });

    let mut app = App::new("anonf651".into(), None);
    app.mesh_status = "searching".into();
    his_history(&mut app);
    app.apply(Update::Notice(Notice::in_room(
        Room::Mesh,
        "Saved locally · no connected peers",
    )));
    frames.push(Frame {
        name: "mesh · history, no peers",
        caption: "Real history (anonf651 / j, Oct 1) plus today's line, restored from disk.",
        width: 110,
        height: 30,
        app,
    });

    let mut app = App::new("anonf651".into(), None);
    app.mesh_status = "2 links".into();
    app.peers = vec![
        peer(PHONE, "j", "4aeb183993009a44", true, ts(17, 41)),
        peer(OTHER, "luna", "77c1aa90d2e39f15", false, ts(17, 40)),
    ];
    let room = Room::Mesh;
    for (author, nickname, text, at, mine) in [
        (PHONE, "j", "hoii", ts(17, 35), false),
        (
            ME,
            "anonf651",
            "hey! at the laptop now 🦀",
            ts(17, 36),
            true,
        ),
        (ME, "anonf651", "can you read me?", ts(17, 36), true),
        (
            OTHER,
            "luna",
            "loud and clear\nthis second line is long enough to wrap around the chat panel edge",
            ts(17, 37),
            false,
        ),
        (PHONE, "j", "nice", ts(17, 38), false),
    ] {
        app.apply(Update::Message(message(
            &room, author, nickname, text, at, mine,
        )));
    }
    frames.push(Frame {
        name: "mesh · linked",
        caption: "Two peers linked; multiline text wraps.",
        width: 110,
        height: 30,
        app,
    });

    let mut app = App::new("anonf651".into(), None);
    app.mesh_status = "searching".into();
    app.apply(Update::Message(message(
        &Room::Mesh,
        ME,
        "anonf651",
        "hi",
        ts(17, 30),
        true,
    )));
    app.apply(Update::SendFailed {
        room: Room::Mesh,
        text: "are you there?".into(),
        reason: "Bluetooth unavailable".into(),
    });
    frames.push(Frame {
        name: "mesh · failed send",
        caption: "The failure stays in the conversation and the text returns to the composer.",
        width: 110,
        height: 30,
        app,
    });

    let mut app = App::new("anonf651".into(), None);
    app.mesh_status = "adapter off".into();
    app.apply(Update::Notice(
        "Bluetooth adapter off: rfkill unblock bluetooth; bluetoothctl power on".into(),
    ));
    frames.push(Frame {
        name: "mesh · adapter off",
        caption: "Adapter off: the fix sits in the footer.",
        width: 110,
        height: 30,
        app,
    });

    let mut app = App::new("alice".into(), Some("dr5rs".into()));
    app.mesh_status = "searching".into();
    app.internet_status = "connected".into();
    app.internet_connected = 3;
    let room = Room::Internet("dr5rs".into());
    for (author, nickname, text, at, mine) in [
        (PHONE, "j", "anyone around?", ts(16, 2), false),
        (ME, "alice", "yep", ts(16, 3), true),
        (OTHER, "luna", "same geohash, nice", ts(16, 4), false),
    ] {
        app.apply(Update::Message(message(
            &room, author, nickname, text, at, mine,
        )));
    }
    frames.push(Frame {
        name: "internet · joined",
        caption: "Location channel #dr5rs connected.",
        width: 110,
        height: 30,
        app,
    });

    let mut app = App::new("anonf651".into(), None);
    app.mesh_status = "searching".into();
    frames.push(Frame {
        name: "internet · not joined",
        caption: "No channel: the room list offers '/join <geohash>' in place of a room.",
        width: 80,
        height: 24,
        app,
    });

    let mut app = App::new("anonf651".into(), None);
    app.mesh_status = "2 links".into();
    for i in 0..14 {
        app.apply(Update::Message(message(
            &Room::Mesh,
            PHONE,
            "j",
            &format!("message number {i} with a bit of text"),
            ts(16, i),
            false,
        )));
    }
    app.scroll = 8;
    frames.push(Frame {
        name: "mesh · scrolled back",
        caption: "Reading older lines.",
        width: 110,
        height: 30,
        app,
    });

    let mut app = App::new("anonf651".into(), None);
    app.mesh_status = "searching".into();
    app.input = Input::new("/".into());
    frames.push(Frame {
        name: "mesh · typing a command",
        caption: "A command being typed.",
        width: 110,
        height: 30,
        app,
    });

    let mut app = App::new("anonf651".into(), None);
    app.mesh_status = "searching".into();
    his_history(&mut app);
    frames.push(Frame {
        name: "mesh · narrow terminal",
        caption: "Below 52 columns the sidebar drops and chat takes the width.",
        width: 64,
        height: 20,
        app,
    });

    let mut app = App::new("anonf651".into(), None);
    app.mesh_status = "searching".into();
    app.help = true;
    frames.push(Frame {
        name: "help overlay",
        caption: "F1 help, open over the mesh room.",
        width: 110,
        height: 30,
        app,
    });

    frames
}

fn css(color: Color) -> String {
    match color {
        Color::Reset => "#d0d0d8".into(),
        Color::Black => "#1c1c22".into(),
        Color::Red => "#e05252".into(),
        Color::Green => "#84c46a".into(),
        Color::Yellow => "#e6c56d".into(),
        Color::Blue => "#6f9fdf".into(),
        Color::Magenta => "#c98bdb".into(),
        Color::Cyan => "#5fbcbe".into(),
        Color::Gray => "#a8a8b3".into(),
        Color::DarkGray => "#6c6c78".into(),
        Color::LightRed => "#ff6b6b".into(),
        Color::LightGreen => "#a5e08a".into(),
        Color::LightYellow => "#ffd98a".into(),
        Color::LightBlue => "#8ab8ff".into(),
        Color::LightMagenta => "#d9a8ff".into(),
        Color::LightCyan => "#8ae8ef".into(),
        Color::White => "#f2f2f5".into(),
        Color::Rgb(r, g, b) => format!("#{r:02x}{g:02x}{b:02x}"),
        Color::Indexed(index) => indexed_css(index),
    }
}

fn indexed_css(index: u8) -> String {
    match index {
        0 => "#1c1c22".into(),
        1 => "#e05252".into(),
        2 => "#84c46a".into(),
        3 => "#e6c56d".into(),
        4 => "#6f9fdf".into(),
        5 => "#c98bdb".into(),
        6 => "#5fbcbe".into(),
        7 => "#a8a8b3".into(),
        8 => "#6c6c78".into(),
        9 => "#ff6b6b".into(),
        10 => "#a5e08a".into(),
        11 => "#ffd98a".into(),
        12 => "#8ab8ff".into(),
        13 => "#d9a8ff".into(),
        14 => "#8ae8ef".into(),
        15 => "#f2f2f5".into(),
        232..=255 => {
            let step = 8 + u16::from(index - 232) * 10;
            format!("rgb({step},{step},{step})")
        }
        other => {
            let n = u16::from(other) - 16;
            let channel = |v: u16| if v == 0 { 0 } else { 55 + v * 40 };
            format!(
                "rgb({},{},{})",
                channel(n / 36),
                channel((n % 36) / 6),
                channel(n % 6)
            )
        }
    }
}

fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn buffer_html(buffer: &Buffer) -> String {
    let area = buffer.area();
    let mut html = String::new();
    for y in area.top()..area.bottom() {
        html.push_str("<div class=\"r\">");
        for x in area.left()..area.right() {
            let index = (y - area.y) as usize * area.width as usize + (x - area.x) as usize;
            let cell = &buffer.content()[index];
            let (fg, bg) = if cell.modifier.contains(Modifier::REVERSED) {
                (
                    if cell.bg == Color::Reset {
                        "#0e0e12".to_owned()
                    } else {
                        css(cell.bg)
                    },
                    if cell.fg == Color::Reset {
                        "#d0d0d8".to_owned()
                    } else {
                        css(cell.fg)
                    },
                )
            } else {
                (
                    if cell.fg == Color::Reset {
                        String::new()
                    } else {
                        css(cell.fg)
                    },
                    if cell.bg == Color::Reset {
                        String::new()
                    } else {
                        css(cell.bg)
                    },
                )
            };
            let mut style = String::new();
            if !fg.is_empty() {
                write!(style, "color:{fg};").unwrap();
            }
            if !bg.is_empty() {
                write!(style, "background:{bg};").unwrap();
            }
            if cell.modifier.contains(Modifier::BOLD) {
                style.push_str("font-weight:700;");
            }
            if cell.modifier.contains(Modifier::DIM) {
                style.push_str("opacity:.65;");
            }
            if cell.modifier.contains(Modifier::ITALIC) {
                style.push_str("font-style:italic;");
            }
            if cell.modifier.contains(Modifier::UNDERLINED) {
                style.push_str("text-decoration:underline;");
            }
            write!(
                html,
                "<span style=\"{style}\">{}</span>",
                escape(cell.symbol())
            )
            .unwrap();
        }
        html.push_str("</div>");
    }
    html
}

const STYLE: &str = r#"
:root { color-scheme: dark; }
* { box-sizing: border-box; }
body { margin: 0; padding: 28px 32px 48px; background: #0a0a0e; color: #c9c9d1;
       font: 15px/1.5 system-ui, -apple-system, "Segoe UI", sans-serif; }
h1 { font-size: 20px; margin: 0 0 4px; color: #f2f2f5; }
.lede { margin: 0 0 22px; color: #8f8f9c; max-width: 980px; }
.lede code { color: #b8b8c4; }
main { display: grid; gap: 22px; grid-template-columns: repeat(auto-fit, minmax(800px, 1fr)); }
.card { background: #111117; border: 1px solid #23232c; border-radius: 12px; padding: 16px 18px 18px; }
.head { display: flex; align-items: baseline; justify-content: space-between; gap: 12px; }
h2 { font-size: 15px; margin: 0; color: #e8e8ee; font-weight: 600; }
.dims { color: #70707c; font: 12px ui-monospace, monospace; }
.caption { margin: 4px 0 12px; color: #8f8f9c; font-size: 13px; }
.term { margin: 0; padding: 12px 14px; background: #0e0e12; border: 1px solid #1d1d24;
        border-radius: 8px; overflow-x: auto; color: #d0d0d8;
        font: 13px/1.25 ui-monospace, "JetBrains Mono", "Cascadia Mono", Menlo, Consolas, monospace;
        white-space: pre; tab-size: 4; }
.term .r { display: block; min-height: 16px; }
"#;

fn main() {
    let out = PathBuf::from(
        std::env::args()
            .nth(1)
            .unwrap_or_else(|| "/tmp/opencode/bitchat-ui-preview".into()),
    );
    fs::create_dir_all(&out).expect("create output directory");
    let mut cards = String::new();
    for (index, frame) in scenarios().into_iter().enumerate() {
        let mut terminal =
            Terminal::new(TestBackend::new(frame.width, frame.height)).expect("terminal backend");
        terminal
            .draw(|f| ui::render(f, &frame.app))
            .expect("frame renders");
        write!(
            cards,
            "<section class=\"card\"><div class=\"head\"><h2>{:02} · {}</h2><span class=\"dims\">{}×{}</span></div><p class=\"caption\">{}</p><pre class=\"term\">{}</pre></section>",
            index + 1,
            escape(frame.name),
            frame.width,
            frame.height,
            escape(frame.caption),
            buffer_html(terminal.backend().buffer()),
        )
        .unwrap();
    }
    let html = format!(
        "<!doctype html><html><head><meta charset=\"utf-8\"><title>bitchat-linux · UI states</title><style>{STYLE}</style></head><body><header><h1>bitchat-linux · UI states</h1><p class=\"lede\">Rendered from the working tree's <code>ui.rs</code> through Ratatui's TestBackend: every cell is the buffer the app really paints at that size. The text cursor is not drawn. Ticket GURI-1903 (Design-pass and improve the client UI/UX).</p></header><main>{cards}</main></body></html>"
    );
    let path = out.join("index.html");
    fs::write(&path, html).expect("write gallery");
    println!("{}", path.display());
}
