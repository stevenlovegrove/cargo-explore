//! Talking to cargo: discovering runnable targets, building them, and
//! assembling `cargo run` command lines.

use anyhow::{anyhow, bail, Context, Result};
use serde_json::Value;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Kind {
    Bin,
    Example,
}

impl Kind {
    pub fn label(self) -> &'static str {
        match self {
            Kind::Bin => "bin",
            Kind::Example => "example",
        }
    }
}

#[derive(Clone, Debug)]
pub struct Target {
    pub package: String,
    pub name: String,
    pub kind: Kind,
    pub src_path: PathBuf,
    pub required_features: Vec<String>,
    /// The `Cargo.toml` of the package.
    pub manifest_path: PathBuf,
    /// Names of the package's dependencies, of every kind.
    pub deps: Vec<String>,
}

impl Target {
    /// Stable identifier used for history and caches.
    pub fn key(&self) -> String {
        format!("{}::{}::{}", self.package, self.kind.label(), self.name)
    }

    /// Cargo arguments that select this target (`-p pkg --bin name`, ...).
    pub fn selector_args(&self) -> Vec<String> {
        let mut args = vec![
            "-p".to_string(),
            self.package.clone(),
            match self.kind {
                Kind::Bin => "--bin",
                Kind::Example => "--example",
            }
            .to_string(),
            self.name.clone(),
        ];
        if !self.required_features.is_empty() {
            args.push("--features".into());
            args.push(self.required_features.join(","));
        }
        args
    }

    /// The full argv for `cargo run` with the given program arguments.
    pub fn run_argv(&self, release: bool, program_args: &[String]) -> Vec<String> {
        let mut argv = vec!["cargo".to_string(), "run".to_string()];
        argv.extend(self.selector_args());
        if release {
            argv.push("--release".into());
        }
        if !program_args.is_empty() {
            argv.push("--".into());
            argv.extend(program_args.iter().cloned());
        }
        argv
    }

    /// Where cargo puts the executable of this target, built or not.
    pub fn exe_path(&self, target_dir: &Path, release: bool) -> PathBuf {
        let mut p = target_dir.join(if release { "release" } else { "debug" });
        if self.kind == Kind::Example {
            p.push("examples");
        }
        p.push(&self.name);
        p
    }
}

#[derive(Clone, Debug)]
pub struct Workspace {
    pub root: PathBuf,
    pub target_dir: PathBuf,
    pub targets: Vec<Target>,
}

/// Whether a target's executable is there and newer than what it was built from.
#[derive(Clone, Debug, PartialEq)]
pub enum BuildState {
    Unbuilt,
    Fresh,
    /// Built, but these files changed after (the first few of them).
    Stale(Vec<PathBuf>),
}

impl BuildState {
    pub fn circle(&self) -> &'static str {
        match self {
            BuildState::Unbuilt => "○",
            BuildState::Fresh => "●",
            BuildState::Stale(_) => "◐",
        }
    }
}

/// Compares the executable's time with those of the files that cargo's
/// dep-info lists for it, the package's manifest and the workspace's lock
/// file. Cheap, stats only; it cannot see a change of features or flags.
pub fn build_state(ws_root: &Path, target_dir: &Path, t: &Target, release: bool) -> BuildState {
    let exe = t.exe_path(target_dir, release);
    let Ok(built) = std::fs::metadata(&exe).and_then(|m| m.modified()) else {
        return BuildState::Unbuilt;
    };
    let mut sources = vec![t.manifest_path.clone(), ws_root.join("Cargo.lock")];
    let dep_info = exe.with_file_name(format!("{}.d", t.name));
    match std::fs::read_to_string(&dep_info) {
        Ok(text) => sources.extend(parse_dep_info(&text)),
        Err(_) => sources.push(t.src_path.clone()),
    }
    let newer: Vec<PathBuf> = sources
        .into_iter()
        .filter(|p| {
            std::fs::metadata(p)
                .and_then(|m| m.modified())
                .is_ok_and(|m| m > built)
        })
        .take(5)
        .collect();
    if newer.is_empty() {
        BuildState::Fresh
    } else {
        BuildState::Stale(newer)
    }
}

