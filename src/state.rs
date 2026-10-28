//! Persistent state: recent directories, the last selected target per
//! workspace, and for each target in each worktree the flags read from it
//! and the arguments it was run with.
//!
//! A worktree is named by the root of its copy of the workspace. Where a
//! target has no record in the worktree asked for, the newest record of the
//! same target in another worktree stands in, and says where it is from.

use crate::help::HelpInfo;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

const MAX_DIRS: usize = 20;
const MAX_RUNS: usize = 30;

/// Flags read from a program, and when.
#[derive(Clone, Serialize, Deserialize)]
pub struct ParamRecord {
    pub info: HelpInfo,
    /// Seconds since the Unix epoch.
    pub updated: u64,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct RunRecord {
    pub args: String,
    /// Seconds since the Unix epoch; 0 if not known.
    pub when: u64,
}

/// A run as listed for a target: from this worktree, or from `other`.
#[derive(Clone)]
pub struct RunView {
    pub args: String,
    pub when: u64,
    pub other: Option<PathBuf>,
}

#[derive(Default, Serialize, Deserialize)]
#[serde(default)]
pub struct State {
    pub recent_dirs: Vec<PathBuf>,
    /// workspace root -> target key.
    pub last_target: HashMap<String, String>,
    pub release: bool,
    /// target key -> worktree root -> flags.
    pub params: HashMap<String, HashMap<String, ParamRecord>>,
    /// target key -> worktree root -> runs, most recent first.
    pub runs: HashMap<String, HashMap<String, Vec<RunRecord>>>,
    /// The format before runs: `<root>#<target key>` -> args. Read once.
    #[serde(skip_serializing)]
    history: HashMap<String, Vec<String>>,
    #[serde(skip)]
    path: Option<PathBuf>,
}

pub fn default_path() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("CARGO_EXPLORE_STATE") {
        return Some(PathBuf::from(p));
    }
    dirs::config_dir().map(|d| d.join("cargo-explore").join("state.json"))
}

pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// How long ago a time was, shortly: `just now`, `5m ago`, `3d ago`.
pub fn ago(when: u64) -> String {
    if when == 0 {
        return "some time ago".into();
    }
    let s = now().saturating_sub(when);
    match s {
        0..60 => "just now".into(),
        60..3600 => format!("{}m ago", s / 60),
        3600..86400 => format!("{}h ago", s / 3600),
        _ => format!("{}d ago", s / 86400),
    }
}

fn root_key(root: &Path) -> String {
    root.display().to_string()
}

impl State {
    pub fn load(path: Option<PathBuf>) -> State {
        let mut state: State = path
            .as_ref()
            .and_then(|p| std::fs::read(p).ok())
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        state.path = path;
        for (k, list) in std::mem::take(&mut state.history) {
            let Some((root, target)) = k.rsplit_once('#') else {
                continue;
            };
            let runs = state
                .runs
                .entry(target.to_string())
                .or_default()
                .entry(root.to_string())
                .or_default();
            for args in list {
                if !runs.iter().any(|r| r.args == args) {
                    runs.push(RunRecord { args, when: 0 });
                }
            }
        }
        state
    }

    pub fn save(&self) {
        let Some(path) = &self.path else { return };
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Ok(json) = serde_json::to_vec_pretty(self) {
            let tmp = path.with_extension("json.tmp");
            if std::fs::write(&tmp, json).is_ok() {
                let _ = std::fs::rename(&tmp, path);
            }
        }
    }

    pub fn push_dir(&mut self, dir: &Path) {
        self.recent_dirs.retain(|d| d != dir);
        self.recent_dirs.insert(0, dir.to_path_buf());
        self.recent_dirs.truncate(MAX_DIRS);
    }

    /// The flags of a target in this worktree, or else the newest record of
    /// it in another, with that worktree's root.
    pub fn params(&self, root: &Path, target_key: &str) -> Option<(&ParamRecord, Option<PathBuf>)> {
        let by_root = self.params.get(target_key)?;
        if let Some(r) = by_root.get(&root_key(root)) {
            return Some((r, None));
        }
        by_root
            .iter()
            .max_by_key(|(_, r)| r.updated)
            .map(|(other, r)| (r, Some(PathBuf::from(other))))
    }

    pub fn set_params(&mut self, root: &Path, target_key: &str, info: HelpInfo) {
        self.params.entry(target_key.to_string()).or_default().insert(
            root_key(root),
            ParamRecord {
                info,
                updated: now(),
            },
        );
    }

