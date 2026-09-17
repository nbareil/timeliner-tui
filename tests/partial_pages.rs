use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::{fs, sync::atomic::Ordering, time::Duration};
use tempfile::TempDir;
use timeliner_tui::{
    burst,
    ioc::Rules,
    model::{DisplayRow, EventKey, Session, View},
    query::{self, Cancellation, Page},
    source,
    store::Store,
    ui::{Action, App},
    worker::{Message, PageRequest, Workers},
};

fn fixture(fold: Option<i64>) -> (TempDir, Store, Vec<EventKey>) {
    let dir = tempfile::tempdir().unwrap();
    let body = dir.path().join("source.body");
    let mut contents = String::new();
    for epoch in [100, 20_000, 40_000] {
        for i in 0..9 {
            contents.push_str(&format!(
                "0|/keep/{epoch}/{i}|1|-rw-r--r--|0|0|1|{epoch}|{epoch}|{epoch}|{epoch}\n"
            ));
        }
        if epoch != 40_000 {
            for i in 1..=8192 {
                let time = epoch + i;
                contents.push_str(&format!(
                    "0|/noise/{time}|1|-rw-r--r--|0|0|1|{time}|{time}|{time}|{time}\n"
                ));
            }
        }
    }
    fs::write(&body, contents).unwrap();
    let mut store = Store::open(&dir.path().join("workspace")).unwrap();
    store
        .import(
            &source::resolve(&body, None, false).unwrap(),
            &Rules::default(),
            |_, _| true,
        )
        .unwrap();
    let mut session = Session::default();
    let view = &mut session.views[0];
    view.predicate.exclude.push("^/noise/".into());
    let rows = query::page(
        &store,
        view,
        Page::default(),
        &Cancellation::never(),
        |_| {},
    )
    .unwrap();
    let keys: Vec<_> = rows.iter().map(|row| row.key).collect();
    assert_eq!(
        keys.len(),
        27,
        "Raw queries must still retrieve all matching events"
    );
    view.top = Some(keys[0]);
    view.selected = 4;
    if let Some(epoch) = fold {
        let origin = rows.iter().find(|row| row.key.epoch == epoch).unwrap();
        view.bursts.push(
            burst::detect(
                &store,
                view,
                origin.key,
                &origin.host,
                1,
                &Cancellation::never(),
                |_| {},
            )
            .unwrap()
            .unwrap(),
        );
        view.selected = 0;
    }
    store.save_session(&session).unwrap();
    (dir, store, keys)
}

fn receive(workers: &Workers) -> Message {
    match workers
        .messages
        .recv_timeout(Duration::from_secs(10))
        .unwrap()
    {
        Message::Error(error) | Message::QueryError(_, error) => panic!("{error}"),
        message => message,
    }
}

fn ready(workers: &Workers) -> Session {
    loop {
        if let Message::Ready(session, _) = receive(workers) {
            return session;
        }
    }
}

fn request(actions: Vec<Action>) -> PageRequest {
    assert_eq!(actions.len(), 1);
    let Action::Page(request) = actions.into_iter().next().unwrap() else {
        panic!("Expected page request");
    };
    request
}

fn fetch(workers: &Workers, request: PageRequest) -> Message {
    let generation = request.generation;
    workers.clock.store(generation, Ordering::Relaxed);
    workers.pages.send(request).unwrap();
    loop {
        match receive(workers) {
            message @ (Message::Page(actual, _) | Message::PartialPage(actual, _)) => {
                assert_eq!(actual, generation);
                return message;
            }
            Message::Progress(_, _) => {}
            _ => panic!("Unexpected page response"),
        }
    }
}

fn finish_fill(workers: &Workers, app: &mut App, mut actions: Vec<Action>) {
    for _ in 0..50 {
        if actions.is_empty() {
            return;
        }
        let page = request(actions);
        assert!(
            page.limit <= app.height,
            "Background filling must not request a full prefetch buffer"
        );
        actions = app.message(fetch(workers, page));
    }
    panic!("Background filling did not finish");
}

