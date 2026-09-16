use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{Terminal, backend::TestBackend};
use std::fs;
use tempfile::TempDir;
use timeliner_tui::{
    bodyfile::Record,
    ioc::Rules,
    model::{DisplayRow, Row, Tag, View, display_path},
    query::{self, Cancellation, Page},
    source,
    store::Store,
    theme::Theme,
    ui::{Action, App},
};

fn body_line(path: &str, mode: &str) -> String {
    format!(
        "0|{}|1|{mode}|0|0|10|100|100|100|100\n",
        path.replace('\\', "\\\\")
    )
}

fn fixture() -> (TempDir, Store) {
    let dir = tempfile::tempdir().unwrap();
    let body = dir.path().join("source.body");
    fs::write(
        &body,
        [
            body_line("/etc", "drwxr-xr-x"),
            body_line(r"C:\Windows", "d/drwxrwxrwx"),
            body_line(r"\\server\share", "d/d"),
            body_line("/etc/", "040755"),
            body_line("/etc/file", "-rwxr-xr-x"),
            body_line("/link -> /etc", "l/lrwxrwxrwx"),
        ]
        .concat(),
    )
    .unwrap();
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

#[test]
fn directory_modes_use_metadata_and_never_guess_from_the_filename() {
    for (mode, expected) in [
        ("drwxr-xr-x", true),
        ("d/drwxrwxrwx", true),
        ("d/d", true),
        ("040755", true),
        ("0o40755", true),
        ("-rwxr-xr-x", false),
        ("r/rrwxrwxrwx", false),
        ("l/lrwxrwxrwx", false),
        ("100755", false),
        ("0755", false),
        ("mode", false),
    ] {
        let record = Record::parse(body_line("/looks.like.a.file", mode).as_bytes()).unwrap();
        assert_eq!(record.is_directory(), expected, "{mode}");
    }
    for (path, expected) in [
        ("/etc", "/etc/"),
        ("/etc/", "/etc/"),
        ("/", "/"),
        (r"C:\Windows", "C:\\Windows\\"),
        ("C:\\", "C:\\"),
        (r"\\server\share", "\\\\server\\share\\"),
        (r"/unix/back\slash", "/unix/back\\slash/"),
    ] {
        assert_eq!(display_path(path, true), expected);
        assert_eq!(display_path(path, false), path);
    }
}

#[test]
fn indexed_directory_types_preserve_paths_filters_and_work_without_source_files() {
    let (dir, store) = fixture();
    let original = fs::read(dir.path().join("source.body")).unwrap();
    let result = rows(&store, &View::default());
    assert_eq!(
        result
            .iter()
            .map(|row| row.is_directory)
            .collect::<Vec<_>>(),
        [true, true, true, true, false, false]
    );
    assert_eq!(result[0].path, "/etc");
    assert_eq!(result[1].path, r"C:\Windows");
    assert_eq!(result[2].path, r"\\server\share");
    let mut view = View::default();
    view.predicate.include = "^/etc$".into();
    assert_eq!(rows(&store, &view).len(), 1);
    view.predicate.exclude.push("^/etc$".into());
    assert!(rows(&store, &view).is_empty());
    assert_eq!(fs::read(dir.path().join("source.body")).unwrap(), original);
    fs::remove_file(dir.path().join("source.body")).unwrap();
    assert_eq!(
        rows(&store, &View::default())
            .iter()
            .map(|row| row.is_directory)
            .collect::<Vec<_>>(),
        [true, true, true, true, false, false]
    );
}

#[test]
fn older_investigations_gain_directory_markers_without_reimporting() {
    let (dir, store) = fixture();
    store
        .conn
        .execute("ALTER TABLE records DROP COLUMN is_directory", [])
        .unwrap();
    let root = store.root.clone();
    drop(store);
    let store = Store::open(&root).unwrap();
    let result = rows(&store, &View::default());
    assert_eq!(
        result
            .iter()
            .map(|row| row.is_directory)
            .collect::<Vec<_>>(),
        [true, true, true, true, false, false]
    );
    // Original path indexing stays usable even when an old source is unavailable.
    fs::remove_file(dir.path().join("source.body")).unwrap();
    assert_eq!(rows(&store, &View::default()).len(), result.len());
}

#[test]
fn directory_suffixes_remain_visible_on_clipped_rows_in_both_themes() {
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
            app.rows[3].event_mut().unwrap().path = format!("/{}", "long-directory".repeat(20));
            app.rows[3].event_mut().unwrap().tag = Tag::Suspicious;
            let mut terminal = Terminal::new(TestBackend::new(width, 24)).unwrap();
            terminal.draw(|frame| app.draw(frame)).unwrap();
            let buffer = terminal.backend().buffer();
            let text = |row| {
                (0..width)
                    .map(|x| buffer[(x, row)].symbol())
                    .collect::<String>()
            };
            assert!(text(5).contains("/etc/"));
            assert!(text(6).contains(if width == 60 {
                "…\\"
            } else {
                "C:\\Windows\\"
            }));
            assert!(text(8).contains("…/"));
            assert!(text(8).contains("SUSPICIOUS"));
            assert!(!text(9).contains("/etc/file/"));
            assert_eq!(app.rows[0].event().unwrap().path, "/etc");
            let actions = app.key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE));
            assert!(
                matches!(actions.as_slice(), [Action::Page(request)] if request.view.predicate.exclude == ["^/etc$"])
            );
        }
    }
}
