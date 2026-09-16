use crate::{
    export::Export,
    model::{
        Bookmark, EventKey, Predicate, Row, Session, Tag, View, boundary, display_text, macb,
        validate_view,
    },
    worker::{Command, Message, PageRequest, Workers},
};
use anyhow::{Result, bail};
use chrono::TimeZone;
use crossterm::{
    event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{
    Frame, Terminal,
    backend::CrosstermBackend,
    layout::{Alignment, Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Clear, Padding, Paragraph, Tabs, Wrap},
};
use std::{
    io::{self, IsTerminal},
    path::PathBuf,
    sync::atomic::Ordering,
    time::{Duration, Instant},
};

mod render;

const HELP: &str = "\
TIMELINER TUI

↑/↓ scroll the view; the highlight stays in its screen slot.
j/k move the highlight within the visible screen.
PageUp/PageDown scroll a screen. gg/G jump to beginning/end.
TAB cycles a record's assessment. ENTER/F3 opens full details.
Tags belong to source records, so all their MACB events agree.

F1 Help   F2 Tags / reload IOC rules   F3 Details
F4 Timeframe   F5 Filters   F6 Fork view   F7 Smart tabs
F8 Bookmarks   F9 Export   F10 Quit
F11 switches between light and dark themes.
gt/gT switch tabs. Smart tabs start across the whole investigation.
F7 also offers rename/close. F6 copies filters and location.

ma sets mark a; 'a jumps to it; '' returns after a jump.
If a mark is hidden, a new unfiltered view reveals it.
F8 lists marks; d then a letter deletes a mark in that dialog.
No Vim text registers are implemented.

x excludes the selected pathname; X excludes its directory subtree.
/ opens the path filter editor. Exclusions are regular expressions.
Filters use Rust regex syntax (no lookaround/backreferences).
F2 → a restores automatic IOC assessment, including a cleared tag.
Automatic IOC matches have amber paths; the right column is the assessment.

F9 exports all tagged records by default. Choose current for this view.
Exports contain original bodyfile records and a .metadata.jsonl sidecar.
Source bytes are verified. Existing destinations are never replaced.

F2 → r reloads IOC YAML; manual assessments remain authoritative.
D shows malformed-record diagnostics and source locations.
Escape cancels pending work or closes a dialog. Ctrl+C also quits.
Forms: Tab/Shift+Tab change field, Ctrl+U clears, Enter applies.
Filter forms: F2 adds another exclusion. Details: ↑/↓ scroll.
State is saved automatically. Sources are never modified.";

#[derive(Clone, Copy)]
enum Load {
    Replace,
    Before(usize),
    After(usize),
    Last,
    Mark(EventKey, char),
}
struct Pending {
    generation: u64,
    mode: Load,
    candidate: Option<View>,
    anchor: Option<EventKey>,
    limit: usize,
}
#[derive(Clone, Copy, PartialEq)]
enum FormKind {
    Time,
    Filters,
    Export,
    Reload,
    Rename,
}
struct Form {
    kind: FormKind,
    fields: Vec<(String, String)>,
    active: usize,
    error: String,
}
enum Modal {
    Text(&'static str, String, usize),
    Tags,
    Views,
    Marks,
    Form(Form),
}

pub enum Action {
    Page(PageRequest),
    Command(Command),
    Cancel,
    Quit,
}

pub struct App {
    pub session: Session,
    pub rows: Vec<Row>,
    pub top: usize,
    pub height: usize,
    detail_scroll_max: usize,
    pub status: String,
    pub ready: bool,
    generation: u64,
    pending: Option<Pending>,
    modal: Option<Modal>,
    prefix: Option<char>,
    previous: Option<(usize, View)>,
    end_before: bool,
    end_after: bool,
    dirty_session: bool,
}

impl Default for App {
    fn default() -> Self {
        Self {
            session: Session::default(),
            rows: Vec::new(),
            top: 0,
            height: 20,
            detail_scroll_max: 0,
            status: "Opening investigation…".into(),
            ready: false,
            generation: 0,
            pending: None,
            modal: None,
            prefix: None,
            previous: None,
            end_before: false,
            end_after: false,
            dirty_session: false,
        }
    }
}

impl App {
    fn view(&self) -> &View {
        &self.session.views[self.session.active]
    }
    fn view_mut(&mut self) -> &mut View {
        &mut self.session.views[self.session.active]
    }
    fn selected(&self) -> Option<&Row> {
        self.rows.get(self.top + self.view().selected)
    }
    fn remember(&mut self) {
        let key = self.rows.get(self.top).map(|r| r.key);
        self.view_mut().top = key;
        self.dirty_session = true;
    }
    fn load(
        &mut self,
        mode: Load,
        candidate: Option<View>,
        anchor: Option<EventKey>,
        inclusive: bool,
    ) -> Action {
        self.generation += 1;
        let limit = (self.height * 8).clamp(128, 1024);
        self.pending = Some(Pending {
            generation: self.generation,
            mode,
            candidate: candidate.clone(),
            anchor,
            limit,
        });
        self.status = "Loading… (Escape cancels)".into();
        Action::Page(PageRequest {
            generation: self.generation,
            view: candidate.unwrap_or_else(|| self.view().clone()),
            anchor,
            reverse: matches!(mode, Load::Before(_) | Load::Last),
            inclusive,
            limit,
        })
    }
    fn refresh(&mut self) -> Action {
        self.load(
            Load::Replace,
            None,
            self.rows.get(self.top).map(|r| r.key).or(self.view().top),
            true,
        )
    }
    fn switch(&mut self, active: usize) -> Action {
        self.remember();
        self.session.active = active;
        self.rows.clear();
        self.top = 0;
        self.load(Load::Replace, None, self.view().top, true)
    }
    fn open_view(&mut self, view: View) -> Action {
        self.remember();
        self.session.views.push(view);
        self.session.active = self.session.views.len() - 1;
        self.rows.clear();
        self.top = 0;
        self.dirty_session = true;
        self.load(Load::Replace, None, self.view().top, true)
    }
    fn scroll(&mut self, delta: isize) -> Option<Action> {
        if delta > 0 {
            let room = self.rows.len().saturating_sub(self.top + self.height);
            if room > 0 {
                self.top += room.min(delta as usize);
            } else if !self.end_after && self.pending.is_none() {
                return Some(self.load(
                    Load::After(delta as usize),
                    None,
                    self.rows.last().map(|r| r.key),
                    false,
                ));
            }
        } else if self.top > 0 {
            self.top = self.top.saturating_sub(delta.unsigned_abs());
        } else if !self.end_before && self.pending.is_none() {
            return Some(self.load(
                Load::Before(delta.unsigned_abs()),
                None,
                self.rows.first().map(|r| r.key),
                false,
            ));
        }
        self.remember();
        None
    }
    fn mark_jump(&mut self, name: char) -> Vec<Action> {
        let Some(mark) = self
            .session
            .bookmarks
            .iter()
            .find(|m| m.name == name)
            .cloned()
        else {
            self.status = format!("Mark {name} is not set");
            return vec![];
        };
        self.remember();
        self.previous = Some((self.session.active, self.view().clone()));
        self.modal = None;
        if let Some(index) = self.rows.iter().position(|r| r.key == mark.key) {
            self.top = index.saturating_sub(self.height / 2);
            self.view_mut().selected = index - self.top;
            self.remember();
            vec![]
        } else {
            vec![self.load(Load::Mark(mark.key, name), None, Some(mark.key), true)]
        }
    }
    pub fn message(&mut self, message: Message) -> Vec<Action> {
        match message {
            Message::Ready(mut session, warnings) => {
                if session.views.is_empty() {
                    session = Session::default();
                }
                session.active = session.active.min(session.views.len() - 1);
                self.session = session;
                self.ready = true;
                let action = self.load(Load::Replace, None, self.view().top, true);
                if !warnings.is_empty() {
                    self.modal = Some(Modal::Text("Source warnings", warnings, 0));
                }
                vec![action]
            }
            Message::Page(generation, mut rows) => {
                let Some(pending) = self.pending.take() else {
                    return vec![];
                };
                if pending.generation != generation {
                    self.pending = Some(pending);
                    return vec![];
                }
                let Pending {
                    mode,
                    candidate,
                    anchor,
                    limit,
                    ..
                } = pending;
                if let Load::Mark(key, name) = mode {
                    if rows.first().is_none_or(|r| r.key != key) {
                        return vec![self.open_view(View {
                            name: format!("Mark {name}"),
                            top: Some(key),
                            bogus: key.epoch <= 0,
                            ..View::default()
                        })];
                    }
                    self.view_mut().selected = 0;
                }
                if rows.is_empty() && anchor.is_some() && matches!(mode, Load::Replace) {
                    return vec![self.load(Load::Last, candidate, None, true)];
                }
                match mode {
                    Load::Replace | Load::Last | Load::Mark(_, _) => {
                        if let Some(view) = candidate {
                            *self.view_mut() = view;
                        }
                        self.end_before = (anchor.is_none() && matches!(mode, Load::Replace))
                            || (matches!(mode, Load::Last) && rows.len() < limit);
                        self.end_after = rows.len() < limit || matches!(mode, Load::Last);
                        self.rows = rows;
                        self.top = if matches!(mode, Load::Last) {
                            self.rows.len().saturating_sub(self.height)
                        } else {
                            0
                        };
                        if matches!(mode, Load::Last) {
                            self.view_mut().selected = self
                                .height
                                .saturating_sub(1)
                                .min(self.rows.len().saturating_sub(1));
                        }
                    }
                    Load::After(delta) => {
                        self.end_after = rows.len() < limit;
                        self.rows.append(&mut rows);
                        self.top =
                            (self.top + delta).min(self.rows.len().saturating_sub(self.height));
                        if self.rows.len() > limit * 2 {
                            let discard = self.rows.len() - limit * 2;
                            self.rows.drain(..discard);
                            self.top = self.top.saturating_sub(discard);
                            self.end_before = false;
                        }
                    }
                    Load::Before(delta) => {
                        self.end_before = rows.len() < limit;
                        let added = rows.len();
                        rows.append(&mut self.rows);
                        self.rows = rows;
                        self.top = (self.top + added).saturating_sub(delta);
                        if self.rows.len() > limit * 2 {
                            self.rows.truncate(limit * 2);
                            self.end_after = false;
                        }
                    }
                }
                self.view_mut().selected = self
                    .view()
                    .selected
                    .min(self.height.saturating_sub(1))
                    .min(self.rows.len().saturating_sub(self.top + 1));
                self.status = if self.rows.is_empty() {
                    "No matching events. F4 timeframe · F5 filters".into()
                } else {
                    "Ready".into()
                };
                self.remember();
                vec![]
            }
            Message::Changed => vec![self.refresh()],
            Message::Progress(generation, text) => {
                if generation == 0
                    || self
                        .pending
                        .as_ref()
                        .is_some_and(|p| p.generation == generation)
                {
                    self.status = text;
                }
                vec![]
            }
            Message::Detail(text) => {
                self.modal = Some(Modal::Text("Record inspector", text, 0));
                vec![]
            }
            Message::Error(error) => {
                self.status = error;
                vec![]
            }
            Message::QueryError(generation, error) => {
                if self
                    .pending
                    .as_ref()
                    .is_some_and(|p| p.generation == generation)
                {
                    self.pending = None;
                    self.status = error;
                }
                vec![]
            }
            Message::Status(text) => {
                self.status = text;
                vec![]
            }
        }
    }
    fn form(&mut self, kind: FormKind) {
        let fields: Vec<(&str, String)> = match kind {
            FormKind::Time => {
                let tz: chrono_tz::Tz = self.session.timezone.parse().unwrap_or(chrono_tz::UTC);
                let format = |epoch: Option<i64>| {
                    epoch
                        .and_then(|e| tz.timestamp_opt(e, 0).single())
                        .map(|t| t.format("%Y-%m-%d %H:%M:%S").to_string())
                        .unwrap_or_default()
                };
                vec![
                    ("Begin (blank = unbounded)", format(self.view().after)),
                    ("End (blank = unbounded)", format(self.view().before)),
                    ("Timezone", self.session.timezone.clone()),
                ]
            }
            FormKind::Filters => {
                let mut fields = vec![
                    ("Include path regex", self.view().predicate.include.clone()),
                    (
                        "Hosts (comma separated; blank = all)",
                        self.view().predicate.hosts.join(","),
                    ),
                    (
                        "Timestamp types (macb)",
                        macb(self.view().time_mask).replace('.', ""),
                    ),
                    (
                        "Include nonpositive timestamps (yes/no)",
                        if self.view().bogus { "yes" } else { "no" }.into(),
                    ),
                ];
                for pattern in &self.view().predicate.exclude {
                    fields.push(("Exclude path regex", pattern.clone()));
                }
                fields.push(("Exclude path regex", String::new()));
                fields
            }
            FormKind::Export => vec![
                ("Output bodyfile", "tagged.bodyfile".into()),
                ("Scope (all/current)", "all".into()),
                ("Tags (all or SUSPICIOUS,IOC,MALICIOUS)", "all".into()),
            ],
            FormKind::Reload => vec![("IOC YAML file", "~/ioc.filenames.yaml".into())],
            FormKind::Rename => vec![("Tab name", self.view().name.clone())],
        };
        self.modal = Some(Modal::Form(Form {
            kind,
            fields: fields.into_iter().map(|(a, b)| (a.into(), b)).collect(),
            active: 0,
            error: String::new(),
        }));
    }
    fn apply_form(&mut self, form: &Form) -> Result<Vec<Action>> {
        let value = |i: usize| form.fields[i].1.trim();
        match form.kind {
            FormKind::Time => {
                let tz = value(2).parse()?;
                let mut view = self.view().clone();
                view.after = boundary(value(0), tz, false)?;
                view.before = boundary(value(1), tz, true)?;
                view.top = None;
                view.selected = 0;
                validate_view(&view)?;
                self.session.timezone = value(2).into();
                Ok(vec![self.load(Load::Replace, Some(view), None, true)])
            }
            FormKind::Filters => {
                let mut view = self.view().clone();
                view.predicate.include = value(0).into();
                view.predicate.hosts = value(1)
                    .split(',')
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_owned)
                    .collect();
                view.time_mask = 0;
                for c in value(2).chars() {
                    view.time_mask |= match c {
                        'm' => 1,
                        'a' => 2,
                        'c' => 4,
                        'b' => 8,
                        _ => bail!("Timestamp types must use m, a, c, b"),
                    };
                }
                view.bogus = match value(3) {
                    "yes" => true,
                    "no" => false,
                    _ => bail!("Use yes or no for nonpositive timestamps"),
                };
                view.predicate.exclude = form.fields[4..]
                    .iter()
                    .map(|f| f.1.clone())
                    .filter(|s| !s.is_empty())
                    .collect();
                view.top = None;
                view.selected = 0;
                validate_view(&view)?;
                Ok(vec![self.load(Load::Replace, Some(view), None, true)])
            }
            FormKind::Export => {
                let tags = if value(2).eq_ignore_ascii_case("all") {
                    vec![]
                } else {
                    value(2)
                        .split(',')
                        .map(|t| match t.trim().to_uppercase().as_str() {
                            "SUSPICIOUS" => Ok(Tag::Suspicious),
                            "IOC" => Ok(Tag::Ioc),
                            "MALICIOUS" => Ok(Tag::Malicious),
                            _ => bail!("Unknown tag {t}"),
                        })
                        .collect::<Result<Vec<_>>>()?
                };
                let view = match value(1) {
                    "all" => None,
                    "current" => Some(self.view().clone()),
                    _ => bail!("Scope must be all or current"),
                };
                if value(0).is_empty() {
                    bail!("Choose an output filename");
                }
                self.status = "Preparing export… (Escape cancels)".into();
                Ok(vec![Action::Command(Command::Export(Export {
                    output: expand_home(value(0)),
                    view,
                    tags,
                }))])
            }
            FormKind::Reload => Ok(vec![Action::Command(Command::Reload(expand_home(value(
                0,
            ))))]),
            FormKind::Rename => {
                if value(0).is_empty() {
                    bail!("Tab name must not be empty");
                }
                self.view_mut().name = value(0).into();
                self.dirty_session = true;
                Ok(vec![])
            }
        }
    }
    pub fn key(&mut self, key: KeyEvent) -> Vec<Action> {
        if key.kind == KeyEventKind::Release {
            return vec![];
        }
        if key.code == KeyCode::F(10)
            || (key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL))
        {
            return if self.ready {
                vec![
                    Action::Command(Command::Save(self.session.clone())),
                    Action::Quit,
                ]
            } else {
                vec![Action::Quit]
            };
        }
        if key.code == KeyCode::Esc {
            if self.modal.take().is_none() {
                self.pending = None;
                self.status = "Cancelled".into();
                self.generation += 1;
                return vec![Action::Cancel];
            }
            self.prefix = None;
            return vec![];
        }
        if key.code == KeyCode::F(11) && self.ready {
            self.session.theme = self.session.theme.toggle();
            self.dirty_session = true;
            return vec![];
        }
        if let Some(modal) = self.modal.take() {
            match modal {
                Modal::Text(title, text, mut scroll) => {
                    match key.code {
                        KeyCode::Up | KeyCode::Char('k') => scroll = scroll.saturating_sub(1),
                        KeyCode::Down | KeyCode::Char('j') => {
                            scroll = (scroll + 1).min(self.detail_scroll_max)
                        }
                        KeyCode::PageDown => {
                            scroll = (scroll + self.height).min(self.detail_scroll_max)
                        }
                        KeyCode::PageUp => scroll = scroll.saturating_sub(self.height),
                        _ => {}
                    }
                    self.modal = Some(Modal::Text(title, text, scroll));
                }
                Modal::Form(mut form) => {
                    match key.code {
                        KeyCode::Tab | KeyCode::Down => {
                            form.active = (form.active + 1) % form.fields.len()
                        }
                        KeyCode::BackTab | KeyCode::Up => {
                            form.active = (form.active + form.fields.len() - 1) % form.fields.len()
                        }
                        KeyCode::F(2) if form.kind == FormKind::Filters => {
                            form.fields
                                .push(("Exclude path regex".into(), String::new()));
                            form.active = form.fields.len() - 1;
                        }
                        KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                            form.fields[form.active].1.clear()
                        }
                        KeyCode::Backspace => {
                            form.fields[form.active].1.pop();
                        }
                        KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                            form.fields[form.active].1.push(c)
                        }
                        KeyCode::Enter => match self.apply_form(&form) {
                            Ok(actions) => return actions,
                            Err(error) => form.error = error.to_string(),
                        },
                        _ => {}
                    }
                    self.modal = Some(Modal::Form(form));
                }
                Modal::Tags => {
                    if key.code == KeyCode::Char('r') {
                        self.form(FormKind::Reload);
                    } else if let Some(record) = self.selected().map(|r| r.key.record) {
                        let tag = match key.code {
                            KeyCode::Char('0') => Some(Some(Tag::None)),
                            KeyCode::Char('1') => Some(Some(Tag::Suspicious)),
                            KeyCode::Char('2') => Some(Some(Tag::Ioc)),
                            KeyCode::Char('3') => Some(Some(Tag::Malicious)),
                            KeyCode::Char('a') => Some(None),
                            _ => None,
                        };
                        if let Some(tag) = tag {
                            return vec![Action::Command(Command::Tag(record, tag))];
                        }
                        self.modal = Some(Modal::Tags);
                    }
                }
                Modal::Views => match key.code {
                    KeyCode::Char('1'..='4') => {
                        let (name, tags) = match key.code {
                            KeyCode::Char('1') => ("All tagged", vec![]),
                            KeyCode::Char('2') => ("SUSPICIOUS", vec![Tag::Suspicious]),
                            KeyCode::Char('3') => ("IOC", vec![Tag::Ioc]),
                            _ => ("MALICIOUS", vec![Tag::Malicious]),
                        };
                        return vec![self.open_view(View {
                            name: name.into(),
                            predicate: Predicate {
                                tags: Some(tags),
                                ..Predicate::default()
                            },
                            ..View::default()
                        })];
                    }
                    KeyCode::Char('n') => self.form(FormKind::Rename),
                    KeyCode::Char('x') if self.session.views.len() > 1 => {
                        if let Some((index, _)) = &mut self.previous {
                            if *index == self.session.active {
                                *index = usize::MAX;
                            } else if *index > self.session.active && *index != usize::MAX {
                                *index -= 1;
                            }
                        }
                        self.session.views.remove(self.session.active);
                        self.session.active = self.session.active.min(self.session.views.len() - 1);
                        self.rows.clear();
                        self.top = 0;
                        self.dirty_session = true;
                        return vec![self.load(Load::Replace, None, self.view().top, true)];
                    }
                    _ => self.modal = Some(Modal::Views),
                },
                Modal::Marks => {
                    if self.prefix == Some('d') {
                        if let KeyCode::Char(name) = key.code {
                            self.session.bookmarks.retain(|m| m.name != name);
                            self.dirty_session = true;
                        }
                        self.prefix = None;
                    } else if key.code == KeyCode::Char('d') {
                        self.prefix = Some('d');
                    } else if let KeyCode::Char(name @ 'a'..='z') = key.code {
                        return self.mark_jump(name);
                    }
                    self.modal = Some(Modal::Marks);
                }
            }
            return vec![];
        }
        if key.code == KeyCode::F(1) {
            self.modal = Some(Modal::Text("Keyboard shortcuts", HELP.into(), 0));
            return vec![];
        }
        if !self.ready {
            return vec![];
        }
        if let Some(prefix) = self.prefix.take() {
            match (prefix, key.code) {
                ('g', KeyCode::Char('g')) => {
                    return vec![self.load(Load::Replace, None, None, true)];
                }
                ('g', KeyCode::Char('t')) => {
                    return vec![self.switch((self.session.active + 1) % self.session.views.len())];
                }
                ('g', KeyCode::Char('T')) => {
                    return vec![self.switch(
                        (self.session.active + self.session.views.len() - 1)
                            % self.session.views.len(),
                    )];
                }
                ('m', KeyCode::Char(name @ 'a'..='z')) => {
                    if let Some(key) = self.selected().map(|r| r.key) {
                        self.session.bookmarks.retain(|m| m.name != name);
                        self.session.bookmarks.push(Bookmark { name, key });
                        self.dirty_session = true;
                        self.status = format!("Mark {name} set");
                    }
                    return vec![];
                }
                ('\'', KeyCode::Char('\'')) => {
                    if let Some((index, view)) = self.previous.take() {
                        self.previous = Some((self.session.active, self.view().clone()));
                        if index < self.session.views.len() {
                            self.session.active = index;
                            self.session.views[index] = view;
                            self.rows.clear();
                            self.top = 0;
                            return vec![self.load(Load::Replace, None, self.view().top, true)];
                        }
                        return vec![self.open_view(view)];
                    }
                    return vec![];
                }
                ('\'', KeyCode::Char(name @ 'a'..='z')) => return self.mark_jump(name),
                _ => {}
            }
        }
        match key.code {
            KeyCode::Char('g' | 'm' | '\'') => {
                if let KeyCode::Char(c) = key.code {
                    self.prefix = Some(c);
                    self.status = format!("{c}…");
                }
            }
            KeyCode::Char('G') => return vec![self.load(Load::Last, None, None, true)],
            KeyCode::Up => return self.scroll(-1).into_iter().collect(),
            KeyCode::Down => return self.scroll(1).into_iter().collect(),
            KeyCode::PageUp => return self.scroll(-(self.height as isize)).into_iter().collect(),
            KeyCode::PageDown => return self.scroll(self.height as isize).into_iter().collect(),
            KeyCode::Char('j') => {
                self.view_mut().selected = (self.view().selected + 1)
                    .min(self.height.saturating_sub(1))
                    .min(self.rows.len().saturating_sub(self.top + 1));
                self.remember();
            }
            KeyCode::Char('k') => {
                self.view_mut().selected = self.view().selected.saturating_sub(1);
                self.remember();
            }
            KeyCode::Tab => {
                if let Some((record, tag)) = self.selected().map(|r| (r.key.record, r.tag.next())) {
                    for row in &mut self.rows {
                        if row.key.record == record {
                            row.tag = tag;
                            row.automatic = false;
                        }
                    }
                    return vec![Action::Command(Command::Tag(record, Some(tag)))];
                }
            }
            KeyCode::Enter | KeyCode::F(3) => {
                if let Some(row) = self.selected() {
                    let timezone = self.session.timezone.parse().unwrap_or(chrono_tz::UTC);
                    return vec![Action::Command(Command::Detail(row.key.record, timezone))];
                }
            }
            KeyCode::F(1) => self.modal = Some(Modal::Text("Keyboard shortcuts", HELP.into(), 0)),
            KeyCode::F(2) => self.modal = Some(Modal::Tags),
            KeyCode::F(4) => self.form(FormKind::Time),
            KeyCode::F(5) | KeyCode::Char('/') => self.form(FormKind::Filters),
            KeyCode::F(6) => {
                self.remember();
                let mut view = self.view().clone();
                view.name = format!("{} copy", view.name);
                return vec![self.open_view(view)];
            }
            KeyCode::F(7) => self.modal = Some(Modal::Views),
            KeyCode::F(8) => self.modal = Some(Modal::Marks),
            KeyCode::F(9) => self.form(FormKind::Export),
            KeyCode::Char('D') => return vec![Action::Command(Command::Diagnostics)],
            KeyCode::Char('x' | 'X') => {
                if let Some(path) = self.selected().map(|r| r.path.clone()) {
                    let pattern = if key.code == KeyCode::Char('X') {
                        let parent = path
                            .rsplit_once(['/', '\\'])
                            .map(|(p, _)| p)
                            .unwrap_or(&path);
                        format!("^{}(?:[/\\\\]|$)", regex::escape(parent))
                    } else {
                        format!("^{}$", regex::escape(&path))
                    };
                    let mut view = self.view().clone();
                    view.predicate.exclude.push(pattern);
                    return vec![self.load(
                        Load::Replace,
                        Some(view),
                        self.rows.get(self.top).map(|r| r.key),
                        true,
                    )];
                }
            }
            _ => {}
        }
        vec![]
    }
}

