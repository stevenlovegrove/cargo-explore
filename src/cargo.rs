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
}

#[derive(Clone, Debug)]
pub struct Workspace {
    pub root: PathBuf,
    pub targets: Vec<Target>,
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
    let mut targets = Vec::new();
    for pkg in v["packages"].as_array().into_iter().flatten() {
        let package = pkg["name"].as_str().unwrap_or_default().to_string();
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
            });
        }
    }
    targets.sort_by(|a, b| (a.kind, &a.package, &a.name).cmp(&(b.kind, &b.package, &b.name)));
    Ok(Workspace { root, targets })
}

/// Builds the target (debug profile) and returns the path of the executable.
pub fn build(root: &Path, target: &Target) -> Result<PathBuf> {
    let out = Command::new("cargo")
        .arg("build")
        .args(target.selector_args())
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
    pub success: bool,
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
    let (status, timed_out) = loop {
        if let Some(status) = child.try_wait()? {
            break (Some(status), false);
        }
        if start.elapsed() > timeout {
            let _ = child.kill();
            let _ = child.wait();
            break (None, true);
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
        success: status.is_some_and(|s| s.success()),
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
    }
}
