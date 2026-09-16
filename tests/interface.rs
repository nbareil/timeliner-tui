use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{Terminal, backend::TestBackend, style::Color};
use timeliner_tui::{
    model::{DisplayRow, EventKey, Row, Tag, View},
    ui::{Action, App},
    worker::Command,
};

fn key(app: &mut App, code: KeyCode) -> Vec<Action> {
    app.key(KeyEvent::new(code, KeyModifiers::NONE))
}
fn app() -> App {
    let mut app = App::default();
    app.ready = true;
    app.height = 10;
    app.rows = (0..100)
        .map(|id| {
            DisplayRow::Event(Row {
                key: EventKey {
                    epoch: 1_600_000_000 + id,
                    record: id,
                },
                macb: 15,
                host: "host-a".into(),
                path: format!("/tmp/file-{id}"),
                tag: Tag::None,
                automatic: false,
                ioc_match: false,
            })
        })
        .collect();
    app
}
#[test]
fn arrows_keep_the_same_event_selected_and_jk_scroll_only_at_edges() {
    let mut app = app();
    for _ in 0..5 {
        key(&mut app, KeyCode::Char('j'));
    }
    assert_eq!(app.session.views[0].selected, 5);
    assert_eq!(app.top, 0);
    key(&mut app, KeyCode::Down);
    assert_eq!(app.top, 1);
    assert_eq!(app.session.views[0].selected, 4);
    assert_eq!(
        app.rows[app.top + app.session.views[0].selected]
            .key()
            .record,
        5
    );
    for _ in 0..20 {
        key(&mut app, KeyCode::Char('j'));
    }
    assert_eq!(app.session.views[0].selected, 9);
    assert_eq!(app.top, 16);
    assert_eq!(app.rows[app.top + 9].key().record, 25);
    for _ in 0..20 {
        key(&mut app, KeyCode::Char('k'));
    }
    assert_eq!(app.session.views[0].selected, 0);
    assert_eq!(app.top, 5);
    assert_eq!(app.rows[app.top].key().record, 5);
}
#[test]
fn tab_tags_record_and_enter_requests_details() {
    let mut app = app();
    let actions = key(&mut app, KeyCode::Tab);
    assert!(matches!(
        actions.as_slice(),
        [Action::Command(Command::Tag(0, Some(Tag::Suspicious)))]
    ));
    assert_eq!(app.rows[0].event().unwrap().tag, Tag::Suspicious);
    app.session.timezone = "America/New_York".into();
    for code in [KeyCode::Enter, KeyCode::F(3)] {
        let actions = key(&mut app, code);
        assert!(matches!(
            actions.as_slice(),
            [Action::Command(Command::Detail(
                0,
                chrono_tz::America::New_York
            ))]
        ));
    }
}

#[test]
fn q_saves_and_quits_while_f10_no_longer_quits() {
    let mut app = app();
    assert!(key(&mut app, KeyCode::F(10)).is_empty());
    assert!(matches!(
        key(&mut app, KeyCode::Char('q')).as_slice(),
        [Action::Command(Command::Save(_)), Action::Quit]
    ));
    key(&mut app, KeyCode::F(1));
    assert!(matches!(
        key(&mut app, KeyCode::Char('q')).as_slice(),
        [Action::Command(Command::Save(_)), Action::Quit]
    ));
    let mut opening = App::default();
    assert!(matches!(
        key(&mut opening, KeyCode::Char('q')).as_slice(),
        [Action::Quit]
    ));
}

#[test]
fn q_remains_available_in_forms_and_bookmark_commands() {
    let mut app = app();
    key(&mut app, KeyCode::Char('m'));
    assert!(key(&mut app, KeyCode::Char('q')).is_empty());
    assert_eq!(app.session.bookmarks[0].name, 'q');
    key(&mut app, KeyCode::Char('j'));
    key(&mut app, KeyCode::Char('\''));
    assert!(key(&mut app, KeyCode::Char('q')).is_empty());
    assert_eq!(
        app.rows[app.top + app.session.views[0].selected]
            .key()
            .record,
        0
    );
    key(&mut app, KeyCode::F(8));
    assert!(key(&mut app, KeyCode::Char('q')).is_empty());
    key(&mut app, KeyCode::F(5));
    assert!(key(&mut app, KeyCode::Char('q')).is_empty());
    let actions = key(&mut app, KeyCode::Enter);
    assert!(matches!(actions.as_slice(), [Action::Page(request)]
        if request.view.predicate.include == "q"));
    assert!(matches!(
        app.key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL))
            .as_slice(),
        [Action::Command(Command::Save(_)), Action::Quit]
    ));
}
#[test]
fn fork_copies_filters_but_smart_tab_is_global() {
    let mut app = app();
    app.session.views[0].after = Some(100);
    app.session.views[0].predicate.exclude.push("/tmp".into());
    key(&mut app, KeyCode::F(6));
    assert_eq!(app.session.views.len(), 3);
    assert_eq!(app.session.views[2].after, Some(100));
    app.session.views[2].predicate.exclude.push("other".into());
    assert_eq!(app.session.views[0].predicate.exclude.len(), 1);
    key(&mut app, KeyCode::F(7));
    key(&mut app, KeyCode::Char('3'));
    let view = &app.session.views[3];
    assert_eq!(view.after, None);
    assert!(view.predicate.exclude.is_empty());
    assert_eq!(view.predicate.tags, Some(vec![Tag::Ioc]));
}