#[test]
fn reopening_a_sparse_view_shows_nearby_rows_and_keeps_later_events_reachable() {
    let (_dir, store, keys) = fixture(None);
    let root = store.root.clone();
    drop(store);
    let workers = Workers::start(root, vec![], None, None, None, None);
    let mut app = App::default();
    let initial = request(app.message(Message::Ready(ready(&workers), String::new())));
    assert!(initial.allow_partial);
    let message = fetch(&workers, initial);
    let Message::PartialPage(_, rows) = &message else {
        panic!("Restore should return available rows before filling the prefetch buffer");
    };
    assert_eq!(
        rows.iter().map(DisplayRow::key).collect::<Vec<_>>(),
        keys[..9]
    );
    let filling = app.message(message);
    assert_eq!(
        filling.len(),
        1,
        "A partial first page must keep filling the screen"
    );
    assert!(app.status.contains("Loading visible rows"));
    assert_eq!(app.session.views[0].selected, 4);
    finish_fill(&workers, &mut app, filling);
    assert_eq!(app.rows.len() - app.top, app.height);
    assert_eq!(app.rows.len(), 20, "Stop when the viewport is full");
    assert_eq!(
        app.rows[app.top + app.session.views[0].selected].key(),
        keys[4]
    );
    assert_eq!(app.status, "Ready");
    // Every later event remains reachable, including after another short page.
    for expected in &keys[5..] {
        let actions = app.key(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE));
        if !actions.is_empty() {
            let message = fetch(&workers, request(actions));
            let filling = app.message(message);
            finish_fill(&workers, &mut app, filling);
        }
        assert_eq!(
            app.rows[app.top + app.session.views[0].selected].key(),
            *expected
        );
    }
    workers.shutdown();
}

#[test]
fn partial_pages_keep_cursor_order_in_both_directions_and_full_queries_still_finish() {
    let (_dir, store, expected) = fixture(None);
    let root = store.root.clone();
    drop(store);
    let workers = Workers::start(root, vec![], None, None, None, None);
    let session = ready(&workers);
    let mut generation = 0;
    for reverse in [false, true] {
        let mut anchor = None;
        let mut found = Vec::new();
        for _ in 0..30 {
            generation += 1;
            let response = fetch(
                &workers,
                PageRequest {
                    generation,
                    view: session.views[0].clone(),
                    anchor,
                    reverse,
                    inclusive: false,
                    limit: 128,
                    context_before: 0,
                    allow_partial: true,
                    detect_burst: None,
                    jump: None,
                },
            );
            let (rows, complete) = match response {
                Message::PartialPage(_, rows) => (rows, false),
                Message::Page(_, rows) => (rows, true),
                _ => unreachable!(),
            };
            assert!(!rows.is_empty() || complete);
            if reverse {
                anchor = rows.first().map(DisplayRow::key);
                found.splice(0..0, rows.iter().map(DisplayRow::key));
            } else {
                anchor = rows.last().map(DisplayRow::key);
                found.extend(rows.iter().map(DisplayRow::key));
            }
            if complete && rows.len() < 128 {
                break;
            }
        }
        assert_eq!(found, expected);
    }
    workers.shutdown();
}

