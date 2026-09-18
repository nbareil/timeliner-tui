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
fn access_only_folds_count_restore_and_assess_only_exact_matching_events() {
    let fixture = Fixture::new(&[(0, 6, 0), (1, 6, 0)]);
    let mut view = View {
        time_mask: 2,
        time_exact: true,
        ..View::default()
    };
    let raw = fixture.raw(&view);
    assert_eq!(raw.len(), 4);
    assert!(fixture.detect(&view, &raw[0], 2).is_none());
    let summary = fixture.detect(&view, &raw[0], 1).unwrap();
    assert_eq!(summary.count, 4);
    assert_eq!(summary.macb, 2);
    view.bursts.push(summary);
    let combined = fixture
        .raw(&View::default())
        .into_iter()
        .find(|row| row.macb == 3)
        .unwrap();
    for (row, expected_tagged) in [(&combined, 0), (&raw[0], 1)] {
        let change = fixture
            .store
            .set_tag(row.key.record, Some(Tag::Suspicious))
            .unwrap();
        burst::apply_tag_change(&mut view, &change).unwrap();
        assert_eq!(view.bursts[0].tagged, expected_tagged);
    }
    let saved = view.bursts.clone();
    burst::restore(&fixture.store, &mut view, &Cancellation::never(), |_| {}).unwrap();
    assert_eq!(view.bursts, saved);
    for reverse in [false, true] {
        let displayed = fixture.displayed(&view, reverse, 2);
        assert_eq!(displayed.len(), 1);
        assert!(matches!(&displayed[0], DisplayRow::Burst(burst) if burst.count == 4));
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
    let mut actions = key(app, KeyCode::Char('c'));
    for _ in 0..10_000 {
        let query = request(actions);
        actions = apply_query(fixture, app, query);
        if actions.is_empty() {
            return;
        }
    }
    panic!("Automatic collapsing did not terminate");
}

#[test]
fn collapse_continues_through_adjacent_bursts_and_stops_at_a_sparse_entry() {
    let fixture = Fixture::new(&[(0, 8, 0), (2, 9, 0), (4, 10, 0), (6, 5, 0), (8, 12, 0)]);
    let raw = fixture.raw(&View::default());
    let mut app = loaded(&fixture, raw[3].key);
    fold(&fixture, &mut app);
    let view = &app.session.views[0];
    assert_eq!(
        view.bursts
            .iter()
            .map(|burst| burst.count)
            .collect::<Vec<_>>(),
        [8, 9, 10]
    );
    assert_eq!(app.rows[app.top + view.selected].key().epoch, BASE + 4);
    assert!(
        app.rows
            .iter()
            .any(|row| row.event().is_some() && row.key().epoch == BASE + 8)
    );
    assert!(app.status.contains("next entry"));
    let mut terminal = Terminal::new(TestBackend::new(160, 22)).unwrap();
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
    for count in [8, 9, 10] {
        assert!(text.contains(&format!("{count} events")), "{text}");
    }
    if std::env::var_os("TIMELINER_CAPTURE_COLLAPSE_CHAIN").is_some() {
        println!("{text}");
    }
}

#[test]
fn collapse_continues_on_an_interleaved_host_and_stops_at_the_end() {
    let fixture = Fixture::new(&[(0, 8, 9), (1, 10, 11)]);
    let raw = fixture.raw(&View::default());
    let mut app = loaded(&fixture, raw[0].key);
    fold(&fixture, &mut app);
    assert_eq!(app.rows.len(), 2);
    assert_eq!(app.session.views[0].bursts.len(), 2);
    for (row, host, count) in [(&app.rows[0], "host-a", 18), (&app.rows[1], "host-b", 20)] {
        let DisplayRow::Burst(summary) = row else {
            panic!("Expected both hosts to collapse");
        };
        assert_eq!(summary.host, host);
        assert_eq!(summary.count, count);
    }
    assert_eq!(app.status, "Ready");
}

#[test]
fn collapse_does_not_skip_a_sparse_first_entry() {
    let fixture = Fixture::new(&[(0, 2, 0), (2, 8, 0)]);
    let mut app = loaded(&fixture, fixture.raw(&View::default())[0].key);
    fold(&fixture, &mut app);
    assert!(app.session.views[0].bursts.is_empty());
    assert!(app.status.contains("No burst here"));
}

#[test]
fn automatic_collapse_stops_before_an_existing_summary() {
    let fixture = Fixture::new(&[(0, 8, 0), (2, 9, 0), (4, 2, 0)]);
    let raw = fixture.raw(&View::default());
    let mut app = loaded(&fixture, raw[8].key);
    fold(&fixture, &mut app);
    let existing = app.session.views[0].bursts[0].clone();
    key(&mut app, KeyCode::Char('g'));
    let first = request(key(&mut app, KeyCode::Char('g')));
    assert!(apply_query(&fixture, &mut app, first).is_empty());
    fold(&fixture, &mut app);
    assert_eq!(app.session.views[0].bursts.len(), 2);
    assert!(app.session.views[0].bursts.contains(&existing));
    assert!(matches!(app.rows[0], DisplayRow::Burst(_)));
    assert!(matches!(app.rows[1], DisplayRow::Burst(_)));
}

#[test]
fn cancelling_an_automatic_followup_keeps_completed_folds_and_ignores_late_results() {
    let fixture = Fixture::new(&[(0, 8, 0), (2, 9, 0)]);
    for cancel_projection in [false, true] {
        let mut app = loaded(&fixture, fixture.raw(&View::default())[0].key);
        let detection = request(key(&mut app, KeyCode::Char('c')));
        let projection = request(apply_query(&fixture, &mut app, detection));
        let mut followup = request(apply_query(&fixture, &mut app, projection));
        assert_eq!(
            followup.detect_burst.as_ref().unwrap().origin.epoch,
            BASE + 2
        );
        if cancel_projection {
            followup = request(apply_query(&fixture, &mut app, followup));
        }
        assert!(matches!(
            key(&mut app, KeyCode::Esc).as_slice(),
            [Action::Cancel]
        ));
        assert!(apply_query(&fixture, &mut app, followup).is_empty());
        assert_eq!(app.session.views[0].bursts.len(), 1);
        assert!(matches!(app.rows[0], DisplayRow::Burst(_)));
        assert!(app.rows[1].event().is_some());
        assert_eq!(app.status, "Cancelled");
        let expansion = request(key(&mut app, KeyCode::Char('c')));
        assert!(apply_query(&fixture, &mut app, expansion).is_empty());
        assert!(app.session.views[0].bursts.is_empty());
    }
}

#[test]
fn automatic_collapse_reserves_room_for_the_next_entry_in_a_full_page() {
    let mut seconds: Vec<_> = (0..1100).map(|second| (second, 1, 0)).collect();
    seconds.extend([(1100, 8, 0), (1102, 9, 0)]);
    let fixture = Fixture::new(&seconds);
    let mut app = loaded(&fixture, fixture.raw(&View::default())[1100].key);
    app.height = 2200;
    fold(&fixture, &mut app);
    assert_eq!(app.session.views[0].bursts.len(), 2);
}

#[test]
fn automatic_collapse_continues_beyond_the_initial_cached_page() {
    let seconds: Vec<_> = (0..150).map(|index| (index * 2, 6, 0)).collect();
    let fixture = Fixture::new(&seconds);
    let mut app = loaded(&fixture, fixture.raw(&View::default())[0].key);
    assert_eq!(app.rows.len(), 128);
    fold(&fixture, &mut app);
    let view = &app.session.views[0];
    assert_eq!(view.bursts.len(), 150);
    assert_eq!(app.rows[app.top + view.selected].key().epoch, BASE + 298);
    assert!(app.rows.len() <= 128);
    fixture.store.save_session(&app.session).unwrap();
    assert_eq!(
        fixture.store.session().unwrap().views[0].bursts,
        view.bursts
    );
}

#[test]
fn a_followup_query_error_stops_collapsing_and_keeps_the_last_summary() {
    let fixture = Fixture::new(&[(0, 8, 0), (2, 9, 0)]);
    let mut app = loaded(&fixture, fixture.raw(&View::default())[0].key);
    let detection = request(key(&mut app, KeyCode::Char('c')));
    let projection = request(apply_query(&fixture, &mut app, detection));
    let followup = request(apply_query(&fixture, &mut app, projection));
    assert!(
        app.message(Message::QueryError(
            followup.generation,
            "Query failed".into()
        ))
        .is_empty()
    );
    assert!(apply_query(&fixture, &mut app, followup).is_empty());
    assert_eq!(app.session.views[0].bursts.len(), 1);
    assert_eq!(app.status, "Query failed");
    let expansion = request(key(&mut app, KeyCode::Char('c')));
    assert!(apply_query(&fixture, &mut app, expansion).is_empty());
    assert!(app.session.views[0].bursts.is_empty());
}

#[test]
fn goto_finds_the_first_filtered_event_at_or_after_the_requested_instant() {
    let fixture = Fixture::new(&[(0, 8, 2), (2, 9, 2), (4, 2, 2)]);
    let mut view = View {
        after: Some(BASE + 1),
        before: Some(BASE + 3),
        time_mask: 1,
        ..View::default()
    };
    view.predicate.include = "/usr/lib/".into();
    view.predicate.hosts = vec!["host-a".into()];
    let expected = fixture.raw(&view)[0].clone();
    for instant in [BASE, BASE + 1, BASE + 2] {
        let target = Jump::Goto(instant)
            .find(&fixture.store, &view, &Cancellation::never(), |_| {})
            .unwrap()
            .unwrap();
        assert_eq!(target.key, expected.key);
        assert_eq!(target.key.epoch, BASE + 2);
    }
    assert!(
        Jump::Goto(BASE + 3)
            .find(&fixture.store, &view, &Cancellation::never(), |_| {})
            .unwrap()
            .is_none()
    );
    view.predicate.tags = Some(vec![Tag::Malicious]);
    assert!(
        Jump::Goto(BASE)
            .find(&fixture.store, &view, &Cancellation::never(), |_| {})
            .unwrap()
            .is_none()
    );
    fixture
        .store
        .set_tag(expected.key.record, Some(Tag::Malicious))
        .unwrap();
    assert_eq!(
        Jump::Goto(BASE)
            .find(&fixture.store, &view, &Cancellation::never(), |_| {})
            .unwrap()
            .unwrap()
            .key,
        expected.key
    );
}

#[test]
fn goto_reveals_a_folded_destination_and_return_restores_the_fold() {
    let fixture = Fixture::new(&[(0, 8, 0), (1, 9, 0), (4, 2, 0)]);
    let raw = fixture.raw(&View::default());
    let mut app = loaded(&fixture, raw[0].key);
    fold(&fixture, &mut app);
    let folded = app.session.views[0].bursts.clone();
    let expected = raw
        .iter()
        .find(|row| row.key.epoch == BASE + 1)
        .unwrap()
        .key;
    let text = chrono::DateTime::from_timestamp(BASE + 1, 0)
        .unwrap()
        .to_rfc3339();
    key(&mut app, KeyCode::Char('g'));
    for character in text.chars() {
        key(&mut app, KeyCode::Char(character));
    }
    let find = request(key(&mut app, KeyCode::Enter));
    assert_eq!(find.jump, Some(Jump::Goto(BASE + 1)));
    let centered = request(apply_query(&fixture, &mut app, find));
    assert!(apply_query(&fixture, &mut app, centered).is_empty());
    let selected = &app.rows[app.top + app.session.views[0].selected];
    assert_eq!(selected.key(), expected);
    assert!(selected.event().is_some());
    assert!(app.session.views[0].bursts.is_empty());
    key(&mut app, KeyCode::Char('\''));
    let previous = request(key(&mut app, KeyCode::Char('\'')));
    assert!(apply_query(&fixture, &mut app, previous).is_empty());
    assert_eq!(app.session.views[0].bursts, folded);
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
    for code in [KeyCode::Up, KeyCode::PageUp, KeyCode::Char('k')] {
        let mut app = loaded(&fixture, raw[600].key);
        fold(&fixture, &mut app);
        for _ in 0..450 {
            let actions = key(&mut app, code);
            if !actions.is_empty() {
                let page = request(actions);
                assert!(page.reverse);
                assert!(apply_query(&fixture, &mut app, page).is_empty());
            }
            let slot = if code == KeyCode::Char('k') {
                app.session.views[0].selected
            } else {
                0
            };
            if app.rows[app.top + slot].key() == raw[0].key {
                break;
            }
        }
        assert_eq!(app.rows[app.top].key(), raw[0].key);
        if code == KeyCode::Char('k') {
            assert_eq!(app.session.views[0].selected, 0);
            assert_eq!(app.session.views[0].bursts.len(), 1);
        }
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
fn shift_enter_shows_only_the_bursts_tagged_events_and_returns_to_the_fold() {
    let mut fixture = Fixture::new(&[(0, 2, 0), (1, 1100, 3), (2, 1100, 3), (3, 2, 0)]);
    fixture
        .store
        .reload_rules(
            &Rules::from_yaml("- path: /usr/lib/file-2").unwrap(),
            |_| true,
        )
        .unwrap();
    let raw = fixture.raw(&View::default());
    let members: Vec<_> = raw
        .iter()
        .filter(|row| row.host == "host-a" && (BASE + 1..=BASE + 2).contains(&row.key.epoch))
        .collect();
    for (index, tag) in [
        (0, Tag::Suspicious),
        (2, Tag::None), // An automatic match whose assessment was cleared.
        (400, Tag::Ioc),
        (2199, Tag::Malicious),
    ] {
        fixture
            .store
            .set_tag(members[index].key.record, Some(tag))
            .unwrap();
    }
    for row in raw
        .iter()
        .filter(|row| row.host == "host-b" || row.key.epoch == BASE || row.key.epoch == BASE + 3)
    {
        fixture
            .store
            .set_tag(row.key.record, Some(Tag::Malicious))
            .unwrap();
    }
    let expected: Vec<_> = fixture
        .raw(&View::default())
        .into_iter()
        .filter(|row| {
            row.host == "host-a"
                && (BASE + 1..=BASE + 2).contains(&row.key.epoch)
                && row.tag != Tag::None
        })
        .map(|row| row.key)
        .collect();
    assert_eq!(expected.len(), 4);
    let mut app = loaded(&fixture, members[1500].key);
    fold(&fixture, &mut app);
    let mut terminal = Terminal::new(TestBackend::new(210, 18)).unwrap();
    terminal.draw(|frame| app.draw(frame)).unwrap();
    let capture = |terminal: &Terminal<TestBackend>| {
        let buffer = terminal.backend().buffer();
        (0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
                    .trim_end()
                    .to_owned()
            })
            .collect::<Vec<_>>()
            .join("\n")
    };
    let folded_capture = capture(&terminal);
    assert!(folded_capture.contains("Shift+Enter tagged"));
    let folded = serde_json::to_value(&app.session.views[0]).unwrap();
    let folded_key = app.rows[app.top + app.session.views[0].selected].key();

    let expansion = request(app.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT)));
    assert_eq!(serde_json::to_value(&app.session.views[0]).unwrap(), folded);
    assert!(apply_query(&fixture, &mut app, expansion).is_empty());
    assert_eq!(
        app.rows.iter().map(DisplayRow::key).collect::<Vec<_>>(),
        expected
    );
    assert_eq!(app.top + app.session.views[0].selected, 0);
    assert!(app.session.views[0].bursts.is_empty());
    assert!(app.status.contains("'' returns"));
    terminal.draw(|frame| app.draw(frame)).unwrap();
    if std::env::var_os("TIMELINER_CAPTURE_TAGGED_BURST").is_some() {
        println!(
            "Folded burst:\n{folded_capture}\n\nAfter Shift+Enter:\n{}",
            capture(&terminal)
        );
    }

    key(&mut app, KeyCode::Char('\''));
    let back = request(key(&mut app, KeyCode::Char('\'')));
    assert!(apply_query(&fixture, &mut app, back).is_empty());
    assert_eq!(serde_json::to_value(&app.session.views[0]).unwrap(), folded);
    assert_eq!(
        app.rows[app.top + app.session.views[0].selected].key(),
        folded_key
    );
    assert!(matches!(
        app.rows[app.top + app.session.views[0].selected],
        DisplayRow::Burst(_)
    ));
}