#[test]
fn number_keys_select_tabs_and_ignore_current_or_missing_tabs() {
    let mut app = app();
    app.session.views.push(View {
        name: "Third".into(),
        ..View::default()
    });
    let original_rows = app.rows.clone();
    let original_top = original_rows[0].key();
    assert!(key(&mut app, KeyCode::Char('1')).is_empty());
    assert!(key(&mut app, KeyCode::Char('9')).is_empty());
    assert_eq!(app.session.active, 0);
    assert_eq!(app.rows.len(), original_rows.len());

    for (number, active) in [('2', 1), ('3', 2), ('1', 0)] {
        let actions = key(&mut app, KeyCode::Char(number));
        let [Action::Page(request)] = actions.as_slice() else {
            panic!("Selecting tab {number} must load its view");
        };
        assert_eq!(app.session.active, active);
        assert_eq!(request.view.name, app.session.views[active].name);
        if number == '2' {
            assert_eq!(request.view.predicate.tags, Some(vec![Tag::Ioc]));
        }
        if number == '1' {
            assert_eq!(request.anchor, Some(original_top));
        }
        app.message(timeliner_tui::worker::Message::Page(
            request.generation,
            original_rows.clone(),
        ));
    }
}

#[test]
fn shifted_number_keys_select_tabs() {
    let mut app = app();
    app.session.views = (1..=9)
        .map(|number| View {
            name: format!("Tab {number}"),
            ..View::default()
        })
        .collect();
    for number in ['2', '3', '4', '5', '6', '7', '8', '9', '1'] {
        let actions = app.key(KeyEvent::new(KeyCode::Char(number), KeyModifiers::SHIFT));
        let [Action::Page(request)] = actions.as_slice() else {
            panic!("Shift+{number} must select its tab");
        };
        assert_eq!(app.session.active, number as usize - '1' as usize);
        assert_eq!(request.view.name, format!("Tab {number}"));
    }
    for modifiers in [
        KeyModifiers::CONTROL,
        KeyModifiers::ALT,
        KeyModifiers::CONTROL | KeyModifiers::SHIFT,
        KeyModifiers::ALT | KeyModifiers::SHIFT,
    ] {
        assert!(
            app.key(KeyEvent::new(KeyCode::Char('2'), modifiers))
                .is_empty()
        );
        assert_eq!(app.session.active, 0);
    }
}

#[test]
fn number_keys_keep_their_dialog_actions() {
    for (number, tag) in [
        ('1', Tag::Suspicious),
        ('2', Tag::Ioc),
        ('3', Tag::Malicious),
    ] {
        let mut app = app();
        key(&mut app, KeyCode::F(2));
        let actions = key(&mut app, KeyCode::Char(number));
        assert!(matches!(
            actions.as_slice(),
            [Action::Command(Command::Tag(0, Some(actual)))] if *actual == tag
        ));
        assert_eq!(app.session.active, 0);
    }
}

#[test]
fn tab_names_active_marker_and_position_remain_visible_when_switching() {
    let mut app = app();
    app.session.views = ["Timeline", "IOC", "Malicious"]
        .into_iter()
        .map(|name| View {
            name: name.into(),
            ..View::default()
        })
        .collect();
    let mut terminal = Terminal::new(TestBackend::new(120, 30)).unwrap();
    for active in 0..3 {
        terminal.draw(|frame| app.draw(frame)).unwrap();
        let buffer = terminal.backend().buffer();
        let tabs = (0..120)
            .map(|x| buffer[(x, 1)].symbol())
            .collect::<String>();
        let position = (0..120)
            .map(|x| buffer[(x, 0)].symbol())
            .collect::<String>();
        for title in ["1 Timeline", "2 IOC", "3 Malicious"] {
            assert!(tabs.contains(title), "{tabs}");
        }
        assert!(tabs.contains(&format!("● {}", active + 1)), "{tabs}");
        assert!(
            position.contains(&format!("Tab {}/3", active + 1)),
            "{position}"
        );
        assert!(position.contains("gt/gT switch"), "{position}");
        assert!(position.contains("1–9 select"), "{position}");
        let marker = (0..120).find(|&x| buffer[(x, 1)].symbol() == "●").unwrap();
        assert_eq!(buffer[(marker, 1)].bg, app.session.theme.palette().selected);
        assert_eq!(buffer[(marker, 1)].fg, app.session.theme.palette().accent);
        key(&mut app, KeyCode::Char('g'));
        key(&mut app, KeyCode::Char('t'));
    }
    assert_eq!(app.session.active, 0);
}