#[test]
fn partial_pages_show_reached_folds_without_inserting_future_summaries_early() {
    for folded_epoch in [100, 20_000, 40_000] {
        let (_dir, store, _) = fixture(Some(folded_epoch));
        let root = store.root.clone();
        drop(store);
        let workers = Workers::start(root, vec![], None, None, None, None);
        let mut app = App::default();
        let initial = request(app.message(Message::Ready(ready(&workers), String::new())));
        let response = fetch(&workers, initial);
        let Message::PartialPage(_, rows) = &response else {
            panic!("A saved fold must not force a complete sparse scan");
        };
        if folded_epoch == 100 {
            assert_eq!(rows.len(), 1);
            assert!(matches!(rows[0], DisplayRow::Burst(_)));
        } else {
            assert_eq!(rows.len(), 9);
            assert!(rows.iter().all(|row| matches!(row, DisplayRow::Event(_))));
        }
        let filling = app.message(response);
        finish_fill(&workers, &mut app, filling);
        let last = request(app.key(KeyEvent::new(KeyCode::Char('G'), KeyModifiers::NONE)));
        let response = fetch(&workers, last);
        let Message::PartialPage(_, rows) = &response else {
            panic!("Reverse browsing must also return available rows before filling its buffer");
        };
        if folded_epoch == 40_000 {
            assert_eq!(rows.len(), 1);
            assert!(matches!(rows[0], DisplayRow::Burst(_)));
        } else {
            assert_eq!(rows.len(), 9);
            assert!(rows.iter().all(|row| matches!(row, DisplayRow::Event(_))));
        }
        workers.shutdown();
    }
}

#[test]
fn a_short_reverse_page_does_not_mark_the_start_of_the_timeline() {
    let mut app = App::default();
    let mut session = Session::default();
    session.views[0] = View::default();
    let initial = request(app.message(Message::Ready(session, String::new())));
    let (_dir, store, _) = fixture(None);
    let events = query::page(
        &store,
        &store.session().unwrap().views[0],
        Page::default(),
        &Cancellation::never(),
        |_| {},
    )
    .unwrap();
    app.message(Message::Page(
        initial.generation,
        events.into_iter().map(DisplayRow::Event).collect(),
    ));
    let last = request(app.key(KeyEvent::new(KeyCode::Char('G'), KeyModifiers::NONE)));
    assert!(last.allow_partial && last.reverse);
    let final_row = app.rows.last().unwrap().clone();
    let earlier = request(app.message(Message::PartialPage(last.generation, vec![final_row])));
    assert!(
        earlier.reverse,
        "A short partial page is not the start of the timeline"
    );
    assert!(
        app.key(KeyEvent::new(KeyCode::Char('k'), KeyModifiers::NONE))
            .is_empty(),
        "The scroll must reuse the already requested earlier rows"
    );
}

#[test]
fn filling_preserves_selection_changes_and_adapts_to_a_taller_viewport() {
    let (_dir, store, keys) = fixture(None);
    let workers = Workers::start(store.root.clone(), vec![], None, None, None, None);
    let mut app = App::default();
    let initial = request(app.message(Message::Ready(ready(&workers), String::new())));
    let filling = app.message(fetch(&workers, initial));
    assert_eq!(app.rows.len(), 9);
    assert!(
        app.key(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE))
            .is_empty()
    );
    app.height = 24;
    finish_fill(&workers, &mut app, filling);
    assert_eq!(app.rows.len() - app.top, 24);
    assert_eq!(
        app.rows[app.top + app.session.views[0].selected].key(),
        keys[5]
    );
    workers.shutdown();
}

#[test]
fn scrolling_into_an_active_fill_reuses_its_request_and_moves_to_the_next_event() {
    let (_dir, store, keys) = fixture(None);
    let workers = Workers::start(store.root.clone(), vec![], None, None, None, None);
    let mut app = App::default();
    let initial = request(app.message(Message::Ready(ready(&workers), String::new())));
    let filling = request(app.message(fetch(&workers, initial)));
    for _ in 0..5 {
        assert!(
            app.key(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE))
                .is_empty()
        );
    }
    assert_eq!(
        app.rows[app.top + app.session.views[0].selected].key(),
        keys[8]
    );
    let further = app.message(fetch(&workers, filling));
    assert_eq!(
        app.rows[app.top + app.session.views[0].selected].key(),
        keys[9]
    );
    finish_fill(&workers, &mut app, further);
    assert_eq!(
        app.rows[app.top + app.session.views[0].selected].key(),
        keys[9]
    );
    workers.shutdown();
}

