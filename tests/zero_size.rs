use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{Terminal, backend::TestBackend, style::Modifier};
use std::fs;
use tempfile::TempDir;
use timeliner_tui::{
    ioc::Rules,
    model::{DisplayRow, Row, Tag, View},
    query::{self, Cancellation, Page},
    source,
    store::Store,
    theme::Theme,
    ui::{Action, App},
};

fn fixture() -> (TempDir, Store) {
    let dir = tempfile::tempdir().unwrap();
    let body = dir.path().join("source.body");
    let contents = [
        ("/tmp/zero", "0", "-rw-r--r--"),
        ("/tmp/nonzero", "1", "-rw-r--r--"),
        ("/tmp/padded-zero", "000", "-rw-r--r--"),
        ("/proc/directory", "0", "d/drwxr-xr-x"),
        (r"C:\Temp\zero", "0", "r/rrw-r--r--"),
        ("/tmp/huge", "18446744073709551615", "-rw-r--r--"),
    ]
    .map(|(path, size, mode)| format!("0|{path}|1|{mode}|0|0|{size}|100|100|100|100\n"))
    .concat();
    fs::write(&body, contents).unwrap();
    let mut store = Store::open(&dir.path().join("workspace")).unwrap();
    store
        .import(
            &source::resolve(&body, None, false).unwrap(),
            &Rules::default(),
            |_, _| true,
        )
        .unwrap();
    (dir, store)
}

fn rows(store: &Store, view: &View) -> Vec<Row> {
    query::page(store, view, Page::default(), &Cancellation::never(), |_| {}).unwrap()
}

fn assert_sizes(rows: &[Row]) {
    assert_eq!(
        rows.iter().map(|row| row.zero_size).collect::<Vec<_>>(),
        [true, false, true, true, true, false]
    );
    assert!(rows[3].is_directory);
}

#[test]
fn zero_size_uses_recorded_bytes_and_survives_restart_without_sources() {
    let (dir, store) = fixture();
    let original = fs::read(dir.path().join("source.body")).unwrap();
    let result = rows(&store, &View::default());
    assert_sizes(&result);
    assert_eq!(result[0].path, "/tmp/zero");
    assert_eq!(result[4].path, r"C:\Temp\zero");
    assert!(
        result
            .iter()
            .all(|row| row.tag == Tag::None && !row.ioc_match)
    );
    let mut view = View::default();
    view.predicate.include = "^/tmp/zero$".into();
    assert_eq!(rows(&store, &view).len(), 1);
    view.predicate.exclude.push("^/tmp/zero$".into());
    assert!(rows(&store, &view).is_empty());
    assert_eq!(fs::read(dir.path().join("source.body")).unwrap(), original);

    let root = store.root.clone();
    drop(store);
    fs::remove_file(dir.path().join("source.body")).unwrap();
    let store = Store::open(&root).unwrap();
    assert_sizes(&rows(&store, &View::default()));
}

#[test]
fn older_indexes_read_missing_size_metadata_without_mistaking_unknown_for_zero() {
    for old_directory_metadata in [false, true] {
        let (dir, store) = fixture();
        store
            .conn
            .execute("ALTER TABLE records DROP COLUMN zero_size", [])
            .unwrap();
        if old_directory_metadata {
            store
                .conn
                .execute("ALTER TABLE records DROP COLUMN is_directory", [])
                .unwrap();
        }
        let root = store.root.clone();
        drop(store);
        drop(Store::open(&root).unwrap());
        let reader = Store::reader(&root).unwrap();
        assert_sizes(&rows(&reader, &View::default()));
        fs::remove_file(dir.path().join("source.body")).unwrap();
        let result = rows(&reader, &View::default());
        assert_eq!(result.len(), 6);
        assert!(result.iter().all(|row| !row.zero_size));
        assert_eq!(result[3].is_directory, !old_directory_metadata);
    }
}