/// The prerequisites of a make-style dep-info file (`out: a b\ c ...`).
fn parse_dep_info(text: &str) -> Vec<PathBuf> {
    let Some(line) = text.lines().next() else {
        return Vec::new();
    };
    // The output is an absolute path; its colon is the first one followed by a space.
    let Some(pos) = line.find(": ") else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut chars = line[pos + 2..].chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => {
                if let Some(n) = chars.next() {
                    cur.push(n);
                }
            }
            ' ' => {
                if !cur.is_empty() {
                    out.push(PathBuf::from(std::mem::take(&mut cur)));
                }
            }
            _ => cur.push(c),
        }
    }
    if !cur.is_empty() {
        out.push(PathBuf::from(cur));
    }
    out
}

/// A git worktree of the repository a workspace is in.
#[derive(Clone, Debug)]
pub struct Worktree {
    /// The workspace root inside this worktree.
    pub root: PathBuf,
    pub branch: Option<String>,
    pub head: String,
}

impl Worktree {
    pub fn label(&self) -> String {
        match &self.branch {
            Some(b) => b.clone(),
            None => format!("detached {}", &self.head[..self.head.len().min(7)]),
        }
    }
}

/// The worktrees of the repository that holds `ws_root`, each with the path
/// at which the same workspace sits in it. Empty if it is not in git.
pub fn worktrees(ws_root: &Path) -> Vec<Worktree> {
    let git = |args: &[&str]| {
        Command::new("git")
            .args(args)
            .current_dir(ws_root)
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
    };
    let Some(top) = git(&["rev-parse", "--show-toplevel"]) else {
        return Vec::new();
    };
    let top = PathBuf::from(top.trim());
    let top = top.canonicalize().unwrap_or(top);
    let rel = ws_root
        .strip_prefix(&top)
        .map(Path::to_path_buf)
        .unwrap_or_default();
    let Some(list) = git(&["worktree", "list", "--porcelain"]) else {
        return Vec::new();
    };
    parse_worktrees(&list, &rel)
}

fn parse_worktrees(porcelain: &str, rel: &Path) -> Vec<Worktree> {
    let mut out = Vec::new();
    for block in porcelain.split("\n\n") {
        let mut path = None;
        let mut head = String::new();
        let mut branch = None;
        let mut bare = false;
        for line in block.lines() {
            if let Some(p) = line.strip_prefix("worktree ") {
                path = Some(PathBuf::from(p));
            } else if let Some(h) = line.strip_prefix("HEAD ") {
                head = h.to_string();
            } else if let Some(b) = line.strip_prefix("branch ") {
                branch = Some(b.trim_start_matches("refs/heads/").to_string());
            } else if line == "bare" {
                bare = true;
            }
        }
        if let (Some(p), false) = (path, bare) {
            let root = p.join(rel);
            out.push(Worktree {
                root: root.canonicalize().unwrap_or(root),
                branch,
                head,
            });
        }
    }
    out
}

impl Workspace {
    /// The same target as it would be in another worktree's copy of this
    /// workspace, if its source file is there.
    pub fn target_in(&self, t: &Target, other_root: &Path) -> Option<Target> {
        let moved = |p: &Path| p.strip_prefix(&self.root).ok().map(|r| other_root.join(r));
        let src = moved(&t.src_path)?;
        if !src.is_file() {
            return None;
        }
        Some(Target {
            src_path: src,
            manifest_path: moved(&t.manifest_path)?,
            ..t.clone()
        })
    }

    /// The target directory another worktree's copy of this workspace would
    /// use: moved with it if it is inside the workspace, else shared.
    pub fn target_dir_in(&self, other_root: &Path) -> PathBuf {
        match self.target_dir.strip_prefix(&self.root) {
            Ok(rel) => other_root.join(rel),
            Err(_) => self.target_dir.clone(),
        }
    }
}

/// Runs `cargo metadata` in `dir` and collects every bin and example target
/// of the workspace members.
pub fn discover(dir: &Path) -> Result<Workspace> {
    let out = Command::new("cargo")
        .args(["metadata", "--no-deps", "--format-version", "1"])
        .current_dir(dir)
        .stdin(Stdio::null())
        .output()
        .context("failed to run `cargo metadata` (is cargo on PATH?)")?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        let msg = err
            .lines()
            .find(|l| l.contains("error"))
            .unwrap_or(err.trim());
        bail!("{}", msg.trim());
    }
    parse_metadata(&out.stdout)
}

