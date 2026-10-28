//! Application state and key handling.
//!
//! The arrow keys explore: up and down select within a pane, left and right
//! move between the targets, their recent runs and their flags. Nothing is
//! built or run unless asked: `h` and `u` build the target and read its
//! `--help` or `-h`, and what they read is kept for the target in this
//! worktree. Cmd-enter (or ctrl- or alt-enter, or `x`) runs at once with the
//! arguments shown.

use crate::cargo::{BuildState, Target, Workspace, Worktree};
use crate::help::{HelpInfo, Source};
use crate::input::LineInput;
use crate::state::{RunView, State};
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Focus {
    Targets,
    Filter,
    Runs,
    Flags,
    Args,
    DirPicker,
    Worktrees,
}

/// A request to build a target and read its `--help` (Help) or `-h` (Usage).
pub struct Request {
    pub key: String,
    pub root: PathBuf,
    pub target: Target,
    pub release: bool,
    pub mode: Source,
}

pub struct Response {
    pub key: String,
    pub root: PathBuf,
    pub info: HelpInfo,
}

pub enum Action {
    None,
    Quit,
    Run {
        argv: Vec<String>,
        env: Vec<(String, String)>,
    },
}

/// One row of the flags pane.
pub struct FlagRow {
    pub kind: &'static str,
    pub spec: String,
    pub desc: String,
    /// Text inserted into the args line when chosen.
    pub insert: Option<String>,
}

/// Where the flags shown came from.
pub enum Origin {
    /// Scanned from the source just now.
    Scan,
    /// Read from the program, kept; `other` is the worktree it was read in,
    /// if not this one.
    Kept { updated: u64, other: Option<PathBuf> },
}

/// One row of the worktree picker.
pub struct WorktreeRow {
    pub worktree: Worktree,
    pub build: BuildState,
    pub current: bool,
    pub runs: usize,
}

pub struct App {
    pub dir: PathBuf,
    pub ws: Option<Workspace>,
    pub error: Option<String>,
    pub focus: Focus,
    pub filter: LineInput,
    pub visible: Vec<usize>,
    pub sel: usize,
    pub args: LineInput,
    pub flag_sel: usize,
    pub run_sel: usize,
    hist_nav: Option<usize>,
    pub raw: bool,
    pub raw_scroll: u16,
    /// Target keys whose `--help` is being read, in this workspace.
    pub reading: HashSet<String>,
    scans: HashMap<String, (HelpInfo, &'static str)>,
    /// Build states of this workspace's targets, for the profile chosen.
    pub builds: HashMap<String, BuildState>,
    pub state: State,
    tx: Sender<Request>,
    pub status: Option<String>,
    pub dir_input: LineInput,
    pub dir_sel: usize,
    selected_key: Option<String>,
    /// The worktree this workspace is in, if it is in git.
    pub worktree: Option<Worktree>,
    pub worktree_count: usize,
    pub wt_rows: Vec<WorktreeRow>,
    pub wt_sel: usize,
}

impl App {
    pub fn new(dir: PathBuf, state: State, tx: Sender<Request>) -> App {
        let mut app = App {
            dir: dir.clone(),
            ws: None,
            error: None,
            focus: Focus::Targets,
            filter: LineInput::default(),
            visible: Vec::new(),
            sel: 0,
            args: LineInput::default(),
            flag_sel: 0,
            run_sel: 0,
            hist_nav: None,
            raw: false,
            raw_scroll: 0,
            reading: HashSet::new(),
            scans: HashMap::new(),
            builds: HashMap::new(),
            state,
            tx,
            status: None,
            dir_input: LineInput::default(),
            dir_sel: 0,
            selected_key: None,
            worktree: None,
            worktree_count: 0,
            wt_rows: Vec::new(),
            wt_sel: 0,
        };
        app.load_dir(&dir);
        app
    }

