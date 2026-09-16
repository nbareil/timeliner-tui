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
        assert!(text.contains("Size: 10 B\n"));
        assert!(text.ends_with(original));
    }
}

#[test]
fn detail_sizes_use_readable_decimal_units_and_preserve_original_records() {
    for (size, expected) in [
        (0, "0 B"),
        (1, "1 B"),
        (999, "999 B"),
        (1000, "1.00 KB"),
        (1500, "1.50 KB"),
        (48216, "48.22 KB"),
        (1_000_000, "1.00 MB"),
        (1_500_000, "1.50 MB"),
        (1_000_000_000, "1.00 GB"),
        (1_500_000_000, "1.50 GB"),
        (1_000_000_000_000, "1.00 TB"),
        (1_000_000_000_000_000, "1.00 PB"),
        (1_000_000_000_000_000_000, "1.00 EB"),
        (u64::MAX, "18.45 EB"),
    ] {
        let original = format!("0|/sample|1|mode|0|0|{size}|1|2|3|4\n");
        let (_dir, workers) = start(&original);
        let text = details(&workers, chrono_tz::UTC);
        workers.shutdown();

        assert!(text.contains(&format!("Size: {expected}\n")), "{text}");
        assert!(text.ends_with(&original));
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

#[test]
fn uac_details_show_collected_hashes_with_source_lines_after_resuming() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("uac");
    fs::create_dir_all(root.join("bodyfile")).unwrap();
    fs::create_dir_all(root.join("hash_executables")).unwrap();
    let original = "0|/usr/bin/tool|1|-rwxr-xr-x|0|0|10|100|200|300|400\n";
    fs::write(root.join("bodyfile/bodyfile.txt"), original).unwrap();
    let md5 = "0123456789abcdef0123456789abcdef";
    let sha1 = "0123456789abcdef0123456789abcdef01234567";
    fs::write(
        root.join("hash_executables/hash_executables.md5"),
        format!("{md5}  /usr/bin/tool\n"),
    )
    .unwrap();
    fs::write(
        root.join("hash_executables/hash_executables.sha1"),
        format!("SHA1 (/usr/bin/tool) = {sha1}\n"),
    )
    .unwrap();
    for inputs in [vec![source::resolve(&root, None, false).unwrap()], vec![]] {
        let workers = Workers::start(dir.path().join("workspace"), inputs, None, None, None, None);
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
        let text = details(&workers, chrono_tz::UTC);
        assert!(text.contains(&format!("UAC MD5: {md5}\n")));
        assert!(text.contains(&format!("UAC SHA-1: {sha1}\n")));
        assert!(text.contains("hash_executables.md5 (line 1)"));
        assert!(text.contains("hash_executables.sha1 (line 1)"));
        assert!(text.contains("Hash / original first field: 0\n"));
        assert!(text.ends_with(original));
        workers.shutdown();
    }
}