#[test]
fn overflowing_tabs_keep_the_active_tab_visible_and_signal_hidden_neighbors() {
    let mut app = app();
    app.session.views = (1..=12)
        .map(|i| View {
            name: format!("Host-{i}"),
            ..View::default()
        })
        .collect();
    let mut terminal = Terminal::new(TestBackend::new(40, 24)).unwrap();
    for (active, hidden_left, hidden_right) in
        [(0, false, true), (5, true, true), (11, true, false)]
    {
        app.session.active = active;
        terminal.draw(|frame| app.draw(frame)).unwrap();
        let buffer = terminal.backend().buffer();
        let tabs = (0..40).map(|x| buffer[(x, 1)].symbol()).collect::<String>();
        let position = (0..40).map(|x| buffer[(x, 0)].symbol()).collect::<String>();
        assert!(
            tabs.contains(&format!("● {} Host-{}", active + 1, active + 1)),
            "{tabs}"
        );
        assert_eq!(tabs.starts_with('<'), hidden_left, "{tabs}");
        assert_eq!(tabs.ends_with('>'), hidden_right, "{tabs}");
        assert!(
            position.contains(&format!("Tab {}/12", active + 1)),
            "{position}"
        );
    }
}

#[test]
fn long_unicode_tab_names_do_not_hide_the_active_tab_after_resize() {
    let mut app = app();
    app.session.views = [
        "very long previous view ".repeat(8),
        "調査🔎".repeat(20),
        "Next".into(),
    ]
    .into_iter()
    .map(|name| View {
        name,
        ..View::default()
    })
    .collect();
    app.session.active = 1;
    for width in [30, 60, 120] {
        let mut terminal = Terminal::new(TestBackend::new(width, 24)).unwrap();
        terminal.draw(|frame| app.draw(frame)).unwrap();
        let buffer = terminal.backend().buffer();
        let tabs = (0..width)
            .map(|x| buffer[(x, 1)].symbol())
            .collect::<String>();
        let position = (0..width)
            .map(|x| buffer[(x, 0)].symbol())
            .collect::<String>();
        // Wide characters occupy a second buffer cell whose symbol is a space.
        assert!(tabs.replace(' ', "").contains("●2調査"), "{tabs}");
        assert!(tabs.starts_with('<'), "{tabs}");
        assert!(tabs.ends_with('>'), "{tabs}");
        assert!(position.contains("Tab 2/3"), "{position}");
    }
}
#[test]
fn bookmarks_and_return_jump() {
    let mut app = app();
    key(&mut app, KeyCode::Char('m'));
    key(&mut app, KeyCode::Char('a'));
    key(&mut app, KeyCode::Char('j'));
    key(&mut app, KeyCode::Char('\''));
    key(&mut app, KeyCode::Char('a'));
    assert_eq!(app.session.views[0].selected, 0);
    assert_eq!(app.session.bookmarks[0].key.record, 0);
}
#[test]
fn delete_bookmark_shortcut_works_in_the_timeline_and_bookmark_dialog() {
    for in_dialog in [false, true] {
        let mut app = app();
        for name in ['a', 'b', 'd', 'z'] {
            key(&mut app, KeyCode::Char('m'));
            key(&mut app, KeyCode::Char(name));
        }
        if in_dialog {
            key(&mut app, KeyCode::F(8));
        }
        for name in ['a', 'd', 'z'] {
            key(&mut app, KeyCode::Char('d'));
            assert!(app.status.contains("Delete bookmark"));
            key(&mut app, KeyCode::Char(name));
            assert!(!app.session.bookmarks.iter().any(|mark| mark.name == name));
            assert!(app.session.bookmarks.iter().any(|mark| mark.name == 'b'));
            assert_eq!(app.status, format!("Mark {name} deleted"));
        }
        key(&mut app, KeyCode::Char('d'));
        key(&mut app, KeyCode::Char('a'));
        assert_eq!(app.status, "Mark a is not set");
        assert_eq!(app.session.bookmarks.len(), 1);
    }
}

#[test]
fn escape_cancels_bookmark_deletion_in_the_timeline_and_bookmark_dialog() {
    for in_dialog in [false, true] {
        let mut app = app();
        key(&mut app, KeyCode::Char('m'));
        key(&mut app, KeyCode::Char('a'));
        if in_dialog {
            key(&mut app, KeyCode::F(8));
        }
        key(&mut app, KeyCode::Char('d'));
        key(&mut app, KeyCode::Esc);
        key(&mut app, KeyCode::Char('a'));
        assert_eq!(app.session.bookmarks.len(), 1);
        key(&mut app, KeyCode::Char('d'));
        key(&mut app, KeyCode::Char('a'));
        assert!(app.session.bookmarks.is_empty());
    }
}

