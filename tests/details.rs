use std::{fs, time::Duration};
use tempfile::TempDir;
use timeliner_tui::{
    ioc::Rules,
    source,
    worker::{Command, Message, Workers},
};

fn start(contents: &str) -> (TempDir, Workers) {
    let dir = tempfile::tempdir().unwrap();
    let body = dir.path().join("source.body");
    fs::write(&body, contents).unwrap();
    let workers = Workers::start(
        dir.path().join("workspace"),
        vec![source::resolve(&body, None, false).unwrap()],
        Some(Rules::default()),
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
            Message::Ready(_, _) => break,
            Message::Progress(_, _) => {}
            Message::Error(error) => panic!("{error}"),
            _ => panic!("Unexpected message during import"),
        }
    }
    (dir, workers)
}

fn details(workers: &Workers, timezone: chrono_tz::Tz) -> String {
    workers.commands.send(Command::Detail(1, timezone)).unwrap();
    match workers
        .messages
        .recv_timeout(Duration::from_secs(5))
        .unwrap()
    {
        Message::Detail(text) => text,
        Message::Error(error) => panic!("{error}"),
        _ => panic!("Expected record details"),
    }
}

#[test]
fn detail_timestamps_use_iso_dates_and_the_requested_timezone_without_saving() {
    let original = "0|/sample|1|mode|0|0|10|1609459200|1625097600|0|-1\n";
    let (_dir, workers) = start(original);
    let utc = details(&workers, chrono_tz::UTC);
    let new_york = details(&workers, chrono_tz::America::New_York);
    workers.shutdown();

    for expected in [
        "Access time: 2021-01-01T00:00:00+00:00\n",
        "Modification time: 2021-07-01T00:00:00+00:00\n",
        "Change time: 1970-01-01T00:00:00+00:00\n",
        "Birth time: 1969-12-31T23:59:59+00:00\n",
    ] {
        assert!(utc.contains(expected), "{utc}");
    }
    for expected in [
        "Access time: 2020-12-31T19:00:00-05:00\n",
        "Modification time: 2021-06-30T20:00:00-04:00\n",
        "Change time: 1969-12-31T19:00:00-05:00\n",
        "Birth time: 1969-12-31T18:59:59-05:00\n",
    ] {
        assert!(new_york.contains(expected), "{new_york}");
    }
    for text in [utc, new_york] {
        assert!(text.contains("Size: 10\n"));
        assert!(text.ends_with(original));
    }
}

#[test]
fn out_of_range_timestamps_remain_inspectable() {
    let original = format!("0|/sample|1|mode|0|0|10|{}|{}|0|0\n", i64::MIN, i64::MAX);
    let (_dir, workers) = start(&original);
    let text = details(&workers, chrono_tz::UTC);
    workers.shutdown();
    assert!(text.contains("Access time: -9223372036854775808 (outside supported date range)\n"));
    assert!(
        text.contains("Modification time: 9223372036854775807 (outside supported date range)\n")
    );
    assert!(text.contains("Change time: 1970-01-01T00:00:00+00:00\n"));
    assert!(text.ends_with(&original));
}