fn parse_metadata(json: &[u8]) -> Result<Workspace> {
    let v: Value = serde_json::from_slice(json).context("bad cargo metadata json")?;
    let root = PathBuf::from(v["workspace_root"].as_str().unwrap_or("."));
    let target_dir = v["target_directory"]
        .as_str()
        .map(PathBuf::from)
        .unwrap_or_else(|| root.join("target"));
    let mut targets = Vec::new();
    for pkg in v["packages"].as_array().into_iter().flatten() {
        let package = pkg["name"].as_str().unwrap_or_default().to_string();
        let manifest_path = PathBuf::from(pkg["manifest_path"].as_str().unwrap_or_default());
        let deps: Vec<String> = pkg["dependencies"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|d| d["name"].as_str().map(String::from))
            .collect();
        for t in pkg["targets"].as_array().into_iter().flatten() {
            let kinds: Vec<&str> = t["kind"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|k| k.as_str())
                .collect();
            let kind = if kinds.contains(&"bin") {
                Kind::Bin
            } else if kinds.contains(&"example")
                && t["crate_types"]
                    .as_array()
                    .is_some_and(|c| c.iter().any(|x| x == "bin"))
            {
                Kind::Example
            } else {
                continue;
            };
            targets.push(Target {
                package: package.clone(),
                name: t["name"].as_str().unwrap_or_default().to_string(),
                kind,
                src_path: PathBuf::from(t["src_path"].as_str().unwrap_or_default()),
                required_features: t["required-features"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|f| f.as_str().map(String::from))
                    .collect(),
                manifest_path: manifest_path.clone(),
                deps: deps.clone(),
            });
        }
    }
    targets.sort_by(|a, b| (a.kind, &a.package, &a.name).cmp(&(b.kind, &b.package, &b.name)));
    Ok(Workspace {
        root,
        target_dir,
        targets,
    })
}

/// Builds the target and returns the path of the executable.
pub fn build(root: &Path, target: &Target, release: bool) -> Result<PathBuf> {
    let out = Command::new("cargo")
        .arg("build")
        .args(target.selector_args())
        .args(release.then_some("--release"))
        .args(["--message-format", "json-render-diagnostics"])
        .current_dir(root)
        .stdin(Stdio::null())
        .output()
        .context("failed to run `cargo build`")?;
    let stdout = String::from_utf8_lossy(&out.stdout);
    let mut exe = None;
    for line in stdout.lines() {
        let Ok(msg) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if msg["reason"] == "compiler-artifact" && msg["target"]["name"] == target.name.as_str() {
            if let Some(path) = msg["executable"].as_str() {
                exe = Some(PathBuf::from(path));
            }
        }
    }
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        let first_error = err
            .lines()
            .find(|l| l.starts_with("error"))
            .unwrap_or("build failed");
        bail!("{}", first_error.trim());
    }
    exe.ok_or_else(|| anyhow!("cargo did not report an executable for {}", target.name))
}

/// Output of running a program with a timeout.
pub struct Captured {
    pub output: String,
    pub timed_out: bool,
}

