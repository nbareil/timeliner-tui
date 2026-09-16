use anyhow::{Result, bail};
use std::collections::BTreeMap;

#[derive(Debug, Clone)]
pub struct Record {
    pub fields: [String; 11],
    /// atime, mtime, ctime, btime
    pub times: [i64; 4],
}

impl Record {
    pub fn parse(bytes: &[u8]) -> Result<Self> {
        let bytes = bytes.strip_suffix(b"\n").unwrap_or(bytes);
        let bytes = bytes.strip_suffix(b"\r").unwrap_or(bytes);
        let line = String::from_utf8_lossy(bytes);
        let fields = if !line.contains(['\\', '"']) {
            line.split('|').map(str::to_owned).collect::<Vec<_>>()
        } else {
            let mut fields = Vec::new();
            let mut current = String::new();
            let mut escaped = false;
            let mut quoted = false;
            for c in line.chars() {
                if escaped {
                    if quoted || !matches!(c, '\\' | '|') {
                        current.push('\\');
                    }
                    current.push(c);
                    escaped = false;
                } else {
                    match c {
                        '\\' => escaped = true,
                        '"' => {
                            quoted = !quoted;
                            current.push(c);
                        }
                        '|' if !quoted => fields.push(std::mem::take(&mut current)),
                        _ => current.push(c),
                    }
                }
            }
            if escaped {
                current.push('\\');
            }
            fields.push(current);
            fields
        };
        if fields.len() != 11 {
            bail!("Expected 11 fields, found {}", fields.len());
        }
        if fields[6].parse::<u64>().is_err() {
            bail!("Invalid unsigned file size: {}", fields[6]);
        }
        let mut times = [0; 4];
        for (i, time) in times.iter_mut().enumerate() {
            *time = fields[i + 7]
                .parse()
                .map_err(|_| anyhow::anyhow!("Invalid timestamp field {}", i + 7))?;
        }
        Ok(Self {
            fields: fields.try_into().unwrap(),
            times,
        })
    }

    pub fn events(&self) -> BTreeMap<i64, u8> {
        let mut result = BTreeMap::new();
        for (time, mask) in self.times.iter().zip([2, 1, 4, 8]) {
            *result.entry(*time).or_default() |= mask;
        }
        result
    }

    pub fn is_directory(&self) -> bool {
        let mode = self.fields[3].trim().trim_matches('"');
        let symbolic = mode.rsplit('/').next().unwrap_or(mode);
        symbolic.starts_with('d')
            || u32::from_str_radix(mode.strip_prefix("0o").unwrap_or(mode), 8)
                .is_ok_and(|bits| bits & 0o170000 == 0o040000)
    }

    pub fn is_zero_size(&self) -> bool {
        self.fields[6].parse::<u64>() == Ok(0)
    }
}
