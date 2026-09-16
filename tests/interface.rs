use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{Terminal, backend::TestBackend, style::Color};
use timeliner_tui::{
    model::{EventKey, Row, Tag},
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
        .map(|id| Row {
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
        .collect();
    app
}
#[test]
fn arrows_scroll_under_highlight_and_jk_never_scroll() {
    let mut app = app();
    for _ in 0..5 {
        key(&mut app, KeyCode::Char('j'));
    }
    assert_eq!(app.session.views[0].selected, 5);
    assert_eq!(app.top, 0);
    key(&mut app, KeyCode::Down);
    assert_eq!(app.top, 1);
    assert_eq!(app.session.views[0].selected, 5);
    for _ in 0..20 {
        key(&mut app, KeyCode::Char('j'));
    }
    assert_eq!(app.session.views[0].selected, 9);
    assert_eq!(app.top, 1);
    for _ in 0..20 {
        key(&mut app, KeyCode::Char('k'));
    }
    assert_eq!(app.session.views[0].selected, 0);
    assert_eq!(app.top, 1);
}
#[test]
fn tab_tags_record_and_enter_requests_details() {
    let mut app = app();
    let actions = key(&mut app, KeyCode::Tab);
    assert!(matches!(
        actions.as_slice(),
        [Action::Command(Command::Tag(0, Some(Tag::Suspicious)))]
    ));
    assert_eq!(app.rows[0].tag, Tag::Suspicious);
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
fn fork_copies_filters_but_smart_tab_is_global() {
    let mut app = app();
    app.session.views[0].after = Some(100);
    app.session.views[0].predicate.exclude.push("/tmp".into());
    key(&mut app, KeyCode::F(6));
    assert_eq!(app.session.views.len(), 2);
    assert_eq!(app.session.views[1].after, Some(100));
    app.session.views[1].predicate.exclude.push("other".into());
    assert_eq!(app.session.views[0].predicate.exclude.len(), 1);
    key(&mut app, KeyCode::F(7));
    key(&mut app, KeyCode::Char('3'));
    let view = &app.session.views[2];
    assert_eq!(view.after, None);
    assert!(view.predicate.exclude.is_empty());
    assert_eq!(view.predicate.tags, Some(vec![Tag::Ioc]));
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
fn tag_is_right_aligned_and_keys_survive_standard_terminal_resize() {
    let mut app = app();
    app.rows[0].tag = Tag::Suspicious;
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
        assert!(text.contains("F1 Help"));
        assert!(text.contains("F10 Quit"));
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
fn ordinary_rows_filters_and_status_use_the_terminal_default_colors() {
    let mut app = app();
    let mut terminal = Terminal::new(TestBackend::new(120, 30)).unwrap();
    terminal.draw(|frame| app.draw(frame)).unwrap();
    let buffer = terminal.backend().buffer();
    for (x, y) in [(0, 2), (0, 6), (20, 6), (44, 6), (0, 27), (0, 28)] {
        let cell = &buffer[(x, y)];
        assert_eq!(cell.fg, Color::Reset, "foreground at ({x}, {y})");
        assert_eq!(cell.bg, Color::Reset, "background at ({x}, {y})");
    }
}

#[test]
fn ioc_highlights_and_tags_have_explicit_contrasting_backgrounds() {
    let mut app = app();
    app.rows[1].ioc_match = true;
    app.rows[2].tag = Tag::Suspicious;
    app.rows[3].tag = Tag::Ioc;
    app.rows[4].tag = Tag::Malicious;
    let mut terminal = Terminal::new(TestBackend::new(120, 30)).unwrap();
    terminal.draw(|frame| app.draw(frame)).unwrap();
    let buffer = terminal.backend().buffer();
    for (x, y) in [(44, 5), (44, 6), (119, 7), (119, 8), (119, 9)] {
        let cell = &buffer[(x, y)];
        assert_ne!(cell.fg, Color::Reset, "foreground at ({x}, {y})");
        assert_ne!(cell.bg, Color::Reset, "background at ({x}, {y})");
        assert_ne!(cell.fg, cell.bg, "contrast at ({x}, {y})");
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
        .map(|id| Row {
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
    let actions = app.message(timeliner_tui::worker::Message::Page(
        request.generation,
        vec![],
    ));
    assert!(
        matches!(actions.as_slice(),[Action::Page(request)] if request.reverse && request.anchor.is_none())
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
    let row = Row {
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
    };
    app.message(timeliner_tui::worker::Message::Page(
        new.generation,
        vec![row],
    ));
    assert_eq!(app.rows[0].path, "/new");
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
    app.session.views[1].name = "Return target".into();
    key(&mut app, KeyCode::Char('\''));
    key(&mut app, KeyCode::Char('a'));
    key(&mut app, KeyCode::F(6));
    key(&mut app, KeyCode::Char('g'));
    key(&mut app, KeyCode::Char('t'));
    key(&mut app, KeyCode::Char('g'));
    key(&mut app, KeyCode::Char('t'));
    assert_eq!(app.session.active, 1);
    key(&mut app, KeyCode::F(7));
    key(&mut app, KeyCode::Char('x'));
    key(&mut app, KeyCode::Char('\''));
    key(&mut app, KeyCode::Char('\''));
    assert_eq!(app.session.views.len(), 3);
    assert_eq!(app.session.views[1].name, "Return target copy");
    assert_eq!(app.session.views[2].name, "Return target");
}
