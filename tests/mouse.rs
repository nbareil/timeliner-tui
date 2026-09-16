use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{Terminal, backend::TestBackend};
use timeliner_tui::{
    model::{Bookmark, EventKey, Row, Session, Tag, View},
    ui::{Action, App},
    worker::{Command, Message},
};

fn rows(start: i64, count: usize) -> Vec<Row> {
    (start..start + count as i64)
        .map(|id| Row {
            key: EventKey {
                epoch: 1_600_000_000 + id,
                record: id,
            },
            macb: 15,
            host: "mouse-host".into(),
            path: format!("/mouse/event-{id:04}"),
            tag: Tag::None,
            automatic: false,
            ioc_match: false,
        })
        .collect()
}

fn setup(width: u16, height: u16, count: usize) -> (App, Terminal<TestBackend>) {
    let mut app = App::default();
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    draw(&mut app, &mut terminal);
    let actions = app.message(Message::Ready(Session::default(), String::new()));
    let [Action::Page(request)] = actions.as_slice() else {
        panic!()
    };
    app.message(Message::Page(request.generation, rows(0, count)));
    draw(&mut app, &mut terminal);
    (app, terminal)
}

fn draw(app: &mut App, terminal: &mut Terminal<TestBackend>) {
    terminal.draw(|frame| app.draw(frame)).unwrap();
}

fn key(app: &mut App, code: KeyCode) -> Vec<Action> {
    app.key(KeyEvent::new(code, KeyModifiers::NONE))
}

fn mouse(app: &mut App, kind: MouseEventKind, column: u16, row: u16) -> Vec<Action> {
    app.mouse(MouseEvent {
        kind,
        column,
        row,
        modifiers: KeyModifiers::NONE,
    })
}

fn click(app: &mut App, column: u16, row: u16) -> Vec<Action> {
    let actions = mouse(app, MouseEventKind::Down(MouseButton::Left), column, row);
    mouse(app, MouseEventKind::Up(MouseButton::Left), column, row);
    actions
}

