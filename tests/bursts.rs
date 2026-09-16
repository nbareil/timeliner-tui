use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{Terminal, backend::TestBackend};
use std::{fs, sync::atomic::Ordering};
use tempfile::TempDir;
use timeliner_tui::{
    burst,
    export::{self, Export},
    ioc::Rules,
    model::{Bookmark, Burst, DisplayRow, EventKey, Row, Session, Tag, View},
    navigation::Jump,
    query::{self, Cancellation, Page},
    source,
    store::Store,
    ui::{Action, App},
    worker::{Command, Message, PageRequest, Workers},
};

const BASE: i64 = 1_600_000_000;

struct Fixture {
    dir: TempDir,
    store: Store,
}

impl Fixture {
    fn new(seconds: &[(i64, usize, usize)]) -> Self {
        let dir = TempDir::new().unwrap();
        let mut text = String::new();
        for &(second, host_a, host_b) in seconds {
            let epoch = BASE + second;
            for index in 0..host_a.max(host_b) {
                for (host, count) in [("host-a", host_a), ("host-b", host_b)] {
                    if index >= count {
                        continue;
                    }
                    let directory = if index % 2 == 0 {
                        "/usr/lib"
                    } else {
                        "/usr/share"
                    };
                    let times = match index % 3 {
                        0 => format!("{epoch}|0|0|0"),
                        1 => format!("0|{epoch}|0|0"),
                        _ => format!("{epoch}|{epoch}|0|0"),
                    };
                    text.push_str(&format!(
                        "{host}|{directory}/file-{index}|{index}|mode|0|0|12|{times}\n"
                    ));
                }
            }
        }
        let body = dir.path().join("timeline.body");
        fs::write(&body, text).unwrap();
        let mut store = Store::open(&dir.path().join("workspace")).unwrap();
        store
            .import(
                &source::resolve(&body, None, true).unwrap(),
                &Rules::default(),
                |_, _| true,
            )
            .unwrap();
        Self { dir, store }
    }

    fn raw(&self, view: &View) -> Vec<Row> {
        let mut result = Vec::new();
        loop {
            let rows = query::page(
                &self.store,
                view,
                Page {
                    anchor: result.last().map(|r: &Row| r.key),
                    ..Page::default()
                },
                &Cancellation::never(),
                |_| {},
            )
            .unwrap();
            if rows.is_empty() {
                break;
            }
            result.extend(rows);
        }
        result
    }

    fn detect(&self, view: &View, origin: &Row, threshold: u64) -> Option<Burst> {
        burst::detect(
            &self.store,
            view,
            origin.key,
            &origin.host,
            threshold,
            &Cancellation::never(),
            |_| {},
        )
        .unwrap()
    }

    fn displayed(&self, view: &View, reverse: bool, limit: usize) -> Vec<DisplayRow> {
        let mut result = Vec::new();
        let mut anchor = None;
        for _ in 0..10_000 {
            let rows = burst::page(
                &self.store,
                view,
                Page {
                    anchor,
                    reverse,
                    limit,
                    inclusive: false,
                },
                &Cancellation::never(),
                |_| {},
            )
            .unwrap();
            if rows.is_empty() {
                return result;
            }
            if reverse {
                let next = rows.first().unwrap().key();
                assert!(anchor.is_none_or(|anchor| next < anchor));
                anchor = Some(next);
                result.splice(0..0, rows);
            } else {
                let next = rows.last().unwrap().key();
                assert!(anchor.is_none_or(|anchor| next > anchor));
                anchor = Some(next);
                result.extend(rows);
            }
        }
        panic!("Paging did not terminate");
    }
}

#[test]
fn collapse_from_any_dense_second_has_identical_boundaries_across_paths_and_macb_types() {
    let fixture = Fixture::new(&[
        (40, 4, 0),
        (41, 180, 0),
        (42, 240, 0),
        (43, 160, 0),
        (44, 5, 0),
    ]);
    let view = View::default();
    let raw = fixture.raw(&view);
    for offset in [4, 93, 183, 184, 320, 423, 424, 500, 583] {
        let burst = fixture.detect(&view, &raw[offset], 100).unwrap();
        assert_eq!(burst.first, raw[4].key);
        assert_eq!(burst.last, raw[583].key);
        assert_eq!(burst.count, 580);
        assert_eq!(burst.origin, raw[offset].key);
        assert_eq!(burst.macb, 3);
        assert!(burst.exclusive);
    }
    assert!(fixture.detect(&view, &raw[0], 100).is_none());
}

