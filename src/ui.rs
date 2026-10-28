//! Rendering.

use crate::app::{App, Focus};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Clear, List, ListItem, ListState, Paragraph, Wrap};
use ratatui::Frame;

const ACCENT: Color = Color::Cyan;

fn pane(title: String, focused: bool) -> Block<'static> {
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .title(title);
    if focused {
        block
            .border_style(Style::new().fg(ACCENT))
            .title_style(Style::new().fg(ACCENT).bold())
    } else {
        block.border_style(Style::new().dark_gray())
    }
}

fn highlight(focused: bool) -> Style {
    if focused {
        Style::new().bg(ACCENT).fg(Color::Black).bold()
    } else {
        Style::new().add_modifier(Modifier::REVERSED)
    }
}

pub fn draw(f: &mut Frame, app: &App) {
    let [header, main, args, cmd, footer] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(8),
        Constraint::Length(3),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .areas(f.area());

    draw_header(f, app, header);
    let [left, right] =
        Layout::horizontal([Constraint::Percentage(32), Constraint::Percentage(68)]).areas(main);
    draw_targets(f, app, left);
    draw_detail(f, app, right);
    draw_args(f, app, args);

    if let Some(c) = app.command_preview() {
        f.render_widget(
            Paragraph::new(Line::from(vec![" $ ".dark_gray(), Span::raw(c).gray()])),
            cmd,
        );
    }
    draw_footer(f, app, footer);

    if app.focus == Focus::DirPicker {
        draw_dir_picker(f, app);
    }
}

fn draw_header(f: &mut Frame, app: &App, area: Rect) {
    let profile = if app.state.release {
        " release ".black().on_yellow()
    } else {
        " debug ".black().on_green()
    };
    let mut spans = vec![
        " cargo-explore ".bold().fg(ACCENT),
        Span::raw(app.dir.display().to_string()),
    ];
    if let Some(ws) = &app.ws {
        if ws.root != app.dir {
            spans.push(format!("  (workspace {})", ws.root.display()).dark_gray());
        }
    }
    let [l, r] = Layout::horizontal([Constraint::Min(0), Constraint::Length(9)]).areas(area);
    f.render_widget(Paragraph::new(Line::from(spans)), l);
    f.render_widget(Paragraph::new(Line::from(profile)).right_aligned(), r);
}

fn draw_targets(f: &mut Frame, app: &App, area: Rect) {
    let focused = matches!(app.focus, Focus::Targets | Focus::Filter);
    let title = if app.focus == Focus::Filter || !app.filter.text.is_empty() {
        format!(" Targets /{} ", app.filter.text)
    } else {
        format!(" Targets ({}) ", app.visible.len())
    };
    let block = pane(title, focused);

    let Some(ws) = &app.ws else {
        let msg = app.error.clone().unwrap_or_default();
        let p = Paragraph::new(vec![
            Line::from("No Cargo project here.".bold()),
            Line::from(""),
            Line::from(msg.red()),
            Line::from(""),
            Line::from("Press d to pick another directory."),
        ])
        .wrap(Wrap { trim: true })
        .block(block);
        f.render_widget(p, area);
        return;
    };

    let multi = app.multi_package();
    let name_w = app
        .visible
        .iter()
        .map(|&i| ws.targets[i].name.chars().count())
        .max()
        .unwrap_or(0);
    let items: Vec<ListItem> = app
        .visible
        .iter()
        .map(|&i| {
            let t = &ws.targets[i];
            let kind = match t.kind {
                crate::cargo::Kind::Bin => "bin ".green(),
                crate::cargo::Kind::Example => "ex  ".magenta(),
            };
            let mut spans = vec![kind, Span::raw(format!("{:<name_w$}", t.name))];
            if multi {
                spans.push(format!("  {}", t.package).dark_gray());
            }
            ListItem::new(Line::from(spans))
        })
        .collect();
    let list = List::new(items)
        .block(block)
        .highlight_style(highlight(focused))
        .highlight_symbol("▸ ");
    let mut state =
        ListState::default().with_selected((!app.visible.is_empty()).then_some(app.sel));
    f.render_stateful_widget(list, area, &mut state);

    if app.focus == Focus::Filter {
        let x = area.x + 11 + app.filter.before_cursor().chars().count() as u16;
        f.set_cursor_position((x.min(area.right().saturating_sub(2)), area.y));
    }
}