    pub fn load_dir(&mut self, dir: &Path) {
        let dir = dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf());
        self.dir = dir.clone();
        self.reading.clear();
        self.scans.clear();
        self.selected_key = None;
        self.filter.clear();
        self.sel = 0;
        self.worktree = None;
        self.worktree_count = 0;
        match crate::cargo::discover(&dir) {
            Ok(ws) => {
                self.state.push_dir(&dir);
                self.state.save();
                self.error = None;
                let wts = crate::cargo::worktrees(&ws.root);
                self.worktree_count = wts.len();
                self.worktree = wts.into_iter().find(|w| w.root == ws.root);
                self.ws = Some(ws);
            }
            Err(e) => {
                self.ws = None;
                self.error = Some(e.to_string());
            }
        }
        self.refresh_builds();
        self.refilter();
        let last = self
            .ws
            .as_ref()
            .and_then(|ws| self.state.last_target(&ws.root))
            .map(String::from);
        if let Some(last) = last {
            self.select_key(&last);
        }
        self.on_select();
    }

    fn select_key(&mut self, key: &str) {
        if let Some(ws) = &self.ws {
            if let Some(pos) = self.visible.iter().position(|&i| ws.targets[i].key() == key) {
                self.sel = pos;
            }
        }
    }

    /// Looks again at whether each target is built and up to date. Stats only.
    pub fn refresh_builds(&mut self) {
        self.builds.clear();
        let Some(ws) = &self.ws else { return };
        for t in &ws.targets {
            let b = crate::cargo::build_state(&ws.root, &ws.target_dir, t, self.state.release);
            self.builds.insert(t.key(), b);
        }
    }

    fn refresh_selected_build(&mut self) {
        if let (Some(ws), Some(t)) = (&self.ws, self.selected()) {
            let b = crate::cargo::build_state(&ws.root, &ws.target_dir, t, self.state.release);
            self.builds.insert(t.key(), b);
        }
    }

    pub fn refilter(&mut self) {
        let needle = self.filter.text.to_lowercase();
        self.visible = match &self.ws {
            Some(ws) => ws
                .targets
                .iter()
                .enumerate()
                .filter(|(_, t)| {
                    needle.is_empty()
                        || fuzzy_match(&t.name.to_lowercase(), &needle)
                        || t.package.to_lowercase().contains(&needle)
                })
                .map(|(i, _)| i)
                .collect(),
            None => Vec::new(),
        };
        self.sel = self.sel.min(self.visible.len().saturating_sub(1));
    }

    pub fn selected(&self) -> Option<&Target> {
        let ws = self.ws.as_ref()?;
        self.visible.get(self.sel).map(|&i| &ws.targets[i])
    }

    pub fn multi_package(&self) -> bool {
        self.ws.as_ref().is_some_and(|ws| {
            ws.targets
                .iter()
                .any(|t| t.package != ws.targets[0].package)
        })
    }

    pub fn runs(&self) -> Vec<RunView> {
        match (&self.ws, self.selected()) {
            (Some(ws), Some(t)) => self.state.runs(&ws.root, &t.key()),
            _ => Vec::new(),
        }
    }

    pub fn selected_build(&self) -> Option<&BuildState> {
        self.builds.get(&self.selected()?.key())
    }

    fn on_select(&mut self) {
        let Some(t) = self.selected().cloned() else {
            self.selected_key = None;
            return;
        };
        let key = t.key();
        if self.selected_key.as_deref() == Some(key.as_str()) {
            return;
        }
        self.selected_key = Some(key.clone());
        self.flag_sel = 0;
        self.run_sel = 0;
        self.hist_nav = None;
        self.raw_scroll = 0;
        let root = self.ws.as_ref().unwrap().root.clone();
        let last_args = self.runs().first().map(|r| r.args.clone()).unwrap_or_default();
        self.args.set(&last_args);
        self.state.set_last_target(&root, &key);
        self.refresh_selected_build();
        self.scans.entry(key).or_insert_with(|| {
            let sources = crate::source::read_sources(&t.src_path);
            let parser = crate::source::parser_of(&sources, &t.deps);
            (crate::source::scan_sources(&sources), parser)
        });
    }