#[test]
fn shift_enter_intersects_existing_filters_and_keeps_paging_within_the_burst() {
    let fixture = Fixture::new(&[(0, 2, 0), (1, 1200, 2), (2, 1200, 2), (3, 2, 0)]);
    for row in fixture.raw(&View::default()) {
        let index: usize = row.path.rsplit('-').next().unwrap().parse().unwrap();
        fixture
            .store
            .set_tag(
                row.key.record,
                Some([Tag::None, Tag::Suspicious, Tag::Ioc, Tag::Malicious][index / 2 % 4]),
            )
            .unwrap();
    }
    for tags in [
        None,
        Some(vec![]),
        Some(vec![Tag::None, Tag::Ioc]),
        Some(vec![Tag::Ioc]),
    ] {
        let mut view = View {
            after: Some(BASE + 1),
            before: Some(BASE + 2),
            time_mask: 1,
            ..View::default()
        };
        view.predicate.include = "^/usr/lib/".into();
        view.predicate.exclude = vec!["file-0$".into()];
        view.predicate.hosts = vec!["host-a".into(), "host-b".into()];
        view.predicate.tags = tags.clone();
        let matching = fixture.raw(&view);
        let expected: Vec<_> = matching
            .iter()
            .filter(|row| row.host == "host-a" && row.tag != Tag::None)
            .map(|row| row.key)
            .collect();
        assert!(expected.len() > 128);
        view.top = Some(expected[expected.len() / 2]);
        let mut app = App::default();
        app.height = 10;
        let initial = request(app.message(Message::Ready(
            Session {
                views: vec![view.clone()],
                burst_threshold: 5,
                ..Session::default()
            },
            String::new(),
        )));
        apply_query(&fixture, &mut app, initial);
        fold(&fixture, &mut app);
        let expansion = request(app.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT)));
        assert_eq!(expansion.view.predicate.include, view.predicate.include);
        assert_eq!(expansion.view.predicate.exclude, view.predicate.exclude);
        assert_eq!(expansion.view.time_mask, view.time_mask);
        assert_eq!(
            expansion.view.predicate.tags,
            Some(if tags.as_ref().is_some_and(|tags| !tags.is_empty()) {
                vec![Tag::Ioc]
            } else {
                vec![]
            })
        );
        apply_query(&fixture, &mut app, expansion);
        for reverse in [false, true] {
            assert_eq!(
                fixture
                    .displayed(&app.session.views[0], reverse, 128)
                    .iter()
                    .map(DisplayRow::key)
                    .collect::<Vec<_>>(),
                expected
            );
        }
        let last = request(key(&mut app, KeyCode::Char('G')));
        apply_query(&fixture, &mut app, last);
        assert_eq!(
            app.rows[app.top + app.session.views[0].selected].key(),
            *expected.last().unwrap()
        );
    }
}

