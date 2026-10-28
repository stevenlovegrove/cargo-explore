//! Rendering.

use crate::app::{App, Focus, Origin};
use crate::cargo::BuildState;
use crate::state::ago;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Clear, List, ListItem, ListState, Paragraph, Wrap};
use ratatui::Frame;
use std::path::Path;

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

fn circle(b: Option<&BuildState>) -> Span<'static> {
    match b {
        Some(b @ BuildState::Fresh) => b.circle().green(),
        Some(b @ BuildState::Stale(_)) => b.circle().yellow(),
        Some(b) => b.circle().into(),
        None => " ".into(),
    }
}

/// The last part of a path, to name a worktree shortly.
fn short(p: &Path) -> String {
    p.file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| p.display().to_string())
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

    // What enter runs, always: the selected target with the args shown and
    // the profile chosen.
    if let Some(c) = app.command_preview() {
        f.render_widget(
            Paragraph::new(Line::from(vec![" ⏎ $ ".fg(ACCENT).bold(), c.bold()])),
            cmd,
        );
    }
    draw_footer(f, app, footer);

    match app.focus {
        Focus::DirPicker => draw_dir_picker(f, app),
        Focus::Worktrees => draw_worktrees(f, app),
        _ => {}
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
            spans.push(format!("  (workspace {})", ws.root.display()).italic());
        }
    }
    if let Some(w) = &app.worktree {
        spans.push(format!("  ⎇ {}", w.label()).magenta());
        if app.worktree_count > 1 {
            spans.push(format!(" ({} worktrees)", app.worktree_count).italic());
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
            let mut spans = vec![
                circle(app.builds.get(&t.key())),
                " ".into(),
                kind,
                Span::raw(format!("{:<name_w$}", t.name)),
            ];
            if multi {
                spans.push(format!("  {}", t.package).italic());
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

fn build_line(app: &App) -> Line<'static> {
    let profile = if app.state.release { "release" } else { "debug" };
    let b = app.selected_build();
    let mut spans = vec![circle(b), " ".into()];
    match b {
        Some(BuildState::Fresh) => spans.push(format!("built, up to date ({profile})").green()),
        Some(BuildState::Stale(newer)) => {
            let root = app.ws.as_ref().map(|w| w.root.as_path());
            let names: Vec<String> = newer
                .iter()
                .map(|p| {
                    root.and_then(|r| p.strip_prefix(r).ok())
                        .unwrap_or(p)
                        .display()
                        .to_string()
                })
                .collect();
            spans.push(format!("built ({profile}), changed since: ").yellow());
            spans.push(names.join(", ").italic());
            spans.push("  · x cargo run · X run as built".italic());
        }
        _ => spans.push(format!("not built ({profile})").italic()),
    }
    Line::from(spans)
}

fn draw_detail(f: &mut Frame, app: &App, area: Rect) {
    let Some(t) = app.selected() else {
        f.render_widget(pane(" Details ".into(), false), area);
        return;
    };
    let help = app.current_help();
    let runs = app.runs();

    // Info block.
    let mut info: Vec<Line> = Vec::new();
    let mut head = vec![
        t.name.clone().bold(),
        format!("  {} of {}", t.kind.label(), t.package).italic(),
    ];
    if !t.required_features.is_empty() {
        head.push(format!("  needs features: {}", t.required_features.join(",")).yellow());
    }
    info.push(Line::from(head));
    let root = app.ws.as_ref().map(|w| w.root.as_path());
    let src = root
        .and_then(|r| t.src_path.strip_prefix(r).ok())
        .unwrap_or(&t.src_path);
    let mut src_line = vec![src.display().to_string().italic()];
    if let Some(p) = app.parser() {
        src_line.push("  · parser: ".italic());
        src_line.push(p.to_string().into());
    }
    info.push(Line::from(src_line));
    info.push(build_line(app));
    if let Some((h, _)) = &help {
        if !h.about.is_empty() {
            info.push(Line::from(h.about.clone()));
        }
        if !h.usage.is_empty() {
            info.push(Line::from(vec![
                "Usage: ".italic(),
                Span::raw(h.usage.clone()),
            ]));
        }
        if let Some(n) = &h.note {
            info.push(Line::from(n.clone().yellow()));
        }
    }
    let info_h = (info.len() as u16 + 1).min(area.height / 3).max(4);
    let runs_h = (runs.len() as u16 + 2).clamp(3, 10);
    let [info_area, runs_area, flags_area] = Layout::vertical([
        Constraint::Length(info_h),
        Constraint::Length(runs_h),
        Constraint::Min(4),
    ])
    .areas(area);
    f.render_widget(
        Paragraph::new(info)
            .wrap(Wrap { trim: false })
            .block(Block::new().padding(ratatui::widgets::Padding::horizontal(1))),
        info_area,
    );

    draw_runs(f, app, &runs, runs_area);

    // Flags pane (or raw help text), with where its flags are from in the corner.
    let focused = app.focus == Focus::Flags;
    let corner: Line = if app.is_reading() {
        Line::from(" building and reading… ".yellow())
    } else {
        match &help {
            Some((h, Origin::Kept { updated, other: None })) => Line::from(vec![
                format!(" {} · ", h.source.label()).italic(),
                format!("updated {} ", ago(*updated)).into(),
            ]),
            Some((h, Origin::Kept { updated, other: Some(o) })) => Line::from(vec![
                format!(" {} · ", h.source.label()).italic(),
                format!("from {} {} · h to refresh ", short(o), ago(*updated)).yellow(),
            ]),
            _ => Line::from(" source scan · h --help · u -h ".italic()),
        }
    };
    let corner = corner.right_aligned();
    if app.raw {
        let raw = help.as_ref().map(|(h, _)| h.raw.clone()).unwrap_or_default();
        let raw = if raw.trim().is_empty() {
            "(no output captured; h reads --help, u reads -h)".to_string()
        } else {
            raw
        };
        let p = Paragraph::new(raw)
            .scroll((app.raw_scroll, 0))
            .block(pane(" Raw help output ".into(), focused).title_top(corner));
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
                    r.desc.clone().into(),
                ]))
            })
            .collect();
        let title = format!(" Flags & arguments ({}) ", rows.len());
        let block = pane(title, focused).title_top(corner);
        if items.is_empty() {
            let msg = if app.is_reading() {
                "building and reading…"
            } else {
                "no flags found · h reads --help, u reads -h (builds the target)"
            };
            f.render_widget(Paragraph::new(msg.italic()).block(block), flags_area);
        } else {
            let list = List::new(items)
                .block(block)
                .highlight_style(highlight(focused));
            let mut state = ListState::default().with_selected(focused.then_some(app.flag_sel));
            f.render_stateful_widget(list, flags_area, &mut state);
        }
    }
}

