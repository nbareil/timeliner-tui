use std::{fs, sync::atomic::Ordering, time::Duration};
use timeliner_tui::{
    ioc::Rules,
    model::{DisplayRow, Predicate, Tag, View},
    source,
    store::Store,
    worker::{Command, Message, PageRequest, Workers},
};

fn receive(workers: &Workers) -> Message {
    let message = workers
        .messages
        .recv_timeout(Duration::from_secs(10))
        .unwrap();
    match message {
        Message::Error(error) | Message::QueryError(_, error) => panic!("{error}"),
        message => message,
    }
}

fn request(workers: &Workers, view: View, generation: u64) {
    workers.clock.store(generation, Ordering::Relaxed);
    workers
        .pages
        .send(PageRequest {
            generation,
            view,
            anchor: None,
            reverse: false,
            inclusive: false,
            limit: 10,
            context_before: 0,
            allow_partial: false,
            detect_burst: None,
            jump: None,
        })
        .unwrap();
}

fn page(workers: &Workers, view: View, generation: u64) -> Vec<DisplayRow> {
    request(workers, view, generation);
    loop {
        if let Message::Page(actual, rows) = receive(workers) {
            assert_eq!(actual, generation);
            return rows;
        }
    }
}

#[test]
fn query_reader_observes_tag_commits_and_recovers_after_cancellation() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("workspace");
    let body = dir.path().join("source.body");
    let content = (1..=20_000)
        .map(|i| format!("0|/file{i}|{i}|-rw-r--r--|0|0|1|1|2|3|4\n"))
        .collect::<String>();
    fs::write(&body, content).unwrap();
    let mut store = Store::open(&root).unwrap();
    store
        .import(
            &source::resolve(&body, None, false).unwrap(),
            &Rules::default(),
            |_, _| true,
        )
        .unwrap();
    drop(store);

    let workers = Workers::start(root, vec![], None, None, None, None);
    while !matches!(receive(&workers), Message::Ready(..)) {}
    let first = page(&workers, View::default(), 1);
    let DisplayRow::Event(row) = &first[0] else {
        panic!("Expected event")
    };
    assert_eq!(row.tag, Tag::None);
    let id = row.key.record;
    let tagged = View {
        predicate: Predicate {
            tags: Some(vec![Tag::Ioc]),
            ..Predicate::default()
        },
        ..View::default()
    };
    assert!(page(&workers, tagged.clone(), 2).is_empty());
    workers
        .commands
        .send(Command::Tag(id, Some(Tag::Ioc)))
        .unwrap();
    while !matches!(receive(&workers), Message::Tagged(_)) {}
    let all = page(&workers, View::default(), 3);
    let DisplayRow::Event(row) = &all[0] else {
        panic!("Expected event")
    };
    assert_eq!(row.tag, Tag::Ioc);
    let assessed = page(&workers, tagged.clone(), 4);
    assert_eq!(assessed.len(), 4);
    assert!(assessed.iter().all(|row| matches!(
        row, DisplayRow::Event(event) if event.key.record == id && event.tag == Tag::Ioc
    )));

    let empty = View {
        predicate: Predicate {
            include: "never-matches".into(),
            ..Predicate::default()
        },
        ..View::default()
    };
    request(&workers, empty, 5);
    // Wait for actual scanning before cancellation; no timing-dependent sleep.
    loop {
        match receive(&workers) {
            Message::Progress(5, _) => break,
            Message::Page(5, _) => panic!("Search finished without reporting progress"),
            _ => {}
        }
    }
    workers.cancel();
    request(&workers, View::default(), 7);
    loop {
        match receive(&workers) {
            Message::Page(7, rows) => {
                assert_eq!(rows.len(), 10);
                break;
            }
            Message::Page(5, _) | Message::Progress(..) => {}
            _ => panic!("Unexpected response after cancellation"),
        }
    }
    workers
        .commands
        .send(Command::Tag(id, Some(Tag::None)))
        .unwrap();
    while !matches!(receive(&workers), Message::Tagged(_)) {}
    assert!(page(&workers, tagged, 8).is_empty());
    workers.shutdown();
}
