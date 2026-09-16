use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{Terminal, backend::TestBackend};
use std::ops::Range;
use timeliner_tui::{
    model::{EventKey, Row, Session, Tag},
    ui::{Action, App},
    worker::{Command, Message, PageRequest},
};

fn event(id: i64) -> EventKey {
    EventKey {
        epoch: 1_600_000_000 + id,
        record: id,
    }
}

fn rows(ids: Range<i64>) -> Vec<Row> {
    ids.map(|id| Row {
        key: event(id),
        macb: 15,
        host: "host-a".into(),
        path: format!("/tmp/file-{id}"),
        tag: Tag::None,
        automatic: false,
        ioc_match: false,
    })
    .collect()
}

fn page(actions: Vec<Action>) -> PageRequest {
    assert_eq!(actions.len(), 1);
    let Action::Page(request) = actions.into_iter().next().unwrap() else {
        panic!("Expected a page request");
    };
    request
}

fn loaded(ids: Range<i64>, height: usize) -> App {
    let mut app = App::default();
    app.height = height;
    let mut session = Session::default();
    session.views[0].top = (ids.start > 0).then(|| event(ids.start));
    let request = page(app.message(Message::Ready(session, String::new())));
    assert!(
        app.message(Message::Page(request.generation, rows(ids)))
            .is_empty()
    );
    app
}

fn key(app: &mut App, code: KeyCode) -> Vec<Action> {
    app.key(KeyEvent::new(code, KeyModifiers::NONE))
}

fn reposition(app: &mut App, code: KeyCode) -> Vec<Action> {
    assert!(key(app, KeyCode::Char('z')).is_empty());
    key(app, code)
}

fn assert_selection(app: &App, id: i64, slot: usize) {
    let view = &app.session.views[app.session.active];
    assert_eq!(view.selected, slot);
    assert_eq!(app.rows[app.top + slot].key, event(id));
    assert_eq!(view.top, Some(app.rows[app.top].key));
}

#[test]
fn arrows_keep_the_selected_event_until_it_leaves_the_screen() {
    for (code, slots, last_event, last_slot) in [
        (KeyCode::Down, vec![2, 1, 0], 44, 0),
        (KeyCode::Up, vec![4, 5, 6, 7, 8, 9], 42, 9),
    ] {
        let mut app = loaded(0..128, 10);
        app.top = 40;
        app.session.views[0].selected = 3;
        for slot in slots {
            assert!(key(&mut app, code).is_empty());
            assert_selection(&app, 43, slot);
        }
        assert!(key(&mut app, code).is_empty());
        assert_selection(&app, last_event, last_slot);
        assert!(matches!(
            key(&mut app, KeyCode::Enter).as_slice(),
            [Action::Command(Command::Detail(id, _))] if *id == last_event
        ));
    }
}

#[test]
fn arrow_page_loads_preserve_or_clamp_selection_when_the_cache_is_trimmed() {
    for reverse in [false, true] {
        for slot in [0, 4, 9] {
            let mut app = loaded(1000..1128, 10);
            app.top = 118;
            let request = page(key(&mut app, KeyCode::Down));
            app.message(Message::Page(request.generation, rows(1128..1256)));

            app.top = if reverse { 0 } else { 246 };
            app.session.views[0].selected = slot;
            let original_top = app.top;
            let focus = 1000 + (original_top + slot) as i64;
            let request = page(key(
                &mut app,
                if reverse { KeyCode::Up } else { KeyCode::Down },
            ));
            assert_eq!(request.reverse, reverse);
            assert!(!request.inclusive);
            assert_eq!(app.top, original_top);
            assert_eq!(app.session.views[0].selected, slot);
            app.message(Message::Page(
                request.generation,
                rows(if reverse { 872..1000 } else { 1256..1384 }),
            ));
            let (event, slot) = match (reverse, slot) {
                (true, 9) => (focus - 1, 9),
                (true, slot) => (focus, slot + 1),
                (false, 0) => (focus + 1, 0),
                (false, slot) => (focus, slot - 1),
            };
            assert_selection(&app, event, slot);
            assert_eq!(app.rows.len(), request.limit * 2);
        }
    }
}

#[test]
fn arrow_page_loads_preserve_selection_changes_made_while_waiting() {
    for reverse in [false, true] {
        let mut app = loaded(1000..1128, 10);
        app.top = if reverse { 0 } else { 118 };
        app.session.views[0].selected = 4;
        let request = page(key(
            &mut app,
            if reverse { KeyCode::Up } else { KeyCode::Down },
        ));
        key(&mut app, KeyCode::Char('j'));
        app.message(Message::Page(
            request.generation,
            rows(if reverse { 872..1000 } else { 1128..1256 }),
        ));
        assert_selection(
            &app,
            if reverse { 1005 } else { 1123 },
            if reverse { 6 } else { 4 },
        );
    }
}