fn draw_runs(f: &mut Frame, app: &App, runs: &[crate::state::RunView], area: Rect) {
    let focused = app.focus == Focus::Runs;
    let items: Vec<ListItem> = runs
        .iter()
        .map(|r| {
            let mut spans = vec![if r.args.is_empty() {
                "(no args)".italic()
            } else {
                Span::raw(r.args.clone())
            }];
            if r.when > 0 {
                spans.push(format!("  {}", ago(r.when)).italic());
            }
            if let Some(o) = &r.other {
                spans.push(format!("  in {}", short(o)).yellow());
            }
            ListItem::new(Line::from(spans))
        })
        .collect();
    let recorded = runs.iter().filter(|r| r.when > 0 || !r.args.is_empty()).count();
    let block = pane(format!(" Runs ({recorded} recent) "), focused);
    let list = List::new(items)
        .block(block)
        .highlight_style(highlight(focused));
    let mut state = ListState::default().with_selected(focused.then_some(app.run_sel));
    f.render_stateful_widget(list, area, &mut state);
}

fn draw_args(f: &mut Frame, app: &App, area: Rect) {
    let focused = app.focus == Focus::Args;
    let block = pane(" Args ".into(), focused);
    let inner = block.inner(area);
    let text = if app.args.text.is_empty() && !focused {
        Line::from("none · e to edit · ⏎ runs the command below".italic())
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
                ("→", "runs, flags"),
                ("⏎", "run"),
                ("X", "run as built"),
                ("e", "edit args"),
                ("h/u", "read --help/-h"),
                ("w", "worktree"),
                ("/", "filter"),
                ("r", "release"),
                ("d", "dir"),
                ("q", "quit"),
            ],
            Focus::Filter => &[("type", "filter"), ("enter", "done"), ("esc", "clear")],
            Focus::Runs => &[
                ("↑↓", "choose args"),
                ("←→", "panes"),
                ("⏎", "run"),
                ("e", "edit"),
                ("del", "forget"),
            ],
            Focus::Flags => &[
                ("↑↓", "select"),
                ("→/space", "add to args"),
                ("←", "back"),
                ("⏎", "run"),
                ("e", "edit args"),
                ("v", "raw help"),
            ],
            Focus::Args => &[
                ("enter", "run"),
                ("tab", "complete flag"),
                ("↑↓", "recent runs"),
                ("esc", "back"),
            ],
            Focus::DirPicker => &[
                ("type", "path or filter"),
                ("tab", "complete"),
                ("↑↓", "recent"),
                ("enter", "open"),
                ("esc", "cancel"),
            ],
            Focus::Worktrees => &[("↑↓", "select"), ("enter/→", "switch"), ("esc/←", "cancel")],
        };
        let mut spans = vec![Span::raw(" ")];
        for (k, d) in hints {
            spans.push(k.to_string().bold().reversed());
            spans.push(format!(" {d}  ").into());
        }
        Line::from(spans)
    };
    f.render_widget(Paragraph::new(line), area);
}

