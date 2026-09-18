//! Count actual regex exclusions independently of pagination and SQL callbacks.
use crate::{
    model::{EventKey, Predicate, Session, Tag, View},
    query::Matcher,
    store::Store,
};
use anyhow::{Result, ensure};
use rusqlite::params;
use serde::Serialize;
use std::time::Instant;

#[derive(Serialize, Default)]
struct Counts {
    label: String,
    candidates: u64,
    excluded: u64,
    rejected_by_other_filters: u64,
    matching_fold_members: u64,
    unfolded_matches: u64,
    first: Option<EventKey>,
    last: Option<EventKey>,
    exhausted: bool,
}

fn bounds(view: &View) -> (i64, i64) {
    (
        view.after
            .unwrap_or(i64::MIN)
            .max(if view.bogus { i64::MIN } else { 1 }),
        view.before.unwrap_or(i64::MAX),
    )
}

fn scan(
    store: &Store,
    view: &View,
    label: &str,
    anchor: EventKey,
    candidate_limit: u64,
    match_limit: u64,
) -> Result<Counts> {
    let exclusions = Matcher::new(&Predicate {
        exclude: view.predicate.exclude.clone(),
        ..Predicate::default()
    })?;
    let full = Matcher::new(&view.predicate)?;
    let (_, end) = bounds(view);
    let mut stmt = store.conn.prepare(
        "SELECT e.epoch,e.record,r.path,r.host,r.tag
         FROM events e CROSS JOIN records r ON r.id=e.record
         WHERE (e.epoch,e.record)>=(?,?)
         AND (e.epoch,e.record)<=(?,?) AND (e.macb & ?5) != 0
         AND (?6 = 0 OR e.macb = ?5)
         ORDER BY e.epoch,e.record",
    )?;
    let mut rows = stmt.query(params![
        anchor.epoch,
        anchor.record,
        end,
        i64::MAX,
        view.time_mask,
        view.time_exact,
    ])?;
    let mut counts = Counts {
        label: label.into(),
        ..Counts::default()
    };
    while counts.candidates < candidate_limit && counts.unfolded_matches < match_limit {
        let Some(row) = rows.next()? else {
            counts.exhausted = true;
            break;
        };
        let key = EventKey {
            epoch: row.get(0)?,
            record: row.get(1)?,
        };
        counts.first.get_or_insert(key);
        counts.last = Some(key);
        counts.candidates += 1;
        let path = row.get_ref(2)?.as_str()?;
        if !exclusions.matches(path, "", Tag::None) {
            counts.excluded += 1;
            continue;
        }
        let host = row.get_ref(3)?.as_str()?;
        if !full.matches(path, host, Tag::from_i64(row.get(4)?)) {
            counts.rejected_by_other_filters += 1;
        } else if view
            .bursts
            .iter()
            .any(|b| b.host == host && b.contains_key(key))
        {
            counts.matching_fold_members += 1;
        } else {
            counts.unfolded_matches += 1;
        }
    }
    ensure!(
        counts.candidates
            == counts.excluded
                + counts.rejected_by_other_filters
                + counts.matching_fold_members
                + counts.unfolded_matches
    );
    Ok(counts)
}