#[test]
fn arrows_at_timeline_boundaries_and_after_zt_keep_the_selected_event() {
    let mut app = loaded(0..20, 10);
    app.session.views[0].selected = 5;
    assert!(key(&mut app, KeyCode::Up).is_empty());
    assert_selection(&app, 5, 5);
    app.top = 10;
    assert!(key(&mut app, KeyCode::Down).is_empty());
    assert_selection(&app, 15, 5);

    app.session.views[0].selected = 9;
    reposition(&mut app, KeyCode::Char('t'));
    assert!(key(&mut app, KeyCode::Down).is_empty());
    assert_selection(&app, 19, 0);
    assert!(key(&mut app, KeyCode::Up).is_empty());
    assert_selection(&app, 19, 1);

    for reverse in [false, true] {
        let mut app = loaded(1000..1128, 10);
        app.top = if reverse { 0 } else { 118 };
        app.session.views[0].selected = 4;
        let request = page(key(
            &mut app,
            if reverse { KeyCode::Up } else { KeyCode::Down },
        ));
        app.message(Message::Page(request.generation, vec![]));
        assert_selection(&app, if reverse { 1004 } else { 1122 }, 4);
    }

    for count in [0, 1] {
        let mut app = loaded(0..count, 1);
        assert!(key(&mut app, KeyCode::Up).is_empty());
        assert!(key(&mut app, KeyCode::Down).is_empty());
        assert_eq!(app.top, 0);
        assert_eq!(app.session.views[0].selected, 0);
    }
}

#[test]
fn page_keys_keep_the_highlight_in_its_screen_slot() {
    let mut app = loaded(1000..1128, 10);
    app.top = 40;
    app.session.views[0].selected = 4;
    key(&mut app, KeyCode::PageDown);
    assert_selection(&app, 1054, 4);
    key(&mut app, KeyCode::PageUp);
    assert_selection(&app, 1044, 4);

    for reverse in [false, true] {
        app.top = if reverse {
            0
        } else {
            app.rows.len() - app.height
        };
        let request = page(key(
            &mut app,
            if reverse {
                KeyCode::PageUp
            } else {
                KeyCode::PageDown
            },
        ));
        app.message(Message::Page(
            request.generation,
            rows(if reverse { 872..1000 } else { 1128..1256 }),
        ));
        assert_selection(&app, if reverse { 994 } else { 1132 }, 4);
    }
}

#[test]
fn arrow_scrolling_renders_the_highlight_on_the_selected_event() {
    let mut app = loaded(0..128, 10);
    let mut terminal = Terminal::new(TestBackend::new(100, 19)).unwrap();
    app.top = 40;
    app.session.views[0].selected = 3;
    let mut capture = String::new();
    for (label, steps, id, slot) in [
        ("Before scrolling", 0, 43, 3),
        ("After Down — event 43 stays selected", 1, 43, 2),
        (
            "After four Down presses — event 43 has left the screen",
            3,
            44,
            0,
        ),
    ] {
        for _ in 0..steps {
            assert!(key(&mut app, KeyCode::Down).is_empty());
        }
        terminal.draw(|frame| app.draw(frame)).unwrap();
        let buffer = terminal.backend().buffer();
        let selected_line = (0..buffer.area.width)
            .map(|x| buffer[(x, 5 + slot)].symbol())
            .collect::<String>();
        assert!(selected_line.contains(&format!("/tmp/file-{id}")));
        assert!(selected_line.contains('▌'));
        capture.push_str(label);
        capture.push('\n');
        for y in 4..15 {
            let line = (0..buffer.area.width)
                .map(|x| buffer[(x, y)].symbol())
                .collect::<String>();
            capture.push_str(line.trim_end());
            capture.push('\n');
        }
        capture.push('\n');
    }
    if std::env::var_os("TIMELINER_CAPTURE_ARROWS").is_some() {
        print!("{capture}");
    }
}

#[test]
fn z_shortcuts_and_aliases_reposition_cached_events_and_save_the_location() {
    for height in [1, 2, 9, 10] {
        for (code, slot) in [
            (KeyCode::Char('t'), 0),
            (KeyCode::Enter, 0),
            (KeyCode::Char('z'), (height - 1) / 2),
            (KeyCode::Char('.'), (height - 1) / 2),
            (KeyCode::Char('b'), height - 1),
            (KeyCode::Char('-'), height - 1),
        ] {
            let mut app = loaded(0..128, height);
            app.top = 40;
            app.session.views[0].selected = height / 2;
            let selected = (40 + height / 2) as i64;
            for _ in 0..2 {
                assert!(reposition(&mut app, code).is_empty());
                assert_selection(&app, selected, slot);
            }
            let actions = key(&mut app, KeyCode::Enter);
            assert!(matches!(
                actions.as_slice(),
                [Action::Command(Command::Detail(id, _))] if *id == selected
            ));
            let actions = key(&mut app, KeyCode::Char('q'));
            let [Action::Command(Command::Save(session)), Action::Quit] = actions.as_slice() else {
                panic!("Expected session save before quitting");
            };
            assert_eq!(session.views[0].selected, slot);
            assert_eq!(session.views[0].top, Some(event(selected - slot as i64)));
        }
    }
}

