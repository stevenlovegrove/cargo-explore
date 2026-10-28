# cargo-explore

An interactive terminal UI for browsing, inspecting and running the binaries
and examples of a Cargo project, by the arrow keys.

```
cargo install --path .
cd some/cargo/project
cargo-explore            # or: cargo explore, or: cargo-explore path/to/project
```

## What it does

- **Finds targets** with `cargo metadata`: every `[[bin]]` and `[[example]]` in
  the workspace, including `required-features` (passed automatically as `--features`).
- **Shows whether each is built**: `●` built and up to date, `◐` built but a file
  it was built from (by cargo's dep-info), its `Cargo.toml` or the `Cargo.lock`
  has changed since, `○` not built, for the debug or release profile as chosen.
  It compares file times only, so a change of features or flags is not seen.
- **Builds and runs nothing to show a target.** Its flags come from its source:
  the fields of its clap/structopt/argh derive structs, with `#[command(flatten)]`
  followed into the modules it declares, or the `"--flags"` spelled in a parser
  written by hand. The parser it uses is named (`clap, derive`, `std::env::args,
  by hand`, ...) from its source and its package's dependencies.
- **Reads `--help` when asked**: `h` builds the target and reads its `--help`,
  `u` its `-h`. Programs that ignore them are killed after 5s. What is read is
  kept for that target in that worktree, and the corner of the flags pane says
  when it was updated. A target with nothing kept in this worktree shows what
  was kept for it in another, marked as from there, so that `h` can refresh it.
- **Remembers runs** of each target in each worktree, newest first, with those of
  other worktrees after them, marked. The arguments of the last are filled in.
- **Switches worktree**: `w` lists the git worktrees whose copy of the workspace
  has the selected target, with whether it is built in each, and opens one.
- **Runs** `cargo run -p <pkg> --bin|--example <name> [--release] -- <args>`, or with
  `X` the executable as it was last built, with no build, in the directory you
  started in. The explorer becomes the program (an exec): nothing of it is left
  or printed once the program ends.

State is in `<config dir>/cargo-explore/state.json` (override with `CARGO_EXPLORE_STATE`).

## Keys

Up and down select; left and right move between the targets, their recent runs and
their flags. Enter (or cmd-enter) runs at once, from any pane, the command line
shown at the bottom: the selected target, debug or release, with the args shown,
which are those of its last run (in this worktree, else another) until changed.

| Where    | Keys |
|----------|------|
| Anywhere | `enter`, `cmd-enter` (also `ctrl-enter`, `alt-enter`, `F5`, `x`) run the command shown |
| Targets  | `↑↓` select · `→` runs, then flags · `e` edit args · `X` run as built · `h` read `--help` · `u` read `-h` · `w` worktree · `/` filter · `r` debug/release · `v` raw help · `R` reload · `d` directory · `q` quit |
| Runs     | `↑↓` choose (fills the args; the first is always no args) · `←→` panes · `e` edit · `del` forget |
| Flags    | `↑↓` select · `→`/`space` add to args · `e` edit args · `←` back |
| Args     | `enter` run · `tab` complete a flag · `↑↓` recent runs · `ctrl-a/e/u/k/w` editing · `esc` back |
| Worktrees | `↑↓` select · `enter`/`→` open · `esc`/`←` cancel |
| Directory picker | type a path (`tab` completes) or filter recent dirs · `enter` open |

Text is drawn in the terminal's own colours, with no greys, so that it reads on a
light or a dark background alike.

## Non-interactive

```
cargo-explore --list                 # list runnable targets, with whether built
cargo-explore --describe <TARGET>    # print discovered flags (builds and runs --help)
cargo-explore --describe <TARGET> --no-build   # source scan only
```