fn expand_home(value: &str) -> PathBuf {
    if let Some(rest) = value.strip_prefix("~/") {
        PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(rest)
    } else {
        PathBuf::from(value)
    }
}

struct TerminalGuard;
impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), LeaveAlternateScreen, crossterm::cursor::Show);
    }
}
pub fn run(workers: &Workers, theme: crate::theme::Theme) -> Result<()> {
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        bail!("The TUI requires a terminal; use --index-only for noninteractive indexing");
    }
    enable_raw_mode()?;
    let _guard = TerminalGuard;
    execute!(io::stdout(), EnterAlternateScreen)?;
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), LeaveAlternateScreen, crossterm::cursor::Show);
        previous(info);
    }));
    let mut terminal = Terminal::new(CrosstermBackend::new(io::stdout()))?;
    let mut app = App::default();
    app.session.theme = theme;
    let mut dirty = true;
    let mut saved = Instant::now();
    loop {
        let mut actions = Vec::new();
        while let Ok(message) = workers.messages.try_recv() {
            actions.extend(app.message(message));
            dirty = true;
        }
        if event::poll(Duration::from_millis(16))? {
            match event::read()? {
                Event::Key(key) => {
                    actions.extend(app.key(key));
                    dirty = true;
                }
                Event::Resize(_, _) => dirty = true,
                _ => {}
            }
        }
        for action in actions {
            match action {
                Action::Page(request) => {
                    workers.clock.store(request.generation, Ordering::Relaxed);
                    workers.pages.send(request)?;
                }
                Action::Command(command) => workers.commands.send(command)?,
                Action::Cancel => workers.cancel(),
                Action::Quit => return Ok(()),
            }
        }
        if app.ready && app.dirty_session && saved.elapsed() > Duration::from_millis(300) {
            workers.commands.send(Command::Save(app.session.clone()))?;
            app.dirty_session = false;
            saved = Instant::now();
        }
        if dirty {
            terminal.draw(|frame| app.draw(frame))?;
            dirty = false;
        }
    }
}