#[test]
fn exactly_the_threshold_empty_seconds_and_sparse_dips_end_a_burst() {
    let fixture = Fixture::new(&[(0, 6, 0), (1, 5, 0), (2, 8, 0), (4, 7, 0)]);
    let view = View::default();
    let raw = fixture.raw(&view);
    assert!(fixture.detect(&view, &raw[6], 5).is_none());
    for (offset, count) in [(0, 6), (11, 8), (19, 7)] {
        let burst = fixture.detect(&view, &raw[offset], 5).unwrap();
        assert_eq!(burst.first.epoch, raw[offset].key.epoch);
        assert_eq!(burst.last.epoch, burst.first.epoch);
        assert_eq!(burst.count, count);
    }
}

#[test]
fn density_counts_only_the_selected_host_and_matching_filtered_events() {
    let fixture = Fixture::new(&[(0, 3, 150), (1, 12, 150), (2, 12, 150)]);
    let mut view = View::default();
    let raw = fixture.raw(&view);
    assert!(fixture.detect(&view, &raw[0], 5).is_none());
    let origin = raw
        .iter()
        .find(|r| r.host == "host-a" && r.key.epoch == BASE + 1)
        .unwrap();
    let burst = fixture.detect(&view, origin, 5).unwrap();
    assert_eq!(burst.count, 24);
    assert!(!burst.exclusive);

    view.predicate.include = "^/usr/lib/".into();
    view.predicate.exclude = vec!["file-0$".into()];
    // 6 library events minus one excluded event = exactly the threshold.
    let origin = raw
        .iter()
        .find(|r| r.host == "host-a" && r.key.epoch == BASE + 1 && r.path.ends_with("file-2"))
        .unwrap();
    assert!(fixture.detect(&view, origin, 5).is_none());
    view.predicate.exclude.clear();
    view.before = Some(BASE + 1);
    let burst = fixture.detect(&view, origin, 5).unwrap();
    assert_eq!(burst.count, 6);
    assert_eq!(burst.last.epoch, BASE + 1);
    view.time_mask = 1; // Only four of these library records have modification events.
    assert!(fixture.detect(&view, origin, 5).is_none());
    view.predicate.hosts = vec!["host-b".into()];
    assert!(fixture.detect(&view, origin, 1).is_none());
}

#[test]
fn tag_filters_and_summary_assessments_use_actual_record_state() {
    let fixture = Fixture::new(&[(1, 12, 0)]);
    let mut view = View::default();
    let raw = fixture.raw(&view);
    for row in &raw[..6] {
        fixture
            .store
            .set_tag(row.key.record, Some(Tag::Suspicious))
            .unwrap();
    }
    let summary = fixture.detect(&view, &raw[6], 5).unwrap();
    assert_eq!(summary.tagged, 6);
    view.predicate.tags = Some(vec![Tag::Suspicious]);
    assert!(fixture.detect(&view, &raw[6], 5).is_none());
    let summary = fixture.detect(&view, &raw[0], 5).unwrap();
    assert_eq!(summary.count, 6);
    view.predicate.tags = Some(vec![Tag::Malicious]);
    assert!(fixture.detect(&view, &raw[0], 5).is_none());
}

#[test]
fn folded_pages_preserve_other_hosts_and_are_identical_in_both_directions() {
    for mixed in [false, true] {
        let fixture = Fixture::new(&[
            (0, 2, 1),
            (1, 1100, usize::from(mixed) * 20),
            (2, 1100, usize::from(mixed) * 20),
            (3, 2, 1),
            (5, 10, 2),
        ]);
        let mut view = View::default();
        let raw = fixture.raw(&view);
        for epoch in [BASE + 1, BASE + 5] {
            let origin = raw
                .iter()
                .find(|r| r.key.epoch == epoch && r.host == "host-a")
                .unwrap();
            view.bursts.push(fixture.detect(&view, origin, 5).unwrap());
        }
        let mut expected: Vec<_> = raw
            .iter()
            .filter(|row| {
                !view
                    .bursts
                    .iter()
                    .any(|burst| burst.host == row.host && burst.contains_key(row.key))
            })
            .map(|row| row.key)
            .chain(view.bursts.iter().map(|burst| burst.first))
            .collect();
        expected.sort();
        for reverse in [false, true] {
            for limit in [1, 3, 17, 256] {
                let shown = fixture.displayed(&view, reverse, limit);
                assert_eq!(
                    shown.iter().map(DisplayRow::key).collect::<Vec<_>>(),
                    expected
                );
                assert_eq!(
                    shown
                        .iter()
                        .filter(|r| matches!(r, DisplayRow::Burst(_)))
                        .count(),
                    2
                );
                assert_eq!(
                    shown.iter().filter(|r| r.host() == "host-b").count(),
                    raw.iter().filter(|r| r.host == "host-b").count()
                );
            }
        }
    }
}

