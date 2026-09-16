//! Source-scoped lookup of executable hashes collected alongside UAC bodyfiles.
use crate::{
    query::Cancellation,
    source,
    store::{SourceInfo, Store},
};
use anyhow::{Context, Result, ensure};
use rusqlite::{OptionalExtension, params};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    fs::{self, File},
    io::{BufRead, BufReader, Read},
    path::{Path, PathBuf},
};

#[derive(Clone, Copy)]
enum Algorithm {
    Md5,
    Sha1,
}

impl Algorithm {
    fn from_file(path: &Path) -> Option<Self> {
        let name = path.file_name()?.to_str()?.to_ascii_lowercase();
        if !name.starts_with("hash") {
            return None;
        }
        match path.extension()?.to_str()?.to_ascii_lowercase().as_str() {
            "md5" => Some(Self::Md5),
            "sha1" => Some(Self::Sha1),
            _ => None,
        }
    }
    fn label(self) -> &'static str {
        match self {
            Self::Md5 => "MD5",
            Self::Sha1 => "SHA-1",
        }
    }
    fn length(self) -> usize {
        match self {
            Self::Md5 => 32,
            Self::Sha1 => 40,
        }
    }
    fn valid(self, digest: &str) -> bool {
        digest.len() == self.length() && digest.bytes().all(|byte| byte.is_ascii_hexdigit())
    }
}

#[derive(Debug)]
pub struct ExecutableHash {
    pub algorithm: String,
    pub digest: String,
    pub manifest: PathBuf,
    pub line: u64,
}

#[derive(Debug, Default)]
pub struct ExecutableHashes {
    pub values: Vec<ExecutableHash>,
    pub warnings: Vec<String>,
}

/// Accept the native checksum formats used by UAC on GNU, BSD, and OpenSSL hosts.
fn parse_line(line: &str, algorithm: Algorithm) -> Option<(String, String)> {
    let line = line.strip_suffix('\n').unwrap_or(line);
    let line = line.strip_suffix('\r').unwrap_or(line);
    if let Some((left, digest)) = line.rsplit_once('=')
        && let Some((label, path)) = left
            .trim_end()
            .strip_suffix(')')
            .and_then(|left| left.split_once('('))
        && label
            .trim()
            .replace('-', "")
            .eq_ignore_ascii_case(&algorithm.label().replace('-', ""))
    {
        return (algorithm.valid(digest.trim()) && !path.is_empty())
            .then(|| (path.to_owned(), digest.trim().to_ascii_lowercase()));
    }
    let escaped = line.starts_with('\\');
    let line = if escaped { &line[1..] } else { line };
    let digest = line.get(..algorithm.length())?;
    if !algorithm.valid(digest) {
        return None;
    }
    let rest = &line[algorithm.length()..];
    let path = rest
        .strip_prefix("  ")
        .or_else(|| rest.strip_prefix(" *"))
        .or_else(|| rest.strip_prefix(' '))
        .or_else(|| rest.strip_prefix('\t'))?;
    if path.is_empty() {
        return None;
    }
    let path = if escaped {
        let mut decoded = String::new();
        let mut chars = path.chars();
        while let Some(character) = chars.next() {
            decoded.push(if character == '\\' {
                match chars.next()? {
                    '\\' => '\\',
                    'n' => '\n',
                    'r' => '\r',
                    _ => return None,
                }
            } else {
                character
            });
        }
        decoded
    } else {
        path.to_owned()
    };
    Some((path, digest.to_ascii_lowercase()))
}