#[test]
fn tag_cache_joins_keep_display_metadata_and_legacy_source_fallback() {
    let (dir, store) = fixture();
    store
        .set_tags(&(1..=6).collect::<Vec<_>>(), Some(Tag::Ioc))
        .unwrap();
    let mut view = View::default();
    view.predicate.tags = Some(vec![Tag::Ioc]);
    assert_sizes(&rows(&store, &view));
    store
        .conn
        .execute("UPDATE records SET is_directory=NULL,zero_size=NULL", [])
        .unwrap();
    assert_sizes(&rows(&store, &view));
    fs::remove_file(dir.path().join("source.body")).unwrap();
    let result = rows(&store, &view);
    assert_eq!(result.len(), 6);
    assert!(result.iter().all(|row| !row.zero_size && !row.is_directory));
}

#[test]
fn zero_byte_paths_are_faint_and_regular_weight_with_tags_and_selection() {
    let (_dir, store) = fixture();
    for theme in [Theme::Light, Theme::Dark] {
        for width in [60, 100, 144] {
            let mut app = App::default();
            app.ready = true;
            app.session.theme = theme;
            app.rows = rows(&store, &View::default())
                .into_iter()
                .map(DisplayRow::Event)
                .collect();
            app.rows[0].event_mut().unwrap().tag = Tag::Ioc;
            app.rows[0].event_mut().unwrap().ioc_match = true;
            app.rows[3].event_mut().unwrap().path = format!("/{}", "long-directory".repeat(20));
            app.rows[3].event_mut().unwrap().tag = Tag::Suspicious;
            app.key(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE));
            let mut terminal = Terminal::new(TestBackend::new(width, 24)).unwrap();
            terminal.draw(|frame| app.draw(frame)).unwrap();
            let buffer = terminal.backend().buffer();
            let text = |row| {
                (0..width)
                    .map(|x| buffer[(x, row)].symbol())
                    .collect::<String>()
            };
            let path_column = (0..width)
                .find(|&x| buffer[(x, 5)].symbol() == "/")
                .unwrap();
            for (row, empty) in [
                (5, true),
                (6, false),
                (7, true),
                (8, true),
                (9, true),
                (10, false),
            ] {
                assert!(!text(row).contains("[0 B]"), "{}", text(row));
                let path_cell = &buffer[(path_column, row)];
                assert_eq!(path_cell.modifier.contains(Modifier::DIM), empty);
                assert!(!path_cell.modifier.contains(Modifier::BOLD));
                assert!(matches!(path_cell.symbol(), "/" | "C"));
                // Faint styling is restricted to the pathname.
                assert!(!buffer[(3, row)].modifier.contains(Modifier::DIM));
            }
            assert!(text(5).contains('✓'));
            assert!(text(5).contains("IOC"));
            assert!(text(8).contains("…/"));
            assert!(text(8).contains("SUSPICIOUS"));
            assert_eq!(buffer[(path_column, 5)].fg, theme.palette().amber);
            let basename = &buffer[(path_column + 5, 5)];
            assert!(basename.modifier.contains(Modifier::DIM));
            assert!(!basename.modifier.contains(Modifier::BOLD));
            let clipped_separator = (0..width)
                .rfind(|&x| buffer[(x, 8)].symbol() == "/")
                .unwrap();
            assert!(
                buffer[(clipped_separator, 8)]
                    .modifier
                    .contains(Modifier::DIM)
            );
            let tag = &buffer[(width - 1, 5)];
            assert!(tag.modifier.contains(Modifier::BOLD));
            assert!(!tag.modifier.contains(Modifier::DIM));
            assert_eq!(app.rows[0].event().unwrap().path, "/tmp/zero");
            let actions = app.key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE));
            assert!(
                matches!(actions.as_slice(), [Action::Page(request)] if request.view.predicate.exclude == ["^/tmp/zero$"])
            );
        }
    }
}