fn draw_detail(f: &mut Frame, app: &App, area: Rect) {
    let Some(t) = app.selected() else {
        f.render_widget(pane(" Details ".into(), false), area);
        return;
    };
    let help = app.current_help();
    let history = app.history();

    // Info block.
    let mut info: Vec<Line> = Vec::new();
    let mut head = vec![
        t.name.clone().bold(),
        format!("  {} of {}", t.kind.label(), t.package).dark_gray(),
    ];
    if !t.required_features.is_empty() {
        head.push(format!("  needs features: {}", t.required_features.join(",")).yellow());
    }
    info.push(Line::from(head));
    let root = app.ws.as_ref().map(|w| w.root.as_path());
    let src = root
        .and_then(|r| t.src_path.strip_prefix(r).ok())
        .unwrap_or(&t.src_path);
    info.push(Line::from(src.display().to_string().dark_gray()));
    if let Some((h, _)) = help {
        if !h.about.is_empty() {
            info.push(Line::from(h.about.clone()));
        }
        if !h.usage.is_empty() {
            info.push(Line::from(vec![
                "Usage: ".dark_gray(),
                Span::raw(h.usage.clone()),
            ]));
        }
        if let Some(n) = &h.note {
            info.push(Line::from(n.clone().yellow()));
        }
    }
    let info_h = (info.len() as u16 + 1).min(area.height / 3).max(3);
    let hist_h = (history.len() as u16 + 2).clamp(3, 10);
    let [info_area, flags_area, hist_area] = Layout::vertical([
        Constraint::Length(info_h),
        Constraint::Min(4),
        Constraint::Length(hist_h),
    ])
    .areas(area);
    f.render_widget(
        Paragraph::new(info)
            .wrap(Wrap { trim: false })
            .block(Block::new().padding(ratatui::widgets::Padding::horizontal(1))),
        info_area,
    );

    // Flags pane (or raw help text).
    let focused = app.focus == Focus::Flags;
    let label = app.source_label();
    if app.raw {
        let raw = help.map(|(h, _)| h.raw.clone()).unwrap_or_default();
        let raw = if raw.trim().is_empty() {
            "(no output captured)".to_string()
        } else {
            raw
        };
        let p = Paragraph::new(raw)
            .scroll((app.raw_scroll, 0))
            .block(pane(format!(" Raw help output · {label} "), focused));
        f.render_widget(p, flags_area);
    } else {
        let rows = app.flag_rows();
        let spec_w = rows
            .iter()
            .map(|r| r.spec.chars().count())
            .max()
            .unwrap_or(0)
            .min(34);
        let items: Vec<ListItem> = rows
            .iter()
            .map(|r| {
                let kind = match r.kind {
                    "cmd" => "cmd  ".blue(),
                    "arg" => "arg  ".magenta(),
                    _ => "     ".into(),
                };
                ListItem::new(Line::from(vec![
                    kind,
                    format!("{:<spec_w$}", r.spec).fg(if r.kind == "flag" {
                        Color::Green
                    } else {
                        Color::Reset
                    }),
                    "  ".into(),
                    r.desc.clone().gray(),
                ]))
            })
            .collect();
        let title = format!(" Flags & arguments ({}) · {label} ", rows.len());
        let block = pane(title, focused);
        if items.is_empty() {
            let msg = if help.is_some_and(|(_, loading)| loading) {
                "building and reading --help…"
            } else {
                "no flags found (press v to see raw output)"
            };
            f.render_widget(Paragraph::new(msg.dark_gray()).block(block), flags_area);
        } else {
            let list = List::new(items)
                .block(block)
                .highlight_style(highlight(focused));
            let mut state = ListState::default().with_selected(focused.then_some(app.flag_sel));
            f.render_stateful_widget(list, flags_area, &mut state);
        }
    }

    // History pane.
    let focused = app.focus == Focus::History;
    let items: Vec<ListItem> = history
        .iter()
        .map(|a| {
            ListItem::new(if a.is_empty() {
                "(no args)".dark_gray().into()
            } else {
                Line::from(a.clone())
            })
        })
        .collect();
    let list = List::new(items)
        .block(pane(format!(" History ({}) ", history.len()), focused))
        .highlight_style(highlight(focused));
    let mut state = ListState::default().with_selected(focused.then_some(app.hist_sel));
    f.render_stateful_widget(list, hist_area, &mut state);
}

