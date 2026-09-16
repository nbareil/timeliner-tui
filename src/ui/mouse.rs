use super::*;
use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::Position;

const WHEEL_ROWS: usize = 3;
const DOUBLE_CLICK: Duration = Duration::from_millis(400);

#[derive(Clone, Copy)]
pub(super) enum MouseTarget {
    Key(KeyCode),
    Tab(usize),
    Field(usize),
}

#[derive(Default)]
pub(super) struct MouseState {
    pub timeline: Rect,
    pub modal: Option<Rect>,
    pub targets: Vec<(Rect, MouseTarget)>,
    pub mark_scroll: usize,
    pub mark_scroll_max: usize,
    last_click: Option<(EventKey, Position, Instant)>,
}

impl MouseState {
    pub fn reset_click(&mut self) {
        self.last_click = None;
    }

    pub fn target(&mut self, area: Rect, target: MouseTarget) {
        if !area.is_empty() {
            self.targets.push((area, target));
        }
    }
}

impl App {
    fn mouse_cancel_prefix(&mut self) {
        if self.prefix.take().is_some() && self.pending.is_none() {
            self.status = "Ready".into();
        }
    }

    pub fn mouse(&mut self, event: MouseEvent) -> Vec<Action> {
        self.mouse_at(event, Instant::now())
    }

    fn mouse_at(&mut self, event: MouseEvent, now: Instant) -> Vec<Action> {
        if !self.mouse_enabled {
            return vec![];
        }
        let point = Position::new(event.column, event.row);
        if !event.modifiers.is_empty() || self.modal.is_some() != self.mouse_state.modal.is_some() {
            self.mouse_state.last_click = None;
            return vec![];
        }
        match event.kind {
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
                self.mouse_state.last_click = None;
                let down = event.kind == MouseEventKind::ScrollDown;
                if let Some(popup) = self.mouse_state.modal {
                    if !popup.contains(point) {
                        return vec![];
                    }
                    self.mouse_cancel_prefix();
                    match &mut self.modal {
                        Some(Modal::Text(_, _, scroll)) => {
                            *scroll = if down {
                                scroll
                                    .saturating_add(WHEEL_ROWS)
                                    .min(self.detail_scroll_max)
                            } else {
                                scroll.saturating_sub(WHEEL_ROWS)
                            };
                        }
                        Some(Modal::Form(form)) => {
                            form.active = if down {
                                (form.active + 1).min(form.fields.len().saturating_sub(1))
                            } else {
                                form.active.saturating_sub(1)
                            };
                        }
                        Some(Modal::Marks) => {
                            self.mouse_state.mark_scroll = if down {
                                (self.mouse_state.mark_scroll + WHEEL_ROWS)
                                    .min(self.mouse_state.mark_scroll_max)
                            } else {
                                self.mouse_state.mark_scroll.saturating_sub(WHEEL_ROWS)
                            };
                        }
                        _ => {}
                    }
                } else if self.ready && self.mouse_state.timeline.contains(point) {
                    self.mouse_cancel_prefix();
                    let delta = if down {
                        WHEEL_ROWS as isize
                    } else {
                        -(WHEEL_ROWS as isize)
                    };
                    return self.scroll(delta, true).into_iter().collect();
                }
            }
            MouseEventKind::Down(MouseButton::Left | MouseButton::Right)
            | MouseEventKind::Drag(MouseButton::Left) => {
                if self.modal.is_none() && self.ready && self.mouse_state.timeline.contains(point) {
                    let slot = (event.row - self.mouse_state.timeline.y) as usize;
                    let Some(focus) = self.rows.get(self.top + slot).map(|row| row.key()) else {
                        self.mouse_state.last_click = None;
                        return vec![];
                    };
                    self.mouse_cancel_prefix();
                    self.view_mut().selected = slot;
                    self.remember();
                    let double = event.kind == MouseEventKind::Down(MouseButton::Left)
                        && self
                            .mouse_state
                            .last_click
                            .is_some_and(|(key, cell, time)| {
                                key == focus
                                    && cell == point
                                    && now.duration_since(time) <= DOUBLE_CLICK
                            });
                    let inspect = double || event.kind == MouseEventKind::Down(MouseButton::Right);
                    self.mouse_state.last_click =
                        if !inspect && event.kind == MouseEventKind::Down(MouseButton::Left) {
                            Some((focus, point, now))
                        } else {
                            None
                        };
                    if inspect {
                        return self.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
                    }
                    return vec![];
                }
                self.mouse_state.last_click = None;
                if event.kind != MouseEventKind::Down(MouseButton::Left) {
                    return vec![];
                }
                let target = self
                    .mouse_state
                    .targets
                    .iter()
                    .rev()
                    .find(|(area, _)| area.contains(point))
                    .map(|(_, target)| *target);
                let Some(target) = target else { return vec![] };
                self.mouse_cancel_prefix();
                match target {
                    MouseTarget::Key(code) => {
                        return self.key(KeyEvent::new(code, KeyModifiers::NONE));
                    }
                    MouseTarget::Tab(index)
                        if self.ready
                            && index < self.session.views.len()
                            && index != self.session.active =>
                    {
                        return vec![self.switch(index)];
                    }
                    MouseTarget::Field(index) => {
                        if let Some(Modal::Form(form)) = &mut self.modal
                            && index < form.fields.len()
                        {
                            form.active = index;
                        }
                    }
                    _ => {}
                }
            }
            // Release reports are part of a double click. Hover and horizontal
            // wheel events have no action and must not change the selection.
            _ => {}
        }
        vec![]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app() -> App {
        App {
            ready: true,
            rows: (0..3)
                .map(|record| {
                    DisplayRow::Event(Row {
                        key: EventKey { epoch: 1, record },
                        macb: 15,
                        host: "test".into(),
                        path: format!("/test/{record}"),
                        tag: Tag::None,
                        automatic: false,
                        ioc_match: false,
                    })
                })
                .collect(),
            mouse_state: MouseState {
                timeline: Rect::new(0, 5, 100, 10),
                ..MouseState::default()
            },
            ..App::default()
        }
    }

