use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::{Terminal, backend::TestBackend};
use std::{fs, time::Duration};
use timeliner_tui::{
    ioc::Rules,
    model::{Bookmark, DisplayRow, EventKey, Predicate, Row, Tag, View},
    query::{self, Cancellation, Page},
    source,
    store::Store,
    theme::Theme,
    ui::{Action, App},
    worker::{Command, Message, Workers},
};

fn key(app: &mut App, code: KeyCode) -> Vec<Action> {
    app.key(KeyEvent::new(code, KeyModifiers::NONE))
}

fn app() -> App {
    let mut app = App::default();
    app.ready = true;
    app.height = 10;
    app.rows = (0..100)
        .map(|record| {
            DisplayRow::Event(Row {
                key: EventKey {
                    epoch: 1_600_000_000 + record,
                    record,
                },
                macb: 15,
                host: "host".into(),
                path: format!("/tmp/file-{record}"),
                tag: Tag::None,
                automatic: true,
                ioc_match: false,
                is_directory: false,
                zero_size: false,
            })
        })
        .collect();
    app
}

fn select(app: &mut App, records: &[usize]) {
    for &record in records {
        app.session.views[0].selected = record;
        assert!(key(app, KeyCode::Char(' ')).is_empty());
    }
}

fn assert_batch(actions: Vec<Action>, records: &[i64], tag: Option<Tag>) {
    assert!(matches!(actions.as_slice(),
        [Action::Command(Command::TagMany(actual, actual_tag))]
        if actual == records && *actual_tag == tag));
}

fn screen(app: &mut App, width: u16) -> String {
    let mut terminal = Terminal::new(TestBackend::new(width, 24)).unwrap();
    terminal.draw(|frame| app.draw(frame)).unwrap();
    terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(|cell| cell.symbol())
        .collect()
}

#[test]
fn tab_applies_one_tag_to_selected_records_and_all_their_timestamps() {
    let mut app = app();
    app.rows[0].event_mut().unwrap().tag = Tag::Suspicious;
    app.rows[2].event_mut().unwrap().tag = Tag::Malicious;
    app.rows[5].event_mut().unwrap().key.record = 0;
    select(&mut app, &[0, 2]);
    // A focused file outside the selection must not join the batch.
    app.session.views[0].selected = 1;
    assert_batch(key(&mut app, KeyCode::Tab), &[0, 2], Some(Tag::Ioc));
    for index in [0, 2, 5] {
        let row = app.rows[index].event().unwrap();
        assert_eq!(row.tag, Tag::Ioc);
        assert!(!row.automatic);
    }
    assert_eq!(app.rows[1].event().unwrap().tag, Tag::None);
    // Repeated TAB presses use the updated assessment.
    assert_batch(key(&mut app, KeyCode::Tab), &[0, 2], Some(Tag::Malicious));
}

#[test]
fn tab_cycles_the_focused_selected_record_when_assessments_differ() {
    let mut app = app();
    app.rows[2].event_mut().unwrap().tag = Tag::Ioc;
    select(&mut app, &[0, 2]);
    assert_batch(key(&mut app, KeyCode::Tab), &[0, 2], Some(Tag::Malicious));
}

#[test]
fn space_toggles_by_source_record_and_ignores_repeats_releases_and_modifiers() {
    let mut app = app();
    app.rows[1].event_mut().unwrap().key.record = 0;
    select(&mut app, &[0, 2, 1]);
    for kind in [KeyEventKind::Repeat, KeyEventKind::Release] {
        app.key(KeyEvent::new_with_kind(
            KeyCode::Char(' '),
            KeyModifiers::NONE,
            kind,
        ));
    }
    app.key(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::CONTROL));
    assert!(matches!(
        key(&mut app, KeyCode::Tab).as_slice(),
        [Action::Command(Command::Tag(2, Some(Tag::Suspicious)))]
    ));
    assert_eq!(app.rows[0].event().unwrap().tag, Tag::None);
}

#[test]
fn all_f2_assessments_apply_to_the_selection_even_after_paging_away() {
    let mut app = app();
    select(&mut app, &[0, 2]);
    let later = app.rows[50..].to_vec();
    let actions = key(&mut app, KeyCode::Char('G'));
    let [Action::Page(request)] = actions.as_slice() else {
        panic!("Expected page")
    };
    app.message(Message::Page(request.generation, later));
    assert!(app.rows.iter().all(|row| row.key().record >= 50));
    for (code, tag) in [
        ('1', Some(Tag::Suspicious)),
        ('2', Some(Tag::Ioc)),
        ('3', Some(Tag::Malicious)),
        ('0', Some(Tag::None)),
        ('a', None),
    ] {
        key(&mut app, KeyCode::F(2));
        assert_batch(key(&mut app, KeyCode::Char(code)), &[0, 2], tag);
    }
}