    /// Builds the selected target and reads its `--help` or `-h`.
    fn request_help(&mut self, mode: Source) {
        let (Some(ws), Some(t)) = (&self.ws, self.selected().cloned()) else {
            return;
        };
        let key = t.key();
        let req = Request {
            key: key.clone(),
            root: ws.root.clone(),
            target: t,
            release: self.state.release,
            mode,
        };
        self.reading.insert(key);
        let _ = self.tx.send(req);
        self.status = Some(format!("building and reading {}", mode.label()));
    }

    pub fn on_response(&mut self, r: Response) {
        self.state.set_params(&r.root, &r.key, r.info);
        self.state.save();
        if self.ws.as_ref().is_some_and(|ws| ws.root == r.root) {
            self.reading.remove(&r.key);
            if let Some(ws) = &self.ws {
                if let Some(t) = ws.targets.iter().find(|t| t.key() == r.key) {
                    let b = crate::cargo::build_state(&ws.root, &ws.target_dir, t, self.state.release);
                    self.builds.insert(r.key.clone(), b);
                }
            }
        }
    }

    /// The flags of the selected target: those read from it and kept, here or
    /// in another worktree, or else those scanned from its source.
    pub fn current_help(&self) -> Option<(&HelpInfo, Origin)> {
        let key = self.selected_key.as_ref()?;
        let root = &self.ws.as_ref()?.root;
        if let Some((rec, other)) = self.state.params(root, key) {
            return Some((
                &rec.info,
                Origin::Kept {
                    updated: rec.updated,
                    other,
                },
            ));
        }
        self.scans.get(key).map(|(h, _)| (h, Origin::Scan))
    }