/// Runs `exe args...` with stdin closed, capturing stdout and stderr, and
/// kills it if it has not exited after `timeout`.
pub fn run_captured(exe: &Path, args: &[&str], cwd: &Path, timeout: Duration) -> Result<Captured> {
    let mut child = Command::new(exe)
        .args(args)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env("NO_COLOR", "1")
        .env("CLICOLOR", "0")
        .env("COLUMNS", "100")
        .spawn()
        .with_context(|| format!("failed to start {}", exe.display()))?;

    let mut so = child.stdout.take().unwrap();
    let mut se = child.stderr.take().unwrap();
    let t_out = std::thread::spawn(move || {
        let mut s = Vec::new();
        let _ = so.read_to_end(&mut s);
        s
    });
    let t_err = std::thread::spawn(move || {
        let mut s = Vec::new();
        let _ = se.read_to_end(&mut s);
        s
    });

    let start = Instant::now();
    let timed_out = loop {
        if child.try_wait()?.is_some() {
            break false;
        }
        if start.elapsed() > timeout {
            let _ = child.kill();
            let _ = child.wait();
            break true;
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    let mut output = String::from_utf8_lossy(&t_out.join().unwrap_or_default()).into_owned();
    let err = String::from_utf8_lossy(&t_err.join().unwrap_or_default()).into_owned();
    if !err.trim().is_empty() {
        if !output.is_empty() && !output.ends_with('\n') {
            output.push('\n');
        }
        output.push_str(&err);
    }
    Ok(Captured {
        output: strip_ansi(&output),
        timed_out,
    })
}

/// Removes ANSI escape sequences, for programs that color output regardless.
pub fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            if chars.peek() == Some(&'[') {
                chars.next();
                for c in chars.by_ref() {
                    if ('@'..='~').contains(&c) {
                        break;
                    }
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_metadata_targets() {
        let json = br#"{
          "workspace_root": "/ws",
          "packages": [{
            "name": "demo",
            "targets": [
              {"name":"demo","kind":["lib"],"crate_types":["lib"],"src_path":"/ws/src/lib.rs"},
              {"name":"demo","kind":["bin"],"crate_types":["bin"],"src_path":"/ws/src/main.rs"},
              {"name":"hello","kind":["example"],"crate_types":["bin"],"src_path":"/ws/examples/hello.rs","required-features":["fancy"]},
              {"name":"plugin","kind":["example"],"crate_types":["cdylib"],"src_path":"/ws/examples/plugin.rs"},
              {"name":"it","kind":["test"],"crate_types":["bin"],"src_path":"/ws/tests/it.rs"}
            ]
          }]
        }"#;
        let ws = parse_metadata(json).unwrap();
        let names: Vec<_> = ws
            .targets
            .iter()
            .map(|t| (t.kind, t.name.as_str()))
            .collect();
        assert_eq!(names, vec![(Kind::Bin, "demo"), (Kind::Example, "hello")]);
        assert_eq!(
            ws.targets[1].run_argv(true, &["-v".into()]),
            [
                "cargo",
                "run",
                "-p",
                "demo",
                "--example",
                "hello",
                "--features",
                "fancy",
                "--release",
                "--",
                "-v"
            ]
        );
        assert_eq!(ws.target_dir, PathBuf::from("/ws/target"));
        assert_eq!(
            ws.targets[1].exe_path(&ws.target_dir, true),
            PathBuf::from("/ws/target/release/examples/hello")
        );
        assert_eq!(
            ws.targets[0].exe_path(&ws.target_dir, false),
            PathBuf::from("/ws/target/debug/demo")
        );
    }

    #[test]
    fn parses_dep_info() {
        let d = "/t/debug/x: /a/src/main.rs /b/with\\ space.rs\n\n/a/src/main.rs:\n";
        assert_eq!(
            parse_dep_info(d),
            [PathBuf::from("/a/src/main.rs"), PathBuf::from("/b/with space.rs")]
        );
    }

    #[test]
    fn build_state_follows_times() {
        let dir = std::env::temp_dir().join(format!("cargo-explore-build-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("target/debug")).unwrap();
        let src = dir.join("main.rs");
        let manifest = dir.join("Cargo.toml");
        std::fs::write(&src, "fn main() {}").unwrap();
        std::fs::write(&manifest, "").unwrap();
        let t = Target {
            package: "p".into(),
            name: "x".into(),
            kind: Kind::Bin,
            src_path: src.clone(),
            required_features: Vec::new(),
            manifest_path: manifest,
            deps: Vec::new(),
        };
        let td = dir.join("target");
        assert_eq!(build_state(&dir, &td, &t, false), BuildState::Unbuilt);
        std::thread::sleep(Duration::from_millis(20));
        std::fs::write(td.join("debug/x"), "").unwrap();
        std::fs::write(td.join("debug/x.d"), format!("{}: {}\n", td.join("debug/x").display(), src.display())).unwrap();
        assert_eq!(build_state(&dir, &td, &t, false), BuildState::Fresh);
        std::thread::sleep(Duration::from_millis(20));
        std::fs::write(&src, "fn main() { }").unwrap();
        assert_eq!(build_state(&dir, &td, &t, false), BuildState::Stale(vec![src]));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn parses_worktree_list() {
        let p = "worktree /r\nHEAD 1111111111\nbranch refs/heads/main\n\nworktree /w/x\nHEAD 2222222222abc\ndetached\n\nworktree /bare\nbare\n";
        let w = parse_worktrees(p, Path::new("sub"));
        assert_eq!(w.len(), 2);
        assert_eq!(w[0].root, PathBuf::from("/r/sub"));
        assert_eq!(w[0].label(), "main");
        assert_eq!(w[1].label(), "detached 2222222");
    }
}
