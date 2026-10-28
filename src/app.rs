//! Application state and key handling.

use crate::cargo::{Target, Workspace};
use crate::help::{HelpInfo, Source};
use crate::input::LineInput;
use crate::state::State;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Focus {
    Targets,
    Filter,
    Flags,
    History,
    Args,
    DirPicker,
}

pub enum HelpState {
    Loading,
    Ready(HelpInfo),
}

pub struct Request {
    pub key: String,
    pub root: PathBuf,
    pub target: Target,
}

pub enum Response {
    Done(String, HelpInfo),
    Skipped(String),
}

pub enum Action {
    None,
    Quit,
    Run(Vec<String>),
}

/// One row of the flags pane.
pub struct FlagRow {
    pub kind: &'static str,
    pub spec: String,
    pub desc: String,
    /// Text inserted into the args line when chosen.
    pub insert: Option<String>,
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
    pub hist_sel: usize,
    hist_nav: Option<usize>,
    pub raw: bool,
    pub raw_scroll: u16,
    pub helps: HashMap<String, HelpState>,
    scans: HashMap<String, HelpInfo>,
    pub state: State,
    tx: Sender<Request>,
    pub status: Option<String>,
    pub dir_input: LineInput,
    pub dir_sel: usize,
    selected_key: Option<String>,
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
            hist_sel: 0,
            hist_nav: None,
            raw: false,
            raw_scroll: 0,
            helps: HashMap::new(),
            scans: HashMap::new(),
            state,
            tx,
            status: None,
            dir_input: LineInput::default(),
            dir_sel: 0,
            selected_key: None,
        };
        app.load_dir(&dir);
        app
    }

    pub fn load_dir(&mut self, dir: &Path) {
        let dir = dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf());
        self.dir = dir.clone();
        self.helps.clear();
        self.scans.clear();
        self.selected_key = None;
        self.filter.clear();
        self.sel = 0;
        match crate::cargo::discover(&dir) {
            Ok(ws) => {
                self.state.push_dir(&dir);
                self.state.save();
                self.error = None;
                self.ws = Some(ws);
            }
            Err(e) => {
                self.ws = None;
                self.error = Some(e.to_string());
            }
        }
        self.refilter();
        if let Some(ws) = &self.ws {
            if let Some(last) = self.state.last_target(&ws.root) {
                if let Some(pos) = self
                    .visible
                    .iter()
                    .position(|&i| ws.targets[i].key() == last)
                {
                    self.sel = pos;
                }
            }
        }
        self.on_select();
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

    pub fn history(&self) -> &[String] {
        match (&self.ws, self.selected()) {
            (Some(ws), Some(t)) => self.state.history(&ws.root, &t.key()),
            _ => &[],
        }
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
        self.hist_sel = 0;
        self.hist_nav = None;
        self.raw_scroll = 0;
        let root = self.ws.as_ref().unwrap().root.clone();
        let last_args = self
            .state
            .history(&root, &key)
            .first()
            .cloned()
            .unwrap_or_default();
        self.args.set(&last_args);
        self.state.set_last_target(&root, &key);
        self.scans
            .entry(key.clone())
            .or_insert_with(|| crate::source::scan(&t.src_path));
        if !self.helps.contains_key(&key) {
            self.request_help(root, t);
        }
    }

    fn request_help(&mut self, root: PathBuf, target: Target) {
        let key = target.key();
        self.helps.insert(key.clone(), HelpState::Loading);
        let _ = self.tx.send(Request { key, root, target });
    }

    pub fn on_response(&mut self, r: Response) {
        match r {
            Response::Done(key, info) => {
                self.helps.insert(key, HelpState::Ready(info));
            }
            Response::Skipped(key) => {
                self.helps.remove(&key);
                if self.selected_key.as_deref() == Some(key.as_str()) {
                    if let (Some(ws), Some(t)) = (&self.ws, self.selected().cloned()) {
                        self.request_help(ws.root.clone(), t);
                    }
                }
            }
        }
    }

    /// The best available help for the selected target, and whether a
    /// `--help` fetch is still running.
    pub fn current_help(&self) -> Option<(&HelpInfo, bool)> {
        let key = self.selected_key.as_ref()?;
        match self.helps.get(key) {
            Some(HelpState::Ready(h)) => Some((h, false)),
            Some(HelpState::Loading) => self.scans.get(key).map(|h| (h, true)),
            None => self.scans.get(key).map(|h| (h, false)),
        }
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
        let before = self.args.before_cursor();
        if !before.is_empty() && !before.ends_with(' ') {
            self.args.insert(' ');
        }
        self.args.insert_str(s);
        if s.ends_with(' ') {
            return;
        }
        if self.args.cursor == self.args.text.chars().count() {
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

    fn run_action(&mut self) -> Action {
        let Some(t) = self.selected().cloned() else {
            return Action::None;
        };
        match self.program_args() {
            Ok(args) => {
                let root = self.ws.as_ref().unwrap().root.clone();
                self.state.push_args(&root, &t.key(), self.args.text.trim());
                self.state.save();
                self.hist_sel = 0;
                self.hist_nav = None;
                Action::Run(t.run_argv(self.state.release, &args))
            }
            Err(e) => {
                self.status = Some(e);
                Action::None
            }
        }
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
        match self.focus {
            Focus::Targets => self.key_targets(key),
            Focus::Filter => self.key_filter(key),
            Focus::Flags => self.key_flags(key),
            Focus::History => self.key_history(key),
            Focus::Args => self.key_args(key),
            Focus::DirPicker => self.key_dir(key),
        }
    }

    fn open_dir_picker(&mut self) {
        self.dir_input.clear();
        self.dir_sel = 0;
        self.focus = Focus::DirPicker;
    }

    /// Keys shared by the list panes.
    fn key_common(&mut self, key: KeyEvent) -> Option<Action> {
        match key.code {
            KeyCode::Char('q') => Some(Action::Quit),
            KeyCode::Tab => {
                self.focus = match self.focus {
                    Focus::Targets => Focus::Flags,
                    Focus::Flags => Focus::History,
                    Focus::History => Focus::Args,
                    _ => Focus::Targets,
                };
                Some(Action::None)
            }
            KeyCode::BackTab => {
                self.focus = match self.focus {
                    Focus::Targets => Focus::Args,
                    Focus::Flags => Focus::Targets,
                    Focus::History => Focus::Flags,
                    _ => Focus::History,
                };
                Some(Action::None)
            }
            KeyCode::Char('i') | KeyCode::Char('a') => {
                self.args.end();
                self.focus = Focus::Args;
                Some(Action::None)
            }
            KeyCode::Char('x') | KeyCode::F(5) => Some(self.run_action()),
            KeyCode::Char('r') => {
                self.state.release = !self.state.release;
                self.state.save();
                Some(Action::None)
            }
            KeyCode::Char('v') => {
                self.raw = !self.raw;
                self.raw_scroll = 0;
                Some(Action::None)
            }
            KeyCode::Char('h') => {
                if let (Some(ws), Some(t)) = (&self.ws, self.selected().cloned()) {
                    let root = ws.root.clone();
                    self.scans.insert(t.key(), crate::source::scan(&t.src_path));
                    self.request_help(root, t);
                    self.status = Some("rebuilding and re-reading --help".into());
                }
                Some(Action::None)
            }
            KeyCode::Char('R') => {
                let dir = self.dir.clone();
                self.load_dir(&dir);
                self.status = Some("reloaded targets".into());
                Some(Action::None)
            }
            KeyCode::Char('d') | KeyCode::Char('o') => {
                self.open_dir_picker();
                Some(Action::None)
            }
            _ => None,
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
            KeyCode::Enter | KeyCode::Right | KeyCode::Char('l') => {
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
            KeyCode::Enter | KeyCode::Tab => self.focus = Focus::Targets,
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
            KeyCode::Enter | KeyCode::Char(' ') if !self.raw => {
                let rows = self.flag_rows();
                if let Some(row) = rows.get(self.flag_sel) {
                    if let Some(ins) = &row.insert {
                        self.insert_arg(ins);
                    }
                    self.focus = Focus::Args;
                }
            }
            KeyCode::Esc | KeyCode::Left => self.focus = Focus::Targets,
            _ => return self.key_common(key).unwrap_or(Action::None),
        }
        Action::None
    }

    fn key_history(&mut self, key: KeyEvent) -> Action {
        let n = self.history().len();
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => self.hist_sel = self.hist_sel.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => {
                self.hist_sel = (self.hist_sel + 1).min(n.saturating_sub(1))
            }
            KeyCode::Enter => {
                if let Some(a) = self.history().get(self.hist_sel).cloned() {
                    self.args.set(&a);
                    self.focus = Focus::Args;
                }
            }
            KeyCode::Delete | KeyCode::Backspace | KeyCode::Char('D') => {
                if let (Some(ws), Some(t)) = (&self.ws, self.selected()) {
                    let (root, k) = (ws.root.clone(), t.key());
                    self.state.remove_args(&root, &k, self.hist_sel);
                    self.state.save();
                    self.hist_sel = self.hist_sel.min(self.history().len().saturating_sub(1));
                }
            }
            KeyCode::Esc | KeyCode::Left => self.focus = Focus::Targets,
            _ => return self.key_common(key).unwrap_or(Action::None),
        }
        Action::None
    }

    fn key_args(&mut self, key: KeyEvent) -> Action {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Enter => return self.run_action(),
            KeyCode::Esc => self.focus = Focus::Targets,
            KeyCode::Tab => {
                if !self.complete_arg() {
                    self.focus = Focus::Targets;
                }
            }
            KeyCode::BackTab => self.focus = Focus::History,
            KeyCode::Up | KeyCode::Down => {
                let hist = self.history().to_vec();
                if hist.is_empty() {
                    return Action::None;
                }
                let next = match (key.code, self.hist_nav) {
                    (KeyCode::Up, None) => Some(0),
                    (KeyCode::Up, Some(i)) => Some((i + 1).min(hist.len() - 1)),
                    (KeyCode::Down, Some(0)) | (KeyCode::Down, None) => None,
                    (KeyCode::Down, Some(i)) => Some(i - 1),
                    _ => None,
                };
                self.hist_nav = next;
                match next {
                    Some(i) => self.args.set(&hist[i]),
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

    pub fn source_label(&self) -> &'static str {
        match self.current_help() {
            Some((_, true)) => "reading --help…",
            Some((h, false)) if h.source == Source::Help => "from --help",
            Some(_) => "from source scan",
            None => "",
        }
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
