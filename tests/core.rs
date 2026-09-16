use std::{
    fs,
    sync::{Arc, atomic::AtomicU64},
};
use tempfile::TempDir;
use timeliner_tui::{
    bodyfile::Record,
    export::{self, Export},
    ioc::Rules,
    model::{EventKey, Predicate, Session, Tag, View, boundary, validate_view},
    query::{self, Cancellation},
    source,
    store::Store,
};

fn line(path: &str, times: &str) -> String {
    format!("original-hash|{path}|421|-rwsr-xr-x|501|50|9529053861562548261|{times}")
}
fn setup(contents: &[&[u8]]) -> (TempDir, Store) {
    let dir = TempDir::new().unwrap();
    let mut store = Store::open(&dir.path().join("workspace")).unwrap();
    for (i, bytes) in contents.iter().enumerate() {
        let path = dir.path().join(format!("host{i}.body"));
        fs::write(&path, bytes).unwrap();
        store
            .import(
                &source::resolve(&path, None, false).unwrap(),
                &Rules::default(),
                |_, _| true,
            )
            .unwrap();
    }
    (dir, store)
}
fn rows(store: &Store, view: &View) -> Vec<timeliner_tui::model::Row> {
    query::page(
        store,
        view,
        query::Page {
            limit: 4096,
            ..query::Page::default()
        },
        &Cancellation::never(),
        |_| {},
    )
    .unwrap()
}
fn tag(store: &Store, id: i64, value: Option<Tag>) {
    let old = store.revision().unwrap();
    store.set_tag(id, value).unwrap();
    query::update_caches(store, id, old).unwrap();
}

