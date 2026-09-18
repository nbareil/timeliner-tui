use std::fs;
use tempfile::TempDir;
use timeliner_tui::{
    export::{self, Export},
    ioc::Rules,
    model::{Row, Session, Tag, View},
    navigation::Jump,
    query::{self, Cancellation, Page},
    source,
    store::Store,
};

fn fixture() -> (TempDir, Store) {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("host.body");
    let mut lines = Vec::new();
    for mask in 1..=15 {
        // Bodyfile timestamp order is access, modification, change, birth.
        let times = [2, 1, 4, 8].map(|bit| if mask & bit != 0 { "100" } else { "0" });
        lines.push(format!(
            "0|/mask-{mask}|{mask}|-rw-r--r--|0|0|1|{}",
            times.join("|")
        ));
    }
    lines.push("0|/split|16|-rw-r--r--|0|0|1|100|101|0|0".into());
    fs::write(&path, lines.join("\n") + "\n").unwrap();
    let mut store = Store::open(&dir.path().join("workspace")).unwrap();
    store
        .import(
            &source::resolve(&path, None, false).unwrap(),
            &Rules::default(),
            |_, _| true,
        )
        .unwrap();
    (dir, store)
}

fn rows(store: &Store, view: &View, reverse: bool) -> Vec<Row> {
    let mut result = Vec::new();
    let mut anchor = None;
    loop {
        let page = query::page(
            store,
            view,
            Page {
                anchor,
                reverse,
                limit: 2,
                ..Page::default()
            },
            &Cancellation::never(),
            |_| {},
        )
        .unwrap();
        if page.is_empty() {
            return result;
        }
        if reverse {
            anchor = Some(page[0].key);
            result.splice(0..0, page);
        } else {
            anchor = page.last().map(|row| row.key);
            result.extend(page);
        }
        assert!(result.len() <= 17, "Pagination must advance");
    }
}

#[test]
fn access_only_is_distinct_from_any_access_and_hiding_access_only() {
    let (_dir, store) = fixture();
    for (mask, exact, expected) in [
        (2, true, vec![2, 2]),
        (2, false, vec![2, 3, 6, 7, 10, 11, 14, 15, 2]),
        (
            13,
            false,
            vec![1, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 1],
        ),
        (3, true, vec![3]),
        (15, true, vec![15]),
    ] {
        let view = View {
            time_mask: mask,
            time_exact: exact,
            ..View::default()
        };
        for reverse in [false, true] {
            assert_eq!(
                rows(&store, &view, reverse)
                    .iter()
                    .map(|row| row.macb)
                    .collect::<Vec<_>>(),
                expected,
                "mask={mask}, exact={exact}, reverse={reverse}"
            );
        }
    }
}

#[test]
fn exact_timestamps_compose_with_cached_tags_paths_hosts_and_timeframes() {
    let (_dir, store) = fixture();
    let all = rows(&store, &View::default(), false);
    store
        .set_tags(
            &all.iter().map(|row| row.key.record).collect::<Vec<_>>(),
            Some(Tag::Suspicious),
        )
        .unwrap();
    let mut view = View {
        time_mask: 2,
        time_exact: true,
        after: Some(100),
        before: Some(100),
        ..View::default()
    };
    view.predicate.tags = Some(vec![Tag::Suspicious]);
    view.predicate.include = "^/".into();
    view.predicate.exclude = vec!["split$".into()];
    view.predicate.hosts = vec![all[0].host.clone()];
    for reverse in [false, true] {
        let result = rows(&store, &view, reverse);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].path, "/mask-2");
    }
    view.time_exact = false;
    assert_eq!(rows(&store, &view, false).len(), 8);
    assert_eq!(fs::read_dir(store.root.join("views")).unwrap().count(), 1);
    view.time_exact = true;
    view.after = Some(101);
    view.before = Some(101);
    assert!(rows(&store, &view, false).is_empty());
}

#[test]
fn navigation_and_current_exports_respect_exact_access_without_rewriting_records() {
    let (dir, store) = fixture();
    let all = rows(&store, &View::default(), false);
    store
        .set_tags(
            &all.iter().map(|row| row.key.record).collect::<Vec<_>>(),
            Some(Tag::Suspicious),
        )
        .unwrap();
    let view = View {
        time_mask: 2,
        time_exact: true,
        ..View::default()
    };
    let expected = rows(&store, &view, false);
    for (jump, target) in [
        (Jump::Goto(100), &expected[0]),
        (Jump::TaggedAfter(expected[0].key), &expected[1]),
        (Jump::TaggedBefore(expected[1].key), &expected[0]),
        (Jump::PreviousTimeBoundary(101), &expected[1]),
    ] {
        assert_eq!(
            jump.find(&store, &view, &Cancellation::never(), |_| {})
                .unwrap()
                .unwrap()
                .key,
            target.key
        );
    }
    assert!(
        Jump::Goto(101)
            .find(&store, &view, &Cancellation::never(), |_| {})
            .unwrap()
            .is_none()
    );
    let output = dir.path().join("access.body");
    assert_eq!(
        export::run(
            &store,
            &Export {
                output: output.clone(),
                view: Some(view),
                tags: vec![],
            },
            &Cancellation::never(),
            |_| {},
        )
        .unwrap(),
        2
    );
    assert_eq!(
        fs::read_to_string(output).unwrap(),
        concat!(
            "0|/mask-2|2|-rw-r--r--|0|0|1|100|0|0|0\n",
            "0|/split|16|-rw-r--r--|0|0|1|100|101|0|0\n"
        )
    );
}

#[test]
fn timestamp_matching_survives_restart_and_older_sessions_keep_any_matching() {
    let (_dir, store) = fixture();
    let mut session = Session::default();
    session.views[0].time_mask = 2;
    session.views[0].time_exact = true;
    store.save_session(&session).unwrap();
    let root = store.root.clone();
    drop(store);
    let reopened = Store::open(&root).unwrap();
    let restored = reopened.session().unwrap();
    assert!(restored.views[0].time_exact);
    assert_eq!(rows(&reopened, &restored.views[0], false).len(), 2);
    let mut old = serde_json::to_value(session).unwrap();
    for view in old["views"].as_array_mut().unwrap() {
        view.as_object_mut().unwrap().remove("time_exact");
    }
    let old: Session = serde_json::from_value(old).unwrap();
    assert!(!old.views[0].time_exact);
    assert_eq!(rows(&reopened, &old.views[0], false).len(), 9);
}