#[test]
fn known_host_only_folds_seek_past_hidden_events_and_keep_exports_complete() {
    let fixture = Fixture::new(&[(0, 2, 0), (1, 5000, 0), (2, 2, 0)]);
    // Export intentionally selects assessed records, including folded members.
    fixture
        .store
        .conn
        .execute("UPDATE records SET tag=1", [])
        .unwrap();
    let mut view = View::default();
    let raw = fixture.raw(&view);
    view.bursts
        .push(fixture.detect(&view, &raw[2345], 100).unwrap());
    for reverse in [true, false] {
        let mut examined = 0;
        let shown = burst::page(
            &fixture.store,
            &view,
            Page {
                reverse,
                ..Page::default()
            },
            &Cancellation::never(),
            |n| examined = n,
        )
        .unwrap();
        assert_eq!(shown.len(), 5);
        assert_eq!(
            examined, 4,
            "Hidden run must be skipped by cursor, not rescanned"
        );
    }
    assert_eq!(fixture.raw(&view).len(), 5004);
    let output = fixture.dir.path().join("export.body");
    let count = export::run(
        &fixture.store,
        &Export {
            output: output.clone(),
            view: Some(view),
            tags: vec![],
        },
        &Cancellation::never(),
        |_| {},
    )
    .unwrap();
    assert_eq!(count, 5004);
    assert_eq!(
        fs::read(output).unwrap(),
        fs::read(fixture.dir.path().join("timeline.body")).unwrap()
    );
}

#[test]
fn detection_and_folded_pages_are_cancellable() {
    let fixture = Fixture::new(&[(0, 2000, 0)]);
    let view = View::default();
    let origin = &fixture.raw(&view)[100];
    let cancel = Cancellation::never();
    let clock = cancel.clock.clone();
    assert!(
        burst::detect(
            &fixture.store,
            &view,
            origin.key,
            &origin.host,
            100,
            &cancel,
            move |_| {
                clock.fetch_add(1, Ordering::Relaxed);
            },
        )
        .is_err()
    );
    assert!(burst::page(&fixture.store, &view, Page::default(), &cancel, |_| {}).is_err());
    // Cancellation must not leave a progress handler poisoning later queries.
    assert!(fixture.detect(&view, origin, 100).is_some());
}

fn request(actions: Vec<Action>) -> PageRequest {
    let [Action::Page(request)] = actions.as_slice() else {
        panic!("Expected a query")
    };
    request.clone()
}

fn key(app: &mut App, code: KeyCode) -> Vec<Action> {
    app.key(KeyEvent::new(code, KeyModifiers::NONE))
}

fn apply_query(fixture: &Fixture, app: &mut App, request: PageRequest) -> Vec<Action> {
    let message = if let Some(jump) = request.jump {
        Message::JumpTarget(
            request.generation,
            jump.find(
                &fixture.store,
                &request.view,
                &Cancellation::never(),
                |_| {},
            )
            .unwrap(),
        )
    } else if let Some(detection) = request.detect_burst {
        Message::Burst(
            request.generation,
            burst::detect(
                &fixture.store,
                &request.view,
                detection.origin,
                &detection.host,
                detection.threshold,
                &Cancellation::never(),
                |_| {},
            )
            .unwrap(),
        )
    } else {
        Message::Page(
            request.generation,
            burst::page_with_context(
                &fixture.store,
                &request.view,
                Page {
                    anchor: request.anchor,
                    reverse: request.reverse,
                    inclusive: request.inclusive,
                    limit: request.limit,
                },
                request.context_before,
                &Cancellation::never(),
                |_| {},
            )
            .unwrap(),
        )
    };
    app.message(message)
}

fn loaded(fixture: &Fixture, origin: EventKey) -> App {
    let mut app = App::default();
    app.height = 10;
    let mut session = Session {
        burst_threshold: 5,
        ..Session::default()
    };
    session.views[0].top = Some(origin);
    let initial = request(app.message(Message::Ready(session, String::new())));
    assert!(apply_query(fixture, &mut app, initial).is_empty());
    app
}

fn fold(fixture: &Fixture, app: &mut App) {
    let detection = request(key(app, KeyCode::Char('c')));
    let projection = request(apply_query(fixture, app, detection));
    assert!(apply_query(fixture, app, projection).is_empty());
}

#[test]
fn collapse_keeps_previous_entries_visible_for_k_navigation() {
    let fixture = Fixture::new(&[(0, 4, 0), (1, 180, 0), (2, 240, 0), (3, 5, 0)]);
    let raw = fixture.raw(&View::default());
    let mut app = loaded(&fixture, raw[0].key);
    app.session.views[0].selected = 7;
    fold(&fixture, &mut app);
    assert_eq!(app.rows[app.top].key(), raw[0].key);
    assert!(matches!(
        app.rows[app.top + app.session.views[0].selected],
        DisplayRow::Burst(_)
    ));
    for previous in (0..4).rev() {
        assert!(key(&mut app, KeyCode::Char('k')).is_empty());
        assert_eq!(
            app.rows[app.top + app.session.views[0].selected].key(),
            raw[previous].key
        );
    }
}