#[test]
fn bookmark_markers_follow_exact_events_and_update_after_reassignment_and_deletion() {
    for theme in [
        timeliner_tui::theme::Theme::Light,
        timeliner_tui::theme::Theme::Dark,
    ] {
        for width in [60, 80, 120] {
            let mut app = app();
            app.session.theme = theme;
            app.rows[1].event_mut().unwrap().key.record = app.rows[0].key().record;
            app.rows[2].event_mut().unwrap().key.epoch = app.rows[0].key().epoch;
            let mut terminal = Terminal::new(TestBackend::new(width, 24)).unwrap();
            key(&mut app, KeyCode::Char('m'));
            key(&mut app, KeyCode::Char('a'));
            terminal.draw(|frame| app.draw(frame)).unwrap();
            let buffer = terminal.backend().buffer();
            assert_eq!(buffer[(0, 5)].symbol(), "●");
            assert_eq!(buffer[(0, 5)].bg, theme.palette().selected);
            assert_eq!(buffer[(0, 6)].symbol(), " ");
            assert_eq!(buffer[(0, 7)].symbol(), " ");

            key(&mut app, KeyCode::Char('j'));
            terminal.draw(|frame| app.draw(frame)).unwrap();
            assert_eq!(terminal.backend().buffer()[(0, 5)].symbol(), "●");
            assert_eq!(terminal.backend().buffer()[(0, 6)].symbol(), "▌");
            for name in ['a', 'b'] {
                key(&mut app, KeyCode::Char('m'));
                key(&mut app, KeyCode::Char(name));
            }
            terminal.draw(|frame| app.draw(frame)).unwrap();
            assert_eq!(terminal.backend().buffer()[(0, 5)].symbol(), " ");
            assert_eq!(terminal.backend().buffer()[(0, 6)].symbol(), "●");
            for (name, marker) in [('a', "●"), ('b', "▌")] {
                key(&mut app, KeyCode::F(8));
                key(&mut app, KeyCode::Char('d'));
                key(&mut app, KeyCode::Char(name));
                key(&mut app, KeyCode::Esc);
                terminal.draw(|frame| app.draw(frame)).unwrap();
                assert_eq!(terminal.backend().buffer()[(0, 6)].symbol(), marker);
            }
        }
    }
}

#[test]
fn adding_an_exclusion_keeps_the_selected_event_when_earlier_rows_disappear() {
    let mut app = app();
    app.top = 20;
    app.session.views[0].selected = 8;
    // Equal timestamps must still preserve the exact source record.
    for row in &mut app.rows[20..30] {
        row.event_mut().unwrap().key.epoch = 1_600_000_020;
    }
    let focus = app.rows[28].key();
    let top = app.rows[20].key();
    let filtered = app.rows[20..]
        .iter()
        .filter(|row| ![21, 24, 27].contains(&row.key().record))
        .cloned()
        .collect();
    key(&mut app, KeyCode::F(5));
    key(&mut app, KeyCode::F(2));
    for character in "file-(21|24|27)$".chars() {
        key(&mut app, KeyCode::Char(character));
    }
    let actions = key(&mut app, KeyCode::Enter);
    let [Action::Page(request)] = actions.as_slice() else {
        panic!()
    };
    app.message(timeliner_tui::worker::Message::Page(
        request.generation,
        filtered,
    ));
    assert_eq!(app.rows[app.top].key(), top);
    assert_eq!(
        app.rows[app.top + app.session.views[0].selected].key(),
        focus
    );
}

#[test]
fn excluding_the_selected_event_moves_to_the_next_remaining_event() {
    let mut app = app();
    app.top = 20;
    app.session.views[0].selected = 8;
    for row in &mut app.rows[20..=28] {
        row.event_mut().unwrap().path = "/noise".into();
    }
    let next = app.rows[29].key();
    let filtered = app.rows[29..].to_vec();
    let actions = key(&mut app, KeyCode::Char('x'));
    let [Action::Page(request)] = actions.as_slice() else {
        panic!()
    };
    app.message(timeliner_tui::worker::Message::Page(
        request.generation,
        filtered,
    ));
    assert_eq!(
        app.rows[app.top + app.session.views[0].selected].key(),
        next
    );
}

#[test]
fn excluding_all_events_and_undoing_keeps_a_selection_below_the_top_row() {
    let mut app = app();
    app.top = 20;
    app.session.views[0].selected = 8;
    let focus = app.rows[28].key();
    let restored = app.rows[28..].to_vec();
    key(&mut app, KeyCode::Char('X'));
    let actions = key(&mut app, KeyCode::Enter);
    let [Action::Page(request)] = actions.as_slice() else {
        panic!()
    };
    let actions = app.message(timeliner_tui::worker::Message::Page(
        request.generation,
        vec![],
    ));
    let [Action::Page(request)] = actions.as_slice() else {
        panic!()
    };
    app.message(timeliner_tui::worker::Message::Page(
        request.generation,
        vec![],
    ));
    let actions = key(&mut app, KeyCode::Char('u'));
    let [Action::Page(request)] = actions.as_slice() else {
        panic!()
    };
    assert_eq!(request.anchor, Some(focus));
    app.message(timeliner_tui::worker::Message::Page(
        request.generation,
        restored,
    ));
    assert_eq!(
        app.rows[app.top + app.session.views[0].selected].key(),
        focus
    );
}

#[test]
fn removing_an_exclusion_seeks_the_selected_event_if_it_is_beyond_the_new_page() {
    let mut app = app();
    app.rows[21].event_mut().unwrap().key.epoch += 1000;
    app.top = 20;
    app.session.views[0].selected = 1;
    app.session.views[0].predicate.exclude = vec!["noise".into()];
    let focus = app.rows[21].key();
    let focused = app.rows[21].clone();
    let actions = key(&mut app, KeyCode::Char('u'));
    let [Action::Page(request)] = actions.as_slice() else {
        panic!()
    };
    let inserted = (0..request.limit)
        .map(|index| {
            let mut row = app.rows[20].clone();
            row.event_mut().unwrap().key.epoch += index as i64;
            row
        })
        .collect();
    let actions = app.message(timeliner_tui::worker::Message::Page(
        request.generation,
        inserted,
    ));
    let [Action::Page(request)] = actions.as_slice() else {
        panic!("Expected a seek to the event being inspected")
    };
    assert_eq!(request.anchor, Some(focus));
    assert!(!request.reverse);
    app.message(timeliner_tui::worker::Message::Page(
        request.generation,
        vec![focused],
    ));
    assert_eq!(
        app.rows[app.top + app.session.views[0].selected].key(),
        focus
    );
}