#[test]
fn parses_escaped_pipes_quotes_windows_and_large_unsigned_size() {
    for (path, expected) in [
        (r"file\|name\\", r"file|name\"),
        (
            r"C:\Windows\System32\cmd.exe",
            r"C:\Windows\System32\cmd.exe",
        ),
        (
            r#""C:\Program Files\file\|pipe""#,
            r#""C:\Program Files\file\|pipe""#,
        ),
    ] {
        let record = Record::parse(line(path, "1|2|3|4").as_bytes()).unwrap();
        assert_eq!(record.fields[1], expected);
        assert_eq!(record.fields[2], "421");
        assert_eq!(record.fields[6], "9529053861562548261");
    }
    assert!(Record::parse(b"too|few").is_err());
    assert!(Record::parse(line("/a", "1|2|3|").as_bytes()).is_err());
}

#[test]
fn groups_only_timestamps_within_a_physical_record() {
    let bytes = line("/same", "100|100|200|0");
    let duplicates = format!("{bytes}\n{bytes}\n");
    let (_dir, store) = setup(&[duplicates.as_bytes(), bytes.as_bytes()]);
    let result = rows(&store, &View::default());
    assert_eq!(result.len(), 6);
    assert_eq!(
        result
            .iter()
            .filter(|r| r.key.epoch == 100 && r.macb == 3)
            .count(),
        3
    );
    assert_eq!(
        result
            .iter()
            .map(|r| r.key.record)
            .collect::<std::collections::HashSet<_>>()
            .len(),
        3
    );
    assert_ne!(result[0].host, result[2].host);
    assert_eq!(
        rows(
            &store,
            &View {
                bogus: true,
                ..View::default()
            }
        )
        .len(),
        9
    );
}

#[test]
fn malformed_offsets_reported_without_corrupting_valid_records() {
    let content = format!("bad|line\n{}\n", line("/valid", "1|2|3|4"));
    let (_dir, store) = setup(&[content.as_bytes()]);
    let detail = store
        .detail(rows(&store, &View::default())[0].key.record)
        .unwrap();
    assert_eq!(detail.line, 2);
    assert_eq!(detail.offset, 9);
    assert!(
        store
            .diagnostics(20)
            .unwrap()
            .contains("Malformed records: 1")
    );
}

#[test]
fn exact_ioc_rules_and_exclusions_match_scanner_conventions() {
    let rules = Rules::from_yaml(
        r"
- filename: bad.so
  exclusion_path: [/safe]
- path: /exact/item
  exclusion_hash: [trusted]
- filename: bad.so
  exclusion_path: [/other]
",
    )
    .unwrap();
    assert_eq!(rules.matches("/unsafe/bad.so", "hash"), vec![0, 2]);
    assert_eq!(rules.matches("/safeish/bad.so", "hash"), vec![2]);
    assert!(rules.matches("/exact/item", "trusted").is_empty());
    assert_eq!(rules.matches("/exact/item", "different"), vec![1]);
    assert!(rules.matches("/exact/item/child", "different").is_empty());
    assert!(rules.matches("/unsafe/BAD.SO", "hash").is_empty());
    assert_eq!(rules.matches(r"C:\Windows\bad.so", "hash"), vec![0, 2]);
    assert!(Rules::from_yaml("- filename: x\n  glob: true").is_err());
    assert!(Rules::from_yaml("- exclusion_path: [/a]").is_err());
}

#[test]
fn manual_tag_and_clear_survive_ioc_reload_and_auto_reset() {
    let content = format!(
        "{}\n{}",
        line("/evil", "1|2|3|4"),
        line("/other", "5|6|7|8")
    );
    let (_dir, mut store) = setup(&[content.as_bytes()]);
    let rules = Rules::from_yaml("- path: /evil").unwrap();
    store.reload_rules(&rules, |_| true).unwrap();
    let id = rows(&store, &View::default())[0].key.record;
    assert_eq!(store.detail(id).unwrap().tag, Tag::Ioc);
    tag(&store, id, Some(Tag::None));
    store.reload_rules(&rules, |_| true).unwrap();
    assert_eq!(store.detail(id).unwrap().tag, Tag::None);
    tag(&store, id, Some(Tag::Malicious));
    store.reload_rules(&Rules::default(), |_| true).unwrap();
    assert_eq!(store.detail(id).unwrap().tag, Tag::Malicious);
    tag(&store, id, None);
    assert_eq!(store.detail(id).unwrap().tag, Tag::None);
    store.reload_rules(&rules, |_| true).unwrap();
    assert!(
        rows(&store, &View::default())
            .iter()
            .filter(|r| r.key.record == id)
            .all(|r| r.tag == Tag::Ioc)
    );
}

#[test]
fn cached_smart_views_update_incrementally_and_filters_compose() {
    let content = format!(
        "{}\n{}\n{}",
        line("/var/a", "1|2|3|4"),
        line("/var/b", "5|6|7|8"),
        line("/tmp/a", "9|10|11|12")
    );
    let (_dir, store) = setup(&[content.as_bytes()]);
    let view = View {
        predicate: Predicate {
            tags: Some(vec![Tag::Suspicious]),
            include: "^/var/".into(),
            exclude: vec!["/b$".into()],
            ..Predicate::default()
        },
        ..View::default()
    };
    assert!(rows(&store, &view).is_empty());
    let id = rows(&store, &View::default())[0].key.record;
    tag(&store, id, Some(Tag::Suspicious));
    assert_eq!(rows(&store, &view).len(), 4);
    tag(&store, id, Some(Tag::Malicious));
    assert!(rows(&store, &view).is_empty());
    assert!(
        rows(
            &store,
            &View {
                after: Some(1000),
                ..view.clone()
            }
        )
        .is_empty()
    );
    tag(&store, id, Some(Tag::Suspicious));
    assert_eq!(
        rows(
            &store,
            &View {
                after: Some(3),
                before: Some(4),
                ..view
            }
        )
        .len(),
        2
    );
}

#[test]
fn cursor_pagination_is_stable_in_both_directions() {
    let content = (0..150)
        .map(|i| line(&format!("/f{i}"), "100|100|100|100"))
        .collect::<Vec<_>>()
        .join("\n");
    let (_dir, store) = setup(&[content.as_bytes()]);
    let cancel = Cancellation::never();
    let a = query::page(
        &store,
        &View::default(),
        query::Page {
            limit: 50,
            ..query::Page::default()
        },
        &cancel,
        |_| {},
    )
    .unwrap();
    let b = query::page(
        &store,
        &View::default(),
        query::Page {
            anchor: Some(a[49].key),
            limit: 50,
            ..query::Page::default()
        },
        &cancel,
        |_| {},
    )
    .unwrap();
    let back = query::page(
        &store,
        &View::default(),
        query::Page {
            anchor: Some(b[0].key),
            reverse: true,
            limit: 50,
            ..query::Page::default()
        },
        &cancel,
        |_| {},
    )
    .unwrap();
    assert_eq!(
        a.iter().map(|r| r.key).collect::<Vec<_>>(),
        back.iter().map(|r| r.key).collect::<Vec<_>>()
    );
    assert!(a[49].key < b[0].key);
    let end = query::page(
        &store,
        &View::default(),
        query::Page {
            reverse: true,
            limit: 50,
            ..query::Page::default()
        },
        &cancel,
        |_| {},
    )
    .unwrap();
    assert_eq!(end[49].key.record, 150);
}

#[test]
fn timezone_full_day_bounds_and_validation() {
    let tz: chrono_tz::Tz = "America/New_York".parse().unwrap();
    let first = boundary("2024-02-03", tz, false).unwrap().unwrap();
    let last = boundary("2024-02-03", tz, true).unwrap().unwrap();
    assert_eq!(last - first, 86399);
    assert_eq!(
        boundary("2024-02-03 00:00:00", tz, false).unwrap(),
        Some(first)
    );
    assert!(boundary("2024-11-03 01:30", tz, false).is_err());
    assert!(
        validate_view(&View {
            after: Some(2),
            before: Some(1),
            ..View::default()
        })
        .is_err()
    );
    assert!(
        validate_view(&View {
            predicate: Predicate {
                include: "[".into(),
                ..Predicate::default()
            },
            ..View::default()
        })
        .is_err()
    );
}

#[test]
fn cancellation_does_not_publish_partial_filter_or_rules() {
    let content = line("/a", "1|2|3|4");
    let (_dir, mut store) = setup(&[content.as_bytes()]);
    let cancel = Cancellation {
        clock: Arc::new(AtomicU64::new(1)),
        generation: 0,
    };
    let view = View {
        predicate: Predicate {
            include: "a".into(),
            ..Predicate::default()
        },
        ..View::default()
    };
    assert!(
        query::page(
            &store,
            &view,
            query::Page {
                limit: 10,
                ..query::Page::default()
            },
            &cancel,
            |_| {}
        )
        .is_err()
    );
    assert_eq!(fs::read_dir(store.root.join("views")).unwrap().count(), 0);
    assert!(
        store
            .reload_rules(&Rules::from_yaml("- path: /a").unwrap(), |_| false)
            .is_err()
    );
    assert_eq!(rows(&store, &View::default())[0].tag, Tag::None);
}

#[test]
fn export_preserves_bytes_duplicates_all_fields_and_missing_newline() {
    let a = line(r"/a\|pipe", "1|2|3|4");
    let b = format!(
        "{}\r\n{}\n",
        line("/second", "5|6|7|8"),
        line("/second", "5|6|7|8")
    );
    let (dir, store) = setup(&[a.as_bytes(), b.as_bytes()]);
    for id in 1..=3 {
        tag(&store, id, Some(Tag::Suspicious));
    }
    let output = dir.path().join("out.body");
    let export = Export {
        output: output.clone(),
        view: None,
        tags: vec![],
    };
    assert_eq!(
        export::run(&store, &export, &Cancellation::never(), |_| {}).unwrap(),
        3
    );
    let raw = fs::read(&output).unwrap();
    assert_eq!(raw, format!("{a}\n{b}").as_bytes());
    let metadata = fs::read_to_string(export::sidecar(&output)).unwrap();
    for row in metadata.lines().skip(1) {
        let value: serde_json::Value = serde_json::from_str(row).unwrap();
        let offset = value["output_offset"].as_u64().unwrap() as usize;
        let length = value["output_length"].as_u64().unwrap() as usize;
        let id = value["record_id"].as_i64().unwrap();
        assert_eq!(&raw[offset..offset + length], store.detail(id).unwrap().raw);
        assert_eq!(value["tag"], "SUSPICIOUS");
    }
    assert!(export::run(&store, &export, &Cancellation::never(), |_| {}).is_err());
}

#[test]
fn export_keeps_invalid_utf8_and_deduplicates_only_events() {
    let bytes = b"hash|/invalid-\xff|3|mode|0|0|5|1|2|3|4\n";
    let (dir, store) = setup(&[bytes]);
    tag(&store, 1, Some(Tag::Ioc));
    let out = dir.path().join("out.body");
    let count = export::run(
        &store,
        &Export {
            output: out.clone(),
            view: Some(View::default()),
            tags: vec![Tag::Ioc],
        },
        &Cancellation::never(),
        |_| {},
    )
    .unwrap();
    assert_eq!(count, 1);
    assert_eq!(fs::read(out).unwrap(), bytes);
}

#[test]
fn changed_source_blocks_export_and_relink_requires_exact_content() {
    let original = line("/one", "1|2|3|4");
    let (dir, store) = setup(&[original.as_bytes()]);
    tag(&store, 1, Some(Tag::Malicious));
    let source = store.sources().unwrap().remove(0);
    fs::write(&source.path, line("/two", "1|2|3|4")).unwrap();
    assert!(store.detail(1).is_err());
    let out = dir.path().join("out.body");
    assert!(
        export::run(
            &store,
            &Export {
                output: out.clone(),
                view: None,
                tags: vec![]
            },
            &Cancellation::never(),
            |_| {}
        )
        .is_err()
    );
    assert!(!out.exists());
    assert!(!export::sidecar(&out).exists());
    assert!(store.relink(source.id, &source.path).is_err());
    let replacement = dir.path().join("replacement.body");
    fs::write(&replacement, &original).unwrap();
    store.relink(source.id, &replacement).unwrap();
    assert_eq!(store.detail(1).unwrap().raw, original.as_bytes());
    assert_eq!(store.detail(1).unwrap().tag, Tag::Malicious);
}

#[test]
fn interrupted_import_restarts_without_duplicating_records() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("source");
    fs::write(&path, line("/a", "1|2|3|4")).unwrap();
    let mut store = Store::open(&dir.path().join("ws")).unwrap();
    let input = source::resolve(&path, None, false).unwrap();
    assert!(
        store
            .import(&input, &Rules::default(), |_, _| false)
            .is_err()
    );
    store
        .import(&input, &Rules::default(), |_, _| true)
        .unwrap();
    assert_eq!(rows(&store, &View::default()).len(), 4);
}