#[test]
fn collapse_loads_previous_entries_when_the_whole_cached_page_was_inside_the_burst() {
    let fixture = Fixture::new(&[(0, 4, 0), (1, 1100, 0), (2, 1100, 0), (3, 5, 0)]);
    let raw = fixture.raw(&View::default());
    let mut app = loaded(&fixture, raw[1400].key);
    assert!(app.rows.iter().all(|row| row.key() >= raw[1400].key));
    fold(&fixture, &mut app);
    assert!(matches!(
        app.rows[app.top + app.session.views[0].selected],
        DisplayRow::Burst(_)
    ));
    for previous in (0..4).rev() {
        assert!(key(&mut app, KeyCode::Char('k')).is_empty());
        assert_eq!(
            app.rows[app.top + app.session.views[0].selected].key(),
            raw[previous].key
        );
    }
}

#[test]
fn collapsing_still_allows_paging_back_to_the_first_event() {
    let mut seconds: Vec<_> = (0..400).map(|second| (second, 1, 0)).collect();
    seconds.extend([(400, 180, 0), (401, 240, 0), (402, 5, 0)]);
    let fixture = Fixture::new(&seconds);
    let raw = fixture.raw(&View::default());
    for code in [KeyCode::Up, KeyCode::PageUp] {
        let mut app = loaded(&fixture, raw[600].key);
        fold(&fixture, &mut app);
        for _ in 0..450 {
            let actions = key(&mut app, code);
            if !actions.is_empty() {
                let page = request(actions);
                assert!(page.reverse);
                assert!(apply_query(&fixture, &mut app, page).is_empty());
            }
            if app.rows[app.top].key() == raw[0].key {
                break;
            }
        }
        assert_eq!(app.rows[app.top].key(), raw[0].key);
    }
}

#[test]
fn keyboard_fold_expands_to_original_event_and_bookmark_jumps_reveal_members() {
    let fixture = Fixture::new(&[(0, 2, 0), (1, 1100, 0), (2, 1100, 0), (3, 2, 0)]);
    let raw = fixture.raw(&View::default());
    let origin = raw[1400].key;
    for expand in [KeyCode::Enter, KeyCode::Char('c'), KeyCode::F(3)] {
        let mut app = loaded(&fixture, origin);
        fold(&fixture, &mut app);
        assert!(matches!(
            app.rows[app.top + app.session.views[0].selected],
            DisplayRow::Burst(_)
        ));
        assert_eq!(app.session.views[0].bursts.len(), 1);
        for code in [KeyCode::Tab, KeyCode::Char('x'), KeyCode::Char('X')] {
            assert!(key(&mut app, code).is_empty());
        }
        let expansion = request(key(&mut app, expand));
        apply_query(&fixture, &mut app, expansion);
        assert_eq!(
            app.rows[app.top + app.session.views[0].selected].key(),
            origin
        );
        assert!(app.session.views[0].bursts.is_empty());
        assert!(key(&mut app, KeyCode::Char('k')).is_empty());
        assert_eq!(
            app.rows[app.top + app.session.views[0].selected].key(),
            raw[1399].key
        );
        assert!(key(&mut app, KeyCode::Char('j')).is_empty());

        fold(&fixture, &mut app);
        app.session.bookmarks.push(Bookmark {
            name: 'a',
            key: origin,
        });
        key(&mut app, KeyCode::Char('\''));
        let jump = request(key(&mut app, KeyCode::Char('a')));
        apply_query(&fixture, &mut app, jump);
        assert!(
            app.rows[app.top + app.session.views[0].selected]
                .event()
                .is_some()
        );
        assert_eq!(
            app.rows[app.top + app.session.views[0].selected].key(),
            origin
        );
        assert!(app.session.views[0].bursts.is_empty());
    }
}

#[test]
fn cancelled_discovery_or_projection_leaves_the_existing_view_intact() {
    let fixture = Fixture::new(&[(0, 100, 0)]);
    let origin = fixture.raw(&View::default())[50].key;
    for cancel_projection in [false, true] {
        let mut app = loaded(&fixture, origin);
        let mut pending = request(key(&mut app, KeyCode::Char('c')));
        if cancel_projection {
            pending = request(apply_query(&fixture, &mut app, pending));
        }
        assert!(matches!(
            key(&mut app, KeyCode::Esc).as_slice(),
            [Action::Cancel]
        ));
        assert!(apply_query(&fixture, &mut app, pending).is_empty());
        assert_eq!(app.rows[0].key(), origin);
        assert!(app.session.views[0].bursts.is_empty());
    }
}