fn draw_args(f: &mut Frame, app: &App, area: Rect) {
    let focused = app.focus == Focus::Args;
    let block = pane(" Args ".into(), focused);
    let inner = block.inner(area);
    let text = if app.args.text.is_empty() && !focused {
        Line::from("press i to edit arguments, x to run".dark_gray())
    } else {
        Line::from(app.args.text.clone())
    };
    // Keep the cursor visible on long lines.
    let cursor_col = app.args.before_cursor().chars().count() as u16;
    let scroll = cursor_col.saturating_sub(inner.width.saturating_sub(1));
    f.render_widget(Paragraph::new(text).scroll((0, scroll)).block(block), area);
    if focused {
        f.set_cursor_position((inner.x + cursor_col - scroll, inner.y));
    }
}

fn draw_footer(f: &mut Frame, app: &App, area: Rect) {
    let line = if let Some(s) = &app.status {
        Line::from(format!(" {s}").yellow())
    } else {
        let hints: &[(&str, &str)] = match app.focus {
            Focus::Targets => &[
                ("↑↓", "select"),
                ("/", "filter"),
                ("enter", "edit args"),
                ("x", "run"),
                ("tab", "panes"),
                ("d", "dir"),
                ("r", "release"),
                ("v", "raw help"),
                ("h", "re-read help"),
                ("q", "quit"),
            ],
            Focus::Filter => &[("type", "filter"), ("enter", "done"), ("esc", "clear")],
            Focus::Flags => &[
                ("↑↓", "select"),
                ("enter", "insert into args"),
                ("v", "raw help"),
                ("tab", "next pane"),
                ("esc", "back"),
            ],
            Focus::History => &[
                ("↑↓", "select"),
                ("enter", "use"),
                ("del", "forget"),
                ("x", "run"),
                ("esc", "back"),
            ],
            Focus::Args => &[
                ("enter", "run"),
                ("tab", "complete flag"),
                ("↑↓", "history"),
                ("esc", "back"),
            ],
            Focus::DirPicker => &[
                ("type", "path or filter"),
                ("tab", "complete"),
                ("↑↓", "recent"),
                ("enter", "open"),
                ("esc", "cancel"),
            ],
        };
        let mut spans = vec![Span::raw(" ")];
        for (k, d) in hints {
            spans.push(k.to_string().fg(ACCENT).bold());
            spans.push(format!(" {d}  ").dark_gray());
        }
        Line::from(spans)
    };
    f.render_widget(Paragraph::new(line), area);
}

fn draw_dir_picker(f: &mut Frame, app: &App) {
    let area = f.area();
    let w = (area.width * 7 / 10).max(40).min(area.width);
    let h = (area.height * 6 / 10).max(8).min(area.height);
    let popup = Rect::new(
        area.x + (area.width - w) / 2,
        area.y + (area.height - h) / 2,
        w,
        h,
    );
    f.render_widget(Clear, popup);
    let block = pane(" Switch directory ".into(), true);
    let inner = block.inner(popup);
    f.render_widget(block, popup);
    let [input, hint, list_area] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(1),
    ])
    .areas(inner);
    f.render_widget(
        Paragraph::new(Line::from(vec![
            "> ".fg(ACCENT),
            Span::raw(app.dir_input.text.clone()),
        ])),
        input,
    );
    f.render_widget(
        Paragraph::new("Recent directories (type a path, or filter this list)".dark_gray()),
        hint,
    );
    let items: Vec<ListItem> = app
        .dir_candidates()
        .iter()
        .map(|d| ListItem::new(d.display().to_string()))
        .collect();
    let list = List::new(items)
        .highlight_style(highlight(true))
        .highlight_symbol("▸ ");
    let mut state = ListState::default().with_selected(Some(app.dir_sel));
    f.render_stateful_widget(list, list_area, &mut state);
    f.set_cursor_position((
        input.x + 2 + app.dir_input.before_cursor().chars().count() as u16,
        input.y,
    ));
}