#[test]
fn cancelled_failed_or_empty_tagged_expansion_keeps_the_fold_and_return_history() {
    let fixture = Fixture::new(&[(0, 20, 0)]);
    let raw = fixture.raw(&View::default());
    fixture
        .store
        .set_tag(raw[2].key.record, Some(Tag::Ioc))
        .unwrap();
    for outcome in ["cancel", "error", "empty"] {
        let mut app = loaded(&fixture, raw[10].key);
        fold(&fixture, &mut app);
        let folded = serde_json::to_value(&app.session.views[0]).unwrap();
        let expansion = request(app.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT)));
        // A second press while loading must not replace the pending request.
        assert!(
            app.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT))
                .is_empty()
        );
        match outcome {
            "cancel" => {
                key(&mut app, KeyCode::Esc);
                apply_query(&fixture, &mut app, expansion);
            }
            "error" => {
                app.message(Message::QueryError(
                    expansion.generation,
                    "test error".into(),
                ));
            }
            "empty" => {
                app.message(Message::Page(expansion.generation, vec![]));
                assert!(app.status.contains("No tagged events"));
            }
            _ => unreachable!(),
        }
        assert_eq!(serde_json::to_value(&app.session.views[0]).unwrap(), folded);
        assert!(matches!(
            app.rows[app.top + app.session.views[0].selected],
            DisplayRow::Burst(_)
        ));
        key(&mut app, KeyCode::Char('\''));
        assert!(key(&mut app, KeyCode::Char('\'')).is_empty());
    }
}

