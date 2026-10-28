//! Background thread that builds targets and reads their `--help`.

use crate::app::{Request, Response};
use crate::cargo::{self, Target};
use crate::help::{self, HelpInfo};
use std::path::Path;
use std::sync::mpsc::{Receiver, Sender};
use std::time::Duration;

const HELP_TIMEOUT: Duration = Duration::from_secs(5);

pub fn spawn(rx: Receiver<Request>, tx: Sender<Response>) {
    std::thread::spawn(move || {
        while let Ok(mut req) = rx.recv() {
            // Only the newest request matters; the user has moved on.
            while let Ok(newer) = rx.try_recv() {
                let _ = tx.send(Response::Skipped(req.key));
                req = newer;
            }
            let info = describe(&req.root, &req.target);
            if tx.send(Response::Done(req.key, info)).is_err() {
                break;
            }
        }
    });
}

/// Builds the target and parses its `--help`, falling back to scanning the
/// source when that doesn't produce usage text.
pub fn describe(root: &Path, target: &Target) -> HelpInfo {
    let fallback = |why: String| {
        let mut info = crate::source::scan(&target.src_path);
        info.note = Some(match info.note.take() {
            Some(n) => format!("{why}; {n}"),
            None => why,
        });
        info
    };

    let exe = match cargo::build(root, target) {
        Ok(exe) => exe,
        Err(e) => return fallback(format!("build failed: {e}")),
    };

    let mut last_output = String::new();
    for flag in ["--help", "-h"] {
        match cargo::run_captured(&exe, &[flag], root, HELP_TIMEOUT) {
            Ok(c) if c.timed_out => {
                return fallback(format!(
                    "`{flag}` did not exit within {}s (the program probably ignores it)",
                    HELP_TIMEOUT.as_secs()
                ));
            }
            Ok(c) => {
                let info = help::parse(&c.output);
                if info.looks_like_help() {
                    return info;
                }
                last_output = c.output;
                if c.success && !last_output.trim().is_empty() {
                    // It ran normally and printed something else; don't try again.
                    break;
                }
            }
            Err(e) => return fallback(e.to_string()),
        }
    }
    let mut info = fallback("`--help` printed no usage text".into());
    info.raw = last_output;
    info
}
