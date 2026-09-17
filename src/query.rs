use crate::{
    model::{Burst, EventKey, Predicate, Row, Tag, View, validate_view},
    store::{Store, configure},
};
use anyhow::{Result, bail};
use regex::{Regex, RegexSet};
use rusqlite::{Connection, OpenFlags, OptionalExtension, params};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

#[derive(Clone)]
pub struct Cancellation {
    pub clock: Arc<AtomicU64>,
    pub generation: u64,
}
pub(crate) struct ResetCancellation<'a>(pub &'a Connection);
impl Drop for ResetCancellation<'_> {
    fn drop(&mut self) {
        let _ = self.0.progress_handler(0, None::<fn() -> bool>);
    }
}

#[derive(Clone, Copy)]
pub struct Page {
    pub anchor: Option<EventKey>,
    pub reverse: bool,
    pub inclusive: bool,
    pub limit: usize,
}
impl Default for Page {
    fn default() -> Self {
        Self {
            anchor: None,
            reverse: false,
            inclusive: false,
            limit: 256,
        }
    }
}
impl Cancellation {
    pub fn check(&self) -> Result<()> {
        if self.clock.load(Ordering::Relaxed) != self.generation {
            bail!("Cancelled");
        }
        Ok(())
    }
    pub fn never() -> Self {
        Self {
            clock: Arc::new(AtomicU64::new(0)),
            generation: 0,
        }
    }
    pub fn install(&self, conn: &Connection) -> Result<()> {
        let cancel = self.clone();
        conn.progress_handler(1000, Some(move || cancel.check().is_err()))?;
        Ok(())
    }
}

pub struct Matcher {
    pub predicate: Predicate,
    include: Option<Regex>,
    excludes: Exclusions,
}

enum Exclusions {
    Set(RegexSet),
    Separate(Vec<Regex>),
}

impl Exclusions {
    fn new(patterns: &[String]) -> Result<Self> {
        if patterns.len() > 1
            && let Ok(set) = RegexSet::new(patterns)
        {
            return Ok(Self::Set(set));
        }
        // A combined automaton can exceed the regex size limit even when
        // every individual expression is valid. Keep those views usable.
        Ok(Self::Separate(
            patterns
                .iter()
                .map(|s| Regex::new(s))
                .collect::<Result<_, _>>()?,
        ))
    }

    fn is_match(&self, path: &str) -> bool {
        match self {
            Self::Set(set) => set.is_match(path),
            Self::Separate(patterns) => patterns.iter().any(|re| re.is_match(path)),
        }
    }
}
impl Matcher {
    pub fn new(predicate: &Predicate) -> Result<Self> {
        Ok(Self {
            predicate: predicate.clone(),
            include: if predicate.include.is_empty() {
                None
            } else {
                Some(Regex::new(&predicate.include)?)
            },
            excludes: Exclusions::new(&predicate.exclude)?,
        })
    }
    pub fn matches(&self, path: &str, host: &str, tag: Tag) -> bool {
        self.include.as_ref().is_none_or(|re| re.is_match(path))
            && !self.excludes.is_match(path)
            && (self.predicate.hosts.is_empty() || self.predicate.hosts.iter().any(|h| h == host))
            && self.predicate.tags.as_ref().is_none_or(|tags| {
                if tags.is_empty() {
                    tag != Tag::None
                } else {
                    tags.contains(&tag)
                }
            })
    }
}

fn cache_path(root: &Path, predicate: &Predicate) -> Result<PathBuf> {
    let key = format!("{:x}", Sha256::digest(serde_json::to_vec(predicate)?));
    Ok(root.join("views").join(format!("{key}.sqlite")))
}