#[test]
fn exclusion_edits_compose_while_loading_and_undo_preserves_the_position() {
    let mut app = app();
    app.top = 20;
    app.session.views[0].after = Some(1_600_000_010);
    let anchor = app.rows[20].key();
    let actions = key(&mut app, KeyCode::Char('x'));
    let [Action::Page(first)] = actions.as_slice() else {
        panic!()
    };
    assert_eq!(first.anchor, Some(anchor));
    assert_eq!(first.view.predicate.exclude, ["^/tmp/file\\-20$"]);
    key(&mut app, KeyCode::Char('j'));
    let actions = key(&mut app, KeyCode::Char('x'));
    let [Action::Page(second)] = actions.as_slice() else {
        panic!()
    };
    assert_eq!(second.view.predicate.exclude.len(), 2);
    assert_eq!(second.anchor, Some(anchor));
    let actions = key(&mut app, KeyCode::Char('u'));
    let [Action::Page(undo)] = actions.as_slice() else {
        panic!()
    };
    assert_eq!(undo.view.predicate.exclude, first.view.predicate.exclude);
    assert_eq!(undo.view.after, first.view.after);
    assert_eq!(undo.anchor, Some(anchor));
    let actions = key(&mut app, KeyCode::Char('u'));
    let [Action::Page(clear)] = actions.as_slice() else {
        panic!()
    };
    assert!(clear.view.predicate.exclude.is_empty());
    assert_eq!(clear.anchor, Some(anchor));
}

#[test]
fn undoing_an_exclusion_after_no_matches_restores_the_previous_position() {
    let mut app = app();
    app.top = 20;
    let anchor = app.rows[20].key();
    key(&mut app, KeyCode::Char('X'));
    let actions = key(&mut app, KeyCode::Enter);
    let [Action::Page(forward)] = actions.as_slice() else {
        panic!()
    };
    let actions = app.message(timeliner_tui::worker::Message::Page(
        forward.generation,
        vec![],
    ));
    let [Action::Page(backward)] = actions.as_slice() else {
        panic!()
    };
    assert_eq!(backward.anchor, Some(anchor));
    app.message(timeliner_tui::worker::Message::Page(
        backward.generation,
        vec![],
    ));
    assert!(app.rows.is_empty());
    let actions = key(&mut app, KeyCode::Char('u'));
    let [Action::Page(undo)] = actions.as_slice() else {
        panic!()
    };
    assert_eq!(undo.anchor, Some(anchor));
    assert!(undo.view.predicate.exclude.is_empty());
}

#[test]
fn directory_exclusion_backspace_moves_by_components_and_stops_at_roots() {
    for (path, backspaces, directory, descendant, sibling) in [
        (
            "/var/log/audit/events",
            0,
            "/var/log/audit",
            "/var/log/audit/events",
            "/var/log/auditing/events",
        ),
        (
            "/var/log/audit/events",
            1,
            "/var/log",
            "/var/log/other/file",
            "/var/logger/file",
        ),
        (
            "/var/log/audit/events",
            8,
            "/",
            "/any/file",
            "relative/file",
        ),
        (
            r"C:\Users\Alice\AppData\file",
            1,
            r"C:\Users\Alice",
            r"C:\Users\Alice\other\file",
            r"C:\Users\Alicia\file",
        ),
        (
            r"C:\Users\Alice\file",
            8,
            "C:\\",
            r"C:\other\file",
            r"D:\other\file",
        ),
        (
            r"\\server\share\logs\app\file",
            8,
            r"\\server\share",
            r"\\server\share\file",
            r"\\server\share2\file",
        ),
        (
            "/tmp/a[1].d/file",
            0,
            "/tmp/a[1].d",
            "/tmp/a[1].d/child",
            "/tmp/a1xd/child",
        ),
        ("logs/app/file", 1, "logs", "logs/child", "logs-old/child"),
        ("logs/app/file", 8, ".", "other/file", "/absolute/file"),
    ] {
        let mut app = app();
        app.top = 20;
        app.session.views[0].selected = 4;
        app.rows[24].event_mut().unwrap().path = path.into();
        assert!(
            app.key(KeyEvent::new(KeyCode::Char('X'), KeyModifiers::SHIFT))
                .is_empty()
        );
        for _ in 0..backspaces {
            assert!(key(&mut app, KeyCode::Backspace).is_empty());
        }
        assert!(app.session.views[0].predicate.exclude.is_empty());
        let mut terminal = Terminal::new(TestBackend::new(120, 24)).unwrap();
        terminal.draw(|frame| app.draw(frame)).unwrap();
        let text = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(text.contains(directory), "{path}: {text}");
        let actions = key(&mut app, KeyCode::Enter);
        let [Action::Page(request)] = actions.as_slice() else {
            panic!("Expected directory exclusion for {path}")
        };
        assert_eq!(request.anchor, Some(app.rows[20].key()));
        let pattern = regex::Regex::new(&request.view.predicate.exclude[0]).unwrap();
        assert!(pattern.is_match(directory), "{path}: {pattern}");
        assert!(pattern.is_match(descendant), "{path}: {pattern}");
        assert!(!pattern.is_match(sibling), "{path}: {pattern}");
        if directory == "." {
            assert!(!pattern.is_match(r"C:\absolute"));
            assert!(!pattern.is_match(r"\\server\share\absolute"));
        }
    }
}

