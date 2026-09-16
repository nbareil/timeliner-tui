use crate::{
    model::{Tag, View},
    query::{self, Cancellation},
    source,
    store::Store,
};
use anyhow::{Result, bail};
use rusqlite::Connection;
use serde_json::json;
use std::{
    collections::HashMap,
    fs::{self, File},
    io::{BufWriter, Write},
    path::{Path, PathBuf},
};

#[derive(Clone, Debug)]
pub struct Export {
    pub output: PathBuf,
    pub view: Option<View>,
    pub tags: Vec<Tag>,
}
pub fn sidecar(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(".metadata.jsonl");
    name.into()
}

pub fn run(
    store: &Store,
    export: &Export,
    cancel: &Cancellation,
    mut progress: impl FnMut(u64),
) -> Result<u64> {
    let metadata_path = sidecar(&export.output);
    if export.output.exists() || metadata_path.exists() {
        bail!("Export destination already exists; choose a new filename");
    }
    let parent = export
        .output
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let parent = parent.canonicalize()?;
    let resolved_output = parent.join(
        export
            .output
            .file_name()
            .ok_or_else(|| anyhow::anyhow!("Choose an output filename"))?,
    );
    let resolved_metadata = sidecar(&resolved_output);
    let sources = store
        .sources()?
        .into_iter()
        .map(|s| (s.id, s))
        .collect::<HashMap<_, _>>();
    for s in sources.values() {
        if resolved_output == s.path || resolved_metadata == s.path {
            bail!("Export would overwrite a source");
        }
    }
    let temporary_ids = tempfile::NamedTempFile::new_in(&store.root)?;
    let mut ids = Connection::open(temporary_ids.path())?;
    ids.execute_batch(
        "PRAGMA journal_mode=OFF; PRAGMA synchronous=OFF; PRAGMA temp_store=FILE;
        CREATE TABLE selected(id INTEGER PRIMARY KEY);",
    )?;
    let revision = store.revision()?;
    if let Some(view) = &export.view {
        let mut anchor = None;
        loop {
            let rows = query::page(
                store,
                view,
                query::Page {
                    anchor,
                    limit: 4096,
                    ..query::Page::default()
                },
                cancel,
                |_| {},
            )?;
            if rows.is_empty() {
                break;
            }
            anchor = rows.last().map(|r| r.key);
            let tx = ids.transaction()?;
            for row in rows {
                if row.tag != Tag::None
                    && (export.tags.is_empty() || export.tags.contains(&row.tag))
                {
                    tx.execute("INSERT OR IGNORE INTO selected VALUES(?)", [row.key.record])?;
                }
            }
            tx.commit()?;
            cancel.check()?;
        }
    } else {
        let tx = ids.transaction()?;
        let mut stmt = store
            .conn
            .prepare("SELECT id,tag FROM records WHERE tag IN (1,2,3)")?;
        for row in stmt.query_map([], |r| Ok((r.get::<_, i64>(0)?, Tag::from_i64(r.get(1)?))))? {
            cancel.check()?;
            let (id, tag) = row?;
            if export.tags.is_empty() || export.tags.contains(&tag) {
                tx.execute("INSERT INTO selected VALUES(?)", [id])?;
            }
        }
        tx.commit()?;
    }
    let mut output = tempfile::NamedTempFile::new_in(&parent)?;
    let mut metadata = tempfile::NamedTempFile::new_in(&parent)?;
    let mut out = BufWriter::new(output.as_file_mut());
    let mut meta = BufWriter::new(metadata.as_file_mut());
    writeln!(
        meta,
        "{}",
        json!({"type":"header","version":1,"workspace_sources":sources,"ioc_rules":store.rules()?.rules})
    )?;
    let mut selected = ids.prepare("SELECT id FROM selected ORDER BY id")?;
    let mut offset = 0;
    let mut count = 0;
    let mut separator = false;
    let mut checked = std::collections::HashSet::new();
    for id in selected.query_map([], |r| r.get::<_, i64>(0))? {
        cancel.check()?;
        let id = id?;
        let record = store.reference(id)?;
        let source = &sources[&record.source];
        if checked.insert(record.source)
            && source::metadata(&source.path)? != (source.size, source.modified.clone())
        {
            bail!(
                "Source metadata changed: {}; relink and validate the original content before export",
                source.path.display()
            );
        }
        let bytes =
            source::read_record(&source.path, record.offset, record.length, &record.sha256)?;
        if separator {
            out.write_all(b"\n")?;
            offset += 1;
        }
        out.write_all(&bytes)?;
        writeln!(
            meta,
            "{}",
            json!({"type":"record","record_id":id,"output_offset":offset,"output_length":record.length,
            "source_id":record.source,"source_offset":record.offset,"source_line":record.line,"sha256":format_hash(&record.sha256),
            "hostname":record.host,"tag":record.tag.label(),"tag_origin":if record.manual {"manual"} else {"automatic"},
            "ioc_rules":record.matches})
        )?;
        separator = !bytes.ends_with(b"\n");
        offset += record.length;
        count += 1;
        if count % 1024 == 0 {
            progress(count);
        }
    }
    cancel.check()?;
    if store.revision()? != revision {
        bail!("Investigation changed during export; retry with a stable assessment");
    }
    for id in checked {
        let source = &sources[&id];
        if source::metadata(&source.path)? != (source.size, source.modified.clone()) {
            bail!("Source changed during export: {}", source.path.display());
        }
    }
    out.flush()?;
    meta.flush()?;
    drop(out);
    drop(meta);
    output.as_file().sync_all()?;
    metadata.as_file().sync_all()?;
    // Publish the bodyfile last. No existing destination is ever replaced.
    metadata.persist_noclobber(&metadata_path)?;
    if let Err(error) = output.persist_noclobber(&export.output) {
        let _ = fs::remove_file(&metadata_path);
        return Err(error.into());
    }
    File::open(parent)?.sync_all()?;
    progress(count);
    Ok(count)
}
fn format_hash(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