    /// How the selected target reads its command line, from its source.
    pub fn parser(&self) -> Option<&'static str> {
        self.scans.get(self.selected_key.as_ref()?).map(|(_, p)| *p)
    }

    pub fn is_reading(&self) -> bool {
        self.selected_key
            .as_ref()
            .is_some_and(|k| self.reading.contains(k))
    }

    pub fn flag_rows(&self) -> Vec<FlagRow> {
        let Some((h, _)) = self.current_help() else {
            return Vec::new();
        };
        let mut rows = Vec::new();
        for s in &h.subcommands {
            if s.name == "help" {
                continue;
            }
            rows.push(FlagRow {
                kind: "cmd",
                spec: s.name.clone(),
                desc: s.desc.clone(),
                insert: Some(s.name.clone()),
            });
        }
        for p in &h.positionals {
            rows.push(FlagRow {
                kind: "arg",
                spec: p.name.clone(),
                desc: p.desc.clone(),
                insert: None,
            });
        }
        for f in &h.flags {
            let insert = if f.value.is_some() {
                let first = f.possible_values.first().map(String::as_str).unwrap_or("");
                format!("{} {first}", f.spelling())
            } else {
                f.spelling().to_string()
            };
            rows.push(FlagRow {
                kind: "flag",
                spec: f.spec(),
                desc: f.desc.clone(),
                insert: Some(insert),
            });
        }
        rows
    }

    pub fn program_args(&self) -> Result<Vec<String>, String> {
        shlex::split(&self.args.text).ok_or_else(|| "unbalanced quotes in args".to_string())
    }

    pub fn command_preview(&self) -> Option<String> {
        let t = self.selected()?;
        let args = self.program_args().unwrap_or_default();
        let argv = t.run_argv(self.state.release, &args);
        Some(shlex::try_join(argv.iter().map(String::as_str)).unwrap_or_else(|_| argv.join(" ")))
    }

    fn move_sel(&mut self, delta: isize) {
        if self.visible.is_empty() {
            return;
        }
        let max = self.visible.len() as isize - 1;
        self.sel = (self.sel as isize + delta).clamp(0, max) as usize;
        self.on_select();
    }

    fn insert_arg(&mut self, s: &str) {
        self.args.end();
        let before = self.args.before_cursor();
        if !before.is_empty() && !before.ends_with(' ') {
            self.args.insert(' ');
        }
        self.args.insert_str(s);
        if !s.ends_with(' ') {
            self.args.insert(' ');
        }
    }

    fn complete_arg(&mut self) -> bool {
        let word = self.args.current_word().to_string();
        if !word.starts_with('-') {
            return false;
        }
        let Some((h, _)) = self.current_help() else {
            return false;
        };
        let mut cands: Vec<String> = h
            .flags
            .iter()
            .flat_map(|f| [f.long.clone(), f.short.clone()])
            .flatten()
            .filter(|s| s.starts_with(&word))
            .collect();
        cands.sort();
        cands.dedup();
        match cands.len() {
            0 => self.status = Some(format!("no flag matches {word}")),
            1 => {
                self.args.insert_str(&cands[0][word.len()..]);
                self.args.insert(' ');
            }
            _ => {
                let common = cands.iter().skip(1).fold(cands[0].clone(), |acc, c| {
                    acc.chars()
                        .zip(c.chars())
                        .take_while(|(a, b)| a == b)
                        .map(|(a, _)| a)
                        .collect()
                });
                self.args.insert_str(&common[word.len()..]);
                self.status = Some(cands.join("  "));
            }
        }
        true
    }

    /// Runs the selected target with the arguments shown: by `cargo run`, or
    /// `direct`ly as it was last built, which builds nothing.
    fn run_action(&mut self, direct: bool) -> Action {
        let (Some(ws), Some(t)) = (&self.ws, self.selected().cloned()) else {
            return Action::None;
        };
        let args = match self.program_args() {
            Ok(a) => a,
            Err(e) => {
                self.status = Some(e);
                return Action::None;
            }
        };
        let (argv, env) = if direct {
            let exe = t.exe_path(&ws.target_dir, self.state.release);
            if !exe.is_file() {
                self.status = Some(format!(
                    "not built in {}; x runs it by cargo run",
                    if self.state.release { "release" } else { "debug" }
                ));
                return Action::None;
            }
            let mut argv = vec![exe.display().to_string()];
            argv.extend(args);
            let pkg_dir = t.manifest_path.parent().unwrap_or(&ws.root);
            (
                argv,
                vec![("CARGO_MANIFEST_DIR".to_string(), pkg_dir.display().to_string())],
            )
        } else {
            (t.run_argv(self.state.release, &args), Vec::new())
        };
        let root = ws.root.clone();
        self.state.push_run(&root, &t.key(), self.args.text.trim());
        self.state.save();
        self.run_sel = 0;
        self.hist_nav = None;
        Action::Run { argv, env }
    }

    pub fn dir_candidates(&self) -> Vec<PathBuf> {
        let needle = self.dir_input.text.to_lowercase();
        self.state
            .recent_dirs
            .iter()
            .filter(|d| needle.is_empty() || d.to_string_lossy().to_lowercase().contains(&needle))
            .cloned()
            .collect()
    }

    fn resolve_dir_input(&self) -> Option<PathBuf> {
        let text = self.dir_input.text.trim();
        if text.is_empty() {
            return None;
        }
        let p = expand_tilde(text);
        let p = if p.is_absolute() { p } else { self.dir.join(p) };
        p.is_dir().then_some(p)
    }

    fn complete_dir(&mut self) {
        let text = self.dir_input.text.clone();
        let p = expand_tilde(&text);
        let p = if p.is_absolute() { p } else { self.dir.join(p) };
        let (parent, prefix) = if text.ends_with('/') || text.is_empty() {
            (p.clone(), String::new())
        } else {
            (
                p.parent().map(Path::to_path_buf).unwrap_or_default(),
                p.file_name()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_default(),
            )
        };
        let Ok(rd) = std::fs::read_dir(&parent) else {
            return;
        };
        let mut names: Vec<String> = rd
            .flatten()
            .filter(|e| e.path().is_dir())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.starts_with(&prefix) && (prefix.starts_with('.') || !n.starts_with('.')))
            .collect();
        names.sort();
        if names.is_empty() {
            return;
        }
        let common = names.iter().skip(1).fold(names[0].clone(), |acc, c| {
            acc.chars()
                .zip(c.chars())
                .take_while(|(a, b)| a == b)
                .map(|(a, _)| a)
                .collect()
        });
        self.dir_input.end();
        self.dir_input.insert_str(&common[prefix.len()..]);
        if names.len() == 1 {
            self.dir_input.insert('/');
        } else {
            self.status = Some(names.join("  "));
        }
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> Action {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        if ctrl && matches!(key.code, KeyCode::Char('c') | KeyCode::Char('q')) {
            return Action::Quit;
        }
        self.status = None;
        // Cmd-enter runs at once, from anywhere; ctrl- and alt-enter too, for
        // terminals that do not pass Cmd through.
        let run_mods = KeyModifiers::SUPER | KeyModifiers::CONTROL | KeyModifiers::ALT;
        let picking = matches!(self.focus, Focus::DirPicker | Focus::Worktrees);
        if !picking
            && (key.code == KeyCode::F(5)
                || key.code == KeyCode::Enter && key.modifiers.intersects(run_mods))
        {
            return self.run_action(false);
        }
        match self.focus {
            Focus::Targets => self.key_targets(key),
            Focus::Filter => self.key_filter(key),
            Focus::Runs => self.key_runs(key),
            Focus::Flags => self.key_flags(key),
            Focus::Args => self.key_args(key),
            Focus::DirPicker => self.key_dir(key),
            Focus::Worktrees => self.key_worktrees(key),
        }
    }

    fn open_dir_picker(&mut self) {
        self.dir_input.clear();
        self.dir_sel = 0;
        self.focus = Focus::DirPicker;
    }

    /// Lists the worktrees whose copy of the workspace has the selected target.
    fn open_worktrees(&mut self) {
        let (Some(ws), Some(t)) = (&self.ws, self.selected()) else {
            return;
        };
        let key = t.key();
        let rows: Vec<WorktreeRow> = crate::cargo::worktrees(&ws.root)
            .into_iter()
            .filter_map(|w| {
                let there = ws.target_in(t, &w.root)?;
                let build = crate::cargo::build_state(
                    &w.root,
                    &ws.target_dir_in(&w.root),
                    &there,
                    self.state.release,
                );
                let runs = self
                    .state
                    .runs
                    .get(&key)
                    .and_then(|m| m.get(&w.root.display().to_string()))
                    .map_or(0, Vec::len);
                Some(WorktreeRow {
                    current: w.root == ws.root,
                    worktree: w,
                    build,
                    runs,
                })
            })
            .collect();
        if rows.len() < 2 {
            self.status = Some(match rows.len() {
                0 => "this workspace is not in a git repository with worktrees".into(),
                _ => "no other worktree has this target".into(),
            });
            return;
        }
        self.wt_sel = rows.iter().position(|r| r.current).unwrap_or(0);
        self.wt_rows = rows;
        self.focus = Focus::Worktrees;
    }

    /// Keys shared by the list panes.
    fn key_common(&mut self, key: KeyEvent) -> Option<Action> {
        match key.code {
            KeyCode::Char('q') => Some(Action::Quit),
            KeyCode::Tab => {
                self.focus = match self.focus {
                    Focus::Targets => Focus::Runs,
                    Focus::Runs => Focus::Flags,
                    Focus::Flags => Focus::Args,
                    _ => Focus::Targets,
                };
                Some(Action::None)
            }
            KeyCode::BackTab => {
                self.focus = match self.focus {
                    Focus::Targets => Focus::Args,
                    Focus::Runs => Focus::Targets,
                    Focus::Flags => Focus::Runs,
                    _ => Focus::Flags,
                };
                Some(Action::None)
            }
            KeyCode::Char('i') | KeyCode::Char('a') => {
                self.args.end();
                self.focus = Focus::Args;
                Some(Action::None)
            }
            KeyCode::Char('x') => Some(self.run_action(false)),
            KeyCode::Char('X') => Some(self.run_action(true)),
            KeyCode::Char('r') => {
                self.state.release = !self.state.release;
                self.state.save();
                self.refresh_builds();
                Some(Action::None)
            }
            KeyCode::Char('v') => {
                self.raw = !self.raw;
                self.raw_scroll = 0;
                Some(Action::None)
            }
            KeyCode::Char('h') => {
                self.request_help(Source::Help);
                Some(Action::None)
            }
            KeyCode::Char('u') => {
                self.request_help(Source::Usage);
                Some(Action::None)
            }
            KeyCode::Char('w') => {
                self.open_worktrees();
                Some(Action::None)
            }
            KeyCode::Char('R') => {
                let dir = self.dir.clone();
                let key = self.selected_key.clone();
                self.load_dir(&dir);
                if let Some(k) = key {
                    self.select_key(&k);
                    self.on_select();
                }
                self.status = Some("reloaded targets and build states".into());
                Some(Action::None)
            }
            KeyCode::Char('d') | KeyCode::Char('o') => {
                self.open_dir_picker();
                Some(Action::None)
            }
            _ => None,
        }
    }

    /// The pane to the right of the targets: their runs, if any, else flags.
    fn right_of_targets(&self) -> Focus {
        if self.runs().is_empty() {
            Focus::Flags
        } else {
            Focus::Runs
        }
    }

    fn key_targets(&mut self, key: KeyEvent) -> Action {
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => self.move_sel(-1),
            KeyCode::Down | KeyCode::Char('j') => self.move_sel(1),
            KeyCode::PageUp => self.move_sel(-10),
            KeyCode::PageDown => self.move_sel(10),
            KeyCode::Home | KeyCode::Char('g') => self.move_sel(-(self.visible.len() as isize)),
            KeyCode::End | KeyCode::Char('G') => self.move_sel(self.visible.len() as isize),
            KeyCode::Char('/') => self.focus = Focus::Filter,
            KeyCode::Right | KeyCode::Char('l') => {
                if self.selected().is_some() {
                    self.focus = self.right_of_targets();
                    self.run_sel = 0;
                }
            }
            KeyCode::Enter => {
                if self.selected().is_some() {
                    self.args.end();
                    self.focus = Focus::Args;
                } else if self.ws.is_none() {
                    self.open_dir_picker();
                }
            }
            KeyCode::Esc => {
                if !self.filter.text.is_empty() {
                    self.filter.clear();
                    self.refilter();
                    self.on_select();
                }
            }
            _ => return self.key_common(key).unwrap_or(Action::None),
        }
        Action::None
    }

    fn key_filter(&mut self, key: KeyEvent) -> Action {
        match key.code {
            KeyCode::Esc => {
                self.filter.clear();
                self.focus = Focus::Targets;
            }
            KeyCode::Enter | KeyCode::Tab | KeyCode::Right => self.focus = Focus::Targets,
            KeyCode::Up => return self.key_targets(key),
            KeyCode::Down => return self.key_targets(key),
            KeyCode::Backspace => self.filter.backspace(),
            KeyCode::Char(c) => self.filter.insert(c),
            _ => {}
        }
        self.refilter();
        self.on_select();
        Action::None
    }

    /// Up and down choose a past run, and its arguments are put in the args line.
    fn key_runs(&mut self, key: KeyEvent) -> Action {
        let runs = self.runs();
        let n = runs.len();
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => self.run_sel = self.run_sel.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => {
                self.run_sel = (self.run_sel + 1).min(n.saturating_sub(1))
            }
            KeyCode::Enter => {
                self.args.end();
                self.focus = Focus::Args;
                return Action::None;
            }
            KeyCode::Delete | KeyCode::Backspace | KeyCode::Char('D') => {
                if let (Some(ws), Some(t), Some(r)) = (&self.ws, self.selected(), runs.get(self.run_sel)) {
                    let (root, k) = (ws.root.clone(), t.key());
                    self.state.remove_run(&root, &k, r);
                    self.state.save();
                    let n = self.runs().len();
                    self.run_sel = self.run_sel.min(n.saturating_sub(1));
                    if n == 0 {
                        self.focus = Focus::Targets;
                    }
                }
            }
            KeyCode::Esc | KeyCode::Left => {
                self.focus = Focus::Targets;
                return Action::None;
            }
            KeyCode::Right => {
                self.focus = Focus::Flags;
                return Action::None;
            }
            _ => return self.key_common(key).unwrap_or(Action::None),
        }
        if let Some(r) = self.runs().get(self.run_sel) {
            self.args.set(&r.args);
        }
        Action::None
    }

    fn key_flags(&mut self, key: KeyEvent) -> Action {
        let n = self.flag_rows().len();
        match key.code {
            KeyCode::Up | KeyCode::Char('k') if self.raw => {
                self.raw_scroll = self.raw_scroll.saturating_sub(1)
            }
            KeyCode::Down | KeyCode::Char('j') if self.raw => {
                self.raw_scroll = self.raw_scroll.saturating_add(1)
            }
            KeyCode::PageUp if self.raw => self.raw_scroll = self.raw_scroll.saturating_sub(10),
            KeyCode::PageDown if self.raw => self.raw_scroll = self.raw_scroll.saturating_add(10),
            KeyCode::Up | KeyCode::Char('k') => self.flag_sel = self.flag_sel.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => {
                self.flag_sel = (self.flag_sel + 1).min(n.saturating_sub(1))
            }
            KeyCode::PageUp => self.flag_sel = self.flag_sel.saturating_sub(10),
            KeyCode::PageDown => self.flag_sel = (self.flag_sel + 10).min(n.saturating_sub(1)),
            // Right and space add the flag and stay; enter adds it and edits.
            KeyCode::Right | KeyCode::Char(' ') | KeyCode::Enter if !self.raw => {
                let rows = self.flag_rows();
                if let Some(ins) = rows.get(self.flag_sel).and_then(|r| r.insert.clone()) {
                    self.insert_arg(&ins);
                }
                if key.code == KeyCode::Enter {
                    self.focus = Focus::Args;
                }
            }
            KeyCode::Esc => self.focus = Focus::Targets,
            KeyCode::Left => self.focus = self.right_of_targets_back(),
            _ => return self.key_common(key).unwrap_or(Action::None),
        }
        Action::None
    }

    /// Left of the flags: the runs, if any, else the targets.
    fn right_of_targets_back(&self) -> Focus {
        match self.right_of_targets() {
            Focus::Runs => Focus::Runs,
            _ => Focus::Targets,
        }
    }

    fn key_args(&mut self, key: KeyEvent) -> Action {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Enter => return self.run_action(false),
            KeyCode::Esc => self.focus = Focus::Targets,
            KeyCode::Tab => {
                if !self.complete_arg() {
                    self.focus = Focus::Targets;
                }
            }
            KeyCode::BackTab => self.focus = Focus::Flags,
            KeyCode::Up | KeyCode::Down => {
                let runs = self.runs();
                if runs.is_empty() {
                    return Action::None;
                }
                let next = match (key.code, self.hist_nav) {
                    (KeyCode::Up, None) => Some(0),
                    (KeyCode::Up, Some(i)) => Some((i + 1).min(runs.len() - 1)),
                    (KeyCode::Down, Some(0)) | (KeyCode::Down, None) => None,
                    (KeyCode::Down, Some(i)) => Some(i - 1),
                    _ => None,
                };
                self.hist_nav = next;
                match next {
                    Some(i) => self.args.set(&runs[i].args),
                    None => self.args.clear(),
                }
            }
            KeyCode::Left => self.args.left(),
            KeyCode::Right => self.args.right(),
            KeyCode::Home => self.args.home(),
            KeyCode::End => self.args.end(),
            KeyCode::Backspace if ctrl => self.args.kill_word(),
            KeyCode::Backspace => self.args.backspace(),
            KeyCode::Delete => self.args.delete(),
            KeyCode::Char('a') if ctrl => self.args.home(),
            KeyCode::Char('e') if ctrl => self.args.end(),
            KeyCode::Char('u') if ctrl => self.args.kill_to_start(),
            KeyCode::Char('k') if ctrl => self.args.kill_to_end(),
            KeyCode::Char('w') if ctrl => self.args.kill_word(),
            KeyCode::Char(c) if !ctrl => self.args.insert(c),
            _ => {}
        }
        Action::None
    }

    fn key_dir(&mut self, key: KeyEvent) -> Action {
        let n = self.dir_candidates().len();
        match key.code {
            KeyCode::Esc => self.focus = Focus::Targets,
            KeyCode::Up => self.dir_sel = self.dir_sel.saturating_sub(1),
            KeyCode::Down => self.dir_sel = (self.dir_sel + 1).min(n.saturating_sub(1)),
            KeyCode::Tab => self.complete_dir(),
            KeyCode::Enter => {
                let choice = self
                    .resolve_dir_input()
                    .or_else(|| self.dir_candidates().get(self.dir_sel).cloned());
                match choice {
                    Some(d) => {
                        self.focus = Focus::Targets;
                        self.load_dir(&d);
                    }
                    None => self.status = Some(format!("not a directory: {}", self.dir_input.text)),
                }
            }
            KeyCode::Backspace => {
                self.dir_input.backspace();
                self.dir_sel = 0;
            }
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.dir_input.clear()
            }
            KeyCode::Char(c) => {
                self.dir_input.insert(c);
                self.dir_sel = 0;
            }
            _ => {}
        }
        Action::None
    }

    fn key_worktrees(&mut self, key: KeyEvent) -> Action {
        let n = self.wt_rows.len();
        match key.code {
            KeyCode::Esc | KeyCode::Left | KeyCode::Char('w') | KeyCode::Char('q') => {
                self.focus = Focus::Targets
            }
            KeyCode::Up | KeyCode::Char('k') => self.wt_sel = self.wt_sel.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => {
                self.wt_sel = (self.wt_sel + 1).min(n.saturating_sub(1))
            }
            KeyCode::Enter | KeyCode::Right => {
                let Some(row) = self.wt_rows.get(self.wt_sel) else {
                    return Action::None;
                };
                let root = row.worktree.root.clone();
                let key = self.selected_key.clone();
                self.focus = Focus::Targets;
                self.load_dir(&root);
                if let Some(k) = key {
                    self.select_key(&k);
                    self.on_select();
                }
            }
            _ => {}
        }
        Action::None
    }
}

fn expand_tilde(s: &str) -> PathBuf {
    if let Some(rest) = s.strip_prefix('~') {
        if let Some(home) = dirs::home_dir() {
            return home.join(rest.trim_start_matches('/'));
        }
    }
    PathBuf::from(s)
}

/// Case-sensitive subsequence match (callers lowercase both sides).
fn fuzzy_match(hay: &str, needle: &str) -> bool {
    let mut it = hay.chars();
    needle.chars().all(|c| it.any(|h| h == c))
}
