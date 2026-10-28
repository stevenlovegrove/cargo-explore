# cargo-explore

An interactive terminal UI for browsing, inspecting and running the binaries
and examples of a Cargo project.

```
cargo install --path .
cd some/cargo/project
cargo-explore            # or: cargo explore, or: cargo-explore path/to/project
```

## What it does

- **Finds targets** with `cargo metadata`: every `[[bin]]` and `[[example]]` in
  the workspace, including `required-features` (passed automatically as `--features`).
- **Discovers flags** by building the selected target in the background and running
  it with `--help` (then `-h`), parsing clap 2/3/4, structopt, argh, getopts and
  hand-written usage text into subcommands, positional args and flags (with
  `[possible values: ...]`). Programs that ignore `--help` are killed after 5s.
- **Falls back to the source**: if the build fails or `--help` prints no usage,
  it scans the target's source file for `#[derive(Parser/Args/StructOpt/FromArgs)]`
  structs, and notes when a program reads `std::env::args` directly.
- **Runs** `cargo run -p <pkg> --bin|--example <name> [--release] -- <args>` in the
  directory you started in, handing over the terminal, then returns to the UI.
- **Remembers** recent directories, per-target argument history (the last args are
  pre-filled), the last selected target per workspace, and the debug/release toggle,
  in `<config dir>/cargo-explore/state.json` (override with `CARGO_EXPLORE_STATE`).

## Keys

| Where    | Keys |
|----------|------|
| Targets  | `↑↓`/`jk` select · `/` filter · `enter` edit args · `x` run · `tab` next pane · `d` switch directory · `r` debug/release · `v` raw help · `h` rebuild and re-read help · `R` reload targets · `q` quit |
| Flags    | `↑↓` select · `enter`/`space` insert into args |
| History  | `↑↓` select · `enter` use · `del` forget |
| Args     | `enter` run · `tab` complete a flag · `↑↓` cycle history · `ctrl-a/e/u/k/w` editing · `esc` back |
| Directory picker | type a path (`tab` completes) or filter recent dirs · `enter` open |

## Non-interactive

```
cargo-explore --list                 # list runnable targets
cargo-explore --describe <TARGET>    # print discovered flags (builds and runs --help)
cargo-explore --describe <TARGET> --no-build   # source scan only
```