#[test]
fn directory_exclusion_can_be_edited_cancelled_and_combined_with_pending_filters() {
    let mut app = app();
    key(&mut app, KeyCode::Char('x'));
    key(&mut app, KeyCode::Char('X'));
    app.key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
    key(&mut app, KeyCode::Backspace);
    assert!(key(&mut app, KeyCode::Enter).is_empty());
    for character in "/custom/audit[z].".chars() {
        key(&mut app, KeyCode::Char(character));
    }
    key(&mut app, KeyCode::Delete);
    let actions = key(&mut app, KeyCode::Enter);
    let [Action::Page(request)] = actions.as_slice() else {
        panic!()
    };
    assert_eq!(request.view.predicate.exclude.len(), 2);
    assert_eq!(request.view.predicate.exclude[0], "^/tmp/file\\-0$");
    let pattern = regex::Regex::new(&request.view.predicate.exclude[1]).unwrap();
    assert!(pattern.is_match("/custom/audit[z]/file"));
    assert!(!pattern.is_match("/custom/auditz/file"));
    let rows = app.rows.clone();
    app.message(timeliner_tui::worker::Message::Page(
        request.generation,
        rows,
    ));
    let view = app.session.views[0].clone();
    key(&mut app, KeyCode::Char('X'));
    key(&mut app, KeyCode::Backspace);
    key(&mut app, KeyCode::Esc);
    assert_eq!(app.session.views[0].predicate, view.predicate);
    assert_eq!(app.session.views[0].top, view.top);
    assert_eq!(app.session.views[0].selected, view.selected);
}

#[test]
fn directory_editor_shows_the_end_of_long_paths() {
    let mut app = app();
    app.rows[0].event_mut().unwrap().path =
        format!("/{}répertoire-final/file", "long-directory/".repeat(20));
    key(&mut app, KeyCode::Char('X'));
    let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
    terminal.draw(|frame| app.draw(frame)).unwrap();
    let text = terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>();
    assert!(text.contains("répertoire-final"), "{text}");
    assert!(text.contains("Backspace parent directory"), "{text}");
}

#[test]
fn filter_form_removes_individual_exclusions_and_focuses_new_ones() {
    let mut app = app();
    app.top = 20;
    app.session.views[0].predicate.exclude = vec!["first".into(), "second".into()];
    let anchor = app.rows[20].key();
    key(&mut app, KeyCode::F(5));
    for _ in 0..4 {
        key(&mut app, KeyCode::Tab);
    }
    app.key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL));
    key(&mut app, KeyCode::F(2));
    for character in "third".chars() {
        key(&mut app, KeyCode::Char(character));
    }
    let actions = key(&mut app, KeyCode::Enter);
    let [Action::Page(request)] = actions.as_slice() else {
        panic!()
    };
    assert_eq!(request.view.predicate.exclude, ["second", "third"]);
    assert_eq!(request.anchor, Some(anchor));
}

#[test]
fn tag_is_right_aligned_and_keys_survive_standard_terminal_resize() {
    let mut app = app();
    app.rows[0].event_mut().unwrap().tag = Tag::Suspicious;
    for (width, height) in [(120, 30), (80, 24), (60, 12)] {
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| app.draw(frame)).unwrap();
        let buffer = terminal.backend().buffer();
        let row = (0..width)
            .map(|x| buffer[(x, 5)].symbol())
            .collect::<String>();
        assert!(row.ends_with("SUSPICIOUS"), "{width}: {row:?}");
        let text = buffer
            .content
            .iter()
            .map(|c| c.symbol())
            .collect::<String>();
        let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
        assert!(text.contains("F1 Help"));
        assert!(text.contains("q Quit"));
    }
}

#[test]
fn timeline_fills_the_available_height_with_distinct_rows() {
    let mut app = app();
    for (width, height) in [(120, 30), (80, 24), (209, 104)] {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|frame| app.draw(frame)).unwrap();
        let buffer = terminal.backend().buffer();
        let expected = height as usize - if width < 110 { 9 } else { 8 };
        assert_eq!(app.height, expected);
        for slot in 0..expected {
            let row = (0..width)
                .map(|x| buffer[(x, 5 + slot as u16)].symbol())
                .collect::<String>();
            assert!(
                row.contains(&format!("/tmp/file-{slot}")),
                "{slot}: {row:?}"
            );
        }
    }
}

#[test]
fn both_themes_keep_ordinary_rows_and_status_readable() {
    let mut app = app();
    for theme in [
        timeliner_tui::theme::Theme::Light,
        timeliner_tui::theme::Theme::Dark,
    ] {
        app.session.theme = theme;
        let mut terminal = Terminal::new(TestBackend::new(120, 30)).unwrap();
        terminal.draw(|frame| app.draw(frame)).unwrap();
        let buffer = terminal.backend().buffer();
        for (x, y) in [(2, 5), (2, 6), (22, 6), (47, 6), (4, 27), (5, 28)] {
            let cell = &buffer[(x, y)];
            assert!(
                contrast(cell.fg, cell.bg) >= 4.5,
                "{theme:?} at ({x}, {y}): {cell:?}"
            );
        }
    }
}