#[test]
fn filters_clear_folds_only_after_success_and_folds_are_independent_per_tab() {
    let fixture = Fixture::new(&[(0, 10, 0), (1, 10, 0)]);
    let origin = fixture.raw(&View::default())[15].key;
    let mut app = loaded(&fixture, origin);
    fold(&fixture, &mut app);
    let fork = request(key(&mut app, KeyCode::F(6)));
    apply_query(&fixture, &mut app, fork);
    let copied_tab = app.session.active;
    assert_eq!(app.session.views[copied_tab].bursts.len(), 1);
    key(&mut app, KeyCode::F(5));
    let filter = request(key(&mut app, KeyCode::Enter));
    assert!(filter.view.bursts.is_empty());
    assert_eq!(app.session.views[copied_tab].bursts.len(), 1);
    apply_query(&fixture, &mut app, filter);
    assert!(app.session.views[copied_tab].bursts.is_empty());
    assert_eq!(app.session.views[0].bursts.len(), 1);
    app.message(Message::Changed);
    assert!(app.session.views.iter().all(|v| v.bursts.is_empty()));
}

#[test]
fn threshold_is_editable_validated_and_saved_but_folds_are_temporary() {
    let fixture = Fixture::new(&[(0, 10, 0)]);
    let origin = fixture.raw(&View::default())[4].key;
    let mut app = loaded(&fixture, origin);
    fold(&fixture, &mut app);
    key(&mut app, KeyCode::Char('C'));
    app.key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
    key(&mut app, KeyCode::Char('0'));
    key(&mut app, KeyCode::Enter);
    assert_eq!(app.session.burst_threshold, 5);
    app.key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
    for digit in "250".chars() {
        key(&mut app, KeyCode::Char(digit));
    }
    key(&mut app, KeyCode::Enter);
    assert_eq!(app.session.burst_threshold, 250);
    fixture.store.save_session(&app.session).unwrap();
    let saved = fixture.store.session().unwrap();
    assert_eq!(saved.burst_threshold, 250);
    assert!(saved.views.iter().all(|view| view.bursts.is_empty()));
    let mut legacy = serde_json::to_value(&saved).unwrap();
    legacy.as_object_mut().unwrap().remove("burst_threshold");
    assert_eq!(
        serde_json::from_value::<Session>(legacy)
            .unwrap()
            .burst_threshold,
        100
    );
}

#[test]
fn folded_summary_renders_counts_time_range_and_the_new_quit_control() {
    let fixture = Fixture::new(&[
        (40, 4, 0),
        (41, 180, 1),
        (42, 240, 1),
        (43, 160, 1),
        (44, 5, 0),
    ]);
    let origin = fixture.raw(&View::default())[200].key;
    for theme in [
        timeliner_tui::theme::Theme::Light,
        timeliner_tui::theme::Theme::Dark,
    ] {
        for width in [60, 80, 120, 160] {
            let mut app = loaded(&fixture, origin);
            app.session.theme = theme;
            let mut terminal = Terminal::new(TestBackend::new(width, 22)).unwrap();
            fold(&fixture, &mut app);
            terminal.draw(|frame| app.draw(frame)).unwrap();
            let buffer = terminal.backend().buffer();
            let text = (0..buffer.area.height)
                .map(|y| {
                    (0..buffer.area.width)
                        .map(|x| buffer[(x, y)].symbol())
                        .collect::<String>()
                        .trim_end()
                        .to_owned()
                })
                .collect::<Vec<_>>()
                .join("\n");
            assert!(text.contains("580 events"), "{text}");
            assert!(!text.contains("F10"));
            if width >= 120 {
                assert!(text.contains("host-b"));
            }
            if width == 160
                && theme == timeliner_tui::theme::Theme::Light
                && std::env::var_os("TIMELINER_CAPTURE_BURST").is_some()
            {
                println!("{text}");
            }
        }
    }
}

