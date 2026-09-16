use super::*;
use crate::theme::{Palette, blend};

fn fill(frame: &mut Frame, area: Rect, style: Style) {
    frame.render_widget(Block::default().style(style), area);
}

fn label(text: impl Into<String>, color: Color) -> Span<'static> {
    Span::styled(text.into(), Style::default().fg(color))
}

fn keycap(key: &str, p: Palette) -> Span<'static> {
    Span::styled(
        format!(" {key} "),
        Style::default()
            .fg(p.accent)
            .bg(p.elevated)
            .add_modifier(Modifier::BOLD),
    )
}

impl App {
    fn draw_tabs(&self, frame: &mut Frame, area: Rect, p: Palette) {
        if area.is_empty() {
            return;
        }
        fill(frame, area, p.base().bg(p.surface));
        let masthead = Rect::new(area.x, area.y, area.width, 1);
        let brand_width = if area.width >= 85 { 20 } else { 0 };
        let theme_width = if area.width >= 45 { 14 } else { 0 };
        let cells = Layout::horizontal([
            Constraint::Length(brand_width),
            Constraint::Fill(1),
            Constraint::Length(theme_width),
        ])
        .split(masthead);
        if brand_width > 0 {
            let mut brand = vec![label("  ◆ ", p.accent)];
            brand.extend("TIMELINER".chars().enumerate().map(|(i, c)| {
                Span::styled(
                    c.to_string(),
                    Style::default()
                        .fg(blend(p.accent, p.violet, i as u16, 8))
                        .add_modifier(Modifier::BOLD),
                )
            }));
            frame.render_widget(Paragraph::new(Line::from(brand)), cells[0]);
        }
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                label(
                    format!(
                        " Tab {}/{}  ",
                        self.session.active + 1,
                        self.session.views.len()
                    ),
                    p.text,
                ),
                label("1–9 select · gt/gT switch", p.muted),
            ])),
            cells[1],
        );
        if theme_width > 0 {
            frame.render_widget(
                Paragraph::new(Line::from(vec![
                    keycap("F11", p),
                    label(format!(" {} ", self.session.theme.label()), p.muted),
                ]))
                .alignment(Alignment::Right),
                cells[2],
            );
        }
        if area.height < 2 {
            return;
        }
        let strip = Rect::new(area.x, area.y + 1, area.width, 1);
        let columns = Layout::horizontal([
            Constraint::Length(2),
            Constraint::Fill(1),
            Constraint::Length(2),
        ])
        .split(strip);
        let active = self.session.active;
        let titles = self
            .session
            .views
            .iter()
            .enumerate()
            .map(|(index, view)| {
                Line::from(format!(
                    "{}{} {}",
                    if index == active { "● " } else { "" },
                    index + 1,
                    display_text(&view.name)
                ))
            })
            .collect::<Vec<_>>();
        let capacity = columns[1].width as usize;
        let mut start = active;
        let mut end = active + 1;
        let mut used = titles[active].width() + 2;
        while start > 0 && used + titles[start - 1].width() + 3 <= capacity {
            start -= 1;
            used += titles[start].width() + 3;
        }
        while end < titles.len() && used + titles[end].width() + 3 <= capacity {
            used += titles[end].width() + 3;
            end += 1;
        }
        if start > 0 {
            frame.render_widget(
                Paragraph::new("<").style(p.base().fg(p.accent).bg(p.surface)),
                columns[0],
            );
        }
        if end < titles.len() {
            frame.render_widget(
                Paragraph::new(">")
                    .alignment(Alignment::Right)
                    .style(p.base().fg(p.accent).bg(p.surface)),
                columns[2],
            );
        }
        frame.render_widget(
            Tabs::new(titles[start..end].iter().cloned())
                .select(active - start)
                .divider(" ")
                .style(p.base().fg(p.muted).bg(p.surface))
                .highlight_style(
                    p.base()
                        .bg(p.selected)
                        .fg(p.accent)
                        .add_modifier(Modifier::BOLD),
                ),
            columns[1],
        );
    }

    fn draw_filters(&self, frame: &mut Frame, area: Rect, p: Palette) {
        let tz: chrono_tz::Tz = self.session.timezone.parse().unwrap_or(chrono_tz::UTC);
        let stamp = |epoch: Option<i64>, empty: &str| {
            epoch
                .and_then(|e| tz.timestamp_opt(e, 0).single())
                .map(|d| d.format("%Y-%m-%d %H:%M:%S").to_string())
                .unwrap_or_else(|| empty.to_string())
        };
        let view = self.view();
        let timeframe = Line::from(vec![
            label("  TIME  ", p.subtle),
            label(stamp(view.after, "Earliest"), p.text),
            label("  →  ", p.accent),
            label(stamp(view.before, "Latest"), p.text),
            label(format!("   {}   ", self.session.timezone), p.teal),
            label(format!("{}   ", macb(view.time_mask)), p.muted),
            label("F4 edit", p.subtle),
        ]);
        let hosts = if view.predicate.hosts.is_empty() {
            "All hosts".into()
        } else {
            display_text(&view.predicate.hosts.join(", "))
        };
        let query = if view.predicate.include.is_empty() {
            "All paths".into()
        } else {
            display_text(&view.predicate.include)
        };
        let filters = Line::from(vec![
            label("  FIND  ", p.subtle),
            label(query, p.text),
            label("   ·   ", p.border),
            label(hosts, p.teal),
            label("   ·   ", p.border),
            label(
                format!("{} exclusions", view.predicate.exclude.len()),
                p.muted,
            ),
            label("   F5 edit", p.subtle),
        ]);
        frame.render_widget(
            Paragraph::new(vec![timeframe, filters]).style(p.base()),
            area,
        );
    }

    fn draw_timeline(&self, frame: &mut Frame, body: Rect, p: Palette) {
        let tz: chrono_tz::Tz = self.session.timezone.parse().unwrap_or(chrono_tz::UTC);
        let host_width = match body.width {
            0..70 => 10,
            70..140 => 18,
            _ => 26,
        };
        let widths = [
            Constraint::Length(1),
            Constraint::Length(19),
            Constraint::Length(host_width),
            Constraint::Length(4),
            Constraint::Fill(1),
            Constraint::Length(10),
        ];
        let header = Rect::new(body.x, body.y, body.width, body.height.min(1));
        fill(frame, header, p.base().bg(p.elevated));
        let columns = Layout::horizontal(widths).spacing(1).split(header);
        for (cell, name) in
            columns
                .iter()
                .zip(["", "TIMESTAMP", "HOST", "MACB", "PATH", "ASSESSMENT"])
        {
            frame.render_widget(
                Paragraph::new(name)
                    .style(p.base().fg(p.muted).bg(p.elevated))
                    .alignment(if name == "ASSESSMENT" {
                        Alignment::Right
                    } else {
                        Alignment::Left
                    }),
                *cell,
            );
        }
        for (slot, row) in self
            .rows
            .iter()
            .skip(self.top)
            .take(self.height)
            .enumerate()
        {
            if slot + 1 >= body.height as usize {
                break;
            }
            let area = Rect::new(body.x, body.y + 1 + slot as u16, body.width, 1);
            let selected = slot == self.view().selected;
            let background = if selected {
                p.selected
            } else if (self.top + slot).is_multiple_of(2) {
                p.surface
            } else {
                p.alternate
            };
            let base = p.base().bg(background);
            fill(frame, area, base);
            let cells = Layout::horizontal(widths).spacing(1).split(area);
            let bookmarked = self
                .session
                .bookmarks
                .iter()
                .any(|mark| mark.key == row.key);
            if bookmarked || selected {
                frame.render_widget(
                    Paragraph::new(if bookmarked { "●" } else { "▌" }).style(base.fg(p.accent)),
                    cells[0],
                );
            }
            let timestamp = tz
                .timestamp_opt(row.key.epoch, 0)
                .single()
                .map(|d| d.format("%Y-%m-%d %H:%M:%S").to_string())
                .unwrap_or_else(|| row.key.epoch.to_string());
            frame.render_widget(Paragraph::new(timestamp).style(base.fg(p.muted)), cells[1]);
            frame.render_widget(
                Paragraph::new(display_text(&row.host)).style(base.fg(p.host(&row.host))),
                cells[2],
            );
            let flags = ['m', 'a', 'c', 'b']
                .into_iter()
                .enumerate()
                .map(|(i, c)| {
                    let enabled = row.macb & (1 << i) != 0;
                    label(
                        if enabled { c.to_string() } else { "·".into() },
                        if enabled {
                            [p.accent, p.teal, p.amber, p.violet][i]
                        } else {
                            p.subtle
                        },
                    )
                })
                .collect::<Vec<_>>();
            frame.render_widget(Paragraph::new(Line::from(flags)).style(base), cells[3]);
            let path = display_text(&row.path);
            let split = path.rfind(['/', '\\']).map_or(0, |i| i + 1);
            let path_color = if row.ioc_match { p.amber } else { p.text };
            let path_style = if selected {
                Style::default().fg(path_color).add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(path_color)
            };
            frame.render_widget(
                Paragraph::new(Line::from(vec![
                    label(
                        path[..split].to_string(),
                        if row.ioc_match { p.amber } else { p.muted },
                    ),
                    Span::styled(path[split..].to_string(), path_style),
                ]))
                .style(base),
                cells[4],
            );
            if row.tag != Tag::None {
                let (fg, bg) = match row.tag {
                    Tag::Suspicious => (p.amber, p.amber_soft),
                    Tag::Ioc => (p.violet, p.violet_soft),
                    Tag::Malicious => (p.rose, p.rose_soft),
                    Tag::None => unreachable!(),
                };
                frame.render_widget(
                    Paragraph::new(row.tag.label())
                        .alignment(Alignment::Right)
                        .style(base.fg(fg).bg(bg).add_modifier(Modifier::BOLD)),
                    cells[5],
                );
            }
        }
        if self.rows.is_empty() && body.height > 3 {
            let text = if self.ready && self.pending.is_none() {
                vec![
                    Line::from(Span::styled(
                        "No matching events",
                        Style::default().fg(p.text).add_modifier(Modifier::BOLD),
                    )),
                    Line::from(""),
                    Line::from("Adjust the timeframe with F4 or filters with F5."),
                ]
            } else {
                vec![
                    Line::from(Span::styled(
                        "Preparing your timeline",
                        Style::default().fg(p.accent).add_modifier(Modifier::BOLD),
                    )),
                    Line::from(""),
                    Line::from("Progress appears below. Esc cancels pending work."),
                ]
            };
            frame.render_widget(
                Paragraph::new(text)
                    .alignment(Alignment::Center)
                    .style(p.base().fg(p.muted)),
                Rect::new(body.x, body.y + body.height / 2 - 1, body.width, 3),
            );
        }
    }

    fn draw_status(&self, frame: &mut Frame, area: Rect, p: Palette) {
        let busy = !self.ready || self.pending.is_some();
        let state = Line::from(vec![
            label(
                if busy { "  ◌ " } else { "  ● " },
                if busy { p.accent } else { p.teal },
            ),
            label(display_text(&self.status), p.muted),
        ]);
        let hints = Line::from(vec![
            label("  ↑↓ ", p.accent),
            label("scroll   ", p.muted),
            label("j/k ", p.accent),
            label("select   ", p.muted),
            label("TAB ", p.violet),
            label("tag   ", p.muted),
            label("ENTER ", p.accent),
            label("inspect   ", p.muted),
            label("gt/gT ", p.accent),
            label("tabs   ", p.muted),
            label("Esc ", p.muted),
            label("cancel", p.subtle),
        ]);
        frame.render_widget(Paragraph::new(vec![state, hints]).style(p.base()), area);
    }

    fn draw_keybar(&self, frame: &mut Frame, area: Rect, p: Palette) {
        if area.is_empty() {
            return;
        }
        let actions = [
            "Help", "Tags", "Detail", "Time", "Filter", "Fork", "Views", "Marks", "Export", "Quit",
        ];
        fill(frame, area, p.base().bg(p.surface));
        let per_row = if area.height > 1 { 5 } else { 10 };
        for (row, group) in actions.chunks(per_row).enumerate() {
            let strip = Rect::new(area.x, area.y + row as u16, area.width, 1);
            let cells = Layout::horizontal(vec![Constraint::Fill(1); group.len()]).split(strip);
            for (i, action) in group.iter().enumerate() {
                let number = row * per_row + i + 1;
                frame.render_widget(
                    Paragraph::new(Line::from(vec![
                        keycap(&format!("F{number}"), p),
                        label(format!(" {action}"), p.text),
                    ]))
                    .style(p.base().bg(p.surface)),
                    cells[i],
                );
            }
        }
    }

    fn draw_modal(&mut self, frame: &mut Frame, p: Palette) {
        let Some(modal) = &self.modal else { return };
        let area = frame.area();
        let (title, subtitle, max_width, wanted_height) = match modal {
            Modal::Text(title, _, _) => (
                *title,
                "↑↓ / PgUp PgDn scroll",
                112,
                area.height.saturating_sub(4),
            ),
            Modal::Tags => (
                "Assess record",
                "Changes apply to every event for this record",
                82,
                17,
            ),
            Modal::Views => ("Views", "Smart views span the whole investigation", 82, 17),
            Modal::Marks => (
                "Bookmarks",
                "Jump to an exact point in the timeline",
                82,
                area.height.saturating_sub(6),
            ),
            Modal::Form(form) => (
                match form.kind {
                    FormKind::Time => "Timeframe",
                    FormKind::Filters => "Filter timeline",
                    FormKind::Export => "Export records",
                    FormKind::Reload => "IOC rules",
                    FormKind::Rename => "Rename view",
                },
                "Tab next field · Ctrl+U clear · Enter apply",
                94,
                (form.fields.len() as u16)
                    .saturating_mul(3)
                    .saturating_add(7),
            ),
        };
        let width = area.width.saturating_sub(4).min(max_width);
        let height = area.height.saturating_sub(2).min(wanted_height);
        let popup = Rect::new(
            area.x + (area.width - width) / 2,
            area.y + (area.height - height) / 2,
            width,
            height,
        );
        let shadow = Rect::new(
            popup.x + 1,
            popup.y + 1,
            popup.width.min(area.right().saturating_sub(popup.x + 1)),
            popup.height.min(area.bottom().saturating_sub(popup.y + 1)),
        );
        fill(frame, shadow, p.base().bg(p.shadow));
        frame.render_widget(Clear, popup);
        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(p.border))
            .style(p.base().bg(p.surface))
            .title_top(Line::from(Span::styled(
                format!("  {title}  "),
                Style::default().fg(p.accent).add_modifier(Modifier::BOLD),
            )))
            .title_bottom(Line::from(label(" Esc close ", p.muted)).right_aligned())
            .padding(Padding::horizontal(if width >= 50 { 2 } else { 1 }));
        let inner = block.inner(popup);
        frame.render_widget(block, popup);
        if inner.height < 2 {
            return;
        }
        frame.render_widget(
            Paragraph::new(subtitle).style(p.base().fg(p.muted).bg(p.surface)),
            Rect::new(inner.x, inner.y, inner.width, 1),
        );
        let content = Rect::new(
            inner.x,
            inner.y + 2,
            inner.width,
            inner.height.saturating_sub(2),
        );
        match modal {
            Modal::Text(title, text, scroll) => {
                let field_width = if *title == "Record inspector" {
                    text.lines()
                        .take_while(|line| !line.starts_with("Original record"))
                        .filter_map(|line| line.split_once(": "))
                        .map(|(name, _)| name.len() + 1)
                        .max()
                        .unwrap_or_default()
                } else {
                    0
                };
                let mut raw = false;
                let lines = text
                    .lines()
                    .map(|line| {
                        if line.starts_with("Original record") {
                            raw = true;
                        }
                        let safe = display_text(line);
                        if !raw && let Some((name, value)) = safe.split_once(": ") {
                            let is_time = name.ends_with("time");
                            let field = format!("{name}:");
                            Line::from(vec![
                                label(format!("{field:<field_width$} "), p.muted),
                                label(value.to_string(), if is_time { p.accent } else { p.text }),
                            ])
                        } else {
                            Line::from(label(safe, if raw { p.muted } else { p.text }))
                        }
                    })
                    .collect::<Vec<_>>();
                let paragraph = Paragraph::new(lines)
                    .wrap(Wrap { trim: false })
                    .style(p.base().bg(p.surface));
                self.detail_scroll_max = paragraph
                    .line_count(content.width)
                    .saturating_sub(content.height as usize);
                frame.render_widget(
                    paragraph.scroll((
                        (*scroll).min(self.detail_scroll_max).min(u16::MAX as usize) as u16,
                        0,
                    )),
                    content,
                );
            }
            Modal::Tags => {
                let lines = vec![
                    menu("0", "Clear assessment", "Manual override", p.text, p),
                    menu("1", "SUSPICIOUS", "Needs investigation", p.amber, p),
                    menu("2", "IOC", "Indicator of compromise", p.violet, p),
                    menu("3", "MALICIOUS", "Confirmed malicious", p.rose, p),
                    Line::from(""),
                    menu(
                        "a",
                        "Automatic assessment",
                        "Use IOC rule matches",
                        p.teal,
                        p,
                    ),
                    menu("r", "Reload IOC rules", "Choose a YAML file", p.accent, p),
                    Line::from(""),
                    Line::from(label(
                        "TAB cycles assessments directly in the timeline.",
                        p.muted,
                    )),
                ];
                frame.render_widget(Paragraph::new(lines).style(p.base().bg(p.surface)), content);
            }
            Modal::Views => {
                let lines = vec![
                    menu("1", "All tagged", "Every assessed record", p.accent, p),
                    menu("2", "SUSPICIOUS", "Review flagged activity", p.amber, p),
                    menu("3", "IOC", "Inspect matched indicators", p.violet, p),
                    menu("4", "MALICIOUS", "Focus on confirmed activity", p.rose, p),
                    Line::from(""),
                    menu("n", "Rename current view", "", p.text, p),
                    menu("x", "Close current view", "", p.text, p),
                    Line::from(""),
                    Line::from(label(
                        "F6 forks this view. 1–9 selects a tab; gt / gT switches tabs.",
                        p.muted,
                    )),
                ];
                frame.render_widget(Paragraph::new(lines).style(p.base().bg(p.surface)), content);
            }
            Modal::Marks => {
                let mut lines = vec![
                    Line::from(label(
                        "ma … mz sets a mark. Type its letter to jump; d + letter deletes.",
                        p.muted,
                    )),
                    Line::from(""),
                ];
                if self.session.bookmarks.is_empty() {
                    lines.push(Line::from(label(
                        "No bookmarks yet. Close this dialog and press ma to set one.",
                        p.text,
                    )));
                }
                let tz: chrono_tz::Tz = self.session.timezone.parse().unwrap_or(chrono_tz::UTC);
                for mark in &self.session.bookmarks {
                    let time = tz
                        .timestamp_opt(mark.key.epoch, 0)
                        .single()
                        .map(|t| t.to_rfc3339())
                        .unwrap_or_else(|| mark.key.epoch.to_string());
                    lines.push(menu(
                        &mark.name.to_string(),
                        &time,
                        &format!("record {}", mark.key.record),
                        p.text,
                        p,
                    ));
                }
                frame.render_widget(
                    Paragraph::new(lines)
                        .wrap(Wrap { trim: false })
                        .style(p.base().bg(p.surface)),
                    content,
                );
            }
            Modal::Form(form) => {
                let visible = (content.height.saturating_sub(2) as usize / 3).max(1);
                let start = form.active.saturating_sub(visible - 1);
                for (slot, (index, (name, value))) in form
                    .fields
                    .iter()
                    .enumerate()
                    .skip(start)
                    .take(visible)
                    .enumerate()
                {
                    let y = content.y + (slot * 3) as u16;
                    if y + 1 >= content.bottom() {
                        break;
                    }
                    frame.render_widget(
                        Paragraph::new(name.as_str()).style(p.base().fg(p.muted).bg(p.surface)),
                        Rect::new(content.x, y, content.width, 1),
                    );
                    let active = index == form.active;
                    let style = p.base().bg(if active { p.selected } else { p.alternate });
                    let shown = if value.is_empty() {
                        " ".into()
                    } else {
                        display_text(value)
                    };
                    frame.render_widget(
                        Paragraph::new(Line::from(vec![
                            label(if active { "▎ " } else { "  " }, p.accent),
                            label(shown, p.text),
                        ]))
                        .style(style),
                        Rect::new(content.x, y + 1, content.width, 1),
                    );
                }
                if content.height > 1 {
                    let hint = if !form.error.is_empty() {
                        form.error.clone()
                    } else if form.kind == FormKind::Filters {
                        "F2 add exclusion · Ctrl+D remove · Enter apply".into()
                    } else {
                        "Enter applies your changes".into()
                    };
                    frame.render_widget(
                        Paragraph::new(hint).style(
                            p.base()
                                .fg(if form.error.is_empty() {
                                    p.muted
                                } else {
                                    p.rose
                                })
                                .bg(p.surface),
                        ),
                        Rect::new(content.x, content.bottom() - 1, content.width, 1),
                    );
                }
            }
        }
    }

    pub fn draw(&mut self, frame: &mut Frame) {
        let area = frame.area();
        let p = self.session.theme.palette();
        fill(frame, area, p.base());
        let bar_height = if area.width < 110 { 2 } else { 1 };
        let layout = Layout::vertical([
            Constraint::Length(2),
            Constraint::Length(2),
            Constraint::Min(1),
            Constraint::Length(2),
            Constraint::Length(bar_height),
        ])
        .split(area);
        self.height = layout[2].height.saturating_sub(1).max(1) as usize;
        self.view_mut().selected = self.view().selected.min(self.height - 1);
        self.draw_tabs(frame, layout[0], p);
        self.draw_filters(frame, layout[1], p);
        self.draw_timeline(frame, layout[2], p);
        self.draw_status(frame, layout[3], p);
        self.draw_keybar(frame, layout[4], p);
        self.draw_modal(frame, p);
    }
}

fn menu(key: &str, name: &str, description: &str, color: Color, p: Palette) -> Line<'static> {
    Line::from(vec![
        keycap(key, p),
        label(format!("  {name:<23}"), color),
        label(description.to_string(), p.muted),
    ])
}
