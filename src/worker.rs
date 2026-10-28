//! Background thread that builds a target and reads its `--help` or `-h`,
//! when asked to: nothing is built or run unless the user presses a key.

use crate::app::{Request, Response};
use crate::cargo::{self, Target};
use crate::help::{self, HelpInfo, Source};
use std::path::Path;
use std::sync::mpsc::{Receiver, Sender};
use std::time::Duration;

const HELP_TIMEOUT: Duration = Duration::from_secs(5);

pub fn spawn(rx: Receiver<Request>, tx: Sender<Response>) {
    std::thread::spawn(move || {
        while let Ok(req) = rx.recv() {
            let info = describe(&req.root, &req.target, req.release, req.mode);
            let done = Response {
                key: req.key,
                root: req.root,
                info,
            };
            if tx.send(done).is_err() {
                break;
            }
        }
    });
}

/// Builds the target and parses what it prints for `--help` (`mode` Help) or
/// `-h` (Usage). Falls back to scanning the source when that gives no usage
/// text, with a note of why.
pub fn describe(root: &Path, target: &Target, release: bool, mode: Source) -> HelpInfo {
    let flag = if mode == Source::Usage { "-h" } else { "--help" };
    let fallback = |why: String, raw: String| {
        let mut info = crate::source::scan(&target.src_path);
        info.note = Some(match info.note.take() {
            Some(n) => format!("{why}; {n}"),
            None => why,
        });
        info.raw = raw;
        info
    };

    let exe = match cargo::build(root, target, release) {
        Ok(exe) => exe,
        Err(e) => return fallback(format!("build failed: {e}"), String::new()),
    };
    match cargo::run_captured(&exe, &[flag], root, HELP_TIMEOUT) {
        Ok(c) if c.timed_out => fallback(
            format!(
                "`{flag}` did not exit within {}s (the program probably ignores it)",
                HELP_TIMEOUT.as_secs()
            ),
            c.output,
        ),
        Ok(c) => {
            let mut info = help::parse(&c.output);
            if info.looks_like_help() {
                info.source = mode;
                info
            } else {
                fallback(format!("`{flag}` printed no usage text"), c.output)
            }
        }
        Err(e) => fallback(e.to_string(), String::new()),
    }
}