#[test]
fn worker_executes_discovery_and_folded_paging_on_the_query_thread() {
    let fixture = Fixture::new(&[(0, 8, 0), (1, 9, 0)]);
    let origin = fixture.raw(&View::default())[5].clone();
    let workers = Workers::start(fixture.store.root.clone(), vec![], None, None, None, None);
    loop {
        if matches!(
            workers
                .messages
                .recv_timeout(std::time::Duration::from_secs(5))
                .unwrap(),
            Message::Ready(..)
        ) {
            break;
        }
    }
    let mut app = loaded(&fixture, origin.key);
    let discovery = request(key(&mut app, KeyCode::Char('c')));
    workers.clock.store(discovery.generation, Ordering::Relaxed);
    workers.pages.send(discovery).unwrap();
    let projection = loop {
        let message = workers
            .messages
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap();
        if let Message::Burst(_, Some(ref burst)) = message {
            assert_eq!(burst.count, 17);
            break request(app.message(message));
        }
        assert!(!matches!(
            message,
            Message::Error(_) | Message::QueryError(..)
        ));
    };
    workers
        .clock
        .store(projection.generation, Ordering::Relaxed);
    workers.pages.send(projection).unwrap();
    loop {
        let message = workers
            .messages
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap();
        if matches!(message, Message::Page(..)) {
            app.message(message);
            break;
        }
        assert!(!matches!(
            message,
            Message::Error(_) | Message::QueryError(..)
        ));
    }
    assert!(matches!(app.rows[0], DisplayRow::Burst(_)));
    workers.shutdown();
}

#[test]
fn tagging_another_entry_keeps_the_burst_collapsed() {
    let fixture = Fixture::new(&[(0, 8, 0), (1, 9, 0), (3, 2, 0)]);
    let origin = fixture.raw(&View::default())[5].key;
    let workers = Workers::start(fixture.store.root.clone(), vec![], None, None, None, None);
    loop {
        if matches!(
            workers
                .messages
                .recv_timeout(std::time::Duration::from_secs(5))
                .unwrap(),
            Message::Ready(..)
        ) {
            break;
        }
    }
    let mut app = loaded(&fixture, origin);
    fold(&fixture, &mut app);
    let expected = app.session.views[0].bursts.clone();
    key(&mut app, KeyCode::Char('j'));
    let selected = app.rows[app.top + app.session.views[0].selected].key();
    let actions = key(&mut app, KeyCode::Tab);
    let [Action::Command(Command::Tag(record, tag))] = actions.as_slice() else {
        panic!("Expected assessment of the event after the burst");
    };
    workers.commands.send(Command::Tag(*record, *tag)).unwrap();
    let update = loop {
        let message = workers
            .messages
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap();
        match message {
            Message::Progress(..) => continue,
            Message::Error(error) => panic!("{error}"),
            message => break app.message(message),
        }
    };
    assert_eq!(app.session.views[0].bursts, expected);
    apply_query(&fixture, &mut app, request(update));
    assert_eq!(app.session.views[0].bursts, expected);
    assert!(
        app.rows
            .iter()
            .any(|row| matches!(row, DisplayRow::Burst(_)))
    );
    let selected_row = &app.rows[app.top + app.session.views[0].selected];
    assert_eq!(selected_row.key(), selected);
    assert_eq!(selected_row.event().unwrap().tag, Tag::Suspicious);
    workers.shutdown();
}

fn update_tag(fixture: &Fixture, app: &mut App, record: i64, tag: Option<Tag>) {
    let revision = fixture.store.revision().unwrap();
    let change = fixture.store.set_tag(record, tag).unwrap();
    query::update_caches(&fixture.store, record, revision).unwrap();
    let page = request(app.message(Message::Tagged(change)));
    assert!(apply_query(fixture, app, page).is_empty());
}

#[test]
fn tagging_another_timestamp_of_a_folded_record_repairs_counts_without_expanding() {
    let mut fixture = Fixture::new(&[(1, 8, 0), (2, 9, 0), (4, 2, 0)]);
    let body = fixture.dir.path().join("shared.body");
    fs::write(
        &body,
        format!(
            "host-a|/usr/lib/shared.so|1|mode|0|0|12|{}|{}|{}|0\n",
            BASE + 1,
            BASE + 2,
            BASE + 4,
        ),
    )
    .unwrap();
    fixture
        .store
        .import(
            &source::resolve(&body, None, true).unwrap(),
            &Rules::default(),
            |_, _| true,
        )
        .unwrap();
    let raw = fixture.raw(&View::default());
    let record = raw
        .iter()
        .find(|row| row.path.ends_with("shared.so"))
        .unwrap()
        .key
        .record;
    let mut app = loaded(&fixture, raw[4].key);
    fold(&fixture, &mut app);
    let first = app.session.views[0].bursts[0].first;
    let count = app.session.views[0].bursts[0].count;
    let fork = request(key(&mut app, KeyCode::F(6)));
    apply_query(&fixture, &mut app, fork);
    for (tag, expected) in [
        (Tag::Suspicious, 2),
        (Tag::Ioc, 2),
        (Tag::Malicious, 2),
        (Tag::None, 0),
    ] {
        update_tag(&fixture, &mut app, record, Some(tag));
        for index in [0, 2] {
            let burst = &app.session.views[index].bursts[0];
            assert_eq!(burst.first, first);
            assert_eq!(burst.count, count);
            assert_eq!(burst.tagged, expected);
        }
        let shown = app
            .rows
            .iter()
            .find_map(|row| match row {
                DisplayRow::Burst(burst) => Some(burst),
                _ => None,
            })
            .unwrap();
        assert_eq!(shown.tagged, expected);
    }
}