fn locate(terminal: &Terminal<TestBackend>, text: &str) -> (u16, u16) {
    let buffer = terminal.backend().buffer();
    for y in 0..buffer.area.height {
        for x in 0..buffer.area.width {
            let tail = (x..buffer.area.width)
                .map(|x| buffer[(x, y)].symbol())
                .collect::<String>();
            if tail.starts_with(text) {
                return (x, y);
            }
        }
    }
    let screen = (0..buffer.area.height)
        .map(|y| {
            (0..buffer.area.width)
                .map(|x| buffer[(x, y)].symbol())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n");
    panic!("Missing visible control: {text}\n{screen}");
}

fn click_text(app: &mut App, terminal: &Terminal<TestBackend>, text: &str) -> Vec<Action> {
    let (x, y) = locate(terminal, text);
    click(app, x, y)
}

fn selected(app: &App) -> i64 {
    app.rows[app.top + app.session.views[app.session.active].selected]
        .key
        .record
}

#[test]
fn clicks_and_drags_select_rows_and_right_click_opens_the_clicked_record() {
    let (mut app, mut terminal) = setup(300, 19, 128);
    assert!(click(&mut app, 250, 8).is_empty());
    assert_eq!(selected(&app), 3);
    assert!(mouse(&mut app, MouseEventKind::Drag(MouseButton::Left), 250, 10).is_empty());
    assert_eq!(selected(&app), 5);
    assert!(matches!(
        mouse(&mut app, MouseEventKind::Down(MouseButton::Right), 250, 7).as_slice(),
        [Action::Command(Command::Detail(2, _))]
    ));
    assert_eq!(selected(&app), 2);
    draw(&mut app, &mut terminal);
    let buffer = terminal.backend().buffer();
    assert_eq!(buffer[(0, 7)].symbol(), "▌");
}

#[test]
fn wheel_scrolls_around_the_selection_including_page_loads() {
    let (mut app, _terminal) = setup(100, 19, 128);
    click(&mut app, 45, 12);
    mouse(&mut app, MouseEventKind::ScrollDown, 45, 12);
    assert_eq!(app.top, 3);
    assert_eq!(selected(&app), 7);
    assert_eq!(app.session.views[0].selected, 4);
    mouse(&mut app, MouseEventKind::ScrollUp, 45, 12);
    assert_eq!(app.top, 0);
    assert_eq!(selected(&app), 7);
    app.top = 118;
    app.session.views[0].selected = 7;
    let actions = mouse(&mut app, MouseEventKind::ScrollDown, 45, 12);
    let [Action::Page(request)] = actions.as_slice() else {
        panic!()
    };
    assert!(!request.reverse);
    app.message(Message::Page(request.generation, rows(128, request.limit)));
    assert_eq!(app.top, 121);
    assert_eq!(selected(&app), 125);
    assert_eq!(app.session.views[0].selected, 4);
}

#[test]
fn unused_mouse_events_modifiers_headers_and_blank_rows_do_not_change_selection() {
    let (mut app, _terminal) = setup(100, 19, 3);
    click(&mut app, 45, 6);
    for kind in [
        MouseEventKind::Moved,
        MouseEventKind::Up(MouseButton::Left),
        MouseEventKind::Down(MouseButton::Middle),
        MouseEventKind::ScrollLeft,
        MouseEventKind::ScrollRight,
    ] {
        assert!(mouse(&mut app, kind, 45, 5).is_empty());
        assert_eq!(selected(&app), 1);
    }
    for (x, y) in [(45, 4), (45, 10), (100, 6), (45, 30)] {
        assert!(click(&mut app, x, y).is_empty());
        assert_eq!(selected(&app), 1);
    }
    for modifiers in [
        KeyModifiers::SHIFT,
        KeyModifiers::CONTROL,
        KeyModifiers::ALT,
    ] {
        assert!(
            app.mouse(MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: 45,
                row: 5,
                modifiers,
            })
            .is_empty()
        );
        assert_eq!(selected(&app), 1);
    }
}

#[test]
fn tabs_overflow_controls_and_function_buttons_follow_the_rendered_layout() {
    for width in [42, 100, 300] {
        let (mut app, mut terminal) = setup(width, 24, 128);
        app.session.views = (1..=12)
            .map(|i| View {
                name: format!("View {i}"),
                ..View::default()
            })
            .collect();
        draw(&mut app, &mut terminal);
        let actions = click_text(&mut app, &terminal, "2 View 2");
        assert!(matches!(actions.as_slice(), [Action::Page(_)]));
        assert_eq!(app.session.active, 1);
        // A click cancels the pending z prefix before dispatching the button.
        key(&mut app, KeyCode::Char('z'));
        draw(&mut app, &mut terminal);
        click_text(&mut app, &terminal, "F1 ");
        draw(&mut app, &mut terminal);
        locate(&terminal, "Keyboard shortcuts");
        click_text(&mut app, &terminal, "Esc close");
        draw(&mut app, &mut terminal);
        if width < 300 {
            click_text(&mut app, &terminal, ">");
            assert!(app.session.active > 1);
            draw(&mut app, &mut terminal);
            click_text(&mut app, &terminal, "<");
            assert!(app.session.active < 11);
        } else {
            click_text(&mut app, &terminal, "12 View 12");
            assert_eq!(app.session.active, 11);
        }
    }
}

#[test]
fn resized_unicode_tabs_keep_their_correct_click_targets() {
    let (mut app, mut terminal) = setup(100, 24, 128);
    app.session.views[0].name = "事件 🦀 e\u{301}".into();
    draw(&mut app, &mut terminal);
    click_text(&mut app, &terminal, "2 IOC");
    assert_eq!(app.session.active, 1);
    terminal.backend_mut().resize(40, 24);
    terminal
        .resize(ratatui::layout::Rect::new(0, 0, 40, 24))
        .unwrap();
    draw(&mut app, &mut terminal);
    click_text(&mut app, &terminal, "1 ");
    assert_eq!(app.session.active, 0);
}

#[test]
fn dialogs_receive_wheel_input_and_block_the_timeline_and_function_bar() {
    let (mut app, mut terminal) = setup(100, 24, 128);
    app.message(Message::Detail(
        (0..100).map(|i| format!("Detail line {i:03}\n")).collect(),
    ));
    draw(&mut app, &mut terminal);
    let (x, y) = locate(&terminal, "Detail line 000");
    mouse(&mut app, MouseEventKind::ScrollDown, x, y);
    draw(&mut app, &mut terminal);
    locate(&terminal, "Detail line 003");
    assert_eq!(app.top, 0);
    assert_eq!(selected(&app), 0);
    // Backdrop clicks and scrolling do not fall through to the timeline.
    click(&mut app, 0, 10);
    mouse(&mut app, MouseEventKind::ScrollDown, 0, 10);
    click_text(&mut app, &terminal, "F6");
    assert_eq!(app.session.views.len(), 2);
    assert_eq!(app.top, 0);
    draw(&mut app, &mut terminal);
    locate(&terminal, "Detail line 003");
    mouse(&mut app, MouseEventKind::ScrollUp, x, y);
    draw(&mut app, &mut terminal);
    locate(&terminal, "Detail line 000");
    click_text(&mut app, &terminal, "Esc close");
    draw(&mut app, &mut terminal);
    click(&mut app, 45, 8);
    assert_eq!(selected(&app), 3);
}

#[test]
fn form_fields_and_apply_button_work_with_the_mouse() {
    let (mut app, mut terminal) = setup(100, 30, 128);
    key(&mut app, KeyCode::F(5));
    draw(&mut app, &mut terminal);
    click_text(&mut app, &terminal, "Host");
    for ch in "mouse-host".chars() {
        key(&mut app, KeyCode::Char(ch));
    }
    draw(&mut app, &mut terminal);
    let actions = click_text(&mut app, &terminal, "Enter apply");
    let [Action::Page(request)] = actions.as_slice() else {
        panic!()
    };
    assert_eq!(request.view.predicate.hosts, ["mouse-host"]);
    assert!(request.view.predicate.include.is_empty());
}

#[test]
fn tag_and_view_menu_clicks_dispatch_the_visible_choice() {
    let (mut app, mut terminal) = setup(100, 30, 128);
    click(&mut app, 45, 8);
    key(&mut app, KeyCode::F(2));
    draw(&mut app, &mut terminal);
    assert!(matches!(
        click_text(&mut app, &terminal, "MALICIOUS").as_slice(),
        [Action::Command(Command::Tag(3, Some(Tag::Malicious)))]
    ));
    key(&mut app, KeyCode::F(7));
    draw(&mut app, &mut terminal);
    let actions = click_text(&mut app, &terminal, "All tagged");
    let [Action::Page(request)] = actions.as_slice() else {
        panic!()
    };
    assert_eq!(request.view.predicate.tags, Some(vec![]));
    assert_eq!(app.session.views.len(), 3);
}

#[test]
fn wheel_scrolled_wrapped_bookmarks_jump_to_the_clicked_event() {
    let (mut app, mut terminal) = setup(45, 18, 128);
    app.session.bookmarks = ('a'..='z')
        .zip(&app.rows)
        .map(|(name, row)| Bookmark { name, key: row.key })
        .collect();
    key(&mut app, KeyCode::F(8));
    draw(&mut app, &mut terminal);
    // Scroll to the end of the wrapped list in a narrow pane.
    for _ in 0..40 {
        mouse(&mut app, MouseEventKind::ScrollDown, 20, 9);
        draw(&mut app, &mut terminal);
    }
    // The record number wraps onto a continuation line in this narrow dialog.
    click_text(&mut app, &terminal, "25 ");
    assert_eq!(selected(&app), 25);
    draw(&mut app, &mut terminal);
    assert!(matches!(
        key(&mut app, KeyCode::Enter).as_slice(),
        [Action::Command(Command::Detail(25, _))]
    ));
}

#[test]
fn tiny_terminals_and_unready_or_empty_views_ignore_row_clicks() {
    for (width, height) in [(1, 1), (100, 1), (8, 4), (25, 10), (100, 24)] {
        let (mut app, mut terminal) = setup(width, height, 0);
        for (x, y) in [(0, 0), (width - 1, height - 1), (width / 2, height / 2)] {
            mouse(&mut app, MouseEventKind::Down(MouseButton::Right), x, y);
            mouse(&mut app, MouseEventKind::ScrollDown, x, y);
        }
        assert_eq!(app.session.views[0].selected, 0);
        for function in [1, 2, 4, 5, 7, 8, 9] {
            key(&mut app, KeyCode::F(function));
            draw(&mut app, &mut terminal);
            key(&mut app, KeyCode::Esc);
        }
    }
    let mut app = App::default();
    assert!(click(&mut app, 20, 6).is_empty());
    assert!(mouse(&mut app, MouseEventKind::ScrollDown, 20, 6).is_empty());
}