/// Cache only tag membership. Path and host filters run while fetching pages.
pub fn cache(
    store: &Store,
    predicate: &Predicate,
    cancel: &Cancellation,
    progress: &mut impl FnMut(u64),
) -> Result<Option<PathBuf>> {
    cancel.check()?;
    cancel.install(&store.conn)?;
    let _reset = ResetCancellation(&store.conn);
    let predicate = Predicate {
        tags: predicate.tags.clone(),
        ..Predicate::default()
    };
    if predicate.is_all() {
        return Ok(None);
    }
    let path = cache_path(&store.root, &predicate)?;
    let revision = store.revision()?;
    if path.exists() {
        let db = Connection::open(&path)?;
        let stored = db
            .query_row("SELECT revision FROM config", [], |r| r.get::<_, i64>(0))
            .optional()?;
        if stored == Some(revision) {
            return Ok(Some(path));
        }
    }
    let matcher = Matcher::new(&predicate)?;
    let temporary = tempfile::NamedTempFile::new_in(store.root.join("views"))?;
    let mut db = Connection::open(temporary.path())?;
    configure(&db)?;
    cancel.install(&db)?;
    db.execute_batch("PRAGMA journal_mode=OFF; PRAGMA synchronous=OFF;
        CREATE TABLE events(epoch INTEGER,record INTEGER,macb INTEGER,PRIMARY KEY(epoch,record)) WITHOUT ROWID;
        CREATE INDEX by_record ON events(record,epoch);
        CREATE TABLE config(revision INTEGER,predicate TEXT);")?;
    let tx = db.transaction()?;
    {
        let mut insert = tx.prepare_cached("INSERT INTO events VALUES(?,?,?)")?;
        let mut event_stmt = store
            .conn
            .prepare("SELECT epoch,macb FROM events WHERE record=?")?;
        // Sparse smart tabs use the tag index; exclusion edits reuse this cache.
        let tag_clause = match &predicate.tags {
            Some(tags) if tags.is_empty() => " WHERE tag IN (1,2,3)".to_string(),
            Some(tags) => format!(
                " WHERE tag IN ({})",
                tags.iter()
                    .map(|t| (*t as i64).to_string())
                    .collect::<Vec<_>>()
                    .join(",")
            ),
            None => String::new(),
        };
        let mut stmt = store
            .conn
            .prepare(&format!("SELECT id,path,host,tag FROM records{tag_clause}"))?;
        let mut rows = stmt.query([])?;
        let mut count = 0;
        while let Some(row) = rows.next()? {
            if count % 1024 == 0 {
                cancel.check()?;
                progress(count);
            }
            count += 1;
            let id: i64 = row.get(0)?;
            if matcher.matches(
                &row.get::<_, String>(1)?,
                &row.get::<_, String>(2)?,
                Tag::from_i64(row.get(3)?),
            ) {
                let mut events = event_stmt.query([id])?;
                while let Some(event) = events.next()? {
                    insert.execute(params![event.get::<_, i64>(0)?, id, event.get::<_, u8>(1)?])?;
                }
            }
        }
    }
    cancel.check()?;
    if store.revision()? != revision {
        bail!("Annotations changed while filtering; retry");
    }
    tx.execute(
        "INSERT INTO config VALUES(?,?)",
        params![revision, serde_json::to_string(&predicate)?],
    )?;
    tx.commit()?;
    drop(db);
    temporary.persist(&path)?;
    Ok(Some(path))
}

/// Incrementally repair completed view caches after a single record assessment.
pub fn update_caches(store: &Store, id: i64, previous: i64) -> Result<()> {
    update_caches_for_records(store, &[id], previous)
}

/// Repair each completed cache once after a batch assessment.
pub fn update_caches_for_records(store: &Store, ids: &[i64], previous: i64) -> Result<()> {
    let mut records = Vec::with_capacity(ids.len());
    for &id in ids {
        let (path, host, tag): (String, String, i64) =
            store
                .conn
                .query_row("SELECT path,host,tag FROM records WHERE id=?", [id], |r| {
                    Ok((r.get(0)?, r.get(1)?, r.get(2)?))
                })?;
        records.push((id, path, host, Tag::from_i64(tag)));
    }
    for entry in fs::read_dir(store.root.join("views"))? {
        let entry = entry?;
        if entry.path().extension().is_none_or(|e| e != "sqlite") {
            continue;
        }
        let mut conn = Connection::open(entry.path())?;
        let (revision, json): (i64, String) =
            conn.query_row("SELECT revision,predicate FROM config", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })?;
        if revision != previous {
            continue;
        }
        let predicate: Predicate = serde_json::from_str(&json)?;
        if !predicate.include.is_empty()
            || !predicate.exclude.is_empty()
            || !predicate.hosts.is_empty()
        {
            // Older releases cached complete path filters; those caches are no longer used.
            continue;
        }
        let matcher = Matcher::new(&predicate)?;
        let tx = conn.transaction()?;
        for (id, path, host, tag) in &records {
            tx.execute("DELETE FROM events WHERE record=?", [id])?;
            if matcher.matches(path, host, *tag) {
                let mut statement = store
                    .conn
                    .prepare("SELECT epoch,macb FROM events WHERE record=?")?;
                for event in
                    statement.query_map([id], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, u8>(1)?)))?
                {
                    let (epoch, mask) = event?;
                    tx.execute("INSERT INTO events VALUES(?,?,?)", params![epoch, id, mask])?;
                }
            }
        }
        tx.execute("UPDATE config SET revision=?", [store.revision()?])?;
        tx.commit()?;
    }
    Ok(())
}