#[test]
fn exclusion_escapes_and_deduplicates_paths_and_undo_removes_the_whole_batch() {
    let mut app = app();
    app.rows[0].event_mut().unwrap().path = "/tmp/a[1].log".into();
    app.rows[2].event_mut().unwrap().path = r"C:\temp\b+file".into();
    app.rows[3].event_mut().unwrap().path = "/tmp/a[1].log".into();
    app.session.views[0]
        .predicate
        .exclude
        .push("^/existing$".into());
    select(&mut app, &[0, 2, 3]);
    app.session.views[0].selected = 1;
    let actions = key(&mut app, KeyCode::Char('x'));
    let [Action::Page(request)] = actions.as_slice() else {
        panic!("Expected exclusion")
    };
    assert_eq!(request.view.predicate.exclude.len(), 2);
    let pattern = regex::Regex::new(&request.view.predicate.exclude[1]).unwrap();
    for path in ["/tmp/a[1].log", r"C:\temp\b+file"] {
        assert!(pattern.is_match(path));
    }
    for path in [
        "/tmp/a1.log",
        "/tmp/a[1].log.old",
        r"C:\temp\bbfile",
        "/tmp/file-1",
    ] {
        assert!(!pattern.is_match(path), "{path}");
    }
    let filtered = app
        .rows
        .iter()
        .filter(|row| !pattern.is_match(&row.event().unwrap().path))
        .cloned()
        .collect();
    app.message(Message::Page(request.generation, filtered));
    // Successful exclusions clear the selection.
    assert!(matches!(
        key(&mut app, KeyCode::Tab).as_slice(),
        [Action::Command(Command::Tag(1, Some(Tag::Suspicious)))]
    ));
    let actions = key(&mut app, KeyCode::Char('u'));
    let [Action::Page(request)] = actions.as_slice() else {
        panic!("Expected undo")
    };
    assert_eq!(request.view.predicate.exclude, ["^/existing$"]);
}

#[test]
fn escape_closes_the_dialog_before_clearing_the_selection() {
    let mut app = app();
    select(&mut app, &[0, 2]);
    key(&mut app, KeyCode::F(2));
    key(&mut app, KeyCode::Esc);
    assert_batch(key(&mut app, KeyCode::Tab), &[0, 2], Some(Tag::Suspicious));
    key(&mut app, KeyCode::Esc);
    app.session.views[0].selected = 1;
    assert!(matches!(
        key(&mut app, KeyCode::Tab).as_slice(),
        [Action::Command(Command::Tag(1, Some(Tag::Suspicious)))]
    ));
}

#[test]
fn switching_tabs_and_forking_clear_the_selection() {
    for code in [KeyCode::Char('2'), KeyCode::F(6)] {
        let mut app = app();
        select(&mut app, &[0, 2]);
        let rows = app.rows.clone();
        let actions = key(&mut app, code);
        let [Action::Page(request)] = actions.as_slice() else {
            panic!("Expected tab load")
        };
        app.message(Message::Page(request.generation, rows));
        assert!(
            key(&mut app, KeyCode::Tab)
                .iter()
                .any(|action| matches!(action, Action::Command(Command::Tag(..))))
        );
    }
}

#[test]
fn failed_filters_preserve_selection_and_successful_filters_clear_it() {
    let mut app = app();
    select(&mut app, &[0, 2]);
    key(&mut app, KeyCode::F(5));
    key(&mut app, KeyCode::Char('/'));
    let actions = key(&mut app, KeyCode::Enter);
    let [Action::Page(request)] = actions.as_slice() else {
        panic!("Expected filter")
    };
    app.message(Message::QueryError(request.generation, "Cancelled".into()));
    assert_batch(key(&mut app, KeyCode::Tab), &[0, 2], Some(Tag::Suspicious));
    key(&mut app, KeyCode::F(5));
    key(&mut app, KeyCode::Char('/'));
    let actions = key(&mut app, KeyCode::Enter);
    let [Action::Page(request)] = actions.as_slice() else {
        panic!("Expected filter")
    };
    app.message(Message::Page(request.generation, app.rows.clone()));
    assert!(matches!(
        key(&mut app, KeyCode::Tab).as_slice(),
        [Action::Command(Command::Tag(2, Some(Tag::Ioc)))]
    ));
}

#[test]
fn selection_markers_preserve_bookmarks_and_count_files_in_both_themes() {
    for theme in [Theme::Light, Theme::Dark] {
        for width in [60, 80, 144] {
            let mut app = app();
            app.session.theme = theme;
            app.rows[1].event_mut().unwrap().key.record = 0;
            app.session.bookmarks.push(Bookmark {
                name: 'a',
                key: app.rows[0].key(),
            });
            select(&mut app, &[0, 2]);
            let mut terminal = Terminal::new(TestBackend::new(width, 24)).unwrap();
            terminal.draw(|frame| app.draw(frame)).unwrap();
            let buffer = terminal.backend().buffer();
            assert_eq!(buffer[(0, 5)].symbol(), "●");
            assert_eq!(buffer[(0, 7)].symbol(), "▌");
            for y in 5..8 {
                assert_eq!(buffer[(1, y)].symbol(), "✓");
            }
            assert_eq!(buffer[(1, 8)].symbol(), " ");
            assert!(screen(&mut app, width).contains("2 files selected"));
            key(&mut app, KeyCode::F(2));
            let dialog = screen(&mut app, width);
            assert!(dialog.contains("Assess selected files"));
            assert!(dialog.contains("Applies to 2 selected files."));
        }
    }
}