pub fn run(store: &Store, session: &Session) -> Result<()> {
    let view = &session.views[session.active];
    let (begin, end) = bounds(view);
    let started = Instant::now();
    // Bound this read-only measurement even on unexpectedly large workspaces.
    store
        .conn
        .progress_handler(10_000, Some(move || started.elapsed().as_secs() >= 180))?;
    eprintln!("Counting time-eligible events in the saved view...");
    let total: i64 = store.conn.query_row(
        "SELECT COUNT(*) FROM events WHERE epoch>=? AND epoch<=? AND (macb & ?3) != 0
         AND (?4 = 0 OR macb = ?3)",
        params![begin, end, view.time_mask, view.time_exact],
        |row| row.get(0),
    )?;
    ensure!(
        total >= 2_000_000,
        "This sampling plan needs at least 2M events"
    );
    println!(
        "{}",
        serde_json::json!({
            "label": "population",
            "events": total,
            "exclusions": view.predicate.exclude.len(),
            "folds": view.bursts.len(),
            "revision": store.revision()?,
        })
    );
    let anchor = view.top.unwrap_or(EventKey {
        epoch: begin,
        record: i64::MIN,
    });
    let saved = scan(store, view, "saved_position", anchor, 10_000_000, 50)?;
    ensure!(
        saved.unfolded_matches == 50,
        "Saved-position scan hit its limit"
    );
    println!("{}", serde_json::to_string(&saved)?);
    for bucket in 0..10 {
        // Take 100k consecutive events at each decile's midpoint. This gives
        // equal-sized, distributed windows, not a random population estimate.
        let offset = total * (2 * bucket + 1) / 20;
        let anchor = store.conn.query_row(
            "SELECT epoch,record FROM events
             WHERE epoch>=? AND epoch<=? AND (macb & ?3) != 0
             AND (?4 = 0 OR macb = ?3)
             ORDER BY epoch,record LIMIT 1 OFFSET ?",
            params![begin, end, view.time_mask, view.time_exact, offset],
            |row| {
                Ok(EventKey {
                    epoch: row.get(0)?,
                    record: row.get(1)?,
                })
            },
        )?;
        let counts = scan(
            store,
            view,
            &format!("sample_{bucket}"),
            anchor,
            100_000,
            u64::MAX,
        )?;
        eprintln!("Counted sample {} of 10", bucket + 1);
        println!("{}", serde_json::to_string(&counts)?);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Burst;
    use rusqlite::Connection;

    #[test]
    fn rate_counts_separate_exclusions_filters_and_folded_matches() {
        let store = Store {
            conn: Connection::open_in_memory().unwrap(),
            root: Default::default(),
        };
        store
            .conn
            .execute_batch(
                "CREATE TABLE records(id INTEGER PRIMARY KEY,path TEXT,host TEXT,tag INTEGER);
                 CREATE TABLE events(epoch INTEGER,record INTEGER,macb INTEGER,
                    PRIMARY KEY(epoch,record)) WITHOUT ROWID;
                 INSERT INTO records VALUES
                    (1,'/drop','host',0),(2,'/keep/folded','host',0),
                    (3,'/keep/visible','host',0),(4,'/other','host',0),
                    (5,'/keep/later','host',0);
                 INSERT INTO events VALUES
                    (100,1,15),(101,2,15),(102,3,15),(103,4,15),(104,5,15);",
            )
            .unwrap();
        let anchor = EventKey {
            epoch: 100,
            record: i64::MIN,
        };
        let folded = EventKey {
            epoch: 101,
            record: 2,
        };
        let mut view = View::default();
        view.predicate.include = "^/keep/".into();
        view.predicate.exclude = vec!["^/drop$".into()];
        view.bursts.push(Burst {
            first: folded,
            last: folded,
            origin: folded,
            host: "host".into(),
            count: 1,
            macb: 15,
            tagged: 0,
            ioc_matches: 0,
            exclusive: true,
        });
        let counts = scan(&store, &view, "all", anchor, 100, u64::MAX).unwrap();
        assert_eq!(counts.candidates, 5);
        assert_eq!(counts.excluded, 1);
        assert_eq!(counts.rejected_by_other_filters, 1);
        assert_eq!(counts.matching_fold_members, 1);
        assert_eq!(counts.unfolded_matches, 2);
        assert!(counts.exhausted);

        let counts = scan(&store, &view, "one_match", anchor, 100, 1).unwrap();
        assert_eq!(counts.candidates, 3);
        assert_eq!(counts.excluded, 1);
        assert_eq!(counts.matching_fold_members, 1);
        assert_eq!(counts.unfolded_matches, 1);
        assert!(!counts.exhausted);

        view.before = Some(101);
        let counts = scan(&store, &view, "bounded", anchor, 100, u64::MAX).unwrap();
        assert_eq!(counts.candidates, 2);
        assert_eq!(counts.excluded, 1);
        assert_eq!(counts.matching_fold_members, 1);
        assert_eq!(counts.unfolded_matches, 0);
        assert!(counts.exhausted);
    }
}
