//! Persistent state: recent directories, argument history per target, and
//! the last selected target per workspace.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

const MAX_DIRS: usize = 20;
const MAX_HISTORY: usize = 30;

#[derive(Default, Serialize, Deserialize)]
#[serde(default)]
pub struct State {
    pub recent_dirs: Vec<PathBuf>,
    /// `<workspace root>#<target key>` -> args lines, most recent first.
    pub history: HashMap<String, Vec<String>>,
    /// workspace root -> target key.
    pub last_target: HashMap<String, String>,
    pub release: bool,
    #[serde(skip)]
    path: Option<PathBuf>,
}

pub fn default_path() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("CARGO_EXPLORE_STATE") {
        return Some(PathBuf::from(p));
    }
    dirs::config_dir().map(|d| d.join("cargo-explore").join("state.json"))
}

fn history_key(root: &Path, target_key: &str) -> String {
    format!("{}#{}", root.display(), target_key)
}

impl State {
    pub fn load(path: Option<PathBuf>) -> State {
        let mut state: State = path
            .as_ref()
            .and_then(|p| std::fs::read(p).ok())
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        state.path = path;
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

    pub fn history(&self, root: &Path, target_key: &str) -> &[String] {
        self.history
            .get(&history_key(root, target_key))
            .map(|v| v.as_slice())
            .unwrap_or(&[])
    }

    pub fn push_args(&mut self, root: &Path, target_key: &str, args: &str) {
        let list = self
            .history
            .entry(history_key(root, target_key))
            .or_default();
        list.retain(|a| a != args);
        list.insert(0, args.to_string());
        list.truncate(MAX_HISTORY);
    }

    pub fn remove_args(&mut self, root: &Path, target_key: &str, index: usize) {
        if let Some(list) = self.history.get_mut(&history_key(root, target_key)) {
            if index < list.len() {
                list.remove(index);
            }
        }
    }

    pub fn set_last_target(&mut self, root: &Path, target_key: &str) {
        self.last_target
            .insert(root.display().to_string(), target_key.to_string());
    }

    pub fn last_target(&self, root: &Path) -> Option<&str> {
        self.last_target
            .get(&root.display().to_string())
            .map(|s| s.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_and_dedups() {
        let dir = std::env::temp_dir().join(format!("cargo-explore-test-{}", std::process::id()));
        let path = dir.join("state.json");
        let mut s = State::load(Some(path.clone()));
        let root = Path::new("/ws");
        s.push_args(root, "k", "-v");
        s.push_args(root, "k", "--x 1");
        s.push_args(root, "k", "-v");
        s.push_dir(Path::new("/a"));
        s.push_dir(Path::new("/b"));
        s.push_dir(Path::new("/a"));
        s.save();
        let s = State::load(Some(path));
        assert_eq!(s.history(root, "k"), ["-v", "--x 1"]);
        assert_eq!(s.recent_dirs, [PathBuf::from("/a"), PathBuf::from("/b")]);
        let _ = std::fs::remove_dir_all(dir);
    }
}