#[test]
fn repositioning_fetches_missing_rows_in_both_directions() {
    for (code, top, selected, reverse, slot, incoming) in [
        ('z', 0, 2, true, 4, 872..1000),
        ('b', 0, 2, true, 9, 872..1000),
        ('t', 118, 9, false, 0, 1128..1256),
        ('z', 118, 9, false, 4, 1128..1256),
    ] {
        let mut app = loaded(1000..1128, 10);
        app.top = top;
        app.session.views[0].selected = selected;
        let focus = 1000 + (top + selected) as i64;
        let request = page(reposition(&mut app, KeyCode::Char(code)));
        assert_eq!(request.reverse, reverse);
        assert!(!request.inclusive);
        assert_eq!(
            request.anchor,
            Some(event(if reverse { 1000 } else { 1127 }))
        );
        assert_eq!(app.top, top, "Keep the view intact while loading");
        assert_eq!(app.session.views[0].selected, selected);
        assert!(
            app.message(Message::Page(request.generation, rows(incoming)))
                .is_empty()
        );
        assert_selection(&app, focus, slot);
    }
}

#[test]
fn repositioning_keeps_selection_when_the_page_cache_is_trimmed() {
    for reverse in [false, true] {
        let mut app = loaded(1000..1128, 10);
        app.top = 118;
        let request = page(key(&mut app, KeyCode::Down));
        app.message(Message::Page(request.generation, rows(1128..1256)));
        assert_eq!(app.rows.len(), request.limit * 2);

        app.top = if reverse { 0 } else { 246 };
        app.session.views[0].selected = if reverse { 2 } else { 9 };
        let selected = if reverse { 1002 } else { 1255 };
        let request = page(reposition(
            &mut app,
            KeyCode::Char(if reverse { 'b' } else { 't' }),
        ));
        assert_eq!(request.reverse, reverse);
        app.message(Message::Page(
            request.generation,
            rows(if reverse { 872..1000 } else { 1256..1384 }),
        ));
        assert_eq!(app.rows.len(), request.limit * 2);
        assert_selection(&app, selected, if reverse { 9 } else { 0 });
        assert_eq!(
            app.rows.first().unwrap().key.record,
            if reverse { 872 } else { 1128 }
        );
        assert_eq!(
            app.rows.last().unwrap().key.record,
            if reverse { 1127 } else { 1383 }
        );
    }
}

#[test]
fn repositioning_clamps_at_the_beginning_and_allows_blank_space_at_the_end() {
    let mut app = loaded(0..20, 10);
    app.session.views[0].selected = 2;
    for code in ['z', 'b'] {
        assert!(reposition(&mut app, KeyCode::Char(code)).is_empty());
        assert_selection(&app, 2, 2);
        assert_eq!(app.top, 0);
    }

    app.top = 10;
    app.session.views[0].selected = 9;
    for (code, slot) in [('t', 0), ('z', 4), ('b', 9)] {
        assert!(reposition(&mut app, KeyCode::Char(code)).is_empty());
        assert_selection(&app, 19, slot);
    }
}

#[test]
fn short_or_empty_pages_preserve_the_selected_event_at_timeline_boundaries() {
    let mut app = loaded(3..131, 10);
    app.session.views[0].selected = 1;
    let request = page(reposition(&mut app, KeyCode::Char('b')));
    app.message(Message::Page(request.generation, rows(0..3)));
    assert_selection(&app, 4, 4);
    assert!(reposition(&mut app, KeyCode::Char('b')).is_empty());

    for incoming in [1128..1128, 1128..1130] {
        let mut app = loaded(1000..1128, 10);
        app.top = 118;
        app.session.views[0].selected = 9;
        let request = page(reposition(&mut app, KeyCode::Char('t')));
        app.message(Message::Page(request.generation, rows(incoming)));
        assert_selection(&app, 1127, 0);
        assert!(reposition(&mut app, KeyCode::Char('t')).is_empty());
    }
}

