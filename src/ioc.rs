use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};
use std::{collections::HashMap, path::Path};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Rule {
    pub filename: Option<String>,
    pub path: Option<String>,
    #[serde(default)]
    pub exclusion_path: Vec<String>,
    #[serde(default)]
    pub exclusion_hash: Vec<String>,
}

#[derive(Debug, Clone, Default)]
pub struct Rules {
    pub rules: Vec<Rule>,
    filenames: HashMap<String, Vec<usize>>,
    paths: HashMap<String, Vec<usize>>,
}

impl Rules {
    pub fn load(path: &Path) -> Result<Self> {
        Self::from_yaml(&std::fs::read_to_string(path)?)
    }
    pub fn from_yaml(text: &str) -> Result<Self> {
        let rules: Vec<Rule> = serde_saphyr::from_str(text)?;
        Self::new(rules)
    }
    pub fn new(rules: Vec<Rule>) -> Result<Self> {
        let mut result = Self {
            rules,
            ..Self::default()
        };
        for (index, rule) in result.rules.iter().enumerate() {
            if rule.filename.as_deref().is_none_or(str::is_empty)
                && rule.path.as_deref().is_none_or(str::is_empty)
            {
                bail!("IOC rule {} needs a filename or path", index + 1);
            }
            if let Some(name) = &rule.filename {
                result
                    .filenames
                    .entry(name.clone())
                    .or_default()
                    .push(index);
            }
            if let Some(path) = &rule.path {
                result.paths.entry(path.clone()).or_default().push(index);
            }
        }
        Ok(result)
    }
    pub fn matches(&self, path: &str, hash: &str) -> Vec<usize> {
        let name = path.rsplit(['/', '\\']).next().unwrap_or(path);
        let mut matches = Vec::new();
        for &index in self
            .filenames
            .get(name)
            .into_iter()
            .flatten()
            .chain(self.paths.get(path).into_iter().flatten())
        {
            let rule = &self.rules[index];
            // Match the scanner's POSIX Path normalization for exclusions only.
            let normalized = path
                .split('/')
                .filter(|p| !p.is_empty() && *p != ".")
                .collect::<Vec<_>>()
                .join("/");
            let normalized = if path.starts_with('/') {
                format!("/{normalized}")
            } else {
                normalized
            };
            if !rule
                .exclusion_path
                .iter()
                .any(|p| normalized.starts_with(p))
                && !rule.exclusion_hash.iter().any(|h| h == hash)
                && !matches.contains(&index)
            {
                matches.push(index);
            }
        }
        matches
    }
}