#[test]
fn shift_enter_on_an_untagged_burst_keeps_it_collapsed() {
    let fixture = Fixture::new(&[(0, 20, 0)]);
    let origin = fixture.raw(&View::default())[10].key;
    let mut app = loaded(&fixture, origin);
    fold(&fixture, &mut app);
    let folded = serde_json::to_value(&app.session.views[0]).unwrap();
    assert!(
        app.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT))
            .is_empty()
    );
    assert_eq!(app.status, "No tagged events in this burst");
    assert_eq!(serde_json::to_value(&app.session.views[0]).unwrap(), folded);
    // Ordinary Enter still expands every member.
    let expansion = request(key(&mut app, KeyCode::Enter));
    apply_query(&fixture, &mut app, expansion);
    assert_eq!(
        fixture.displayed(&app.session.views[0], false, 128).len(),
        20
    );
    assert_eq!(
        app.rows[app.top + app.session.views[0].selected].key(),
        origin
    );
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
fn threshold_is_editable_and_saved_folds_survive_threshold_changes() {
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
    assert_eq!(saved.views[0].bursts, app.session.views[0].bursts);
    let mut legacy = serde_json::to_value(&saved).unwrap();
    legacy.as_object_mut().unwrap().remove("burst_threshold");
    assert_eq!(
        serde_json::from_value::<Session>(legacy)
            .unwrap()
            .burst_threshold,
        100
    );
}