#[test]
fn empty_views_and_cancelled_or_invalid_prefixes_do_not_trigger_other_commands() {
    let mut empty = loaded(0..0, 10);
    for code in ['t', 'z', 'b'] {
        assert!(reposition(&mut empty, KeyCode::Char(code)).is_empty());
        assert_eq!(empty.top, 0);
        assert_eq!(empty.session.views[0].selected, 0);
    }

    let mut app = loaded(0..128, 10);
    app.top = 40;
    app.session.views[0].selected = 3;
    for code in [KeyCode::Char('x'), KeyCode::Tab, KeyCode::Char('2')] {
        assert!(reposition(&mut app, code).is_empty());
        assert_eq!(app.top, 40);
        assert_eq!(app.session.views[0].selected, 3);
        assert!(app.session.views[0].predicate.exclude.is_empty());
        assert_eq!(app.rows[43].tag, Tag::None);
        assert_eq!(app.session.active, 0);
    }
    key(&mut app, KeyCode::Char('z'));
    key(&mut app, KeyCode::Esc);
    assert!(matches!(
        key(&mut app, KeyCode::Enter).as_slice(),
        [Action::Command(Command::Detail(43, _))]
    ));

    assert!(
        app.key(KeyEvent::new(KeyCode::Char('z'), KeyModifiers::CONTROL))
            .is_empty()
    );
    assert!(matches!(
        key(&mut app, KeyCode::Enter).as_slice(),
        [Action::Command(Command::Detail(43, _))]
    ));
    key(&mut app, KeyCode::Char('z'));
    app.key(KeyEvent::new(KeyCode::Char('t'), KeyModifiers::CONTROL));
    assert_eq!(app.top, 40);
}

#[test]
fn cancelling_or_failing_a_reposition_query_leaves_the_view_intact() {
    for cancel in [false, true] {
        let mut app = loaded(1000..1128, 10);
        app.session.views[0].selected = 2;
        let request = page(reposition(&mut app, KeyCode::Char('b')));
        if cancel {
            assert!(matches!(
                key(&mut app, KeyCode::Esc).as_slice(),
                [Action::Cancel]
            ));
        } else {
            app.message(Message::QueryError(
                request.generation,
                "query failed".into(),
            ));
        }
        app.message(Message::Page(request.generation, rows(872..1000)));
        assert_selection(&app, 1002, 2);
        assert_eq!(app.top, 0);
        assert!(matches!(
            reposition(&mut app, KeyCode::Char('b')).as_slice(),
            [Action::Page(_)]
        ));
    }
}

#[test]
fn repositioning_does_not_replace_an_in_flight_filter_change() {
    let mut app = loaded(0..128, 10);
    let request = page(key(&mut app, KeyCode::Char('x')));
    assert!(reposition(&mut app, KeyCode::Char('t')).is_empty());
    app.message(Message::Page(request.generation, rows(1..129)));
    assert_eq!(app.session.views[0].predicate.exclude, ["^/tmp/file\\-0$"]);
    assert_selection(&app, 1, 0);
}

#[test]
fn resizing_while_loading_uses_the_new_viewport_height() {
    let mut app = loaded(1000..1128, 10);
    app.session.views[0].selected = 2;
    let request = page(reposition(&mut app, KeyCode::Char('b')));
    app.height = 20;
    app.message(Message::Page(request.generation, rows(872..1000)));
    assert_selection(&app, 1002, 19);
}

#[test]
fn z_shortcuts_render_the_same_event_at_the_requested_screen_row() {
    let mut app = loaded(0..128, 10);
    let mut terminal = Terminal::new(TestBackend::new(100, 19)).unwrap();
    terminal.draw(|frame| app.draw(frame)).unwrap();
    assert_eq!(app.height, 10);
    app.top = 40;
    app.session.views[0].selected = 3;
    let mut capture = String::new();
    for (code, slot) in [('t', 0), ('z', 4), ('b', 9)] {
        assert!(reposition(&mut app, KeyCode::Char(code)).is_empty());
        terminal.draw(|frame| app.draw(frame)).unwrap();
        let buffer = terminal.backend().buffer();
        let selected_line = (0..buffer.area.width)
            .map(|x| buffer[(x, 5 + slot as u16)].symbol())
            .collect::<String>();
        assert!(selected_line.contains("/tmp/file-43"));
        assert!(selected_line.contains('▌'));
        capture.push_str(&format!("After z{code} — event 43 stays selected\n"));
        for y in 4..15 {
            let line = (0..buffer.area.width)
                .map(|x| buffer[(x, y)].symbol())
                .collect::<String>();
            capture.push_str(line.trim_end());
            capture.push('\n');
        }
        capture.push('\n');
    }
    if std::env::var_os("TIMELINER_CAPTURE_Z").is_some() {
        print!("{capture}");
    }
}