    fn event(kind: MouseEventKind, row: u16) -> MouseEvent {
        MouseEvent {
            kind,
            column: 40,
            row,
            modifiers: KeyModifiers::NONE,
        }
    }

    #[test]
    fn double_click_requires_two_presses_on_the_same_event_within_the_interval() {
        let mut app = app();
        let now = Instant::now();
        let down = event(MouseEventKind::Down(MouseButton::Left), 6);
        assert!(app.mouse_at(down, now).is_empty());
        assert!(
            app.mouse_at(event(MouseEventKind::Up(MouseButton::Left), 6), now)
                .is_empty()
        );
        assert!(matches!(
            app.mouse_at(down, now + Duration::from_millis(100))
                .as_slice(),
            [Action::Command(Command::Detail(1, _))]
        ));
        // The third press starts a fresh click sequence.
        assert!(
            app.mouse_at(down, now + Duration::from_millis(200))
                .is_empty()
        );
        assert!(
            app.mouse_at(down, now + Duration::from_millis(601))
                .is_empty()
        );
        assert!(
            app.mouse_at(
                event(MouseEventKind::Down(MouseButton::Left), 7),
                now + Duration::from_millis(650)
            )
            .is_empty()
        );
    }

    #[test]
    fn keyboard_and_wheel_input_break_a_double_click_sequence() {
        let mut app = app();
        let now = Instant::now();
        let down = event(MouseEventKind::Down(MouseButton::Left), 6);
        app.mouse_at(down, now);
        app.key(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE));
        assert!(
            app.mouse_at(down, now + Duration::from_millis(100))
                .is_empty()
        );
        app.mouse_at(
            event(MouseEventKind::ScrollUp, 6),
            now + Duration::from_millis(150),
        );
        assert!(
            app.mouse_at(down, now + Duration::from_millis(200))
                .is_empty()
        );
    }
}
