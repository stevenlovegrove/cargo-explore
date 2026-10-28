mod app;
mod cargo;
mod help;
mod input;
mod source;
mod state;
mod ui;
mod worker;

use anyhow::{bail, Result};
use app::{Action, App};
use clap::Parser;
use ratatui::crossterm::event::{self, Event, KeyEventKind};
use ratatui::DefaultTerminal;
use std::io::Write;
use std::path::PathBuf;
use std::process::Command;
use std::sync::mpsc;
use std::time::Duration;

/// Browse, inspect and run the binaries and examples of a Cargo project.
///
/// Starts an interactive terminal UI in DIR (default: the current directory).
/// Also works as a cargo subcommand: `cargo explore [DIR]`.
#[derive(Parser)]
#[command(version, bin_name = "cargo-explore")]
struct Cli {
    /// Directory inside a Cargo project
    dir: Option<PathBuf>,

    /// Print the runnable targets and exit
    #[arg(short, long)]
    list: bool,

    /// Print the flags discovered for TARGET (a bin or example name) and exit
    #[arg(short, long, value_name = "TARGET")]
    describe: Option<String>,

    /// With --describe: only scan the source, don't build and run --help
    #[arg(long, requires = "describe")]
    no_build: bool,
}

fn main() -> Result<()> {
    // Invoked as `cargo explore ...`, cargo passes "explore" as the first argument.
    let mut argv: Vec<String> = std::env::args().collect();
    if argv.get(1).is_some_and(|a| a == "explore") {
        argv.remove(1);
    }
    let cli = Cli::parse_from(argv);
    let dir = match cli.dir {
        Some(d) => d,
        None => std::env::current_dir()?,
    };

    if cli.list {
        let ws = cargo::discover(&dir)?;
        for t in &ws.targets {
            println!("{:<8} {:<30} {}", t.kind.label(), t.name, t.package);
        }
        return Ok(());
    }
    if let Some(name) = cli.describe {
        return describe(&dir, &name, cli.no_build);
    }

    // Keep the explorer alive when the user hits Ctrl-C in a program it runs;
    // the child still gets the signal because handlers reset on exec.
    let _ = ctrlc::set_handler(|| {});

    let state = state::State::load(state::default_path());
    let (req_tx, req_rx) = mpsc::channel();
    let (resp_tx, resp_rx) = mpsc::channel();
    worker::spawn(req_rx, resp_tx);
    let mut app = App::new(dir, state, req_tx);

    let mut terminal = ratatui::init();
    let result = run(&mut terminal, &mut app, &resp_rx);
    ratatui::restore();
    app.state.save();
    result
}

fn run(
    terminal: &mut DefaultTerminal,
    app: &mut App,
    responses: &mpsc::Receiver<app::Response>,
) -> Result<()> {
    loop {
        while let Ok(r) = responses.try_recv() {
            app.on_response(r);
        }
        terminal.draw(|f| ui::draw(f, app))?;
        if !event::poll(Duration::from_millis(100))? {
            continue;
        }
        let Event::Key(key) = event::read()? else {
            continue;
        };
        if key.kind != KeyEventKind::Press {
            continue;
        }
        match app.handle_key(key) {
            Action::None => {}
            Action::Quit => return Ok(()),
            Action::Run(argv) => {
                ratatui::restore();
                let quit = run_foreground(app, &argv);
                *terminal = ratatui::init();
                if quit {
                    return Ok(());
                }
            }
        }
    }
}

/// Runs the command with the terminal handed over, then waits for the user.
/// Returns true if the user asked to quit.
fn run_foreground(app: &mut App, argv: &[String]) -> bool {
    let shown = shlex::try_join(argv.iter().map(String::as_str)).unwrap_or_else(|_| argv.join(" "));
    println!("\x1b[1;36m$ {shown}\x1b[0m   (in {})", app.dir.display());
    let status = Command::new(&argv[0])
        .args(&argv[1..])
        .current_dir(&app.dir)
        .status();
    let summary = match status {
        Ok(s) if s.success() => "\x1b[32mexited successfully\x1b[0m".to_string(),
        Ok(s) => format!("\x1b[31m{s}\x1b[0m"),
        Err(e) => format!("\x1b[31mfailed to start: {e}\x1b[0m"),
    };
    print!("\n{summary} · press Enter to return to cargo-explore, or q + Enter to quit ");
    let _ = std::io::stdout().flush();
    let mut line = String::new();
    let _ = std::io::stdin().read_line(&mut line);
    line.trim() == "q"
}

fn describe(dir: &std::path::Path, name: &str, no_build: bool) -> Result<()> {
    let ws = cargo::discover(dir)?;
    let Some(t) = ws.targets.iter().find(|t| t.name == name) else {
        bail!("no bin or example named {name:?}; try --list");
    };
    let h = if no_build {
        source::scan(&t.src_path)
    } else {
        worker::describe(&ws.root, t)
    };
    println!(
        "{} ({} of {}) [{}]",
        t.name,
        t.kind.label(),
        t.package,
        match h.source {
            help::Source::Help => "from --help",
            help::Source::SourceScan => "from source scan",
        }
    );
    if !h.about.is_empty() {
        println!("about: {}", h.about);
    }
    if !h.usage.is_empty() {
        println!("usage: {}", h.usage);
    }
    if let Some(n) = &h.note {
        println!("note:  {n}");
    }
    for s in &h.subcommands {
        println!("  cmd  {:<32} {}", s.name, s.desc);
    }
    for p in &h.positionals {
        println!("  arg  {:<32} {}", p.name, p.desc);
    }
    for f in &h.flags {
        println!("       {:<32} {}", f.spec(), f.desc);
    }
    Ok(())
}
