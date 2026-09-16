use crate::{
    burst,
    export::Export,
    model::{
        Bookmark, DisplayRow, EventKey, Predicate, Row, Session, Tag, TagChange, View, boundary,
        display_text, macb, validate_view,
    },
    navigation::{Jump, TimeUnit, parse_goto},
    worker::{BurstRequest, Command, Message, PageRequest, Workers},
};
use anyhow::{Result, bail};
use chrono::TimeZone;
use crossterm::{
    event::{
        self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEvent, KeyEventKind,
        KeyModifiers, MouseEventKind,
    },
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
    collections::{BTreeMap, BTreeSet, btree_map::Entry},
    io::{self, IsTerminal},
    path::PathBuf,
    sync::atomic::Ordering,
    time::{Duration, Instant},
};

mod mouse;
mod render;
use mouse::{MouseState, MouseTarget};

const DELETE_MARK_PROMPT: &str = "Delete bookmark: type a–z (Esc cancels)";

const HELP: &str = "\
TIMELINER TUI

↑/↓ scroll the view, keeping the same event selected while visible.
If it leaves the screen, the nearest visible event becomes selected.
j/k select the next/previous event, scrolling one row at screen edges.
Mouse: wheel scrolls; click/drag selects; double-click/right-click inspects.
Click tabs, function buttons, menu choices, and form fields.
Mouse wheel scrolls details/help/bookmarks or moves between form fields.
F10 toggles mouse capture for terminal text selection and copying, even in dialogs.
Under tmux, enable mouse with: set -g mouse on
PageUp/PageDown scroll a screen. gg/G jump to beginning/end.
g opens Goto: type an ISO date/time and Enter to center the first match at or after it.
Use Z or an explicit offset, or omit the offset to use the displayed timezone.
Date-only input means midnight. Current filters apply; '' returns after the jump.
In an empty Goto field, g/t/T still complete gg/gt/gT.
nm/nh/nd/nM/ny jump to the next minute/hour/day/month/year and center it.
nt jumps to the next tagged event and centers it, including within the same second.
bm/bh/bd/bM/by jump backward to the preceding minute/hour/day/month/year boundary.
bt jumps to the previous tagged event. Backward destinations are centered too.
Jumps use the selected timezone and current filters; '' returns.
From a collapsed burst, forward jumps start after its end; backward jumps before its start.
zt/zz/zb put the selected event at the top/middle/bottom.
z Enter / z. / z- do the same. Selection stays on the same event.
SPACE toggles a file in the selection; ✓ marks all its timestamp entries.
Selections survive scrolling and tagging. Esc clears them; changing tabs or filters resets them.
Reloading IOC rules clears the selection.
TAB cycles the focused selected file's assessment and applies it to the selection.
If focus is outside the selection, TAB uses the first selected source record.
F2 sets/clears/restores assessments for the selection. ENTER/F3 opens full details.
Tags belong to source records, so all their MACB events agree.

F1 Help   F2 Tags / reload IOC rules   F3 Details
F4 Timeframe   F5 Filters   F6 Fork view   F7 Smart tabs
F8 Bookmarks   F9 Export   q Quit (also Ctrl+C)
F10 switches mouse support off/on.
F11 switches between light and dark themes.
1–9 select tabs directly; gt/gT switch tabs.
Smart tabs start across the whole investigation.
F7 also offers rename/close. F6 copies filters and location.

ma sets mark a; 'a jumps to it; '' returns after a jump.
● in the left margin marks a bookmarked event.
If a mark is hidden, a new unfiltered view reveals it.
F8 lists marks. da deletes mark a from the timeline or F8.
No Vim text registers are implemented.

x excludes all SPACE-selected pathnames, or the focused pathname without a selection.
X edits a directory exclusion for the focused entry.
In X: Backspace goes up one directory; Enter excludes its subtree.
Type to edit; Delete erases a character; Ctrl+U clears the path.
u removes the most recent exclusion, including a whole batch.
c collapses consecutive dense seconds on the selected host, across all paths.
Each second must exceed the threshold (default 100 events/s). No gaps are bridged.
C edits the threshold. c or ENTER/F3 on a summary expands it.
Other hosts remain visible. Tag edits keep unrelated folds closed.
Filters, IOC rule reloads, and restart reset folds. Bookmark jumps reveal members.
Export includes folded records.
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
Filter forms: F2 adds an exclusion; Ctrl+D removes the focused exclusion.
Details: ↑/↓ scroll.
State is saved automatically. Sources are never modified.";

#[derive(Clone, Copy)]
enum Load {
    DetectBurst,
    FindJump(Jump),
    CenterJump(EventKey),
    Step {
        reverse: bool,
    },
    Replace,
    Filter(Option<EventKey>),
    FilterLast(Option<EventKey>),
    Before {
        delta: usize,
        keep_event: bool,
    },
    After {
        delta: usize,
        keep_event: bool,
    },
    Last,
    Mark(EventKey, char),
    Reposition {
        focus: EventKey,
        position: RowPosition,
        reverse: bool,
    },
}
#[derive(Clone, Copy)]
enum RowPosition {
    Top,
    Middle,
    Bottom,
}
impl RowPosition {
    fn slot(self, height: usize) -> usize {
        match self {
            Self::Top => 0,
            // Vim uses the upper middle row when the viewport height is even.
            Self::Middle => height.saturating_sub(1) / 2,
            Self::Bottom => height.saturating_sub(1),
        }
    }
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
    Burst,
    Goto,
    Time,
    Filters,
    Directory,
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
    MouseCapture(bool),
    Cancel,
    Quit,
}