pub fn page(
    store: &Store,
    view: &View,
    page: Page,
    cancel: &Cancellation,
    progress: impl FnMut(u64),
) -> Result<Vec<Row>> {
    Ok(page_omitting(store, view, page, &[], None, cancel, progress)?.rows)
}

pub(crate) struct PageRows<T> {
    pub rows: Vec<T>,
    /// A bounded scan stopped here with potentially more matching rows ahead.
    pub continuation: Option<EventKey>,
}

/// Folded display pages may omit events; raw queries and exports never do.
pub(crate) fn page_omitting(
    store: &Store,
    view: &View,
    page: Page,
    bursts: &[Burst],
    scan_budget: Option<u64>,
    cancel: &Cancellation,
    mut progress: impl FnMut(u64),
) -> Result<PageRows<Row>> {
    let Page {
        anchor,
        reverse,
        inclusive,
        limit,
    } = page;
    validate_view(view)?;
    cancel.check()?;
    let limit = limit.min(4096);
    if limit == 0 {
        return Ok(PageRows {
            rows: Vec::new(),
            continuation: None,
        });
    }
    let matcher = Matcher::new(&view.predicate)?;
    let path = cache(store, &view.predicate, cancel, &mut progress)?;
    let cache_db = path
        .as_ref()
        .map(|path| Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY))
        .transpose()?;
    if let Some(db) = &cache_db {
        configure(db)?;
        db.execute(
            "ATTACH DATABASE ? AS record_store",
            [store.root.join("index.sqlite").to_string_lossy()],
        )?;
        db.execute_batch("PRAGMA record_store.cache_size=-32768;")?;
    }
    let db = cache_db.as_ref().unwrap_or(&store.conn);
    cancel.install(db)?;
    let _reset = ResetCancellation(db);
    let mut lo = EventKey {
        epoch: view
            .after
            .unwrap_or(i64::MIN)
            .max(if view.bogus { i64::MIN } else { 1 }),
        record: i64::MIN,
    };
    let mut hi = EventKey {
        epoch: view.before.unwrap_or(i64::MAX),
        record: i64::MAX,
    };
    let mut lower = ">=";
    let mut upper = "<=";
    if let Some(anchor) = anchor {
        if reverse && anchor <= hi {
            hi = anchor;
            upper = if inclusive { "<=" } else { "<" };
        } else if !reverse && anchor >= lo {
            lo = anchor;
            lower = if inclusive { ">=" } else { ">" };
        }
    }
    let summaries = bursts.iter().map(|burst| burst.first).filter(|&key| {
        key >= lo && key <= hi && (key != lo || lower == ">=") && (key != hi || upper == "<=")
    });
    let first_summary = if reverse {
        summaries.max()
    } else {
        summaries.min()
    };
    let exclusive_bursts: Vec<_> = bursts
        .iter()
        .filter(|burst| burst.exclusive && burst.last >= lo && burst.first <= hi)
        .collect();
    let mut result = Vec::with_capacity(limit);
    let mut legacy_metadata = HashMap::new();
    let mut checked = 0_u64;
    let mut continuation = None;
    loop {
        cancel.check()?;
        let sql = cursor_sql(reverse, lower, upper, cache_db.is_some());
        let mut stmt = db.prepare_cached(&sql)?;
        let mut candidates = stmt.query(params![
            lo.epoch,
            lo.record,
            hi.epoch,
            hi.record,
            view.time_mask,
        ])?;
        let mut skipped_to = None;
        while let Some(candidate) = candidates.next()? {
            cancel.check()?;
            let key = EventKey {
                epoch: candidate.get(0)?,
                record: candidate.get(1)?,
            };
            // Seek over known host-only runs, rather than reading every hidden
            // event on each visit. Mixed-host runs retain the other hosts.
            if let Some(burst) = exclusive_bursts
                .iter()
                .find(|burst| burst.contains_key(key))
            {
                skipped_to = Some(if reverse { burst.first } else { burst.last });
                break;
            }
            // Borrow text from SQLite while filtering. Only visible rows need
            // owned strings and display metadata.
            let host = candidate.get_ref(3)?.as_str()?;
            let path = candidate.get_ref(4)?.as_str()?;
            let tag = Tag::from_i64(candidate.get(5)?);
            checked += 1;
            if checked.is_multiple_of(1024) {
                progress(checked);
            }
            if matcher.matches(path, host, tag)
                && !bursts
                    .iter()
                    .any(|burst| burst.host == host && burst.contains_key(key))
            {
                let directory: Option<bool> = candidate.get(8)?;
                let zero_size: Option<bool> = candidate.get(9)?;
                let fallback = if directory.is_none() || zero_size.is_none() {
                    *legacy_metadata.entry(key.record).or_insert_with(|| {
                        // Older indexes lack display metadata. Inspect only matching
                        // rows, and keep browsing available if a source is offline.
                        store
                            .detail(key.record)
                            .map(|detail| {
                                (detail.record.is_directory(), detail.record.is_zero_size())
                            })
                            .unwrap_or_default()
                    })
                } else {
                    (false, false)
                };
                result.push(Row {
                    key,
                    macb: candidate.get(2)?,
                    host: host.to_owned(),
                    path: path.to_owned(),
                    tag,
                    automatic: candidate.get(6)?,
                    ioc_match: candidate.get(7)?,
                    is_directory: directory.unwrap_or(fallback.0),
                    zero_size: zero_size.unwrap_or(fallback.1),
                });
                if result.len() == limit {
                    break;
                }
            }
            // Publish usable rows before a sparse filter scans the rest of the
            // investigation merely to fill the prefetch buffer. Empty searches
            // still continue until a row/summary is found or the range is exhausted.
            if scan_budget.is_some_and(|budget| checked >= budget)
                && (!result.is_empty()
                    || first_summary.is_some_and(|summary| {
                        if reverse {
                            summary >= key
                        } else {
                            summary <= key
                        }
                    }))
            {
                continuation = Some(key);
                break;
            }
        }
        let Some(last) = skipped_to else {
            break;
        };
        if reverse {
            hi = last;
            upper = "<";
        } else {
            lo = last;
            lower = ">";
        }
    }
    progress(checked);
    cancel.check()?;
    if reverse {
        result.reverse();
    }
    Ok(PageRows {
        rows: result,
        continuation,
    })
}