fn popup_area(f: &Frame) -> Rect {
    let area = f.area();
    let w = (area.width * 7 / 10).max(40).min(area.width);
    let h = (area.height * 6 / 10).max(8).min(area.height);
    Rect::new(
        area.x + (area.width - w) / 2,
        area.y + (area.height - h) / 2,
        w,
        h,
    )
}

fn draw_dir_picker(f: &mut Frame, app: &App) {
    let popup = popup_area(f);
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
        Paragraph::new("Recent directories (type a path, or filter this list)".italic()),
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

fn draw_worktrees(f: &mut Frame, app: &App) {
    let popup = popup_area(f);
    f.render_widget(Clear, popup);
    let name = app.selected().map(|t| t.name.clone()).unwrap_or_default();
    let block = pane(format!(" Worktrees that have {name} "), true);
    let label_w = app
        .wt_rows
        .iter()
        .map(|r| r.worktree.label().chars().count())
        .max()
        .unwrap_or(0)
        .min(40);
    let items: Vec<ListItem> = app
        .wt_rows
        .iter()
        .map(|r| {
            let mut spans = vec![
                circle(Some(&r.build)),
                " ".into(),
                format!("{:<label_w$}", r.worktree.label()).magenta(),
                format!("  {}", r.worktree.root.display()).into(),
            ];
            if r.runs > 0 {
                spans.push(format!("  {} runs", r.runs).italic());
            }
            if r.current {
                spans.push("  (here)".fg(ACCENT));
            }
            ListItem::new(Line::from(spans))
        })
        .collect();
    let list = List::new(items)
        .block(block)
        .highlight_style(highlight(true))
        .highlight_symbol("▸ ");
    let mut state = ListState::default().with_selected(Some(app.wt_sel));
    f.render_stateful_widget(list, popup, &mut state);
}

#[cfg(test)]
mod tests {
    //! Drives the app by keys on a small workspace in a git repository with
    //! two worktrees, and reads the screen it draws.

    use super::*;
    use crate::app::{Action, Response};
    use crate::help::{Flag, HelpInfo, Source};
    use ratatui::backend::TestBackend;
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use ratatui::Terminal;
    use std::path::PathBuf;
    use std::process::Command;

    fn sh(dir: &Path, args: &[&str]) {
        let ok = Command::new(args[0])
            .args(&args[1..])
            .current_dir(dir)
            .output()
            .unwrap()
            .status
            .success();
        assert!(ok, "{args:?} failed");
    }

    /// A package `demo` with a clap bin and a hand-parsed example, committed,
    /// and a second worktree of it beside.
    fn fixture(name: &str) -> (PathBuf, PathBuf, PathBuf) {
        let base = std::env::temp_dir().join(format!("cargo-explore-ui-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let ws = base.join("ws");
        std::fs::create_dir_all(ws.join("src")).unwrap();
        std::fs::create_dir_all(ws.join("examples")).unwrap();
        std::fs::write(
            ws.join("Cargo.toml"),
            "[package]\nname = \"demo\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[dependencies]\nclap = { version = \"4\", features = [\"derive\"] }\n",
        )
        .unwrap();
        std::fs::write(
            ws.join("src/main.rs"),
            "use clap::Parser;\n/// Does demo things\n#[derive(Parser)]\nstruct Cli {\n    /// Go fast\n    #[arg(long)]\n    fast: bool,\n}\nfn main() { Cli::parse(); }\n",
        )
        .unwrap();
        std::fs::write(
            ws.join("examples/hello.rs"),
            "fn main() { if std::env::args().any(|a| a == \"--loud\") {} }\n",
        )
        .unwrap();
        let git = ["git", "-c", "user.email=t@t", "-c", "user.name=t"];
        sh(&ws, &["git", "init", "-q", "-b", "main"]);
        sh(&ws, &["git", "add", "-A"]);
        sh(&ws, &[git[0], git[1], git[2], git[3], git[4], "commit", "-qm", "x"]);
        sh(&ws, &["git", "worktree", "add", "-q", "-b", "other", "../wt2"]);
        let ws = ws.canonicalize().unwrap();
        let wt2 = base.join("wt2").canonicalize().unwrap();
        (base, ws, wt2)
    }

    fn screen(app: &App) -> String {
        let mut t = Terminal::new(TestBackend::new(140, 32)).unwrap();
        t.draw(|f| draw(f, app)).unwrap();
        let buf = t.backend().buffer();
        let mut s = String::new();
        for y in 0..buf.area.height {
            for x in 0..buf.area.width {
                s.push_str(buf[(x, y)].symbol());
            }
            s.push('\n');
        }
        s
    }

    fn key(app: &mut App, code: KeyCode) -> Action {
        app.handle_key(KeyEvent::new(code, KeyModifiers::NONE))
    }

    fn argv(a: Action) -> Vec<String> {
        match a {
            Action::Run { argv, .. } => argv,
            _ => panic!("expected a run"),
        }
    }

    #[test]
    fn explores_by_arrows_and_runs_at_once() {
        let (base, ws, wt2) = fixture("keys");
        let state = crate::state::State::load(Some(base.join("state.json")));
        let (tx, rx) = std::sync::mpsc::channel();
        let mut app = App::new(ws.clone(), state, tx);

        // Nothing is built or run to show a target: its flags are from its source.
        let s = screen(&app);
        assert!(s.contains("○ bin demo"), "{s}");
        assert!(s.contains("parser: clap, derive"), "{s}");
        assert!(s.contains("--fast"), "{s}");
        assert!(s.contains("source scan"), "{s}");
        assert!(s.contains("⎇ main"), "{s}");
        assert!(rx.try_recv().is_err());

        // Down to the example; right to its runs, where a run with no args is
        // always first; right again to its flags, and right adds one.
        key(&mut app, KeyCode::Down);
        assert_eq!(app.selected().unwrap().name, "hello");
        assert!(screen(&app).contains("std::env::args, by hand"));
        key(&mut app, KeyCode::Right);
        assert_eq!(app.focus, crate::app::Focus::Runs);
        assert!(screen(&app).contains("(no args)"));
        let a = argv(key(&mut app, KeyCode::Enter));
        assert_eq!(a, ["cargo", "run", "-p", "demo", "--example", "hello"]);
        key(&mut app, KeyCode::Right);
        assert_eq!(app.focus, crate::app::Focus::Flags);
        key(&mut app, KeyCode::Right);
        assert_eq!(app.args.text, "--loud ");

        // Cmd-enter runs at once from where we are; alt-enter too.
        let cmd_enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::SUPER);
        let a = argv(app.handle_key(cmd_enter));
        assert_eq!(a[..6], ["cargo", "run", "-p", "demo", "--example", "hello"]);
        assert_eq!(a.last().unwrap(), "--loud");
        let alt_enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::ALT);
        assert!(matches!(app.handle_key(alt_enter), Action::Run { .. }));
        // Plain enter runs too, from the flags, and the command is shown.
        let a = argv(key(&mut app, KeyCode::Enter));
        assert_eq!(a.last().unwrap(), "--loud");
        assert!(screen(&app).contains("⏎ $ cargo run -p demo --example hello -- --loud"));

        // The runs are recorded, the one with no args still first; up to it
        // clears the args.
        key(&mut app, KeyCode::Left);
        assert_eq!(app.focus, crate::app::Focus::Runs);
        let s = screen(&app);
        assert!(s.contains("Runs (2 recent)"), "{s}");
        assert_eq!(app.runs()[0].args, "");
        assert_eq!(app.runs()[1].args, "--loud");
        key(&mut app, KeyCode::Up);
        assert_eq!(app.args.text, "");
        key(&mut app, KeyCode::Down);
        assert_eq!(app.args.text, "--loud");
        key(&mut app, KeyCode::Left);
        assert_eq!(app.focus, crate::app::Focus::Targets);

        // X runs as built, which it is not.
        assert!(matches!(key(&mut app, KeyCode::Char('X')), Action::None));
        assert!(app.status.as_deref().unwrap().contains("not built"));

        // h asks the worker for --help; what comes back is kept, with its date.
        key(&mut app, KeyCode::Char('h'));
        let req = rx.try_recv().unwrap();
        assert_eq!(req.mode, Source::Help);
        assert!(screen(&app).contains("building and reading"));
        app.on_response(Response {
            key: req.key.clone(),
            root: req.root.clone(),
            info: HelpInfo {
                flags: vec![Flag {
                    long: Some("--shout".into()),
                    desc: "from --help".into(),
                    ..Default::default()
                }],
                usage: "hello [--shout]".into(),
                ..Default::default()
            },
        });
        let s = screen(&app);
        assert!(s.contains("--shout"), "{s}");
        assert!(s.contains("updated just now"), "{s}");

        // w lists both worktrees; switching keeps the target, and what was
        // kept in the first stands in, marked as from there.
        key(&mut app, KeyCode::Char('w'));
        assert_eq!(app.focus, crate::app::Focus::Worktrees);
        assert_eq!(app.wt_rows.len(), 2);
        let s = screen(&app);
        assert!(s.contains("Worktrees that have hello"), "{s}");
        assert!(s.contains("other"), "{s}");
        let other = app.wt_rows.iter().position(|r| !r.current).unwrap();
        while app.wt_sel != other {
            let k = if app.wt_sel < other { KeyCode::Down } else { KeyCode::Up };
            key(&mut app, k);
        }
        key(&mut app, KeyCode::Enter);
        assert_eq!(app.ws.as_ref().unwrap().root, wt2);
        assert_eq!(app.selected().unwrap().name, "hello");
        let s = screen(&app);
        assert!(s.contains("⎇ other"), "{s}");
        assert!(s.contains("--shout"), "{s}");
        assert!(s.contains("from ws just now · h to refresh"), "{s}");
        assert!(s.contains("in ws"), "{s}");
        assert_eq!(app.args.text, "--loud", "the run from the other worktree fills the args");

        let _ = std::fs::remove_dir_all(base);
    }
}