#[test]
fn background_fill_errors_escape_and_tab_changes_keep_visible_rows_and_reject_late_results() {
    for interrupt in ["escape", "tab", "error"] {
        let (_dir, store, _) = fixture(None);
        let workers = Workers::start(store.root.clone(), vec![], None, None, None, None);
        let mut app = App::default();
        let initial = request(app.message(Message::Ready(ready(&workers), String::new())));
        let filling = request(app.message(fetch(&workers, initial)));
        let original: Vec<_> = app.rows.iter().map(DisplayRow::key).collect();
        match interrupt {
            "escape" => {
                app.key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
            }
            "tab" => {
                app.key(KeyEvent::new(KeyCode::Char('2'), KeyModifiers::NONE));
            }
            _ => {
                app.message(Message::QueryError(
                    filling.generation,
                    "Fill failed".into(),
                ));
            }
        }
        let late = fetch(&workers, filling);
        assert!(
            app.message(late).is_empty(),
            "Late fill must not restart loading"
        );
        if interrupt == "tab" {
            assert_eq!(app.session.active, 1);
            assert!(app.rows.is_empty());
        } else {
            assert_eq!(
                app.rows.iter().map(DisplayRow::key).collect::<Vec<_>>(),
                original
            );
            assert_eq!(
                app.status,
                if interrupt == "escape" {
                    "Cancelled"
                } else {
                    "Fill failed"
                }
            );
        }
        workers.shutdown();
    }
}

#[test]
fn filling_a_short_last_page_keeps_the_last_event_selected_and_fills_from_earlier_rows() {
    let (_dir, store, keys) = fixture(None);
    let workers = Workers::start(store.root.clone(), vec![], None, None, None, None);
    let mut app = App::default();
    app.message(Message::Ready(ready(&workers), String::new()));
    let last = request(app.key(KeyEvent::new(KeyCode::Char('G'), KeyModifiers::NONE)));
    let filling = app.message(fetch(&workers, last));
    assert_eq!(app.rows.len(), 9);
    finish_fill(&workers, &mut app, filling);
    assert_eq!(app.rows.len() - app.top, app.height);
    assert_eq!(
        app.rows[app.top + app.session.views[0].selected].key(),
        *keys.last().unwrap()
    );
    workers.shutdown();
}

#[test]
fn filling_uses_cached_history_at_the_end_and_saves_the_adjusted_position() {
    let (_dir, store, keys) = fixture(None);
    let workers = Workers::start(store.root.clone(), vec![], None, None, None, None);
    let mut app = App::default();
    let initial = request(app.message(Message::Ready(ready(&workers), String::new())));
    let filling = app.message(fetch(&workers, initial));
    app.height = 5;
    assert!(
        app.key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE))
            .is_empty()
    );
    assert_eq!(app.top, 1);
    app.height = 40;
    finish_fill(&workers, &mut app, filling);
    assert_eq!(
        app.rows.len(),
        keys.len(),
        "Fewer matches exist than fit on the screen"
    );
    assert_eq!(app.top, 0);
    assert_eq!(app.session.views[0].top, Some(keys[0]));
    assert_eq!(app.rows[app.session.views[0].selected].key(), keys[4]);
    assert_eq!(app.status, "Ready");
    workers.shutdown();
}

#[test]
fn shrinking_during_reverse_fill_keeps_the_selected_event_in_view() {
    let (_dir, store, keys) = fixture(None);
    let workers = Workers::start(store.root.clone(), vec![], None, None, None, None);
    let mut app = App::default();
    app.message(Message::Ready(ready(&workers), String::new()));
    let last = request(app.key(KeyEvent::new(KeyCode::Char('G'), KeyModifiers::NONE)));
    let first_fill = request(app.message(fetch(&workers, last)));
    let next_fill = request(app.message(fetch(&workers, first_fill)));
    assert_eq!(app.rows.len(), 10);
    app.height = 12;
    assert!(app.message(fetch(&workers, next_fill)).is_empty());
    assert_eq!(app.rows.len() - app.top, app.height);
    assert_eq!(
        app.rows[app.top + app.session.views[0].selected].key(),
        *keys.last().unwrap()
    );
    workers.shutdown();
}
