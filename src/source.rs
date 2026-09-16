use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs::File,
    io::{BufRead, BufReader, Read, Seek, SeekFrom},
    path::{Path, PathBuf},
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Input {
    pub path: PathBuf,
    pub host: String,
    pub legacy: bool,
}

pub fn resolve(path: &Path, host: Option<&str>, legacy: bool) -> Result<Input> {
    let path = path
        .canonicalize()
        .with_context(|| format!("Cannot open {}", path.display()))?;
    let (path, root) = if path.is_dir() {
        (path.join("bodyfile/bodyfile.txt"), Some(path))
    } else {
        let root = path
            .parent()
            .filter(|p| p.file_name().is_some_and(|n| n == "bodyfile"))
            .and_then(Path::parent)
            .map(Path::to_path_buf);
        (path, root)
    };
    if !path.is_file() {
        bail!("Bodyfile not found: {}", path.display());
    }
    let label = host
        .map(str::to_owned)
        .or_else(|| root.as_deref().and_then(hostname))
        .unwrap_or_else(|| {
            root.as_deref()
                .unwrap_or(&path)
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned()
        });
    Ok(Input {
        path: path.canonicalize()?,
        host: label,
        legacy: legacy && host.is_none(),
    })
}

fn hostname(root: &Path) -> Option<String> {
    for relative in [
        "live_response/network/hostname_-f.txt",
        "live_response/network/hostname.txt",
        "live_response/network/uname_-n.txt",
    ] {
        if let Some(value) = first_hostname(&root.join(relative)) {
            return Some(value);
        }
    }
    if let Ok(file) = File::open(root.join("uac.log")) {
        for line in BufReader::new(file.take(1_048_576))
            .lines()
            .map_while(Result::ok)
        {
            if let Some((_, value)) = line.split_once("Hostname:")
                && valid_hostname(value.trim())
            {
                return Some(value.trim().into());
            }
        }
    }
    for relative in ["[root]/etc/hostname", "etc/hostname"] {
        if let Some(value) = first_hostname(&root.join(relative)) {
            return Some(value);
        }
    }
    None
}
fn valid_hostname(value: &str) -> bool {
    !value.is_empty()
        && value != "unknown"
        && value.len() <= 253
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
}
fn first_hostname(path: &Path) -> Option<String> {
    let mut line = String::new();
    BufReader::new(File::open(path).ok()?.take(4096))
        .read_line(&mut line)
        .ok()?;
    valid_hostname(line.trim()).then(|| line.trim().to_owned())
}

pub fn metadata(path: &Path) -> Result<(u64, String)> {
    let meta = path.metadata()?;
    let modified = meta
        .modified()?
        .duration_since(std::time::UNIX_EPOCH)?
        .as_nanos()
        .to_string();
    Ok((meta.len(), modified))
}
pub fn digest_file(path: &Path) -> Result<Vec<u8>> {
    let mut file = BufReader::with_capacity(1024 * 1024, File::open(path)?);
    let mut hash = Sha256::new();
    let mut buffer = vec![0; 1024 * 1024];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hash.update(&buffer[..count]);
    }
    Ok(hash.finalize().to_vec())
}
pub fn read_record(path: &Path, offset: u64, length: u64, expected: &[u8]) -> Result<Vec<u8>> {
    let mut file = File::open(path)
        .with_context(|| format!("Relink unavailable source {}", path.display()))?;
    file.seek(SeekFrom::Start(offset))?;
    let mut bytes = Vec::new();
    file.take(length).read_to_end(&mut bytes)?;
    if bytes.len() as u64 != length || Sha256::digest(&bytes).as_slice() != expected {
        bail!(
            "Source record changed at {}:{offset}; relink the original evidence",
            path.display()
        );
    }
    Ok(bytes)
}
pub fn default_workspace(inputs: &[Input]) -> PathBuf {
    let root = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".local/share")
        });
    let mut sources = inputs
        .iter()
        .map(|s| s.path.to_string_lossy().to_string())
        .collect::<Vec<_>>();
    sources.sort();
    let key = format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(&sources).unwrap())
    );
    root.join("timeliner-tui").join(&key[..24])
}