#[test]
fn uac_hostname_and_explicit_legacy_mode() {
    let dir = TempDir::new().unwrap();
    fs::create_dir_all(dir.path().join("bodyfile")).unwrap();
    fs::create_dir_all(dir.path().join("live_response/network")).unwrap();
    fs::write(
        dir.path().join("bodyfile/bodyfile.txt"),
        line("/p", "1|2|3|4"),
    )
    .unwrap();
    fs::write(
        dir.path().join("live_response/network/hostname_-f.txt"),
        "server.example\n",
    )
    .unwrap();
    let input = source::resolve(dir.path(), None, false).unwrap();
    assert_eq!(input.host, "server.example");
    let direct = source::resolve(&input.path, None, false).unwrap();
    assert_eq!(direct.host, "server.example");
    let input = source::resolve(dir.path(), Some("override"), true).unwrap();
    assert_eq!(input.host, "override");
    assert!(!input.legacy);
    let mut store = Store::open(&dir.path().join("ws")).unwrap();
    store
        .import(
            &source::resolve(dir.path(), None, true).unwrap(),
            &Rules::default(),
            |_, _| true,
        )
        .unwrap();
    assert_eq!(rows(&store, &View::default())[0].host, "original-hash");
    store
        .import(&input, &Rules::default(), |_, _| true)
        .unwrap();
    assert_eq!(rows(&store, &View::default())[0].host, "override");
}