fn contrast(fg: Color, bg: Color) -> f64 {
    let luminance = |color| {
        let Color::Rgb(r, g, b) = color else {
            panic!("Theme must explicitly set both RGB colors")
        };
        let linear = |channel: u8| {
            let value = f64::from(channel) / 255.0;
            if value <= 0.04045 {
                value / 12.92
            } else {
                ((value + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * linear(r) + 0.7152 * linear(g) + 0.0722 * linear(b)
    };
    let (a, b) = (luminance(fg), luminance(bg));
    (a.max(b) + 0.05) / (a.min(b) + 0.05)
}

#[test]
fn ioc_highlights_and_tags_have_explicit_contrasting_backgrounds() {
    let mut app = app();
    app.rows[1].event_mut().unwrap().ioc_match = true;
    app.rows[2].event_mut().unwrap().tag = Tag::Suspicious;
    app.rows[3].event_mut().unwrap().tag = Tag::Ioc;
    app.rows[4].event_mut().unwrap().tag = Tag::Malicious;
    for theme in [
        timeliner_tui::theme::Theme::Light,
        timeliner_tui::theme::Theme::Dark,
    ] {
        app.session.theme = theme;
        let mut terminal = Terminal::new(TestBackend::new(120, 30)).unwrap();
        terminal.draw(|frame| app.draw(frame)).unwrap();
        let buffer = terminal.backend().buffer();
        for (x, y) in [(47, 5), (47, 6), (119, 7), (119, 8), (119, 9)] {
            let cell = &buffer[(x, y)];
            assert!(
                contrast(cell.fg, cell.bg) >= 4.5,
                "{theme:?} at ({x}, {y}): {cell:?}"
            );
        }
    }
}

#[test]
fn theme_toggle_preserves_the_investigation_and_works_inside_dialogs() {
    let mut app = app();
    app.session.views[0].predicate.include = "file".into();
    app.session.views[0].selected = 4;
    key(&mut app, KeyCode::F(3));
    app.message(timeliner_tui::worker::Message::Detail(
        "Path: /sample".into(),
    ));
    key(&mut app, KeyCode::F(11));
    assert_eq!(app.session.theme, timeliner_tui::theme::Theme::Dark);
    assert_eq!(app.session.views[0].predicate.include, "file");
    assert_eq!(app.session.views[0].selected, 4);
    let mut terminal = Terminal::new(TestBackend::new(120, 30)).unwrap();
    terminal.draw(|frame| app.draw(frame)).unwrap();
    let text = terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(|c| c.symbol())
        .collect::<String>();
    assert!(text.contains("Record inspector"));
    assert!(text.contains("/sample"));
    key(&mut app, KeyCode::F(11));
    assert_eq!(app.session.theme, timeliner_tui::theme::Theme::Light);
}

#[test]
fn dialogs_survive_compact_terminal_sizes_in_both_themes() {
    for theme in [
        timeliner_tui::theme::Theme::Light,
        timeliner_tui::theme::Theme::Dark,
    ] {
        for (width, height) in [(80, 24), (60, 12), (30, 8), (1, 1)] {
            let mut app = app();
            app.session.theme = theme;
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            terminal.draw(|frame| app.draw(frame)).unwrap();
            for function in [1, 2, 4, 5, 7, 8, 9] {
                key(&mut app, KeyCode::F(function));
                terminal.draw(|frame| app.draw(frame)).unwrap();
                key(&mut app, KeyCode::Esc);
            }
        }
    }
}
#[test]
fn control_sequences_are_displayed_as_text() {
    assert_eq!(
        timeliner_tui::model::display_text("/tmp/\u{1b}[31m"),
        "/tmp/\\u{1b}[31m"
    );
}

#[test]
fn jumping_to_end_keeps_backward_pagination_available() {
    let mut app = app();
    let actions = key(&mut app, KeyCode::Char('G'));
    let [Action::Page(request)] = actions.as_slice() else {
        panic!("Expected end-page request")
    };
    let generation = request.generation;
    let rows = (1000..1000 + request.limit as i64)
        .map(|id| {
            DisplayRow::Event(Row {
                key: EventKey {
                    epoch: id,
                    record: id,
                },
                macb: 15,
                host: "a".into(),
                path: "/a".into(),
                tag: Tag::None,
                automatic: true,
                ioc_match: false,
            })
        })
        .collect();
    app.message(timeliner_tui::worker::Message::Page(generation, rows));
    let mut requested_previous = false;
    for _ in 0..200 {
        if key(&mut app, KeyCode::Up)
            .iter()
            .any(|a| matches!(a,Action::Page(p) if p.reverse))
        {
            requested_previous = true;
            break;
        }
    }
    assert!(requested_previous);
}

#[test]
fn clearing_last_tagged_row_falls_back_to_previous_matches() {
    let mut app = app();
    app.session.views[0].predicate.tags = Some(vec![]);
    app.top = 99;
    let actions = app.message(timeliner_tui::worker::Message::Changed);
    let [Action::Page(request)] = actions.as_slice() else {
        panic!("Expected refresh")
    };
    let anchor = request.anchor;
    let actions = app.message(timeliner_tui::worker::Message::Page(
        request.generation,
        vec![],
    ));
    assert!(
        matches!(actions.as_slice(),[Action::Page(request)] if request.reverse && request.anchor == anchor && !request.inclusive)
    );
}

#[test]
fn errors_from_old_queries_do_not_discard_newer_results() {
    let mut app = app();
    let old = key(&mut app, KeyCode::Char('G'));
    let [Action::Page(old)] = old.as_slice() else {
        panic!()
    };
    key(&mut app, KeyCode::Char('g'));
    let new = key(&mut app, KeyCode::Char('g'));
    let [Action::Page(new)] = new.as_slice() else {
        panic!()
    };
    app.message(timeliner_tui::worker::Message::QueryError(
        old.generation,
        "old error".into(),
    ));
    let row = DisplayRow::Event(Row {
        key: EventKey {
            epoch: 200,
            record: 9000,
        },
        macb: 15,
        host: "a".into(),
        path: "/new".into(),
        tag: Tag::None,
        automatic: true,
        ioc_match: false,
    });
    app.message(timeliner_tui::worker::Message::Page(
        new.generation,
        vec![row],
    ));
    assert_eq!(app.rows[0].event().unwrap().path, "/new");
}

#[test]
fn detail_values_and_iso_timestamps_start_in_the_same_column() {
    let fields = [
        ("Host", "sample-host"),
        ("Hash / original first field", "original-hash"),
        ("Size", "48.22 KB"),
        ("Access time", "2026-09-08T09:14:19+00:00"),
        ("Modification time", "2026-09-08T09:14:20+00:00"),
        ("Change time", "2026-09-08T09:14:22+00:00"),
        ("Birth time", "2026-09-08T09:14:17+00:00"),
    ];
    for theme in [
        timeliner_tui::theme::Theme::Light,
        timeliner_tui::theme::Theme::Dark,
    ] {
        for width in [64, 80, 120] {
            let mut app = app();
            app.session.theme = theme;
            app.message(timeliner_tui::worker::Message::Detail(
                fields
                    .iter()
                    .map(|(name, value)| format!("{name}: {value}\n"))
                    .collect(),
            ));
            let mut terminal = Terminal::new(TestBackend::new(width, 24)).unwrap();
            terminal.draw(|frame| app.draw(frame)).unwrap();
            let lines = terminal
                .backend()
                .buffer()
                .content
                .chunks(width as usize)
                .map(|line| line.iter().map(|cell| cell.symbol()).collect::<String>())
                .collect::<Vec<_>>();
            let columns = fields.map(|(name, value)| {
                let line = lines.iter().find(|line| line.contains(name)).unwrap();
                let offset = line.find(value).expect("Field value must be visible");
                line[..offset].chars().count()
            });
            assert!(
                columns.iter().all(|column| *column == columns[0]),
                "{theme:?} at width {width}: {columns:?}"
            );
        }
    }
}

#[test]
fn long_wrapped_details_can_be_scrolled_to_the_end() {
    let mut app = app();
    app.message(timeliner_tui::worker::Message::Detail(format!(
        "{}\nEND_OF_DETAILS",
        "long-path/".repeat(500)
    )));
    let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
    terminal.draw(|frame| app.draw(frame)).unwrap();
    for _ in 0..20 {
        key(&mut app, KeyCode::PageDown);
        terminal.draw(|frame| app.draw(frame)).unwrap();
    }
    let text = terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(|c| c.symbol())
        .collect::<String>();
    assert!(text.contains("END_OF_DETAILS"));
}

#[test]
fn jump_back_after_closing_its_tab_does_not_replace_another_tab() {
    let mut app = app();
    let saved_rows = app.rows.clone();
    key(&mut app, KeyCode::Char('m'));
    key(&mut app, KeyCode::Char('a'));
    let actions = key(&mut app, KeyCode::F(6));
    let [Action::Page(request)] = actions.as_slice() else {
        panic!()
    };
    app.message(timeliner_tui::worker::Message::Page(
        request.generation,
        saved_rows,
    ));
    app.session.views[2].name = "Return target".into();
    key(&mut app, KeyCode::Char('\''));
    key(&mut app, KeyCode::Char('a'));
    key(&mut app, KeyCode::F(6));
    key(&mut app, KeyCode::Char('g'));
    key(&mut app, KeyCode::Char('t'));
    key(&mut app, KeyCode::Char('g'));
    key(&mut app, KeyCode::Char('t'));
    key(&mut app, KeyCode::Char('g'));
    key(&mut app, KeyCode::Char('t'));
    assert_eq!(app.session.active, 2);
    key(&mut app, KeyCode::F(7));
    key(&mut app, KeyCode::Char('x'));
    key(&mut app, KeyCode::Char('\''));
    key(&mut app, KeyCode::Char('\''));
    assert_eq!(app.session.views.len(), 4);
    assert_eq!(app.session.views[2].name, "Return target copy");
    assert_eq!(app.session.views[3].name, "Return target");
}