fn ready_session(workers: &Workers) -> Session {
    loop {
        match workers
            .messages
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap()
        {
            Message::Ready(session, _) => return session,
            Message::Progress(..) => {}
            Message::Error(error) => panic!("{error}"),
            _ => panic!("Unexpected message while restoring the workspace"),
        }
    }
}

#[test]
fn collapsed_groups_survive_restart_and_expansion_stays_saved_per_tab() {
    let fixture = Fixture::new(&[(0, 8, 0), (1, 9, 0)]);
    let origin = fixture.raw(&View::default())[4].key;
    let mut app = loaded(&fixture, origin);
    fold(&fixture, &mut app);
    let fork = request(key(&mut app, KeyCode::F(6)));
    apply_query(&fixture, &mut app, fork);
    let copy = app.session.active;
    let expected = app.session.views[copy].bursts.clone();
    app.session.burst_threshold = 250;
    let workers = Workers::start(fixture.store.root.clone(), vec![], None, None, None, None);
    ready_session(&workers);
    workers
        .commands
        .send(Command::Save(app.session.clone()))
        .unwrap();
    workers.shutdown();

    let workers = Workers::start(fixture.store.root.clone(), vec![], None, None, None, None);
    let session = ready_session(&workers);
    assert_eq!(session.views[0].bursts, expected);
    assert_eq!(session.views[copy].bursts, expected);
    assert_eq!(session.burst_threshold, 250);
    let mut resumed = App::default();
    let initial = request(resumed.message(Message::Ready(session, String::new())));
    apply_query(&fixture, &mut resumed, initial);
    assert!(matches!(resumed.rows[0], DisplayRow::Burst(_)));
    let expanded = request(key(&mut resumed, KeyCode::Char('c')));
    apply_query(&fixture, &mut resumed, expanded);
    assert_eq!(
        resumed.rows[resumed.top + resumed.session.views[copy].selected].key(),
        origin
    );
    workers
        .commands
        .send(Command::Save(resumed.session))
        .unwrap();
    workers.shutdown();

    let workers = Workers::start(fixture.store.root.clone(), vec![], None, None, None, None);
    let session = ready_session(&workers);
    assert!(session.views[copy].bursts.is_empty());
    assert_eq!(session.views[0].bursts, expected);
    workers.shutdown();
}

#[test]
fn restored_summaries_refresh_counts_tags_iocs_and_new_interleaved_hosts() {
    let mut fixture = Fixture::new(&[(0, 8, 0), (1, 9, 0)]);
    let raw = fixture.raw(&View::default());
    let mut app = loaded(&fixture, raw[4].key);
    fold(&fixture, &mut app);
    fixture.store.save_session(&app.session).unwrap();
    fixture
        .store
        .set_tag(raw[1].key.record, Some(Tag::Malicious))
        .unwrap();
    fixture
        .store
        .reload_rules(
            &Rules::from_yaml("- path: /usr/lib/file-0").unwrap(),
            |_| true,
        )
        .unwrap();
    let added = fixture.dir.path().join("extra.body");
    fs::write(
        &added,
        format!(
            "host-a|/new-a|1|mode|0|0|1|{BASE}|0|0|0\nhost-b|/new-b|2|mode|0|0|1|{BASE}|0|0|0\n"
        ),
    )
    .unwrap();
    fixture
        .store
        .import(
            &source::resolve(&added, None, true).unwrap(),
            &Rules::default(),
            |_, _| true,
        )
        .unwrap();
    let expected: Vec<_> = fixture
        .raw(&View::default())
        .into_iter()
        .filter(|row| row.host == "host-a")
        .collect();
    let workers = Workers::start(fixture.store.root.clone(), vec![], None, None, None, None);
    let session = ready_session(&workers);
    let summary = &session.views[0].bursts[0];
    assert_eq!(summary.count, expected.len() as u64);
    assert_eq!(
        summary.tagged,
        expected.iter().filter(|row| row.tag != Tag::None).count() as u64
    );
    assert_eq!(
        summary.ioc_matches,
        expected.iter().filter(|row| row.ioc_match).count() as u64
    );
    assert!(!summary.exclusive);
    let displayed = fixture.displayed(&session.views[0], false, 128);
    assert!(displayed.iter().any(|row| row.host() == "host-b"));
    workers.shutdown();
}