pub struct App {
    pub session: Session,
    pub rows: Vec<DisplayRow>,
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
    mouse_enabled: bool,
    mouse_state: MouseState,
    selected_records: BTreeMap<i64, Row>,
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
            mouse_enabled: true,
            mouse_state: MouseState::default(),
            selected_records: BTreeMap::new(),
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
    fn requested_view(&self) -> &View {
        self.pending
            .as_ref()
            .and_then(|pending| pending.candidate.as_ref())
            .unwrap_or_else(|| self.view())
    }
    fn reload_filters(&mut self, mut view: View) -> Action {
        view.bursts.clear();
        let anchor = self.rows.get(self.top).map(|row| row.key()).or(view.top);
        let focus = self.selected().map(|row| row.key()).or(view.top);
        view.top = anchor;
        view.selected = self.view().selected;
        self.load(Load::Filter(focus), Some(view), anchor, true)
    }
    fn selected(&self) -> Option<&DisplayRow> {
        self.rows.get(self.top + self.view().selected)
    }
    fn selected_event(&self) -> Option<&Row> {
        self.selected().and_then(DisplayRow::event)
    }
    fn toggle_record_selection(&mut self) {
        let Some(row) = self.selected_event() else {
            self.status = if self.selected().is_some() {
                "Expand the burst to select individual files"
            } else {
                "No event selected"
            }
            .into();
            return;
        };
        let row = row.clone();
        match self.selected_records.entry(row.key.record) {
            Entry::Occupied(entry) => {
                entry.remove();
            }
            Entry::Vacant(entry) => {
                entry.insert(row);
            }
        }
        self.status = if self.selected_records.is_empty() {
            "Selection cleared".into()
        } else {
            "SPACE toggles selection · TAB/F2 tag · x excludes · Esc clears".into()
        };
    }
    fn assessment_target(&self) -> Option<&Row> {
        if self.selected_records.is_empty() {
            self.selected_event()
        } else {
            self.selected_event()
                .and_then(|row| self.selected_records.get(&row.key.record))
                .or_else(|| self.selected_records.values().next())
        }
    }
    fn apply_tag(&mut self, tag: Option<Tag>) -> Vec<Action> {
        let records = if self.selected_records.is_empty() {
            let Some(row) = self.selected_event() else {
                self.status = "Expand the burst to assess an individual record".into();
                return vec![];
            };
            vec![row.key.record]
        } else {
            self.selected_records.keys().copied().collect()
        };
        if let Some(tag) = tag {
            for row in self.rows.iter_mut().filter_map(DisplayRow::event_mut) {
                if records.binary_search(&row.key.record).is_ok() {
                    row.tag = tag;
                    row.automatic = false;
                }
            }
            for row in self.selected_records.values_mut() {
                row.tag = tag;
                row.automatic = false;
            }
        }
        vec![Action::Command(if records.len() == 1 {
            Command::Tag(records[0], tag)
        } else {
            Command::TagMany(records, tag)
        })]
    }
    fn tags_changed(&mut self, changes: &[TagChange]) -> Vec<Action> {
        let mut candidate = self
            .pending
            .as_ref()
            .and_then(|pending| pending.candidate.clone());
        for change in changes {
            for (key, _) in &change.events {
                if let Some(row) = self.selected_records.get_mut(&key.record) {
                    row.tag = change.after;
                }
            }
            for view in self
                .session
                .views
                .iter_mut()
                .chain(self.previous.iter_mut().map(|(_, view)| view))
                .chain(candidate.iter_mut())
            {
                if burst::apply_tag_change(view, change).is_err() {
                    view.bursts.clear();
                }
            }
        }
        let anchor = self
            .rows
            .get(self.top)
            .map(DisplayRow::key)
            .or(self.view().top);
        let focus = self.selected().map(DisplayRow::key).or(self.view().top);
        vec![self.load(Load::Filter(focus), candidate, anchor, true)]
    }
    fn load_around(&mut self, focus: EventKey, mut view: View) -> Action {
        let context_before = self
            .view()
            .selected
            .max(self.height / 2)
            .min(self.height.saturating_sub(1));
        view.selected = context_before;
        let Action::Page(mut request) =
            self.load(Load::Filter(Some(focus)), Some(view), Some(focus), true)
        else {
            unreachable!()
        };
        request.context_before = context_before;
        Action::Page(request)
    }
    fn expand_burst(&mut self) -> Option<Action> {
        let DisplayRow::Burst(burst) = self.selected()? else {
            return None;
        };
        let (first, origin) = (burst.first, burst.origin);
        let mut view = self.view().clone();
        view.bursts.retain(|burst| burst.first != first);
        Some(self.load_around(origin, view))
    }
    fn collapse_burst(&mut self) -> Vec<Action> {
        if self.pending.is_some() {
            self.status = "Loading… (Escape cancels)".into();
            return vec![];
        }
        if let Some(action) = self.expand_burst() {
            return vec![action];
        }
        let Some(row) = self.selected_event() else {
            self.status = "No event selected".into();
            return vec![];
        };
        let detection = BurstRequest {
            origin: row.key,
            host: row.host.clone(),
            threshold: self.session.burst_threshold,
        };
        let Action::Page(mut request) =
            self.load(Load::DetectBurst, None, Some(detection.origin), true)
        else {
            unreachable!()
        };
        request.detect_burst = Some(detection);
        self.status = "Finding burst… (Escape cancels)".into();
        vec![Action::Page(request)]
    }
    fn remember(&mut self) {
        if let Some(key) = self.rows.get(self.top).map(|row| row.key()) {
            self.view_mut().top = Some(key);
        }
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
            reverse: matches!(
                mode,
                Load::Before { .. }
                    | Load::Last
                    | Load::FilterLast(_)
                    | Load::Step { reverse: true }
                    | Load::Reposition { reverse: true, .. }
            ),
            inclusive,
            limit,
            context_before: 0,
            detect_burst: None,
            jump: None,
        })
    }
    fn refresh(&mut self) -> Action {
        self.load(
            Load::Replace,
            None,
            self.rows.get(self.top).map(|r| r.key()).or(self.view().top),
            true,
        )
    }
    fn switch(&mut self, active: usize) -> Action {
        self.remember();
        self.selected_records.clear();
        self.session.active = active;
        self.rows.clear();
        self.top = 0;
        self.load(Load::Replace, None, self.view().top, true)
    }
    fn open_view(&mut self, view: View) -> Action {
        self.remember();
        self.selected_records.clear();
        self.session.views.push(view);
        self.session.active = self.session.views.len() - 1;
        self.rows.clear();
        self.top = 0;
        self.dirty_session = true;
        self.load(Load::Replace, None, self.view().top, true)
    }
    fn scroll_to(&mut self, top: usize, keep_event: bool) {
        if keep_event {
            self.view_mut().selected = (self.top + self.view().selected)
                .saturating_sub(top)
                .min(self.height.saturating_sub(1))
                .min(self.rows.len().saturating_sub(top + 1));
        }
        self.top = top;
    }
    fn scroll(&mut self, delta: isize, keep_event: bool) -> Option<Action> {
        if delta > 0 {
            let room = self.rows.len().saturating_sub(self.top + self.height);
            if room > 0 {
                self.scroll_to(self.top + room.min(delta as usize), keep_event);
            } else if !self.end_after && self.pending.is_none() {
                return Some(self.load(
                    Load::After {
                        delta: delta as usize,
                        keep_event,
                    },
                    None,
                    self.rows.last().map(|r| r.key()),
                    false,
                ));
            }
        } else if self.top > 0 {
            self.scroll_to(self.top.saturating_sub(delta.unsigned_abs()), keep_event);
        } else if !self.end_before && self.pending.is_none() {
            return Some(self.load(
                Load::Before {
                    delta: delta.unsigned_abs(),
                    keep_event,
                },
                None,
                self.rows.first().map(|r| r.key()),
                false,
            ));
        }
        self.remember();
        None
    }
    fn move_selection(&mut self, reverse: bool) -> Option<Action> {
        let anchor = self.selected()?.key();
        let selected = self.view().selected;
        let index = self.top + selected;
        if (reverse && index == 0) || (!reverse && index + 1 == self.rows.len()) {
            let at_end = if reverse {
                self.end_before
            } else {
                self.end_after
            };
            if !at_end && self.pending.is_none() {
                return Some(self.load(Load::Step { reverse }, None, Some(anchor), false));
            }
            return None;
        }
        if reverse {
            if selected == 0 {
                self.top -= 1;
            } else {
                self.view_mut().selected -= 1;
            }
        } else if selected >= self.height.saturating_sub(1) {
            self.top += 1;
        } else {
            self.view_mut().selected += 1;
        }
        self.remember();
        None
    }
    fn reposition(&mut self, position: RowPosition) -> Option<Action> {
        if self.pending.is_some() {
            self.status = "Loading… (Escape cancels)".into();
            return None;
        }
        let Some(focus) = self.selected().map(|row| row.key()) else {
            self.status = "No matching events. F4 timeframe · F5 filters".into();
            return None;
        };
        self.status = "Ready".into();
        let index = self.top + self.view().selected;
        let slot = position.slot(self.height);
        let top = index.saturating_sub(slot);
        let reverse = index < slot && !self.end_before;
        if reverse || (top + self.height > self.rows.len() && !self.end_after) {
            let anchor = if reverse {
                self.rows.first()
            } else {
                self.rows.last()
            }
            .map(|row| row.key());
            return Some(self.load(
                Load::Reposition {
                    focus,
                    position,
                    reverse,
                },
                None,
                anchor,
                false,
            ));
        }
        self.top = top;
        self.view_mut().selected = index - top;
        self.remember();
        None
    }
    fn jump_origin(&mut self, reverse: bool) -> Option<EventKey> {
        if self.pending.is_some() {
            self.status = "Loading… (Escape cancels)".into();
            return None;
        }
        let Some(row) = self.selected() else {
            self.status = "No event selected".into();
            return None;
        };
        Some(match row {
            DisplayRow::Event(row) => row.key,
            DisplayRow::Burst(burst) => {
                if reverse {
                    burst.first
                } else {
                    burst.last
                }
            }
        })
    }
    fn jump_time(&mut self, unit: TimeUnit, reverse: bool) -> Option<Action> {
        let epoch = self.jump_origin(reverse)?.epoch;
        let tz = self.session.timezone.parse().unwrap_or(chrono_tz::UTC);
        let boundary = if reverse {
            unit.previous_boundary(epoch, tz)
        } else {
            unit.next_boundary(epoch, tz)
        };
        let boundary = match boundary {
            Ok(boundary) => boundary,
            Err(error) => {
                self.status = error.to_string();
                return None;
            }
        };
        Some(self.find_jump(if reverse {
            Jump::PreviousTimeBoundary(boundary)
        } else {
            Jump::TimeBoundary(boundary)
        }))
    }
    fn jump_tagged(&mut self, reverse: bool) -> Option<Action> {
        let origin = self.jump_origin(reverse)?;
        Some(self.find_jump(if reverse {
            Jump::TaggedBefore(origin)
        } else {
            Jump::TaggedAfter(origin)
        }))
    }
    fn find_jump(&mut self, jump: Jump) -> Action {
        let Action::Page(mut request) = self.load(Load::FindJump(jump), None, None, true) else {
            unreachable!()
        };
        request.jump = Some(jump);
        Action::Page(request)
    }
    fn mark_delete(&mut self, name: char) {
        let count = self.session.bookmarks.len();
        self.session.bookmarks.retain(|mark| mark.name != name);
        if self.session.bookmarks.len() < count {
            self.dirty_session = true;
            self.status = format!("Mark {name} deleted");
        } else {
            self.status = format!("Mark {name} is not set");
        }
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
        if self
            .view()
            .bursts
            .iter()
            .any(|burst| burst.contains_key(mark.key))
        {
            let mut view = self.view().clone();
            view.bursts.retain(|burst| !burst.contains_key(mark.key));
            view.selected = 0;
            return vec![self.load(Load::Mark(mark.key, name), Some(view), Some(mark.key), true)];
        }
        if let Some(index) = self.rows.iter().position(|r| r.key() == mark.key) {
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
                if matches!(mode, Load::DetectBurst | Load::FindJump(_)) {
                    return vec![];
                }
                if let Load::CenterJump(focus) = mode
                    && !rows.iter().any(|row| row.key() == focus)
                {
                    self.status = "Jump target no longer matches this view".into();
                    return vec![];
                }
                if let Load::Mark(key, name) = mode {
                    if rows.first().is_none_or(|r| r.key() != key) {
                        return vec![self.open_view(View {
                            name: format!("Mark {name}"),
                            top: Some(key),
                            bogus: key.epoch <= 0,
                            ..View::default()
                        })];
                    }
                    self.view_mut().selected = 0;
                }
                if rows.is_empty() && anchor.is_some() {
                    let backward = match mode {
                        Load::Replace => Some(Load::Last),
                        Load::Filter(focus) => Some(Load::FilterLast(focus)),
                        _ => None,
                    };
                    if let Some(mode) = backward {
                        return vec![self.load(mode, candidate, anchor, false)];
                    }
                }
                if let Load::Filter(Some(focus)) = mode
                    && rows.len() == limit
                    && rows.last().is_some_and(|row| row.key() < focus)
                {
                    // Removing exclusions can insert more than a page before the
                    // selection. Seek directly to its key instead of losing it.
                    return vec![self.load(mode, candidate, Some(focus), true)];
                }
                if candidate.as_ref().is_some_and(|view| {
                    view.predicate != self.view().predicate
                        || view.after != self.view().after
                        || view.before != self.view().before
                        || view.time_mask != self.view().time_mask
                        || view.bogus != self.view().bogus
                }) {
                    self.selected_records.clear();
                }
                match mode {
                    Load::DetectBurst | Load::FindJump(_) => unreachable!(),
                    Load::CenterJump(focus) => {
                        self.remember();
                        self.previous = Some((self.session.active, self.view().clone()));
                        if let Some(view) = candidate {
                            *self.view_mut() = view;
                        }
                        let index = rows.partition_point(|row| row.key() < focus);
                        self.end_before = index < limit / 2;
                        self.end_after = rows.len() < limit;
                        self.rows = rows;
                        self.top = index.saturating_sub(RowPosition::Middle.slot(self.height));
                        self.view_mut().selected = index - self.top;
                    }
                    Load::Replace
                    | Load::Filter(_)
                    | Load::FilterLast(_)
                    | Load::Last
                    | Load::Mark(_, _) => {
                        if let Some(view) = candidate {
                            *self.view_mut() = view;
                        }
                        let last = matches!(mode, Load::Last | Load::FilterLast(_));
                        self.end_before = (anchor.is_none()
                            && matches!(mode, Load::Replace | Load::Filter(_)))
                            || (last && rows.len() < limit);
                        self.end_after = rows.len() < limit || last;
                        self.rows = rows;
                        self.top = if last {
                            self.rows.len().saturating_sub(self.height)
                        } else {
                            0
                        };
                        if let Load::Filter(Some(focus)) | Load::FilterLast(Some(focus)) = mode {
                            if self.rows.is_empty() {
                                // Keep the event's key so undo can return here,
                                // even though there is no visible row to remember.
                                self.view_mut().top = Some(focus);
                            } else {
                                let index = self
                                    .rows
                                    .partition_point(|row| row.key() < focus)
                                    .min(self.rows.len() - 1);
                                let slot = self.view().selected.min(self.height.saturating_sub(1));
                                self.top = index.saturating_sub(slot);
                                self.view_mut().selected = index - self.top;
                            }
                        } else if last {
                            self.view_mut().selected = self
                                .height
                                .saturating_sub(1)
                                .min(self.rows.len().saturating_sub(1));
                        }
                    }
                    Load::After { .. }
                    | Load::Reposition { reverse: false, .. }
                    | Load::Step { reverse: false } => {
                        self.end_after = rows.len() < limit;
                        let added = rows.len();
                        self.rows.append(&mut rows);
                        if let Load::After { delta, keep_event } = mode {
                            self.scroll_to(
                                (self.top + delta).min(self.rows.len().saturating_sub(self.height)),
                                keep_event,
                            );
                        }
                        if matches!(mode, Load::Step { .. }) && added > 0 {
                            if self.view().selected >= self.height.saturating_sub(1) {
                                self.top += 1;
                            } else {
                                self.view_mut().selected += 1;
                            }
                        }
                        if self.rows.len() > limit * 2 {
                            let discard = self.rows.len() - limit * 2;
                            self.rows.drain(..discard);
                            self.top = self.top.saturating_sub(discard);
                            self.end_before = false;
                        }
                    }
                    Load::Before { .. }
                    | Load::Reposition { reverse: true, .. }
                    | Load::Step { reverse: true } => {
                        self.end_before = rows.len() < limit;
                        let added = rows.len();
                        rows.append(&mut self.rows);
                        self.rows = rows;
                        self.top += added;
                        if let Load::Before { delta, keep_event } = mode {
                            self.scroll_to(self.top.saturating_sub(delta), keep_event);
                        }
                        if matches!(mode, Load::Step { .. }) && added > 0 {
                            if self.view().selected == 0 {
                                self.top -= 1;
                            } else {
                                self.view_mut().selected -= 1;
                            }
                        }
                        if self.rows.len() > limit * 2 {
                            self.rows.truncate(limit * 2);
                            self.end_after = false;
                        }
                    }
                }
                if let Load::Reposition {
                    focus, position, ..
                } = mode
                    && let Some(index) = self.rows.iter().position(|row| row.key() == focus)
                {
                    self.top = index.saturating_sub(position.slot(self.height));
                    self.view_mut().selected = index - self.top;
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
            Message::JumpTarget(generation, target) => {
                let Some(pending) = self
                    .pending
                    .as_ref()
                    .filter(|pending| pending.generation == generation)
                else {
                    return vec![];
                };
                let Load::FindJump(jump) = pending.mode else {
                    return vec![];
                };
                self.pending = None;
                let Some(target) = target else {
                    self.status = match jump {
                        Jump::Goto(_) => "No matching event at or after the requested date/time",
                        Jump::TimeBoundary(_) => {
                            "No matching event at or after the next time boundary"
                        }
                        Jump::TaggedAfter(_) => "No later tagged event matches this view",
                        Jump::PreviousTimeBoundary(_) => {
                            "No matching event at or before the previous time boundary"
                        }
                        Jump::TaggedBefore(_) => "No earlier tagged event matches this view",
                    }
                    .into();
                    return vec![];
                };
                let mut view = self.view().clone();
                view.bursts
                    .retain(|burst| burst.host != target.host || !burst.contains_key(target.key));
                let Action::Page(mut request) = self.load(
                    Load::CenterJump(target.key),
                    Some(view),
                    Some(target.key),
                    true,
                ) else {
                    unreachable!()
                };
                // Resolve the event before loading context: a sparse timeline can
                // have a large gap between the calendar boundary and its next event.
                request.context_before = request.limit / 2;
                vec![Action::Page(request)]
            }
            Message::Burst(generation, burst) => {
                if !self.pending.as_ref().is_some_and(|pending| {
                    pending.generation == generation && matches!(pending.mode, Load::DetectBurst)
                }) {
                    return vec![];
                }
                self.pending = None;
                let Some(burst) = burst else {
                    self.status = format!(
                        "No burst here: each second must exceed {} events on this host (C changes threshold)",
                        self.session.burst_threshold
                    );
                    return vec![];
                };
                let first = burst.first;
                let mut view = self.view().clone();
                view.bursts.retain(|existing| {
                    existing.host != burst.host
                        || existing.last < burst.first
                        || existing.first > burst.last
                });
                view.bursts.push(burst);
                vec![self.load_around(first, view)]
            }
            Message::Tagged(change) => self.tags_changed(&[change]),
            Message::TaggedMany(changes) => self.tags_changed(&changes),
            Message::Changed => {
                self.selected_records.clear();
                for view in &mut self.session.views {
                    view.bursts.clear();
                }
                if let Some((_, view)) = &mut self.previous {
                    view.bursts.clear();
                }
                vec![self.refresh()]
            }
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
            FormKind::Goto => vec![("ISO date/time", String::new())],
            FormKind::Burst => vec![(
                "More than this many events per second",
                self.session.burst_threshold.to_string(),
            )],
            FormKind::Time => {
                let tz: chrono_tz::Tz = self.session.timezone.parse().unwrap_or(chrono_tz::UTC);
                let format = |epoch: Option<i64>| {
                    epoch
                        .and_then(|e| tz.timestamp_opt(e, 0).single())
                        .map(|t| t.format("%Y-%m-%d %H:%M:%S").to_string())
                        .unwrap_or_default()
                };
                vec![
                    (
                        "Begin (blank = unbounded)",
                        format(self.requested_view().after),
                    ),
                    (
                        "End (blank = unbounded)",
                        format(self.requested_view().before),
                    ),
                    ("Timezone", self.session.timezone.clone()),
                ]
            }
            FormKind::Filters => {
                let view = self.requested_view();
                let mut fields = vec![
                    ("Include path regex", view.predicate.include.clone()),
                    (
                        "Hosts (comma separated; blank = all)",
                        view.predicate.hosts.join(","),
                    ),
                    (
                        "Timestamp types (macb)",
                        macb(view.time_mask).replace('.', ""),
                    ),
                    (
                        "Include nonpositive timestamps (yes/no)",
                        if view.bogus { "yes" } else { "no" }.into(),
                    ),
                ];
                for pattern in &view.predicate.exclude {
                    fields.push(("Exclude path regex", pattern.clone()));
                }
                fields.push(("Exclude path regex", String::new()));
                fields
            }
            FormKind::Directory => {
                let Some(row) = self.selected_event() else {
                    return;
                };
                vec![(
                    "Directory (including subdirectories)",
                    parent_directory(&row.path).into(),
                )]
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
        if kind == FormKind::Goto
            && let Some(Modal::Form(form)) = &mut self.modal
        {
            form.fields[0].0 = format!("ISO date/time (no offset = {})", self.session.timezone);
        }
    }
    fn apply_form(&mut self, form: &Form) -> Result<Vec<Action>> {
        let value = |i: usize| form.fields[i].1.trim();
        match form.kind {
            FormKind::Goto => {
                let tz = self.session.timezone.parse()?;
                let epoch = parse_goto(value(0), tz)?;
                if self.pending.is_some() {
                    bail!("Wait for the current query to finish, then press Enter");
                }
                Ok(vec![self.find_jump(Jump::Goto(epoch))])
            }
            FormKind::Burst => {
                let threshold: u64 = value(0).parse().map_err(|_| {
                    anyhow::anyhow!("Enter a positive whole number of events per second")
                })?;
                if threshold == 0 {
                    bail!("Enter a positive whole number of events per second");
                }
                self.session.burst_threshold = threshold;
                self.dirty_session = true;
                self.status = format!("Burst threshold: more than {threshold} events/second");
                Ok(vec![])
            }
            FormKind::Time => {
                let tz = value(2).parse()?;
                let mut view = self.requested_view().clone();
                view.after = boundary(value(0), tz, false)?;
                view.before = boundary(value(1), tz, true)?;
                view.top = None;
                view.selected = 0;
                view.bursts.clear();
                validate_view(&view)?;
                self.session.timezone = value(2).into();
                Ok(vec![self.load(Load::Replace, Some(view), None, true)])
            }
            FormKind::Filters => {
                let mut view = self.requested_view().clone();
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
                validate_view(&view)?;
                Ok(vec![self.reload_filters(view)])
            }
            FormKind::Directory => {
                let directory = &form.fields[0].1;
                if directory.is_empty() {
                    bail!("Enter a directory to exclude");
                }
                let pattern = directory_exclusion(directory);
                let mut view = self.requested_view().clone();
                if view.predicate.exclude.contains(&pattern) {
                    self.status = "Directory is already excluded".into();
                    return Ok(vec![]);
                }
                view.predicate.exclude.push(pattern);
                Ok(vec![self.reload_filters(view)])
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
        self.mouse_state.reset_click();
        if key.code == KeyCode::F(10) && key.modifiers.is_empty() {
            if key.kind == KeyEventKind::Press {
                self.mouse_enabled = !self.mouse_enabled;
                return vec![Action::MouseCapture(self.mouse_enabled)];
            }
            return vec![];
        }
        if (key.code == KeyCode::Char('q')
            && key.modifiers.is_empty()
            && self.prefix.is_none()
            && (self.modal.is_none() || matches!(self.modal, Some(Modal::Text(..)))))
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
            if self.prefix.take().is_some() {
                self.status = "Cancelled".into();
            }
            if self.modal.take().is_none() {
                self.selected_records.clear();
                self.pending = None;
                self.status = "Cancelled".into();
                self.generation += 1;
                return vec![Action::Cancel];
            }
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
                    if form.kind == FormKind::Goto
                        && form.fields[0].1.is_empty()
                        && form.error.is_empty()
                        && key.modifiers.difference(KeyModifiers::SHIFT).is_empty()
                        && matches!(key.code, KeyCode::Char('g' | 't' | 'T'))
                    {
                        if key.kind == KeyEventKind::Repeat {
                            self.modal = Some(Modal::Form(form));
                            return vec![];
                        }
                        return vec![match key.code {
                            KeyCode::Char('g') => self.load(Load::Replace, None, None, true),
                            KeyCode::Char('t') => {
                                self.switch((self.session.active + 1) % self.session.views.len())
                            }
                            _ => self.switch(
                                (self.session.active + self.session.views.len() - 1)
                                    % self.session.views.len(),
                            ),
                        }];
                    }
                    match key.code {
                        KeyCode::Tab | KeyCode::Down => {
                            form.active = (form.active + 1) % form.fields.len()
                        }
                        KeyCode::BackTab | KeyCode::Up => {
                            form.active = (form.active + form.fields.len() - 1) % form.fields.len()
                        }
                        KeyCode::F(2) if form.kind == FormKind::Filters => {
                            if form
                                .fields
                                .last()
                                .is_none_or(|(_, value)| !value.is_empty())
                            {
                                form.fields
                                    .push(("Exclude path regex".into(), String::new()));
                            }
                            form.active = form.fields.len() - 1;
                        }
                        KeyCode::Char('d')
                            if key.modifiers.contains(KeyModifiers::CONTROL)
                                && form.kind == FormKind::Filters
                                && form.active >= 4 =>
                        {
                            form.fields.remove(form.active);
                            if form.fields.len() == 4 {
                                form.fields
                                    .push(("Exclude path regex".into(), String::new()));
                            }
                            form.active = form.active.min(form.fields.len() - 1);
                        }
                        KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                            form.fields[form.active].1.clear()
                        }
                        KeyCode::Backspace if form.kind == FormKind::Directory => {
                            let directory = &mut form.fields[form.active].1;
                            if !directory.is_empty() {
                                *directory = parent_directory(directory).into();
                            }
                        }
                        KeyCode::Delete if form.kind == FormKind::Directory => {
                            form.fields[form.active].1.pop();
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
                    } else {
                        let tag = match key.code {
                            KeyCode::Char('0') => Some(Some(Tag::None)),
                            KeyCode::Char('1') => Some(Some(Tag::Suspicious)),
                            KeyCode::Char('2') => Some(Some(Tag::Ioc)),
                            KeyCode::Char('3') => Some(Some(Tag::Malicious)),
                            KeyCode::Char('a') => Some(None),
                            _ => None,
                        };
                        if let Some(tag) = tag {
                            return self.apply_tag(tag);
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
                        self.selected_records.clear();
                        self.session.active = self.session.active.min(self.session.views.len() - 1);
                        self.rows.clear();
                        self.top = 0;
                        self.dirty_session = true;
                        return vec![self.load(Load::Replace, None, self.view().top, true)];
                    }
                    _ => self.modal = Some(Modal::Views),
                },
                Modal::Marks => {
                    let deleting = self.prefix.take() == Some('d');
                    if key.modifiers.difference(KeyModifiers::SHIFT).is_empty() {
                        match (deleting, key.code) {
                            (true, KeyCode::Char(name @ 'a'..='z')) => self.mark_delete(name),
                            (false, KeyCode::Char('d')) => {
                                self.prefix = Some('d');
                                self.status = DELETE_MARK_PROMPT.into();
                            }
                            (false, KeyCode::Char(name @ 'a'..='z')) => {
                                return self.mark_jump(name);
                            }
                            _ => {}
                        }
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
                ('n' | 'b', code) => {
                    if key.modifiers.difference(KeyModifiers::SHIFT).is_empty() {
                        if code == KeyCode::Char('t') {
                            return self.jump_tagged(prefix == 'b').into_iter().collect();
                        }
                        let unit = match code {
                            KeyCode::Char('m') => Some(TimeUnit::Minute),
                            KeyCode::Char('h') => Some(TimeUnit::Hour),
                            KeyCode::Char('d') => Some(TimeUnit::Day),
                            KeyCode::Char('M') => Some(TimeUnit::Month),
                            KeyCode::Char('y') => Some(TimeUnit::Year),
                            _ => None,
                        };
                        if let Some(unit) = unit {
                            return self.jump_time(unit, prefix == 'b').into_iter().collect();
                        }
                    }
                    self.status = format!(
                        "Unknown {prefix} shortcut: use {prefix}m, {prefix}h, {prefix}d, {prefix}M, {prefix}y, or {prefix}t"
                    );
                    return vec![];
                }
                ('z', code) => {
                    if key.modifiers.difference(KeyModifiers::SHIFT).is_empty() {
                        let position = match code {
                            KeyCode::Char('t') | KeyCode::Enter => Some(RowPosition::Top),
                            KeyCode::Char('z' | '.') => Some(RowPosition::Middle),
                            KeyCode::Char('b' | '-') => Some(RowPosition::Bottom),
                            _ => None,
                        };
                        if let Some(position) = position {
                            return self.reposition(position).into_iter().collect();
                        }
                    }
                    self.status = "Unknown z shortcut: use zt, zz, or zb".into();
                    return vec![];
                }
                ('m', KeyCode::Char(name @ 'a'..='z')) => {
                    if let Some(key) = self.selected().map(|r| r.key()) {
                        self.session.bookmarks.retain(|m| m.name != name);
                        self.session.bookmarks.push(Bookmark { name, key });
                        self.dirty_session = true;
                        self.status = format!("Mark {name} set");
                    }
                    return vec![];
                }
                ('d', KeyCode::Char(name @ 'a'..='z'))
                    if key.modifiers.difference(KeyModifiers::SHIFT).is_empty() =>
                {
                    self.mark_delete(name);
                    return vec![];
                }
                ('\'', KeyCode::Char('\'')) => {
                    if let Some((index, view)) = self.previous.take() {
                        self.previous = Some((self.session.active, self.view().clone()));
                        if index < self.session.views.len() {
                            self.selected_records.clear();
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
            KeyCode::Char(prefix @ ('n' | 'b')) if key.modifiers.is_empty() => {
                self.prefix = Some(prefix);
                self.status = format!(
                    "{prefix}… minute: m · hour: h · day: d · month: M · year: y · tagged: t (Esc cancels)"
                );
            }
            KeyCode::Char('c') if key.modifiers.is_empty() => return self.collapse_burst(),
            KeyCode::Char('C') if key.modifiers.difference(KeyModifiers::SHIFT).is_empty() => {
                self.form(FormKind::Burst);
            }
            KeyCode::Char(number @ '1'..='9')
                if key.modifiers.difference(KeyModifiers::SHIFT).is_empty() =>
            {
                let active = (number as u8 - b'1') as usize;
                if active < self.session.views.len() && active != self.session.active {
                    return vec![self.switch(active)];
                }
            }
            KeyCode::Char('g') if key.modifiers.is_empty() && key.kind == KeyEventKind::Press => {
                self.form(FormKind::Goto);
            }
            KeyCode::Char('m' | '\'') => {
                if let KeyCode::Char(c) = key.code {
                    self.prefix = Some(c);
                    self.status = format!("{c}…");
                }
            }
            KeyCode::Char('d') if key.modifiers.difference(KeyModifiers::SHIFT).is_empty() => {
                self.prefix = Some('d');
                self.status = DELETE_MARK_PROMPT.into();
            }
            KeyCode::Char('z') if key.modifiers.difference(KeyModifiers::SHIFT).is_empty() => {
                self.prefix = Some('z');
                self.status = "z… top: t/Enter · middle: z/. · bottom: b/- (Esc cancels)".into();
            }
            KeyCode::Char('G') => return vec![self.load(Load::Last, None, None, true)],
            KeyCode::Up => return self.scroll(-1, true).into_iter().collect(),
            KeyCode::Down => return self.scroll(1, true).into_iter().collect(),
            KeyCode::PageUp => {
                return self
                    .scroll(-(self.height as isize), false)
                    .into_iter()
                    .collect();
            }
            KeyCode::PageDown => {
                return self
                    .scroll(self.height as isize, false)
                    .into_iter()
                    .collect();
            }
            KeyCode::Char('j' | 'k') => {
                return self
                    .move_selection(key.code == KeyCode::Char('k'))
                    .into_iter()
                    .collect();
            }
            KeyCode::Char(' ') if key.modifiers.is_empty() && key.kind == KeyEventKind::Press => {
                self.toggle_record_selection();
            }
            KeyCode::Tab => {
                if let Some(tag) = self.assessment_target().map(|row| row.tag.next()) {
                    return self.apply_tag(Some(tag));
                }
                self.status = "Expand the burst to assess an individual record".into();
            }
            KeyCode::Enter | KeyCode::F(3) => {
                if matches!(self.selected(), Some(DisplayRow::Burst(_))) {
                    if self.pending.is_none() {
                        return self.expand_burst().into_iter().collect();
                    }
                    return vec![];
                }
                if let Some(row) = self.selected_event() {
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
            KeyCode::Char('u') if key.modifiers.is_empty() => {
                let mut view = self.requested_view().clone();
                if view.predicate.exclude.pop().is_some() {
                    return vec![self.reload_filters(view)];
                }
                self.status = "No exclusions to remove".into();
            }
            KeyCode::Char('X') => self.form(FormKind::Directory),
            KeyCode::Char('x') => {
                let paths: BTreeSet<_> = if self.selected_records.is_empty() {
                    self.selected_event()
                        .into_iter()
                        .map(|row| row.path.as_str())
                        .collect()
                } else {
                    self.selected_records
                        .values()
                        .map(|row| row.path.as_str())
                        .collect()
                };
                if !paths.is_empty() {
                    let paths: Vec<_> = paths.into_iter().map(regex::escape).collect();
                    let pattern = if paths.len() == 1 {
                        format!("^{}$", paths[0])
                    } else {
                        format!("^(?:{})$", paths.join("|"))
                    };
                    let mut view = self.requested_view().clone();
                    if !view.predicate.exclude.contains(&pattern) {
                        view.predicate.exclude.push(pattern);
                        return vec![self.reload_filters(view)];
                    }
                }
            }
            _ => {}
        }
        vec![]
    }
}

// Evidence paths can use a different platform's separators from this machine.
fn directory_root_len(path: &str) -> usize {
    let bytes = path.as_bytes();
    if bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && matches!(bytes[2], b'/' | b'\\')
    {
        3
    } else if path.starts_with("\\\\") || path.starts_with("//") {
        let mut parts = path[2..].split_inclusive(['/', '\\']);
        let server = parts.next().unwrap_or("");
        let share = parts.next().unwrap_or("").trim_end_matches(['/', '\\']);
        2 + server.len() + share.len()
    } else {
        usize::from(path.starts_with(['/', '\\']))
    }
}

fn parent_directory(path: &str) -> &str {
    let root = directory_root_len(path);
    let path = &path[..path.trim_end_matches(['/', '\\']).len().max(root)];
    if path.len() <= root && root != 0 {
        return path;
    }
    match path.rfind(['/', '\\']) {
        Some(index) => &path[..index.max(root)],
        None => ".",
    }
}

fn directory_exclusion(path: &str) -> String {
    let root = directory_root_len(path);
    let path = &path[..path.trim_end_matches(['/', '\\']).len().max(root)];
    if path == "." {
        // Match relative paths without hiding Unix, drive, or UNC absolute paths.
        r"^(?:[^/\\A-Za-z]|[A-Za-z](?:[^:]|$)|[A-Za-z]:(?:[^/\\]|$))".into()
    } else if path.ends_with(['/', '\\']) {
        format!("^{}", regex::escape(path))
    } else {
        format!("^{}(?:[/\\\\]|$)", regex::escape(path))
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
fn set_mouse_capture(enabled: bool) -> io::Result<()> {
    if enabled {
        execute!(io::stdout(), EnableMouseCapture)?;
        // Keep SGR coordinates, including columns beyond 223, but only report
        // motion while a button is held to limit traffic through tmux and SSH.
        #[cfg(unix)]
        execute!(
            io::stdout(),
            crossterm::style::Print("\x1b[?1003l\x1b[?1002h")
        )?;
    } else {
        execute!(io::stdout(), DisableMouseCapture)?;
    }
    Ok(())
}

fn restore_terminal() {
    let _ = execute!(
        io::stdout(),
        DisableMouseCapture,
        LeaveAlternateScreen,
        crossterm::cursor::Show
    );
    let _ = disable_raw_mode();
}
impl Drop for TerminalGuard {
    fn drop(&mut self) {
        restore_terminal();
    }
}
pub fn run(workers: &Workers, theme: crate::theme::Theme) -> Result<()> {
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        bail!("The TUI requires a terminal; use --index-only for noninteractive indexing");
    }
    enable_raw_mode()?;
    let _guard = TerminalGuard;
    execute!(io::stdout(), EnterAlternateScreen)?;
    set_mouse_capture(true)?;
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        restore_terminal();
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
                Event::Mouse(mouse)
                    if app.mouse_enabled
                        && matches!(
                            mouse.kind,
                            MouseEventKind::Down(_)
                                | MouseEventKind::Drag(_)
                                | MouseEventKind::ScrollUp
                                | MouseEventKind::ScrollDown
                        ) =>
                {
                    actions.extend(app.mouse(mouse));
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
                Action::MouseCapture(enabled) => set_mouse_capture(enabled)?,
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
