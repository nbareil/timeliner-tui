//! Render deterministic, synthetic terminal previews without opening an investigation.
use chrono::TimeZone;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{
    Terminal,
    backend::TestBackend,
    style::{Color, Modifier},
};
use serde_json::json;
use timeliner_tui::{
    model::{Bookmark, DisplayRow, EventKey, Row, Tag, View},
    theme::Theme,
    ui::App,
    worker::Message,
};

fn rgb(color: Color) -> [u8; 3] {
    match color {
        Color::Rgb(r, g, b) => [r, g, b],
        _ => panic!("Preview contains an implicit terminal color: {color:?}"),
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut app = App::default();
    app.ready = true;
    app.status = "Ready · 4 collections indexed".into();
    if args.first().is_some_and(|a| a == "dark") {
        app.session.theme = Theme::Dark;
    }
    let begin = chrono_tz::UTC
        .with_ymd_and_hms(2026, 9, 8, 9, 10, 0)
        .unwrap()
        .timestamp();
    app.session.views = ["All activity", "IOC review", "Authentication", "Tagged"]
        .into_iter()
        .map(|name| View {
            name: name.into(),
            after: Some(begin - 3600),
            before: Some(begin + 86400),
            ..View::default()
        })
        .collect();
    app.session.views[0].selected = 7;
    let paths = [
        "/etc/ssh/sshd_config",
        "/var/log/auth.log",
        "/usr/sbin/sshd",
        "/home/deploy/.ssh/authorized_keys",
        "/var/log/nginx/access.log",
        "/etc/systemd/system/backup.service",
        "/var/spool/cron/crontabs/root",
        "/tmp/.cache/systemd-netd",
        "/usr/local/bin/healthcheck",
        "/var/log/audit/audit.log",
        "/etc/passwd",
        "/etc/sudoers.d/deploy",
        "/opt/application/config/production.yaml",
        "/var/lib/postgresql/data/pg_hba.conf",
        "/home/deploy/.bash_history",
        "/etc/cron.d/daily-maintenance",
        "/run/user/1000/session.lock",
        "/var/log/journal/system.journal",
        "/etc/resolv.conf",
        "/usr/lib/systemd/system/ssh.service",
        "/var/www/html/index.html",
        "/opt/application/logs/worker.log",
        "/usr/bin/curl",
        "/etc/ssl/certs/ca-certificates.crt",
        "/home/deploy/.config/environment",
        "/var/log/syslog",
        "/opt/application/releases/current",
        "/etc/hosts",
    ];
    app.rows = paths
        .into_iter()
        .enumerate()
        .map(|(i, path)| {
            DisplayRow::Event(Row {
                key: EventKey {
                    epoch: begin + i as i64 * 37,
                    record: i as i64 + 1,
                },
                macb: [1, 2, 5, 12, 15][i % 5],
                host: ["web-01", "db-01", "edge-02", "app-04"][i % 4].into(),
                path: path.into(),
                tag: match i {
                    3 | 14 => Tag::Suspicious,
                    5 | 15 => Tag::Ioc,
                    7 => Tag::Malicious,
                    _ => Tag::None,
                },
                automatic: i == 5 || i == 15,
                ioc_match: matches!(i, 5 | 7 | 15),
            })
        })
        .collect();
    app.session.bookmarks = vec![
        Bookmark {
            name: 'a',
            key: app.rows[3].key(),
        },
        Bookmark {
            name: 'b',
            key: app.rows[7].key(),
        },
    ];
    match args.get(1).map(String::as_str) {
        Some("uac-details") => {
            app.message(Message::Detail(concat!(
                "Host: app-04\nSource #4: /case/uac-app-04/bodyfile/bodyfile.txt\n",
                "Line: 18423  Byte offset: 2791240\nTag: MALICIOUS (manual)\nIOC rules: [3]\n\n",
                "Hash / original first field: 0\nPath: /tmp/.cache/systemd-netd\n",
                "Inode: 39182\nMode: -rwxr-xr-x\nUID: 0\nGID: 0\nSize: 48.22 KB\n",
                "Access time: 2026-09-08T09:14:19+00:00\n",
                "Modification time: 2026-09-08T09:14:19+00:00\n",
                "Change time: 2026-09-08T09:14:22+00:00\n",
                "Birth time: 2026-09-08T09:14:17+00:00\n",
                "UAC MD5: 0123456789abcdef0123456789abcdef\n",
                "UAC MD5 source: /case/uac-app-04/hash_executables/hash_executables.md5 (line 42)\n",
                "UAC SHA-1: 0123456789abcdef0123456789abcdef01234567\n",
                "UAC SHA-1 source: /case/uac-app-04/hash_executables/hash_executables.sha1 (line 42)\n\n",
                "Original record (display escaped; export preserves bytes):\n",
                "0|/tmp/.cache/systemd-netd|39182|-rwxr-xr-x|0|0|48216|1788858859|1788858859|1788858862|1788858857"
            ).into()));
        }
        Some("goto") => {
            app.key(KeyEvent::new(KeyCode::Char('g'), KeyModifiers::NONE));
            for character in "2026-09-08T09:14:19Z".chars() {
                app.key(KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE));
            }
        }
        Some("selection") => {
            for selected in [3, 5, 7] {
                app.session.views[0].selected = selected;
                app.key(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE));
            }
        }
        Some("details") => {
            app.message(Message::Detail(
                "Host: app-04\nSource #4: /case/uac-app-04/bodyfile/bodyfile.txt\nLine: 18423  Byte offset: 2791240\nTag: MALICIOUS (manual)\nIOC rules: [3]\n\nHash / original first field: 0\nPath: /tmp/.cache/systemd-netd\nInode: 39182\nMode: -rwxr-xr-x\nUID: 0\nGID: 0\nSize: 48.22 KB\nAccess time: 2026-09-08T09:14:19+00:00\nModification time: 2026-09-08T09:14:19+00:00\nChange time: 2026-09-08T09:14:22+00:00\nBirth time: 2026-09-08T09:14:17+00:00\n\nOriginal record (display escaped; export preserves bytes):\n0|/tmp/.cache/systemd-netd|39182|-rwxr-xr-x|0|0|48216|1788858859|1788858859|1788858862|1788858857".into()
            ));
        }
        Some("filters") => {
            app.key(KeyEvent::new(KeyCode::F(5), KeyModifiers::NONE));
        }
        Some("directory") => {
            app.key(KeyEvent::new(KeyCode::Char('X'), KeyModifiers::SHIFT));
        }
        Some("tags") => {
            app.key(KeyEvent::new(KeyCode::F(2), KeyModifiers::NONE));
        }
        Some("views") => {
            app.key(KeyEvent::new(KeyCode::F(7), KeyModifiers::NONE));
        }
        Some("delete-bookmark") => {
            app.key(KeyEvent::new(KeyCode::F(8), KeyModifiers::NONE));
            app.key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::NONE));
        }
        _ => {}
    }
    let width = 144;
    let height = 36;
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal.draw(|frame| app.draw(frame)).unwrap();
    let cells = terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(|cell| {
            json!({
                "text": cell.symbol(),
                "fg": rgb(cell.fg),
                "bg": rgb(cell.bg),
                "bold": cell.modifier.contains(Modifier::BOLD),
            })
        })
        .collect::<Vec<_>>();
    println!(
        "{}",
        json!({ "width": width, "height": height, "cells": cells })
    );
}