#[test]
fn legacy_sessions_load_without_folds_and_restore_is_cancellable() {
    let fixture = Fixture::new(&[(0, 8, 0)]);
    let mut app = loaded(&fixture, fixture.raw(&View::default())[0].key);
    fold(&fixture, &mut app);
    let mut legacy = serde_json::to_value(&app.session).unwrap();
    for view in legacy["views"].as_array_mut().unwrap() {
        view.as_object_mut().unwrap().remove("bursts");
    }
    let legacy: Session = serde_json::from_value(legacy).unwrap();
    assert!(legacy.views.iter().all(|view| view.bursts.is_empty()));
    let expected = app.session.views[0].bursts.clone();
    let cancel = Cancellation::never();
    cancel.clock.fetch_add(1, Ordering::Relaxed);
    assert!(burst::restore(&fixture.store, &mut app.session.views[0], &cancel, |_| {}).is_err());
    assert_eq!(app.session.views[0].bursts, expected);
    app.session.views[0].predicate.include = "^/missing$".into();
    burst::restore(
        &fixture.store,
        &mut app.session.views[0],
        &Cancellation::never(),
        |_| {},
    )
    .unwrap();
    assert!(app.session.views[0].bursts.is_empty());
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
            assert!(text.contains("F10") && text.contains("Mouse on"), "{text}");
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
fn worker_executes_successive_discoveries_and_folded_paging_on_the_query_thread() {
    let fixture = Fixture::new(&[(0, 8, 0), (2, 9, 0), (4, 2, 0)]);
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
    let mut actions = key(&mut app, KeyCode::Char('c'));
    let mut detected = Vec::new();
    let mut queries = 0;
    while !actions.is_empty() {
        queries += 1;
        assert!(queries <= 5, "Automatic collapsing did not terminate");
        let query = request(actions);
        workers.clock.store(query.generation, Ordering::Relaxed);
        workers.pages.send(query).unwrap();
        actions = loop {
            let message = workers
                .messages
                .recv_timeout(std::time::Duration::from_secs(5))
                .unwrap();
            assert!(!matches!(
                message,
                Message::Error(_) | Message::QueryError(..)
            ));
            if let Message::Burst(_, ref burst) = message {
                detected.push(burst.as_ref().map(|burst| burst.count));
            }
            let complete = matches!(message, Message::Burst(..) | Message::Page(..));
            let next = app.message(message);
            if complete {
                break next;
            }
        }
    }
    assert_eq!(detected, [Some(8), Some(9), None]);
    assert!(matches!(app.rows[0], DisplayRow::Burst(_)));
    assert!(matches!(app.rows[1], DisplayRow::Burst(_)));
    assert!(app.rows[2].event().is_some());
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
fn batch_tagging_selected_members_updates_a_fold_without_expanding_it() {
    let fixture = Fixture::new(&[(0, 8, 0), (1, 9, 0)]);
    let origin = fixture.raw(&View::default())[0].key;
    let mut app = loaded(&fixture, origin);
    key(&mut app, KeyCode::Char(' '));
    key(&mut app, KeyCode::Char('j'));
    key(&mut app, KeyCode::Char(' '));
    fold(&fixture, &mut app);
    assert_eq!(app.session.views[0].bursts[0].tagged, 0);
    // A summary cannot itself join the selection.
    key(&mut app, KeyCode::Char(' '));
    let actions = key(&mut app, KeyCode::Tab);
    let [Action::Command(Command::TagMany(records, tag))] = actions.as_slice() else {
        panic!("Expected assessment of the selected members")
    };
    assert_eq!(records.len(), 2);
    let revision = fixture.store.revision().unwrap();
    let changes = fixture.store.set_tags(records, *tag).unwrap();
    query::update_caches_for_records(&fixture.store, records, revision).unwrap();
    let page = request(app.message(Message::TaggedMany(changes)));
    assert!(apply_query(&fixture, &mut app, page).is_empty());
    assert_eq!(app.session.views[0].bursts.len(), 1);
    let DisplayRow::Burst(burst) = &app.rows[app.top + app.session.views[0].selected] else {
        panic!("Expected the updated summary")
    };
    assert_eq!(burst.tagged, 2);
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
            key(&mut app, KeyCode::Char('b'));
            let find = request(key(&mut app, KeyCode::Char('m')));
            let centered = request(worker_query(&workers, &mut app, find));
            let previous = matching
                .iter()
                .rev()
                .find(|row| row.key.epoch == BASE + 10)
                .unwrap();
            assert_eq!(centered.anchor, Some(previous.key));
            assert!(worker_query(&workers, &mut app, centered).is_empty());
            assert_eq!(app.session.views[0].selected, 4);
            assert_eq!(app.rows[app.top + 4].key(), previous.key);
            assert_eq!(app.session.views[0].predicate, view.predicate);
            key(&mut app, KeyCode::Char('k'));
            assert!(app.rows[app.top + 3].key() < expected.key);
        }
    }
    workers.shutdown();
}

