#![cfg(feature = "sqlite-exclusions")]

use std::{fs, sync::atomic::Ordering};
use timeliner_tui::{
    ioc::Rules,
    model::{Row, Tag, View},
    query::{self, Cancellation, Matcher, Page},
    source,
    store::Store,
};

fn fixture() -> (tempfile::TempDir, Store) {
    let dir = tempfile::tempdir().unwrap();
    let body = dir.path().join("source.body");
    let mut contents = String::new();
    for i in 0..3007 {
        let path = match i {
            0 => "/keep/first".into(),
            1000 => "/KEEP/middle".into(),
            1500 => "/café/α".into(),
            2000 => r"C:\Windows\entry".into(),
            3006 => "/keep/last".into(),
            _ => format!("/noise/{i}"),
        };
        let epoch = 100 + i / 5;
        contents.push_str(&format!(
            "0|{path}|{i}|-rw-r--r--|0|0|1|{epoch}|{epoch}|{epoch}|{epoch}\n"
        ));
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
    (dir, store)
}

fn all(store: &Store, view: &View, reverse: bool) -> (Vec<Row>, u64) {
    let mut checked = 0;
    let rows = query::page(
        store,
        view,
        Page {
            limit: 4096,
            reverse,
            ..Page::default()
        },
        &Cancellation::never(),
        |n| checked = n,
    )
    .unwrap();
    (rows, checked)
}

#[test]
fn sql_results_match_rust_filtering_after_replacing_or_clearing_exclusions() {
    let (_dir, store) = fixture();
    let (raw, _) = all(&store, &View::default(), false);
    assert_eq!(raw.len(), 3007);
    for patterns in [
        vec!["^/noise/"],
        vec![".*"],
        vec![],
        vec!["(?i)^/keep/", r"/\p{Greek}+$", r"^C:\\Windows\\"],
        vec![""],
        vec!["^/keep/"],
    ] {
        let mut view = View::default();
        view.predicate.exclude = patterns.iter().map(|s| s.to_string()).collect();
        let matcher = Matcher::new(&view.predicate).unwrap();
        let expected: Vec<_> = raw
            .iter()
            .filter(|r| matcher.matches(&r.path, &r.host, r.tag))
            .cloned()
            .collect();
        for reverse in [false, true] {
            let (actual, checked) = all(&store, &view, reverse);
            assert_eq!(format!("{actual:?}"), format!("{expected:?}"));
            assert_eq!(checked, raw.len() as u64, "{patterns:?}, {reverse}");
        }
    }
    assert!(
        store
            .conn
            .query_row("SELECT timeliner_scan_candidate(1,1,'/keep')", [], |r| {
                r.get::<_, bool>(0)
            })
            .is_err(),
        "The reader must release each query's SQL function"
    );
}

#[test]
fn cancellation_of_fully_excluded_scan_leaves_the_reader_usable() {
    let (_dir, store) = fixture();
    let mut view = View::default();
    view.predicate.exclude.push(".*".into());
    let cancel = Cancellation::never();
    let clock = cancel.clock.clone();
    let mut last_count = 0;
    let result = query::page(&store, &view, Page::default(), &cancel, |n| {
        last_count = n;
        if n >= 1024 {
            clock.fetch_add(1, Ordering::Relaxed);
        }
    });
    assert!(result.is_err());
    assert_eq!(last_count, 1024);
    view.predicate.exclude = vec!["^/noise/".into()];
    assert_eq!(all(&store, &view, false).0.len(), 5);
    view.predicate.exclude = vec!["[".into()];
    assert!(
        query::page(
            &store,
            &view,
            Page::default(),
            &Cancellation::never(),
            |_| {}
        )
        .is_err()
    );
    assert_eq!(all(&store, &View::default(), false).0.len(), 3007);
}

#[test]
fn sql_exclusions_intersect_inclusion_and_tag_cache_membership() {
    let (_dir, store) = fixture();
    let (raw, _) = all(&store, &View::default(), false);
    for i in [0, 1, 2, 1000, 3006] {
        store
            .set_tag(raw[i].key.record, Some(Tag::Suspicious))
            .unwrap();
    }
    let mut view = View::default();
    view.predicate.tags = Some(vec![Tag::Suspicious]);
    view.predicate.include = "(?i)keep".into();
    view.predicate.exclude = vec!["/middle$".into(), "/last$".into()];
    for reverse in [false, true] {
        let (rows, checked) = all(&store, &view, reverse);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].path, "/keep/first");
        assert_eq!(rows[0].tag, Tag::Suspicious);
        assert_eq!(checked, 5);
    }
    view.predicate.exclude.clear();
    assert_eq!(all(&store, &view, false).0.len(), 3);
}