fn rows(store: &Store, view: &View) -> Vec<Row> {
    query::page(
        store,
        view,
        Page {
            limit: 100,
            ..Page::default()
        },
        &Cancellation::never(),
        |_| {},
    )
    .unwrap()
}

#[test]
fn batch_tagging_is_atomic_deduplicated_and_repairs_smart_tabs() {
    let dir = tempfile::tempdir().unwrap();
    let body = dir.path().join("source.body");
    let original = "0|/ioc|1|mode|0|0|10|100|200|300|400\n\
                    0|/clean|2|mode|0|0|10|100|200|300|400\n\
                    0|/untouched|3|mode|0|0|10|100|200|300|400\n";
    fs::write(&body, original).unwrap();
    let mut store = Store::open(&dir.path().join("workspace")).unwrap();
    let rules = Rules::from_yaml("- path: /ioc").unwrap();
    store
        .import(
            &source::resolve(&body, None, false).unwrap(),
            &rules,
            |_, _| true,
        )
        .unwrap();
    let smart = View {
        predicate: Predicate {
            tags: Some(vec![Tag::Malicious]),
            ..Predicate::default()
        },
        ..View::default()
    };
    assert!(rows(&store, &smart).is_empty());
    let revision = store.revision().unwrap();
    assert!(store.set_tags(&[1, 2, 999], Some(Tag::Malicious)).is_err());
    assert_eq!(store.revision().unwrap(), revision);
    assert_eq!(store.detail(1).unwrap().tag, Tag::Ioc);
    assert_eq!(store.detail(2).unwrap().tag, Tag::None);
    let changes = store.set_tags(&[2, 1, 2], Some(Tag::Malicious)).unwrap();
    assert_eq!(changes.len(), 2);
    assert_eq!(store.revision().unwrap(), revision + 1);
    query::update_caches_for_records(&store, &[2, 1, 2], revision).unwrap();
    let tagged = rows(&store, &smart);
    assert_eq!(tagged.len(), 8);
    assert!(
        tagged
            .iter()
            .all(|row| [1, 2].contains(&row.key.record) && row.tag == Tag::Malicious)
    );
    let revision = store.revision().unwrap();
    store.set_tags(&[1, 2], None).unwrap();
    query::update_caches_for_records(&store, &[1, 2], revision).unwrap();
    assert!(rows(&store, &smart).is_empty());
    assert_eq!(store.detail(1).unwrap().tag, Tag::Ioc);
    assert_eq!(store.detail(2).unwrap().tag, Tag::None);
    assert_eq!(store.detail(3).unwrap().tag, Tag::None);
    assert_eq!(fs::read_to_string(body).unwrap(), original);
}

#[test]
fn worker_commits_the_selection_and_refreshes_the_timeline_once() {
    let dir = tempfile::tempdir().unwrap();
    let body = dir.path().join("source.body");
    fs::write(
        &body,
        "0|/a|1|mode|0|0|10|100|200|300|400\n0|/b|2|mode|0|0|10|100|200|300|400\n",
    )
    .unwrap();
    let workers = Workers::start(
        dir.path().join("workspace"),
        vec![source::resolve(&body, None, false).unwrap()],
        None,
        None,
        None,
        None,
    );
    loop {
        match workers
            .messages
            .recv_timeout(Duration::from_secs(5))
            .unwrap()
        {
            Message::Ready(..) => break,
            Message::Progress(..) => {}
            Message::Error(error) => panic!("{error}"),
            _ => panic!("Unexpected import message"),
        }
    }
    let store = Store::reader(&dir.path().join("workspace")).unwrap();
    let mut app = app();
    app.rows = rows(&store, &View::default())
        .into_iter()
        .map(DisplayRow::Event)
        .collect();
    select(&mut app, &[0, 1]);
    let actions = key(&mut app, KeyCode::Tab);
    for action in actions {
        let Action::Command(command) = action else {
            panic!("Expected command")
        };
        workers.commands.send(command).unwrap();
    }
    let message = workers
        .messages
        .recv_timeout(Duration::from_secs(5))
        .unwrap();
    assert!(matches!(&message, Message::TaggedMany(changes) if changes.len() == 2));
    let actions = app.message(message);
    let [Action::Page(request)] = actions.as_slice() else {
        panic!("Expected one refresh")
    };
    let refreshed = rows(&store, &request.view);
    assert!(refreshed.iter().all(|row| row.tag == Tag::Suspicious));
    app.message(Message::Page(
        request.generation,
        refreshed.into_iter().map(DisplayRow::Event).collect(),
    ));
    assert_batch(key(&mut app, KeyCode::Tab), &[1, 2], Some(Tag::Ioc));
    assert!(workers.messages.try_recv().is_err());
    workers.shutdown();
}