#[test]
fn tagged_jumps_advance_within_a_second_and_intersect_current_filters() {
    let fixture = Fixture::new(&[(0, 14, 2), (60, 3, 0)]);
    let raw = fixture.raw(&View::default());
    let host_a: Vec<_> = raw.iter().filter(|row| row.host == "host-a").collect();
    for (index, tag) in [
        (0, Tag::Suspicious),
        (2, Tag::Ioc),
        (4, Tag::Malicious),
        (6, Tag::Suspicious),
        (14, Tag::Ioc),
    ] {
        fixture
            .store
            .set_tag(host_a[index].key.record, Some(tag))
            .unwrap();
    }
    let find = |index: usize, view: &View| {
        Jump::TaggedAfter(host_a[index].key)
            .find(&fixture.store, view, &Cancellation::never(), |_| {})
            .unwrap()
            .map(|row| row.key)
    };
    let view = View::default();
    for (origin, next) in [(0, 2), (2, 4), (4, 6), (6, 14)] {
        assert_eq!(find(origin, &view), Some(host_a[next].key));
    }
    assert_eq!(find(14, &view), None);
    assert!(view.predicate.tags.is_none());
    for (origin, previous) in [(2, 0), (4, 2), (6, 4), (14, 6)] {
        let target = Jump::TaggedBefore(host_a[origin].key)
            .find(&fixture.store, &view, &Cancellation::never(), |_| {})
            .unwrap()
            .unwrap();
        assert_eq!(target.key, host_a[previous].key);
    }

    for tags in [
        None,
        Some(vec![]),
        Some(vec![Tag::None, Tag::Ioc]),
        Some(vec![Tag::Ioc]),
    ] {
        let mut filtered = view.clone();
        filtered.predicate.tags = tags;
        assert_eq!(find(0, &filtered), Some(host_a[2].key));
    }
    let mut filtered = view.clone();
    filtered.predicate.tags = Some(vec![Tag::Ioc]);
    assert_eq!(find(2, &filtered), Some(host_a[14].key));
    filtered.before = Some(BASE + 30);
    assert_eq!(find(2, &filtered), None);
    filtered.predicate.tags = Some(vec![Tag::None]);
    assert_eq!(find(0, &filtered), None);

    let mut filtered = view.clone();
    filtered.predicate.hosts = vec!["host-b".into()];
    assert_eq!(find(0, &filtered), None);
    filtered.predicate.hosts.clear();
    filtered.predicate.include = "^/usr/lib/".into();
    filtered.predicate.exclude = vec!["/file-2$".into()];
    assert_eq!(find(0, &filtered), Some(host_a[4].key));
    filtered.time_mask = 2;
    assert_eq!(find(0, &filtered), Some(host_a[6].key));
    // Clearing a tag must affect the next jump, including cached filtered views.
    fixture
        .store
        .set_tag(host_a[2].key.record, Some(Tag::None))
        .unwrap();
    assert_eq!(find(0, &view), Some(host_a[4].key));
}

#[test]
fn tagged_jumps_from_a_summary_skip_its_members_and_reveal_the_next_tagged_member() {
    let fixture = Fixture::new(&[(0, 20, 0), (1, 20, 0), (60, 20, 0), (120, 20, 0)]);
    let raw = fixture.raw(&View::default());
    let target = &raw[47];
    for row in [&raw[10], target] {
        fixture
            .store
            .set_tag(row.key.record, Some(Tag::Ioc))
            .unwrap();
    }
    let bursts: Vec<_> = [0, 60, 120]
        .into_iter()
        .map(|second| {
            let origin = raw
                .iter()
                .find(|row| row.key.epoch == BASE + second)
                .unwrap();
            fixture.detect(&View::default(), origin, 5).unwrap()
        })
        .collect();
    let mut app = loaded(&fixture, bursts[0].first);
    app.session.views[0].bursts = bursts.clone();
    key(&mut app, KeyCode::Char('g'));
    let initial = request(key(&mut app, KeyCode::Char('g')));
    apply_query(&fixture, &mut app, initial);
    key(&mut app, KeyCode::Char('n'));
    let find = request(key(&mut app, KeyCode::Char('t')));
    assert_eq!(find.jump, Some(Jump::TaggedAfter(bursts[0].last)));
    let centered = request(apply_query(&fixture, &mut app, find));
    assert_eq!(centered.anchor, Some(target.key));
    apply_query(&fixture, &mut app, centered);
    let view = &app.session.views[0];
    assert_eq!(view.bursts, vec![bursts[0].clone(), bursts[2].clone()]);
    assert!(view.predicate.tags.is_none());
    assert_eq!(view.selected, 4);
    assert_eq!(app.rows[app.top + 4].key(), target.key);
    assert_eq!(app.rows[app.top + 3].event().unwrap().tag, Tag::None);
    // No later tagged event: preserve both the position and remaining folds.
    let top = app.top;
    key(&mut app, KeyCode::Char('n'));
    let find = request(key(&mut app, KeyCode::Char('t')));
    assert!(apply_query(&fixture, &mut app, find).is_empty());
    assert!(app.status.contains("No later tagged event"));
    assert_eq!(app.top, top);
    assert_eq!(
        app.session.views[0].bursts,
        vec![bursts[0].clone(), bursts[2].clone()]
    );
}

