use crate::{
    model::{Burst, EventKey, Predicate, Row, Tag, View, validate_view},
    store::{Store, configure},
};
use anyhow::{Result, bail};
use regex::Regex;
use rusqlite::{Connection, OptionalExtension, params};
use sha2::{Digest, Sha256};
use std::{
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
    excludes: Vec<Regex>,
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
            excludes: predicate
                .exclude
                .iter()
                .map(|s| Regex::new(s))
                .collect::<Result<_, _>>()?,
        })
    }
    pub fn matches(&self, path: &str, host: &str, tag: Tag) -> bool {
        self.include.as_ref().is_none_or(|re| re.is_match(path))
            && !self.excludes.iter().any(|re| re.is_match(path))
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
    let (path, host, tag): (String, String, i64) =
        store
            .conn
            .query_row("SELECT path,host,tag FROM records WHERE id=?", [id], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?))
            })?;
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
        tx.execute("DELETE FROM events WHERE record=?", [id])?;
        if matcher.matches(&path, &host, Tag::from_i64(tag)) {
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
    page_omitting(store, view, page, &[], cancel, progress)
}

/// Folded display pages may omit events; raw queries and exports never do.
pub(crate) fn page_omitting(
    store: &Store,
    view: &View,
    page: Page,
    bursts: &[Burst],
    cancel: &Cancellation,
    mut progress: impl FnMut(u64),
) -> Result<Vec<Row>> {
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
        return Ok(Vec::new());
    }
    let matcher = Matcher::new(&view.predicate)?;
    let path = cache(store, &view.predicate, cancel, &mut progress)?;
    let cache_db = path.as_ref().map(Connection::open).transpose()?;
    if let Some(db) = &cache_db {
        configure(db)?;
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
    let mut details = store.conn.prepare_cached(
        "SELECT host,path,tag,manual_tag IS NULL,matches!='[]' FROM records WHERE id=?",
    )?;
    let mut result = Vec::with_capacity(limit);
    let mut checked = 0_u64;
    loop {
        cancel.check()?;
        let sql = cursor_sql(reverse, lower, upper);
        let mut stmt = db.prepare_cached(&sql)?;
        let keys = stmt
            .query_map(
                params![
                    lo.epoch,
                    lo.record,
                    hi.epoch,
                    hi.record,
                    view.time_mask,
                    limit as i64
                ],
                |r| {
                    Ok((
                        EventKey {
                            epoch: r.get(0)?,
                            record: r.get(1)?,
                        },
                        r.get::<_, u8>(2)?,
                    ))
                },
            )?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let mut skipped_to = None;
        for &(key, macb) in &keys {
            cancel.check()?;
            // Seek over known host-only runs, rather than reading every hidden
            // event on each visit. Mixed-host runs retain the other hosts.
            if let Some(burst) = bursts
                .iter()
                .find(|burst| burst.exclusive && burst.contains_key(key))
            {
                skipped_to = Some(if reverse { burst.first } else { burst.last });
                break;
            }
            let row = details.query_row([key.record], |r| {
                Ok(Row {
                    key,
                    macb,
                    host: r.get(0)?,
                    path: r.get(1)?,
                    tag: Tag::from_i64(r.get(2)?),
                    automatic: r.get(3)?,
                    ioc_match: r.get(4)?,
                })
            })?;
            checked += 1;
            if checked.is_multiple_of(1024) {
                progress(checked);
            }
            if matcher.matches(&row.path, &row.host, row.tag)
                && !bursts
                    .iter()
                    .any(|burst| burst.host == row.host && burst.contains_key(key))
            {
                result.push(row);
                if result.len() == limit {
                    break;
                }
            }
        }
        if result.len() == limit || (skipped_to.is_none() && keys.len() < limit) {
            break;
        }
        let last = skipped_to.unwrap_or_else(|| keys.last().unwrap().0);
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
    Ok(result)
}

fn cursor_sql(reverse: bool, lower: &str, upper: &str) -> String {
    let order = if reverse { "DESC" } else { "ASC" };
    // Both bounds must be composite. A separate scalar epoch range can make
    // SQLite scan from the timeframe's beginning, ignoring the cursor seek.
    format!(
        "SELECT epoch,record,macb FROM events WHERE (epoch,record) {lower} (?,?)
        AND (epoch,record) {upper} (?,?) AND (macb & ?) != 0
        ORDER BY epoch {order},record {order} LIMIT ?"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn sqlite_uses_the_complete_cursor_for_both_index_bounds() {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch("CREATE TABLE events(epoch INTEGER,record INTEGER,macb INTEGER,PRIMARY KEY(epoch,record)) WITHOUT ROWID;").unwrap();
        for reverse in [true, false] {
            let sql = format!("EXPLAIN QUERY PLAN {}", cursor_sql(reverse, ">", "<="));
            let plan: String = db
                .query_row(&sql, params![100, 40, 200, 80, 15, 100], |r| r.get(3))
                .unwrap();
            assert!(
                plan.contains("((epoch,record)>(?,?) AND (epoch,record)<(?,?))"),
                "{plan}"
            );
        }
    }
}