#[test]
fn tag_filtered_views_only_invalidate_folds_whose_membership_changes() {
    let fixture = Fixture::new(&[(1, 8, 0), (2, 9, 0), (4, 10, 0)]);
    fixture
        .store
        .conn
        .execute("UPDATE records SET tag=1", [])
        .unwrap();
    let mut view = View::default();
    view.predicate.tags = Some(vec![Tag::Suspicious]);
    let raw = fixture.raw(&view);
    view.bursts.push(fixture.detect(&view, &raw[2], 5).unwrap());
    view.bursts
        .push(fixture.detect(&view, &raw[20], 5).unwrap());
    let unaffected = view.bursts[1].clone();
    let change = fixture
        .store
        .set_tag(raw[2].key.record, Some(Tag::Ioc))
        .unwrap();
    burst::apply_tag_change(&mut view, &change).unwrap();
    assert_eq!(view.bursts, [unaffected]);
}

#[test]
fn tagging_an_interleaved_host_makes_its_events_visible_without_unfolding() {
    let fixture = Fixture::new(&[(1, 8, 2), (2, 9, 2)]);
    fixture
        .store
        .conn
        .execute("UPDATE records SET tag=1 WHERE host='host-a'", [])
        .unwrap();
    let mut view = View::default();
    view.predicate.tags = Some(vec![Tag::Suspicious]);
    let raw = fixture.raw(&View::default());
    view.bursts.push(fixture.detect(&view, &raw[0], 5).unwrap());
    assert!(view.bursts[0].exclusive);
    let other = raw.iter().find(|row| row.host == "host-b").unwrap();
    let change = fixture
        .store
        .set_tag(other.key.record, Some(Tag::Suspicious))
        .unwrap();
    burst::apply_tag_change(&mut view, &change).unwrap();
    assert_eq!(view.bursts.len(), 1);
    assert!(!view.bursts[0].exclusive);
    let displayed = fixture.displayed(&view, false, 10);
    assert!(displayed.iter().any(|row| row.key() == other.key));
    assert!(
        displayed
            .iter()
            .any(|row| matches!(row, DisplayRow::Burst(_)))
    );
}

#[test]
fn summary_tag_counts_respect_path_and_timestamp_type_filters() {
    let fixture = Fixture::new(&[(1, 24, 0)]);
    let mut view = View::default();
    view.predicate.include = "^/usr/lib/".into();
    view.time_mask = 2;
    let raw = fixture.raw(&View::default());
    let origin = raw
        .iter()
        .find(|row| row.path == "/usr/lib/file-0")
        .unwrap();
    view.bursts.push(fixture.detect(&view, origin, 5).unwrap());
    for path in ["/usr/share/file-1", "/usr/lib/file-4"] {
        let excluded = raw.iter().find(|row| row.path == path).unwrap();
        let change = fixture
            .store
            .set_tag(excluded.key.record, Some(Tag::Suspicious))
            .unwrap();
        burst::apply_tag_change(&mut view, &change).unwrap();
        assert_eq!(view.bursts[0].tagged, 0);
    }
    let change = fixture
        .store
        .set_tag(origin.key.record, Some(Tag::Suspicious))
        .unwrap();
    burst::apply_tag_change(&mut view, &change).unwrap();
    assert_eq!(view.bursts[0].tagged, 1);
}

