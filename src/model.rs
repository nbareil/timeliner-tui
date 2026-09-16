use anyhow::{Result, bail};
use chrono::{NaiveDate, NaiveDateTime, TimeZone};
use chrono_tz::Tz;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[repr(i64)]
pub enum Tag {
    #[default]
    None = 0,
    Suspicious = 1,
    Ioc = 2,
    Malicious = 3,
}

impl Tag {
    pub fn from_i64(value: i64) -> Self {
        match value {
            1 => Self::Suspicious,
            2 => Self::Ioc,
            3 => Self::Malicious,
            _ => Self::None,
        }
    }
    pub fn next(self) -> Self {
        Self::from_i64((self as i64 + 1) % 4)
    }
    pub fn label(self) -> &'static str {
        match self {
            Self::None => "",
            Self::Suspicious => "SUSPICIOUS",
            Self::Ioc => "IOC",
            Self::Malicious => "MALICIOUS",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct Predicate {
    pub include: String,
    pub exclude: Vec<String>,
    pub hosts: Vec<String>,
    /// None: no tag filter; Some(empty): any tagged; otherwise selected tags.
    pub tags: Option<Vec<Tag>>,
}

impl Predicate {
    pub fn is_all(&self) -> bool {
        self == &Self::default()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct View {
    pub name: String,
    pub predicate: Predicate,
    pub after: Option<i64>,
    pub before: Option<i64>,
    pub time_mask: u8,
    pub bogus: bool,
    pub top: Option<EventKey>,
    pub selected: usize,
    /// Saved folds, retained when tag edits leave membership unchanged.
    #[serde(default)]
    pub bursts: Vec<Burst>,
}

impl Default for View {
    fn default() -> Self {
        Self {
            name: "Timeline".into(),
            predicate: Predicate::default(),
            after: None,
            before: None,
            time_mask: 15,
            bogus: false,
            top: None,
            selected: 0,
            bursts: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub struct EventKey {
    pub epoch: i64,
    pub record: i64,
}

#[derive(Debug, Clone)]
pub struct Row {
    pub key: EventKey,
    pub macb: u8,
    pub host: String,
    pub path: String,
    pub tag: Tag,
    pub automatic: bool,
    pub ioc_match: bool,
}

#[derive(Debug, Clone)]
pub struct TagChange {
    pub host: String,
    pub path: String,
    pub before: Tag,
    pub after: Tag,
    /// At most four timestamp events for the changed source record.
    pub events: Vec<(EventKey, u8)>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Burst {
    pub first: EventKey,
    pub last: EventKey,
    pub host: String,
    pub count: u64,
    pub macb: u8,
    pub tagged: u64,
    pub ioc_matches: u64,
    /// No matching events from other hosts occur in these seconds.
    pub exclusive: bool,
    /// Restore the event from which the user requested this fold.
    pub origin: EventKey,
}

impl Burst {
    pub fn contains_key(&self, key: EventKey) -> bool {
        self.first <= key && key <= self.last
    }
}

#[derive(Debug, Clone)]
pub enum DisplayRow {
    Event(Row),
    Burst(Burst),
}

impl DisplayRow {
    pub fn key(&self) -> EventKey {
        match self {
            Self::Event(row) => row.key,
            Self::Burst(burst) => burst.first,
        }
    }

    pub fn host(&self) -> &str {
        match self {
            Self::Event(row) => &row.host,
            Self::Burst(burst) => &burst.host,
        }
    }

    pub fn event(&self) -> Option<&Row> {
        match self {
            Self::Event(row) => Some(row),
            Self::Burst(_) => None,
        }
    }

    pub fn event_mut(&mut self) -> Option<&mut Row> {
        match self {
            Self::Event(row) => Some(row),
            Self::Burst(_) => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Bookmark {
    pub name: char,
    pub key: EventKey,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub views: Vec<View>,
    pub active: usize,
    pub bookmarks: Vec<Bookmark>,
    pub timezone: String,
    #[serde(default)]
    pub theme: crate::theme::Theme,
    #[serde(default = "default_burst_threshold")]
    pub burst_threshold: u64,
}

pub fn default_burst_threshold() -> u64 {
    100
}

impl Default for Session {
    fn default() -> Self {
        Self {
            views: vec![
                View::default(),
                View {
                    name: "IOC".into(),
                    predicate: Predicate {
                        tags: Some(vec![Tag::Ioc]),
                        ..Predicate::default()
                    },
                    ..View::default()
                },
            ],
            active: 0,
            bookmarks: Vec::new(),
            timezone: "UTC".into(),
            theme: crate::theme::Theme::default(),
            burst_threshold: default_burst_threshold(),
        }
    }
}

pub fn macb(mask: u8) -> String {
    ['m', 'a', 'c', 'b']
        .iter()
        .enumerate()
        .map(|(i, c)| if mask & (1 << i) != 0 { *c } else { '.' })
        .collect()
}

pub fn boundary(text: &str, tz: Tz, end: bool) -> Result<Option<i64>> {
    if text.trim().is_empty() {
        return Ok(None);
    }
    let text = text.trim();
    let datetime = if let Ok(day) = NaiveDate::parse_from_str(text, "%Y-%m-%d") {
        day.and_hms_opt(
            if end { 23 } else { 0 },
            if end { 59 } else { 0 },
            if end { 59 } else { 0 },
        )
        .unwrap()
    } else {
        ["%Y-%m-%d %H:%M:%S", "%Y-%m-%d %H:%M", "%Y-%m-%dT%H:%M:%S"]
            .iter()
            .find_map(|format| NaiveDateTime::parse_from_str(text, format).ok())
            .ok_or_else(|| anyhow::anyhow!("Use YYYY-MM-DD [HH:MM[:SS]]"))?
    };
    match tz.from_local_datetime(&datetime).single() {
        Some(datetime) => Ok(Some(datetime.timestamp())),
        None => bail!("Ambiguous or nonexistent local time in {tz}; use UTC"),
    }
}

pub fn validate_view(view: &View) -> Result<()> {
    if let (Some(a), Some(b)) = (view.after, view.before)
        && a > b
    {
        bail!("Begin must not be after end");
    }
    if view.time_mask == 0 {
        bail!("Select at least one timestamp type");
    }
    if !view.predicate.include.is_empty() {
        regex::Regex::new(&view.predicate.include)?;
    }
    for pattern in &view.predicate.exclude {
        regex::Regex::new(pattern)?;
    }
    Ok(())
}

/// Evidence text must never send control sequences to the terminal.
pub fn display_text(value: &str) -> String {
    value
        .chars()
        .flat_map(|c| {
            if c.is_control() || matches!(c, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}') {
                c.escape_default().collect::<Vec<_>>()
            } else {
                vec![c]
            }
        })
        .collect()
}