fn cursor_sql(reverse: bool, lower: &str, upper: &str, cached: bool) -> String {
    let order = if reverse { "DESC" } else { "ASC" };
    let records = if cached {
        "record_store.records"
    } else {
        "records"
    };
    // Both bounds must be composite. A separate scalar epoch range can make
    // SQLite scan from the timeframe's beginning, ignoring the cursor seek.
    // CROSS JOIN keeps events as the outer loop, so rows stream in key order
    // without sorting the timeframe before the caller can stop at its limit.
    format!(
        "SELECT e.epoch,e.record,e.macb,r.host,r.path,r.tag,
                r.manual_tag IS NULL,r.matches!='[]',r.is_directory,r.zero_size
         FROM events e CROSS JOIN {records} r ON r.id=e.record
         WHERE (e.epoch,e.record) {lower} (?,?)
         AND (e.epoch,e.record) {upper} (?,?) AND (e.macb & ?) != 0
         ORDER BY e.epoch {order},e.record {order}"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn combined_exclusions_preserve_regex_flags_boundaries_and_unicode() {
        let patterns = [
            r"(?i)^/tmp(?:[/\\]|$)",
            r"^/CaseSensitive$",
            r"^C:\\Windows(?:\\|$)",
            r"/café/\p{Greek}+$",
            r"(?m)^hidden$",
            r"\.log$",
        ];
        let excludes: Vec<_> = patterns.iter().map(|s| s.to_string()).collect();
        let separate: Vec<_> = patterns.iter().map(|s| Regex::new(s).unwrap()).collect();
        for extra in [None, Some(""), Some(r"\b")] {
            let mut predicate = Predicate {
                include: r"(?s).+".into(),
                exclude: excludes.clone(),
                hosts: vec!["host-a".into()],
                tags: Some(vec![Tag::Suspicious, Tag::Malicious]),
            };
            if let Some(pattern) = extra {
                predicate.exclude.push(pattern.into());
            }
            let matcher = Matcher::new(&predicate).unwrap();
            for path in [
                "/TMP/file",
                "/tmp",
                "/tmpfile",
                "/CaseSensitive",
                "/casesensitive",
                r"C:\Windows\System32",
                r"C:\WindowsExtra",
                "/café/αβ",
                "/café/ascii",
                "/prefix\nhidden\nsuffix",
                "/file.log",
                "/file.LOG",
                "/keep",
                "",
            ] {
                for host in ["host-a", "host-b"] {
                    for tag in [Tag::None, Tag::Suspicious, Tag::Malicious] {
                        let expected = !path.is_empty()
                            && !separate.iter().any(|re| re.is_match(path))
                            && extra.is_none_or(|re| !Regex::new(re).unwrap().is_match(path))
                            && host == "host-a"
                            && tag != Tag::None;
                        assert_eq!(
                            matcher.matches(path, host, tag),
                            expected,
                            "{extra:?}: {path:?}, {host}, {tag:?}",
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn invalid_exclusions_still_fail_instead_of_disabling_filtering() {
        for exclude in [vec!["[".into()], vec!["^/valid".into(), "[".into()]] {
            assert!(
                Matcher::new(&Predicate {
                    exclude,
                    ..Predicate::default()
                })
                .is_err()
            );
        }
    }

    #[test]
    fn individually_valid_exclusions_survive_the_combined_size_limit() {
        let predicate = Predicate {
            exclude: (0..20).map(|n| format!(r"^\w{{20}}/{n}$")).collect(),
            ..Predicate::default()
        };
        assert!(RegexSet::new(&predicate.exclude).is_err());
        let matcher = Matcher::new(&predicate).unwrap();
        let prefix = "α".repeat(20);
        assert!(!matcher.matches(&format!("{prefix}/19"), "", Tag::None));
        assert!(matcher.matches(&format!("{prefix}/20"), "", Tag::None));
    }

    #[test]
    fn sqlite_uses_the_complete_cursor_for_both_index_bounds() {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch(
            "CREATE TABLE events(epoch INTEGER,record INTEGER,macb INTEGER,PRIMARY KEY(epoch,record)) WITHOUT ROWID;
             CREATE TABLE records(id INTEGER PRIMARY KEY,host TEXT,path TEXT,tag INTEGER,
                 manual_tag INTEGER,matches TEXT,is_directory INTEGER,zero_size INTEGER);
             ATTACH DATABASE ':memory:' AS record_store;
             CREATE TABLE record_store.records(id INTEGER PRIMARY KEY,host TEXT,path TEXT,tag INTEGER,
                 manual_tag INTEGER,matches TEXT,is_directory INTEGER,zero_size INTEGER);",
        ).unwrap();
        for cached in [false, true] {
            for reverse in [true, false] {
                let sql = format!(
                    "EXPLAIN QUERY PLAN {}",
                    cursor_sql(reverse, ">", "<=", cached)
                );
                let plan = db
                    .prepare(&sql)
                    .unwrap()
                    .query_map(params![100, 40, 200, 80, 15], |r| r.get::<_, String>(3))
                    .unwrap()
                    .collect::<rusqlite::Result<Vec<_>>>()
                    .unwrap();
                assert_eq!(plan.len(), 2, "{plan:?}");
                assert!(
                    plan[0].contains("((epoch,record)>(?,?) AND (epoch,record)<(?,?))"),
                    "{plan:?}"
                );
                assert!(
                    plan[1].contains("SEARCH r USING INTEGER PRIMARY KEY"),
                    "{plan:?}"
                );
            }
        }
    }
}