#[test]
fn session_round_trip_preserves_independent_tabs_marks_and_location() {
    let (_dir, store) = setup(&[]);
    let view = View {
        top: Some(EventKey {
            epoch: 1,
            record: 2,
        }),
        selected: 3,
        ..View::default()
    };
    let mut copy = view.clone();
    copy.predicate.exclude.push("/tmp".into());
    let session = Session {
        views: vec![view, copy],
        active: 1,
        bookmarks: vec![timeliner_tui::model::Bookmark {
            name: 'a',
            key: EventKey {
                epoch: 1,
                record: 2,
            },
        }],
        timezone: "UTC".into(),
    };
    store.save_session(&session).unwrap();
    let restored = store.session().unwrap();
    assert_eq!(restored.active, 1);
    assert!(restored.views[0].predicate.exclude.is_empty());
    assert_eq!(restored.views[1].predicate.exclude, ["/tmp"]);
    assert_eq!(restored.bookmarks[0].name, 'a');
}

#[test]
fn cancelled_sql_filter_does_not_poison_later_database_operations() {
    let content = (0..3000)
        .map(|i| line(&format!("/f{i}"), "1|2|3|4"))
        .collect::<Vec<_>>()
        .join("\n");
    let (_dir, store) = setup(&[content.as_bytes()]);
    let cancel = Cancellation::never();
    let view = View {
        predicate: Predicate {
            include: "never-matches".into(),
            ..Predicate::default()
        },
        ..View::default()
    };
    let result = query::page(&store, &view, query::Page::default(), &cancel, |_| {
        cancel.clock.store(1, std::sync::atomic::Ordering::Relaxed);
    });
    assert!(result.is_err());
    tag(&store, 1, Some(Tag::Malicious));
    assert_eq!(store.detail(1).unwrap().tag, Tag::Malicious);
}