#[test]
fn worker_tagged_jump_centers_a_same_second_target_with_untagged_context() {
    let fixture = Fixture::new(&[(0, 20, 0)]);
    let raw = fixture.raw(&View::default());
    for row in [&raw[0], &raw[11]] {
        fixture
            .store
            .set_tag(row.key.record, Some(Tag::Ioc))
            .unwrap();
    }
    let workers = Workers::start(fixture.store.root.clone(), vec![], None, None, None, None);
    let mut app = App::default();
    app.height = 10;
    let initial = loop {
        let message = workers
            .messages
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap();
        match message {
            Message::Ready(..) => break request(app.message(message)),
            Message::Error(error) => panic!("{error}"),
            _ => {}
        }
    };
    worker_query(&workers, &mut app, initial);
    key(&mut app, KeyCode::Char('n'));
    let find = request(key(&mut app, KeyCode::Char('t')));
    let centered = request(worker_query(&workers, &mut app, find));
    assert_eq!(centered.anchor, Some(raw[11].key));
    worker_query(&workers, &mut app, centered);
    assert_eq!(app.session.views[0].selected, 4);
    assert!(app.session.views[0].predicate.tags.is_none());
    assert_eq!(app.rows[app.top + 4].key(), raw[11].key);
    for slot in [3, 5] {
        assert_eq!(app.rows[app.top + slot].event().unwrap().tag, Tag::None);
    }
    key(&mut app, KeyCode::Char('b'));
    let find = request(key(&mut app, KeyCode::Char('t')));
    let centered = request(worker_query(&workers, &mut app, find));
    assert_eq!(centered.anchor, Some(raw[0].key));
    worker_query(&workers, &mut app, centered);
    assert_eq!(
        app.rows[app.top + app.session.views[0].selected].key(),
        raw[0].key
    );
    assert_eq!(app.session.views[0].selected, 0);
    key(&mut app, KeyCode::Char('b'));
    let find = request(key(&mut app, KeyCode::Char('t')));
    assert!(worker_query(&workers, &mut app, find).is_empty());
    assert!(app.status.contains("No earlier tagged event"));
    workers.shutdown();
}

#[test]
fn backward_jumps_from_a_burst_skip_its_members_and_reveal_only_the_destination() {
    let fixture = Fixture::new(&[(0, 20, 0), (60, 20, 0), (120, 20, 0)]);
    let raw = fixture.raw(&View::default());
    for row in [&raw[35], &raw[45]] {
        fixture
            .store
            .set_tag(row.key.record, Some(Tag::Ioc))
            .unwrap();
    }
    let bursts: Vec<_> = [0, 60, 120]
        .into_iter()
        .map(|second| {
            let origin = raw
                .iter()
                .find(|row| row.key.epoch == BASE + second)
                .unwrap();
            fixture.detect(&View::default(), origin, 5).unwrap()
        })
        .collect();
    // An inclusive reverse time seek picks the final event at a tied boundary.
    assert_eq!(
        Jump::PreviousTimeBoundary(BASE + 60)
            .find(
                &fixture.store,
                &View::default(),
                &Cancellation::never(),
                |_| {}
            )
            .unwrap()
            .unwrap()
            .key,
        raw[39].key,
    );
    for (suffix, target) in [('m', &raw[39]), ('t', &raw[35])] {
        let mut app = loaded(&fixture, bursts[2].first);
        app.session.views[0].bursts = bursts.clone();
        key(&mut app, KeyCode::Char('g'));
        let initial = request(key(&mut app, KeyCode::Char('g')));
        apply_query(&fixture, &mut app, initial);
        app.session.views[0].selected = 2;
        key(&mut app, KeyCode::Char('b'));
        let find = request(key(&mut app, KeyCode::Char(suffix)));
        assert_eq!(
            find.jump,
            Some(if suffix == 't' {
                Jump::TaggedBefore(bursts[2].first)
            } else {
                Jump::PreviousTimeBoundary(BASE + 80)
            })
        );
        let centered = request(apply_query(&fixture, &mut app, find));
        assert_eq!(centered.anchor, Some(target.key));
        apply_query(&fixture, &mut app, centered);
        assert_eq!(
            app.session.views[0].bursts,
            vec![bursts[0].clone(), bursts[2].clone()]
        );
        assert_eq!(app.session.views[0].selected, 4);
        assert_eq!(app.rows[app.top + 4].key(), target.key);
        assert!(app.rows[app.top + 4].event().is_some());
    }
}