    /// The runs of a target: this worktree's, newest first, then those of
    /// other worktrees with arguments not run here, newest first.
    pub fn runs(&self, root: &Path, target_key: &str) -> Vec<RunView> {
        let Some(by_root) = self.runs.get(target_key) else {
            return Vec::new();
        };
        let here = root_key(root);
        let mut out: Vec<RunView> = by_root
            .get(&here)
            .into_iter()
            .flatten()
            .map(|r| RunView {
                args: r.args.clone(),
                when: r.when,
                other: None,
            })
            .collect();
        let mut others: Vec<RunView> = by_root
            .iter()
            .filter(|(k, _)| **k != here)
            .flat_map(|(k, list)| {
                list.iter().map(move |r| RunView {
                    args: r.args.clone(),
                    when: r.when,
                    other: Some(PathBuf::from(k)),
                })
            })
            .collect();
        others.sort_by_key(|r| std::cmp::Reverse(r.when));
        for o in others {
            if !out.iter().any(|r| r.args == o.args) {
                out.push(o);
            }
        }
        out
    }

    pub fn push_run(&mut self, root: &Path, target_key: &str, args: &str) {
        let list = self
            .runs
            .entry(target_key.to_string())
            .or_default()
            .entry(root_key(root))
            .or_default();
        list.retain(|a| a.args != args);
        list.insert(
            0,
            RunRecord {
                args: args.to_string(),
                when: now(),
            },
        );
        list.truncate(MAX_RUNS);
    }

    /// Forgets a run, in the worktree it was recorded in.
    pub fn remove_run(&mut self, root: &Path, target_key: &str, run: &RunView) {
        let root = run.other.as_deref().unwrap_or(root);
        if let Some(list) = self
            .runs
            .get_mut(target_key)
            .and_then(|m| m.get_mut(&root_key(root)))
        {
            list.retain(|r| r.args != run.args);
        }
    }

    pub fn set_last_target(&mut self, root: &Path, target_key: &str) {
        self.last_target
            .insert(root_key(root), target_key.to_string());
    }

    pub fn last_target(&self, root: &Path) -> Option<&str> {
        self.last_target.get(&root_key(root)).map(|s| s.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_state(name: &str) -> (PathBuf, State) {
        let dir = std::env::temp_dir().join(format!("cargo-explore-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("state.json");
        (dir, State::load(Some(path)))
    }

    #[test]
    fn round_trips_and_dedups() {
        let (dir, mut s) = temp_state("state");
        let root = Path::new("/ws");
        s.push_run(root, "k", "-v");
        s.push_run(root, "k", "--x 1");
        s.push_run(root, "k", "-v");
        s.push_dir(Path::new("/a"));
        s.push_dir(Path::new("/b"));
        s.push_dir(Path::new("/a"));
        s.save();
        let s = State::load(Some(dir.join("state.json")));
        let args: Vec<_> = s.runs(root, "k").into_iter().map(|r| r.args).collect();
        assert_eq!(args, ["-v", "--x 1"]);
        assert_eq!(s.recent_dirs, [PathBuf::from("/a"), PathBuf::from("/b")]);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn other_worktrees_stand_in() {
        let (dir, mut s) = temp_state("worktrees");
        let (a, b) = (Path::new("/a"), Path::new("/b"));
        s.set_params(b, "k", HelpInfo {
            about: "from b".into(),
            ..Default::default()
        });
        let (r, other) = s.params(a, "k").unwrap();
        assert_eq!(r.info.about, "from b");
        assert_eq!(other.as_deref(), Some(b));
        s.set_params(a, "k", HelpInfo::default());
        assert!(s.params(a, "k").unwrap().1.is_none());

        s.push_run(b, "k", "--both");
        s.push_run(b, "k", "--only-b");
        s.push_run(a, "k", "--both");
        let runs = s.runs(a, "k");
        let seen: Vec<_> = runs.iter().map(|r| (r.args.as_str(), r.other.is_some())).collect();
        assert_eq!(seen, [("--both", false), ("--only-b", true)]);
        s.remove_run(a, "k", &runs[1]);
        assert_eq!(s.runs(b, "k").len(), 1);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn reads_the_old_history() {
        let (dir, _) = temp_state("old");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("state.json");
        std::fs::write(&path, r#"{"history": {"/ws#p::bin::x": ["-a", "-b"]}}"#).unwrap();
        let s = State::load(Some(path));
        let args: Vec<_> = s.runs(Path::new("/ws"), "p::bin::x").into_iter().map(|r| r.args).collect();
        assert_eq!(args, ["-a", "-b"]);
        let _ = std::fs::remove_dir_all(dir);
    }
}
