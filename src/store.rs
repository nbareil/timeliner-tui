use crate::{
    bodyfile::Record,
    ioc::Rules,
    model::{EventKey, Session, Tag, TagChange},
    source::{self, Input},
};
use anyhow::{Context, Result, bail};
use rusqlite::{Connection, OpenFlags, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File},
    io::{BufRead, BufReader},
    path::{Path, PathBuf},
    time::Duration,
};

pub struct Store {
    pub conn: Connection,
    pub root: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceInfo {
    pub id: i64,
    pub path: PathBuf,
    pub host: String,
    pub size: u64,
    pub modified: String,
    pub sha256: Vec<u8>,
}

#[derive(Debug)]
pub struct Detail {
    pub source: SourceInfo,
    pub offset: u64,
    pub line: u64,
    pub raw: Vec<u8>,
    pub record: Record,
    pub tag: Tag,
    pub manual: bool,
    pub matches: Vec<usize>,
    pub host: String,
}

pub struct RecordReference {
    pub source: i64,
    pub offset: u64,
    pub length: u64,
    pub line: u64,
    pub sha256: Vec<u8>,
    pub tag: Tag,
    pub manual: bool,
    pub matches: Vec<usize>,
    pub host: String,
}

impl Store {
    pub fn open(root: &Path) -> Result<Self> {
        fs::create_dir_all(root.join("views"))?;
        let conn = Connection::open(root.join("index.sqlite"))?;
        configure(&conn)?;
        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA synchronous=FULL;
             CREATE TABLE IF NOT EXISTS meta(key TEXT PRIMARY KEY, value TEXT NOT NULL);
             INSERT OR IGNORE INTO meta VALUES('schema','1');
             INSERT OR IGNORE INTO meta VALUES('revision','0');
             CREATE TABLE IF NOT EXISTS sources(
               id INTEGER PRIMARY KEY, path TEXT NOT NULL UNIQUE, host TEXT NOT NULL,
               legacy INTEGER NOT NULL, size INTEGER NOT NULL, modified TEXT NOT NULL,
               sha256 BLOB, complete INTEGER NOT NULL DEFAULT 0);
             CREATE TABLE IF NOT EXISTS records(
               id INTEGER PRIMARY KEY, source INTEGER NOT NULL REFERENCES sources(id),
               offset INTEGER NOT NULL, length INTEGER NOT NULL, line INTEGER NOT NULL,
               sha256 BLOB NOT NULL, path TEXT NOT NULL, hash TEXT NOT NULL, host TEXT NOT NULL,
               matches TEXT NOT NULL, manual_tag INTEGER, tag INTEGER NOT NULL,
               is_directory INTEGER, zero_size INTEGER,
               UNIQUE(source,offset));
             CREATE INDEX IF NOT EXISTS records_tag ON records(tag,id);
             CREATE TABLE IF NOT EXISTS events(
               epoch INTEGER NOT NULL, record INTEGER NOT NULL REFERENCES records(id) ON DELETE CASCADE,
               macb INTEGER NOT NULL, PRIMARY KEY(epoch,record)) WITHOUT ROWID;
             CREATE INDEX IF NOT EXISTS events_record ON events(record,epoch);
             CREATE TABLE IF NOT EXISTS errors(
               source INTEGER NOT NULL REFERENCES sources(id), offset INTEGER NOT NULL,
               line INTEGER NOT NULL, length INTEGER NOT NULL, message TEXT NOT NULL);
             CREATE INDEX IF NOT EXISTS errors_source ON errors(source);
             CREATE TABLE IF NOT EXISTS uac_hash_files(
               source INTEGER NOT NULL, file TEXT NOT NULL,
               size INTEGER NOT NULL, modified TEXT NOT NULL, PRIMARY KEY(source,file));
             CREATE TABLE IF NOT EXISTS uac_hashes(
               source INTEGER NOT NULL, file TEXT NOT NULL, path TEXT NOT NULL,
               algorithm TEXT NOT NULL, digest TEXT NOT NULL, line INTEGER NOT NULL,
               offset INTEGER NOT NULL, length INTEGER NOT NULL, sha256 BLOB NOT NULL,
               PRIMARY KEY(source,file,line));
             CREATE INDEX IF NOT EXISTS uac_hashes_path ON uac_hashes(source,path,file);
             CREATE TABLE IF NOT EXISTS state(key TEXT PRIMARY KEY, value TEXT NOT NULL);"
        )?;
        let version: String =
            conn.query_row("SELECT value FROM meta WHERE key='schema'", [], |r| {
                r.get(0)
            })?;
        if version != "1" {
            bail!("Unsupported workspace schema {version}");
        }
        for column in ["is_directory", "zero_size"] {
            let present: bool = conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM pragma_table_info('records') WHERE name=?)",
                [column],
                |row| row.get(0),
            )?;
            if !present {
                conn.execute(
                    &format!("ALTER TABLE records ADD COLUMN {column} INTEGER"),
                    [],
                )?;
            }
        }
        Ok(Self {
            conn,
            root: root.to_owned(),
        })
    }

    pub fn reader(root: &Path) -> Result<Self> {
        let conn = Connection::open_with_flags(
            root.join("index.sqlite"),
            OpenFlags::SQLITE_OPEN_READ_ONLY,
        )?;
        configure(&conn)?;
        Ok(Self {
            conn,
            root: root.to_owned(),
        })
    }

    pub fn revision(&self) -> Result<i64> {
        Ok(self.conn.query_row(
            "SELECT CAST(value AS INTEGER) FROM meta WHERE key='revision'",
            [],
            |r| r.get(0),
        )?)
    }
    pub fn bump_revision(&self) -> Result<()> {
        self.conn.execute(
            "UPDATE meta SET value=CAST(value AS INTEGER)+1 WHERE key='revision'",
            [],
        )?;
        Ok(())
    }
    pub fn save_session(&self, session: &Session) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO state VALUES('session',?)",
            [serde_json::to_string(session)?],
        )?;
        Ok(())
    }
    pub fn session(&self) -> Result<Session> {
        let json: Option<String> = self
            .conn
            .query_row("SELECT value FROM state WHERE key='session'", [], |r| {
                r.get(0)
            })
            .optional()?;
        match json {
            Some(json) => Ok(serde_json::from_str(&json)?),
            None => Ok(Session::default()),
        }
    }
    pub fn rules(&self) -> Result<Rules> {
        let json: Option<String> = self
            .conn
            .query_row("SELECT value FROM state WHERE key='rules'", [], |r| {
                r.get(0)
            })
            .optional()?;
        Rules::new(match json {
            Some(json) => serde_json::from_str(&json)?,
            None => Vec::new(),
        })
    }
    pub fn save_rules(&self, rules: &Rules) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO state VALUES('rules',?)",
            [serde_json::to_string(&rules.rules)?],
        )?;
        Ok(())
    }

    pub fn sources(&self) -> Result<Vec<SourceInfo>> {
        let mut statement = self.conn.prepare(
            "SELECT id,path,host,size,modified,COALESCE(sha256,x'') FROM sources ORDER BY id",
        )?;
        Ok(statement
            .query_map([], |r| {
                Ok(SourceInfo {
                    id: r.get(0)?,
                    path: PathBuf::from(r.get::<_, String>(1)?),
                    host: r.get(2)?,
                    size: r.get::<_, i64>(3)? as u64,
                    modified: r.get(4)?,
                    sha256: r.get(5)?,
                })
            })?
            .collect::<rusqlite::Result<_>>()?)
    }

    pub fn pending_inputs(&self) -> Result<Vec<Input>> {
        let mut statement = self
            .conn
            .prepare("SELECT path,host,legacy FROM sources WHERE complete=0 ORDER BY id")?;
        Ok(statement
            .query_map([], |r| {
                Ok(Input {
                    path: PathBuf::from(r.get::<_, String>(0)?),
                    host: r.get(1)?,
                    legacy: r.get(2)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn source_warnings(&self) -> Result<Vec<String>> {
        Ok(self
            .sources()?
            .into_iter()
            .filter_map(|s| match source::metadata(&s.path) {
                Ok((size, modified)) if size == s.size && modified == s.modified => None,
                Ok(_) => Some(format!(
                    "Source changed: {} (relink original content)",
                    s.path.display()
                )),
                Err(_) => Some(format!(
                    "Source missing: {} (use --relink)",
                    s.path.display()
                )),
            })
            .collect())
    }

    pub fn relink(&self, id: i64, replacement: &Path) -> Result<()> {
        let expected: Vec<u8> = self.conn.query_row(
            "SELECT sha256 FROM sources WHERE id=? AND complete=1",
            [id],
            |r| r.get(0),
        )?;
        let path = replacement.canonicalize()?;
        if source::digest_file(&path)? != expected {
            bail!("Replacement does not match source SHA-256");
        }
        let (size, modified) = source::metadata(&path)?;
        self.conn.execute(
            "UPDATE sources SET path=?,size=?,modified=? WHERE id=?",
            params![path.to_string_lossy(), i64::try_from(size)?, modified, id],
        )?;
        Ok(())
    }

    /// Streaming import. Only a batch and one capped record occupy RAM.
    pub fn import(
        &mut self,
        input: &Input,
        rules: &Rules,
        mut progress: impl FnMut(u64, u64) -> bool,
    ) -> Result<()> {
        let existing: Option<(i64, bool)> = self
            .conn
            .query_row(
                "SELECT id,complete FROM sources WHERE path=?",
                [input.path.to_string_lossy()],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        if let Some((id, true)) = existing {
            let (host, legacy): (String, bool) =
                self.conn
                    .query_row("SELECT host,legacy FROM sources WHERE id=?", [id], |r| {
                        Ok((r.get(0)?, r.get(1)?))
                    })?;
            if host != input.host || legacy != input.legacy {
                let tx = self.conn.transaction()?;
                tx.execute("UPDATE records SET host=CASE WHEN ? AND hash NOT IN ('','0','-') THEN hash ELSE ? END WHERE source=?",
                    params![input.legacy,input.host,id])?;
                tx.execute(
                    "UPDATE sources SET host=?,legacy=? WHERE id=?",
                    params![input.host, input.legacy, id],
                )?;
                tx.execute(
                    "UPDATE meta SET value=CAST(value AS INTEGER)+1 WHERE key='revision'",
                    [],
                )?;
                tx.commit()?;
            }
            return Ok(());
        }
        let (size, modified) = source::metadata(&input.path)?;
        let sql_size = i64::try_from(size)?;
        if let Some((id, _)) = existing {
            self.conn
                .execute("DELETE FROM records WHERE source=?", [id])?;
            self.conn
                .execute("DELETE FROM errors WHERE source=?", [id])?;
            self.conn.execute("DELETE FROM sources WHERE id=?", [id])?;
        }
        self.conn.execute(
            "INSERT INTO sources(path,host,legacy,size,modified) VALUES(?,?,?,?,?)",
            params![
                input.path.to_string_lossy(),
                input.host,
                input.legacy,
                sql_size,
                modified
            ],
        )?;
        let source_id = self.conn.last_insert_rowid();
        let mut reader = BufReader::with_capacity(1024 * 1024, File::open(&input.path)?);
        let mut total_hash = Sha256::new();
        let mut offset = 0_u64;
        let mut line_no = 0_u64;
        loop {
            let transaction = self.conn.transaction()?;
            let mut eof = false;
            {
                let mut record_insert = transaction.prepare_cached(
                    "INSERT INTO records(source,offset,length,line,sha256,path,hash,host,matches,tag,is_directory,zero_size)
                     VALUES(?,?,?,?,?,?,?,?,?,?,?,?)")?;
                let mut event_insert =
                    transaction.prepare_cached("INSERT INTO events VALUES(?,?,?)")?;
                let mut error_insert =
                    transaction.prepare_cached("INSERT INTO errors VALUES(?,?,?,?,?)")?;
                for _ in 0..4096 {
                    let (bytes, length, checksum) = read_line(&mut reader, &mut total_hash)?;
                    if length == 0 {
                        eof = true;
                        break;
                    }
                    line_no += 1;
                    let parsed = if length > bytes.len() as u64 {
                        Err(anyhow::anyhow!(
                            "Record exceeds 16 MiB; original retained in source"
                        ))
                    } else {
                        Record::parse(&bytes)
                    };
                    match parsed {
                        Ok(record) => {
                            let hits = rules.matches(&record.fields[1], &record.fields[0]);
                            let tag = if hits.is_empty() { Tag::None } else { Tag::Ioc };
                            let host = if input.legacy
                                && !matches!(record.fields[0].as_str(), "" | "0" | "-")
                            {
                                &record.fields[0]
                            } else {
                                &input.host
                            };
                            record_insert.execute(params![
                                source_id,
                                offset as i64,
                                length as i64,
                                line_no as i64,
                                checksum,
                                record.fields[1],
                                record.fields[0],
                                host,
                                serde_json::to_string(&hits)?,
                                tag as i64,
                                record.is_directory(),
                                record.is_zero_size(),
                            ])?;
                            let record_id = transaction.last_insert_rowid();
                            for (epoch, mask) in record.events() {
                                event_insert.execute(params![epoch, record_id, mask])?;
                            }
                        }
                        Err(error) => {
                            error_insert.execute(params![
                                source_id,
                                offset as i64,
                                line_no as i64,
                                length as i64,
                                error.to_string()
                            ])?;
                        }
                    }
                    offset += length;
                }
            }
            transaction.commit()?;
            if !progress(offset, size) {
                bail!("Import cancelled; reopening restarts the incomplete source");
            }
            if eof {
                break;
            }
        }
        if source::metadata(&input.path)? != (size, modified) {
            bail!("Source changed during import: {}", input.path.display());
        }
        let tx = self.conn.transaction()?;
        tx.execute(
            "UPDATE sources SET sha256=?,complete=1 WHERE id=?",
            params![total_hash.finalize().to_vec(), source_id],
        )?;
        tx.execute(
            "UPDATE meta SET value=CAST(value AS INTEGER)+1 WHERE key='revision'",
            [],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn set_tag(&self, record: i64, manual: Option<Tag>) -> Result<TagChange> {
        Ok(self.set_tags(&[record], manual)?.remove(0))
    }

    /// Assess each distinct source record in one transaction.
    pub fn set_tags(&self, records: &[i64], manual: Option<Tag>) -> Result<Vec<TagChange>> {
        if records.is_empty() {
            return Ok(Vec::new());
        }
        let tx = self.conn.unchecked_transaction()?;
        let mut changes = Vec::new();
        for record in records
            .iter()
            .copied()
            .collect::<std::collections::BTreeSet<_>>()
        {
            let (host, path, before): (String, String, i64) = tx.query_row(
                "SELECT host,path,tag FROM records WHERE id=?",
                [record],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )?;
            let changed = tx.execute(
                "UPDATE records SET manual_tag=?, tag=COALESCE(?, CASE WHEN matches='[]' THEN 0 ELSE 2 END) WHERE id=?",
                params![manual.map(|t| t as i64), manual.map(|t| t as i64), record],
            )?;
            if changed != 1 {
                bail!("Record no longer exists");
            }
            let after = tx.query_row("SELECT tag FROM records WHERE id=?", [record], |row| {
                row.get::<_, i64>(0)
            })?;
            let events = tx
                .prepare("SELECT epoch,macb FROM events WHERE record=? ORDER BY epoch")?
                .query_map([record], |row| {
                    Ok((
                        EventKey {
                            epoch: row.get(0)?,
                            record,
                        },
                        row.get(1)?,
                    ))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            changes.push(TagChange {
                host,
                path,
                before: Tag::from_i64(before),
                after: Tag::from_i64(after),
                events,
            });
        }
        tx.execute(
            "UPDATE meta SET value=CAST(value AS INTEGER)+1 WHERE key='revision'",
            [],
        )?;
        tx.commit()?;
        Ok(changes)
    }

    pub fn reference(&self, id: i64) -> Result<RecordReference> {
        let mut statement=self.conn.prepare_cached("SELECT source,offset,length,line,sha256,tag,manual_tag IS NOT NULL,matches,host FROM records WHERE id=?")?;
        let result = statement.query_row([id], |r| {
            Ok(RecordReference {
                source: r.get(0)?,
                offset: r.get::<_, i64>(1)? as u64,
                length: r.get::<_, i64>(2)? as u64,
                line: r.get::<_, i64>(3)? as u64,
                sha256: r.get(4)?,
                tag: Tag::from_i64(r.get(5)?),
                manual: r.get(6)?,
                matches: Vec::new(),
                host: r.get(8)?,
            })
        })?;
        let hits: String = statement.query_row([id], |r| r.get(7))?;
        Ok(RecordReference {
            matches: serde_json::from_str(&hits)?,
            ..result
        })
    }
    pub fn detail(&self, id: i64) -> Result<Detail> {
        let record = self.reference(id)?;
        let source = self
            .sources()?
            .into_iter()
            .find(|s| s.id == record.source)
            .context("Missing source")?;
        let raw = source::read_record(&source.path, record.offset, record.length, &record.sha256)?;
        Ok(Detail {
            record: Record::parse(&raw)?,
            source,
            offset: record.offset,
            line: record.line,
            raw,
            tag: record.tag,
            manual: record.manual,
            matches: record.matches,
            host: record.host,
        })
    }

    pub fn diagnostics(&self, limit: usize) -> Result<String> {
        let count: i64 = self
            .conn
            .query_row("SELECT count(*) FROM errors", [], |r| r.get(0))?;
        let mut output =
            format!("Malformed records: {count}\nShowing at most {limit} locations.\n\n");
        let mut statement = self.conn.prepare("SELECT s.path,e.line,e.offset,e.message FROM errors e JOIN sources s ON s.id=e.source LIMIT ?")?;
        for row in statement.query_map([limit.min(10000) as i64], |r| {
            Ok(format!(
                "{}:{} (byte {}) — {}",
                r.get::<_, String>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, String>(3)?
            ))
        })? {
            output.push_str(&row?);
            output.push('\n');
        }
        Ok(output)
    }

    pub fn reload_rules(
        &mut self,
        rules: &Rules,
        mut progress: impl FnMut(u64) -> bool,
    ) -> Result<()> {
        // A transaction makes rule replacement atomic. Readers retain the old snapshot.
        let tx = self.conn.transaction()?;
        let mut last = 0_i64;
        let mut count = 0;
        loop {
            let batch = {
                let mut stmt = tx.prepare(
                    "SELECT id,path,hash FROM records WHERE id>? ORDER BY id LIMIT 4096",
                )?;
                stmt.query_map([last], |r| {
                    Ok((
                        r.get::<_, i64>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                    ))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?
            };
            if batch.is_empty() {
                break;
            }
            let mut update = tx.prepare_cached(
                "UPDATE records SET matches=?, tag=COALESCE(manual_tag,?) WHERE id=?",
            )?;
            for (id, path, hash) in batch {
                let hits = rules.matches(&path, &hash);
                update.execute(params![
                    serde_json::to_string(&hits)?,
                    if hits.is_empty() { 0 } else { 2 },
                    id
                ])?;
                last = id;
                count += 1;
            }
            if !progress(count) {
                bail!("IOC reload cancelled; previous rules retained");
            }
        }
        tx.execute(
            "INSERT OR REPLACE INTO state VALUES('rules',?)",
            [serde_json::to_string(&rules.rules)?],
        )?;
        tx.execute(
            "UPDATE meta SET value=CAST(value AS INTEGER)+1 WHERE key='revision'",
            [],
        )?;
        tx.commit()?;
        Ok(())
    }
}

pub fn configure(conn: &Connection) -> Result<()> {
    conn.busy_timeout(Duration::from_secs(5))?;
    conn.execute_batch("PRAGMA foreign_keys=ON; PRAGMA cache_size=-32768; PRAGMA temp_store=FILE; PRAGMA mmap_size=0;")?;
    Ok(())
}

fn read_line(reader: &mut impl BufRead, total: &mut Sha256) -> Result<(Vec<u8>, u64, Vec<u8>)> {
    const MAX: usize = 16 * 1024 * 1024;
    let mut bytes = Vec::new();
    let mut length = 0;
    let mut hash = Sha256::new();
    loop {
        let available = reader.fill_buf()?;
        if available.is_empty() {
            break;
        }
        let ending = available.iter().position(|&b| b == b'\n');
        let count = ending.map_or(available.len(), |i| i + 1);
        let part = &available[..count];
        total.update(part);
        hash.update(part);
        let keep = count.min(MAX.saturating_sub(bytes.len()));
        bytes.extend_from_slice(&part[..keep]);
        length += count as u64;
        reader.consume(count);
        if ending.is_some() {
            break;
        }
    }
    Ok((bytes, length, hash.finalize().to_vec()))
}