#[test]
fn time_jumps_start_after_a_burst_and_reveal_only_the_destination_on_success() {
    let mut seconds: Vec<_> = (0..=65).map(|second| (second, 8, 0)).collect();
    seconds.extend([(121, 9, 0), (122, 9, 0), (500, 10, 0)]);
    let fixture = Fixture::new(&seconds);
    let raw = fixture.raw(&View::default());
    let bursts: Vec<_> = [0, 121, 500]
        .into_iter()
        .map(|second| {
            let origin = raw
                .iter()
                .find(|row| row.key.epoch == BASE + second)
                .unwrap();
            fixture.detect(&View::default(), origin, 5).unwrap()
        })
        .collect();
    for cancel in [false, true] {
        let mut app = loaded(&fixture, bursts[0].first);
        app.session.views[0].bursts = bursts.clone();
        key(&mut app, KeyCode::Char('g'));
        let initial = request(key(&mut app, KeyCode::Char('g')));
        assert!(apply_query(&fixture, &mut app, initial).is_empty());
        assert!(matches!(app.rows[0], DisplayRow::Burst(_)));
        key(&mut app, KeyCode::Char('n'));
        let find = request(key(&mut app, KeyCode::Char('m')));
        // The source burst spans a minute boundary. Jump from its end, not its start.
        assert_eq!(find.jump, Some(Jump::TimeBoundary(BASE + 80)));
        let centered = request(apply_query(&fixture, &mut app, find));
        assert_eq!(centered.anchor, Some(bursts[1].first));
        assert_eq!(
            centered.view.bursts,
            vec![bursts[0].clone(), bursts[2].clone()]
        );
        assert_eq!(app.session.views[0].bursts, bursts);
        if cancel {
            key(&mut app, KeyCode::Esc);
        }
        assert!(apply_query(&fixture, &mut app, centered).is_empty());
        if cancel {
            assert_eq!(app.session.views[0].bursts, bursts);
            assert!(matches!(app.rows[0], DisplayRow::Burst(_)));
        } else {
            let view = &app.session.views[0];
            assert_eq!(view.bursts, vec![bursts[0].clone(), bursts[2].clone()]);
            assert_eq!(app.rows[app.top + view.selected].key(), bursts[1].first);
            assert!(app.rows[app.top + view.selected].event().is_some());
            // The previous summary is still reachable with k.
            key(&mut app, KeyCode::Char('k'));
            assert!(matches!(
                app.rows[app.top + app.session.views[0].selected],
                DisplayRow::Burst(_)
            ));
        }
    }
}

fn worker_query(workers: &Workers, app: &mut App, page: PageRequest) -> Vec<Action> {
    workers.clock.store(page.generation, Ordering::Relaxed);
    workers.pages.send(page).unwrap();
    loop {
        let message = workers
            .messages
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap();
        match &message {
            Message::Page(..) | Message::JumpTarget(..) => return app.message(message),
            Message::Error(error) | Message::QueryError(_, error) => panic!("{error}"),
            _ => {
                app.message(message);
            }
        }
    }
}

#[test]
fn worker_time_jumps_find_the_first_filtered_event_across_a_gap_and_center_it() {
    let fixture = Fixture::new(&[(0, 300, 10), (10, 300, 10), (10000, 20, 10)]);
    let raw = fixture.raw(&View::default());
    for row in &raw {
        if row.path != "/usr/lib/file-4" {
            fixture
                .store
                .set_tag(row.key.record, Some(Tag::Suspicious))
                .unwrap();
        }
    }
    let mut view = View::default();
    view.predicate.hosts = vec!["host-a".into()];
    view.predicate.include = "^/usr/lib/".into();
    view.predicate.exclude = vec!["/file-2$".into()];
    view.predicate.tags = Some(vec![Tag::Suspicious]);
    view.time_mask = 1;
    view.after = Some(BASE);
    let matching = fixture.raw(&view);
    let expected = matching
        .iter()
        .find(|row| row.key.epoch == BASE + 10000)
        .unwrap();
    assert_eq!(expected.path, "/usr/lib/file-8");

    let workers = Workers::start(fixture.store.root.clone(), vec![], None, None, None, None);
    loop {
        match workers
            .messages
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap()
        {
            Message::Ready(..) => break,
            Message::Error(error) => panic!("{error}"),
            _ => {}
        }
    }
    for before in [None, Some(BASE + 5000)] {
        view.before = before;
        let mut app = App::default();
        app.height = 10;
        let mut session = Session::default();
        session.views[0] = view.clone();
        let initial = request(app.message(Message::Ready(session, String::new())));
        assert!(worker_query(&workers, &mut app, initial).is_empty());
        let origin = app.rows[app.top + app.session.views[0].selected].key();
        key(&mut app, KeyCode::Char('n'));
        let find = request(key(&mut app, KeyCode::Char('m')));
        let actions = worker_query(&workers, &mut app, find);
        if before.is_some() {
            assert!(actions.is_empty());
            assert!(app.status.contains("No matching event"));
            assert_eq!(
                app.rows[app.top + app.session.views[0].selected].key(),
                origin
            );
        } else {
            let centered = request(actions);
            assert_eq!(centered.anchor, Some(expected.key));
            assert!(worker_query(&workers, &mut app, centered).is_empty());
            assert_eq!(app.session.views[0].selected, 4);
            assert_eq!(app.rows[app.top + 4].key(), expected.key);
            assert_eq!(app.rows[app.top + 4].event().unwrap().macb & 1, 1);
            assert_eq!(app.session.views[0].predicate, view.predicate);
            key(&mut app, KeyCode::Char('k'));
            assert!(app.rows[app.top + 3].key() < expected.key);
        }
    }
    workers.shutdown();
}