fn index_manifest(
    store: &Store,
    source: i64,
    manifest: &Path,
    algorithm: Algorithm,
    cancel: &Cancellation,
) -> Result<()> {
    let (size, modified) = source::metadata(manifest)?;
    let sql_size = i64::try_from(size)?;
    let filename = manifest.to_string_lossy();
    let previous: Option<(i64, String)> = store
        .conn
        .query_row(
            "SELECT size,modified FROM uac_hash_files WHERE source=? AND file=?",
            params![source, filename],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    if previous
        .as_ref()
        .is_some_and(|(old_size, old_modified)| *old_size == sql_size && old_modified == &modified)
    {
        return Ok(());
    }
    let mut reader = BufReader::new(File::open(manifest)?);
    let tx = store.conn.unchecked_transaction()?;
    tx.execute(
        "DELETE FROM uac_hashes WHERE source=? AND file=?",
        params![source, filename],
    )?;
    let mut offset = 0_u64;
    let mut line = 0_u64;
    let mut bytes = Vec::new();
    {
        let mut insert = tx.prepare(
            "INSERT INTO uac_hashes(source,file,path,algorithm,digest,line,offset,length,sha256)
             VALUES(?,?,?,?,?,?,?,?,?)",
        )?;
        loop {
            cancel.check()?;
            bytes.clear();
            // Bound memory even when a malformed manifest contains a huge line.
            let mut limited = Read::by_ref(&mut reader).take(1_048_577);
            let mut length = limited.read_until(b'\n', &mut bytes)? as u64;
            if length == 0 {
                break;
            }
            line += 1;
            if length > 1_048_576 {
                if bytes.last() != Some(&b'\n') {
                    length += reader.skip_until(b'\n')? as u64;
                }
            } else if let Ok(text) = std::str::from_utf8(&bytes)
                && let Some((path, digest)) = parse_line(text, algorithm)
            {
                insert.execute(params![
                    source,
                    filename,
                    path,
                    algorithm.label(),
                    digest,
                    i64::try_from(line)?,
                    i64::try_from(offset)?,
                    i64::try_from(length)?,
                    Sha256::digest(&bytes).to_vec(),
                ])?;
            }
            offset += length;
        }
    }
    ensure!(
        source::metadata(manifest)? == (size, modified.clone()),
        "Hash manifest changed while reading {}",
        manifest.display()
    );
    cancel.check()?;
    tx.execute(
        "INSERT OR REPLACE INTO uac_hash_files(source,file,size,modified) VALUES(?,?,?,?)",
        params![source, filename, sql_size, modified],
    )?;
    tx.commit()?;
    Ok(())
}

/// Lazily index manifests on disk. Only this record's exact pathname is returned.
/// Missing optional files never prevent inspecting the original bodyfile record.
pub fn executable_hashes(
    store: &Store,
    source: &SourceInfo,
    path: &str,
    cancel: &Cancellation,
) -> Result<Option<ExecutableHashes>> {
    let Some(root) = source
        .path
        .parent()
        .filter(|parent| parent.file_name().is_some_and(|name| name == "bodyfile"))
        .and_then(Path::parent)
    else {
        return Ok(None);
    };
    let directory = root.join("hash_executables");
    let entries = match fs::read_dir(&directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(error).with_context(|| format!("Cannot read {}", directory.display()));
        }
    };
    let mut manifests = Vec::new();
    for entry in entries {
        let entry = entry?;
        let file = entry.path();
        if let Some(algorithm) = Algorithm::from_file(&file)
            && file.is_file()
        {
            manifests.push((file, algorithm));
        }
    }
    manifests.sort_by(|a, b| a.0.cmp(&b.0));
    if manifests.is_empty() {
        return Ok(None);
    }
    let mut result = ExecutableHashes::default();
    let mut seen = BTreeSet::new();
    for (manifest, algorithm) in manifests {
        cancel.check()?;
        if let Err(error) = index_manifest(store, source.id, &manifest, algorithm, cancel) {
            cancel.check()?;
            result
                .warnings
                .push(format!("{}: {error:#}", manifest.display()));
            continue;
        }
        let mut statement = store.conn.prepare_cached(
            "SELECT algorithm,digest,line,offset,length,sha256 FROM uac_hashes
             WHERE source=? AND path=? AND file=? ORDER BY line",
        )?;
        let mut rows = statement.query(params![source.id, path, manifest.to_string_lossy()])?;
        while let Some(row) = rows.next()? {
            cancel.check()?;
            let algorithm: String = row.get(0)?;
            let digest: String = row.get(1)?;
            let line = u64::try_from(row.get::<_, i64>(2)?)?;
            if let Err(error) = source::read_record(
                &manifest,
                row.get::<_, i64>(3)?.try_into()?,
                row.get::<_, i64>(4)?.try_into()?,
                &row.get::<_, Vec<u8>>(5)?,
            ) {
                result
                    .warnings
                    .push(format!("{}:{line}: {error:#}", manifest.display()));
                continue;
            }
            if seen.insert((algorithm.clone(), digest.clone())) {
                result.values.push(ExecutableHash {
                    algorithm,
                    digest,
                    manifest: manifest.clone(),
                    line,
                });
            }
        }
    }
    Ok(Some(result))
}
