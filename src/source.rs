//! Build-free flag discovery from a target's source: which parser it uses,
//! and the fields of its clap/structopt/argh derive structs, with flattened
//! structs followed into the modules the target declares. Cheap, but only
//! approximate. A hand-written parser gives the `--flags` spelled in it.

use crate::help::{Flag, HelpInfo, Item, Source};
use std::path::{Path, PathBuf};

const MAX_FILES: usize = 40;

/// The target's source file and those of the modules it declares, the root first.
pub fn read_sources(root: &Path) -> Vec<(PathBuf, String)> {
    let mut out = Vec::new();
    let mut queue = vec![(root.to_path_buf(), true)];
    while let Some((path, is_root)) = queue.pop() {
        if out.len() >= MAX_FILES || out.iter().any(|(p, _)| *p == path) {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        // A crate root and a mod.rs hold their modules beside them; foo.rs in foo/.
        let dir = match (is_root, path.file_stem().and_then(|s| s.to_str())) {
            (false, Some(stem)) if stem != "mod" => path.with_extension("").to_path_buf(),
            _ => path.parent().map(Path::to_path_buf).unwrap_or_default(),
        };
        for name in declared_modules(&text) {
            for cand in [dir.join(format!("{name}.rs")), dir.join(&name).join("mod.rs")] {
                if cand.is_file() {
                    queue.insert(0, (cand, false));
                    break;
                }
            }
        }
        out.push((path, text));
    }
    out
}

/// Names of `mod name;` declarations (not inline modules).
fn declared_modules(src: &str) -> Vec<String> {
    src.lines()
        .filter_map(|l| {
            let t = l.trim();
            let t = t.strip_prefix("pub(crate) ").or_else(|| t.strip_prefix("pub ")).unwrap_or(t);
            let name = t.strip_prefix("mod ")?.strip_suffix(';')?.trim();
            name.chars()
                .all(|c| c.is_alphanumeric() || c == '_')
                .then(|| name.to_string())
        })
        .collect()
}

/// A short name of how the target reads its command line, from its source
/// and its package's dependencies: what the target's own file says, else
/// the first of its modules that says anything.
pub fn parser_of(sources: &[(PathBuf, String)], deps: &[String]) -> &'static str {
    sources
        .iter()
        .map(|(_, text)| parser_in(text, deps))
        .find(|p| *p != NONE_SEEN)
        .unwrap_or(NONE_SEEN)
}

const NONE_SEEN: &str = "none seen";

fn parser_in(text: &str, deps: &[String]) -> &'static str {
    let has = |s: &str| text.contains(s);
    let dep = |s: &str| deps.iter().any(|d| d == s);
    let clap = dep("clap") && has("clap");
    if has("StructOpt") && dep("structopt") {
        "structopt"
    } else if has("FromArgs") && dep("argh") {
        "argh"
    } else if clap && (has("derive(") && has("Parser") || has("::parse()")) {
        "clap, derive"
    } else if clap && (has("Command::new") || has("App::new") || has("Arg::new")) {
        "clap, builder"
    } else if has("bpaf::") && dep("bpaf") {
        "bpaf"
    } else if has("pico_args") {
        "pico-args"
    } else if has("lexopt") {
        "lexopt"
    } else if has("getopts") {
        "getopts"
    } else if has("gumdrop") {
        "gumdrop"
    } else if has("env::args") || has("args_os()") {
        "std::env::args, by hand"
    } else {
        NONE_SEEN
    }
}

pub fn scan(path: &Path) -> HelpInfo {
    let sources = read_sources(path);
    if sources.is_empty() {
        return HelpInfo {
            source: Source::SourceScan,
            note: Some(format!("could not read {}", path.display())),
            ..Default::default()
        };
    }
    scan_sources(&sources)
}

fn kebab(name: &str) -> String {
    name.trim_start_matches("r#").replace('_', "-")
}

/// Extracts `key = "value"` or `key = 'v'` from attribute text.
fn attr_value(attr: &str, key: &str) -> Option<String> {
    let mut rest = attr;
    while let Some(pos) = rest.find(key) {
        let before_ok = pos == 0
            || !rest.as_bytes()[pos - 1].is_ascii_alphanumeric()
                && rest.as_bytes()[pos - 1] != b'_';
        let after = rest[pos + key.len()..].trim_start();
        if before_ok {
            if let Some(v) = after.strip_prefix('=') {
                let v = v.trim_start();
                let quote = v.chars().next()?;
                if quote == '"' || quote == '\'' {
                    let end = v[1..].find(quote)?;
                    return Some(v[1..1 + end].to_string());
                }
            }
        }
        rest = &rest[pos + key.len()..];
    }
    None
}

fn has_word(attr: &str, word: &str) -> bool {
    attr.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .any(|w| w == word)
}

/// The struct a field's type names: `Option<foo::Bar>` gives `Bar`.
fn type_name(ty: &str) -> String {
    let ty = ty.strip_prefix("Option<").and_then(|t| t.strip_suffix('>')).unwrap_or(ty);
    let ty = ty.split('<').next().unwrap_or(ty);
    ty.rsplit("::").next().unwrap_or(ty).trim().to_string()
}

/// A struct with a derive of a parser, as its fields give it.
#[derive(Default)]
struct Scanned {
    name: String,
    /// Derives the parser itself (`Parser`, `StructOpt`, `FromArgs`), not
    /// only a group of arguments (`Args`).
    top: bool,
    about: String,
    flags: Vec<Flag>,
    positionals: Vec<Item>,
    flatten: Vec<String>,
}

fn scan_structs(src: &str, out: &mut Vec<Scanned>) {
    let mut pending: Option<bool> = None;
    let mut cur: Option<Scanned> = None;
    let mut depth = 0i32;
    let mut docs: Vec<String> = Vec::new();
    let mut attrs = String::new();
    let mut attr_open = 0i32;
    let mut struct_docs: Vec<String> = Vec::new();

    for line in src.lines() {
        let t = line.trim();

        if attr_open > 0 {
            attrs.push_str(t);
            attr_open += t.matches('(').count() as i32 - t.matches(')').count() as i32;
            continue;
        }

        let Some(s) = cur.as_mut() else {
            if t.starts_with("#[derive") {
                let top = has_word(t, "Parser") || has_word(t, "StructOpt") || has_word(t, "FromArgs");
                if top || has_word(t, "Args") {
                    pending = Some(top);
                    continue;
                }
            }
            if let Some(d) = t.strip_prefix("///") {
                struct_docs.push(d.trim().to_string());
                continue;
            }
            if let Some(top) = pending {
                let decl = t.strip_prefix("pub ").unwrap_or(t);
                if let Some(rest) = decl.strip_prefix("struct ") {
                    if t.contains('{') {
                        cur = Some(Scanned {
                            name: rest.split(|c: char| !(c.is_alphanumeric() || c == '_')).next().unwrap_or("").to_string(),
                            top,
                            about: struct_docs.join(" "),
                            ..Default::default()
                        });
                        depth = 1;
                        docs.clear();
                        attrs.clear();
                        continue;
                    }
                }
            }
            if !t.starts_with("#[") {
                pending = None;
                struct_docs.clear();
            }
            continue;
        };

        // Inside a parser's struct.
        depth += t.matches('{').count() as i32 - t.matches('}').count() as i32;
        if depth <= 0 {
            out.extend(cur.take());
            pending = None;
            struct_docs.clear();
            continue;
        }
        if let Some(d) = t.strip_prefix("///") {
            docs.push(d.trim().to_string());
            continue;
        }
        if t.starts_with("#[") {
            attrs.push_str(t);
            attr_open = t.matches('(').count() as i32 - t.matches(')').count() as i32;
            continue;
        }
        if t.starts_with("//") || t.is_empty() {
            continue;
        }
        // A field: `pub name: Type,`
        let field = t
            .trim_start_matches("pub(crate)")
            .trim_start_matches("pub")
            .trim();
        if let Some((name, ty)) = field.split_once(':') {
            let name = name.trim();
            let ty = ty.trim().trim_end_matches(',').trim();
            let desc = attr_value(&attrs, "help")
                .or_else(|| attr_value(&attrs, "description"))
                .unwrap_or_else(|| docs.join(" "));
            let valid = name
                .chars()
                .all(|c| c.is_alphanumeric() || c == '_' || c == '#');
            if has_word(&attrs, "flatten") {
                s.flatten.push(type_name(ty));
            } else if valid && !has_word(&attrs, "subcommand") && !has_word(&attrs, "skip") {
                let is_argh_switch = has_word(&attrs, "switch");
                let is_argh_option = has_word(&attrs, "option");
                let long = has_word(&attrs, "long") || is_argh_switch || is_argh_option;
                let short = has_word(&attrs, "short");
                if long || short {
                    let long_name = attr_value(&attrs, "long").unwrap_or_else(|| kebab(name));
                    let short_name = attr_value(&attrs, "short")
                        .unwrap_or_else(|| name.chars().next().unwrap_or('?').to_string());
                    let is_bool = ty == "bool"
                        || is_argh_switch
                        || has_word(&attrs, "SetTrue")
                        || has_word(&attrs, "Count");
                    s.flags.push(Flag {
                        short: short.then(|| format!("-{short_name}")),
                        long: long.then(|| format!("--{long_name}")),
                        value: (!is_bool).then(|| {
                            format!(
                                "<{}>",
                                attr_value(&attrs, "value_name")
                                    .unwrap_or_else(|| name.to_ascii_uppercase())
                            )
                        }),
                        desc,
                        possible_values: Vec::new(),
                    });
                } else {
                    let optional = ty.starts_with("Option<") || ty.starts_with("Vec<");
                    let n = name.to_ascii_uppercase();
                    s.positionals.push(Item {
                        name: if optional {
                            format!("[{n}]")
                        } else {
                            format!("<{n}>")
                        },
                        desc,
                    });
                }
            }
            docs.clear();
            attrs.clear();
        }
    }
}

/// Adds a struct's fields, and those of the structs it flattens, to `info`.
fn merge(structs: &[Scanned], s: &Scanned, info: &mut HelpInfo, depth: usize) {
    info.flags.extend(s.flags.iter().cloned());
    info.positionals.extend(s.positionals.iter().cloned());
    if depth > 8 {
        return;
    }
    for name in &s.flatten {
        if let Some(inner) = structs.iter().find(|x| &x.name == name) {
            merge(structs, inner, info, depth + 1);
        }
    }
}

/// `"--flag"` string literals, for parsers written by hand.
fn literal_flags(sources: &[(PathBuf, String)]) -> Vec<Flag> {
    let mut seen: Vec<String> = Vec::new();
    for (_, text) in sources {
        for piece in text.split('"').skip(1).step_by(2) {
            let ok = piece.len() > 2
                && piece.starts_with("--")
                && piece[2..].chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
                && piece[2..].starts_with(|c: char| c.is_ascii_alphanumeric());
            if ok && !seen.iter().any(|s| s == piece) {
                seen.push(piece.to_string());
            }
        }
    }
    seen.into_iter()
        .map(|long| Flag {
            long: Some(long),
            desc: "spelled in the source".into(),
            ..Default::default()
        })
        .collect()
}

pub fn scan_sources(sources: &[(PathBuf, String)]) -> HelpInfo {
    let mut info = HelpInfo {
        source: Source::SourceScan,
        ..Default::default()
    };
    let mut structs = Vec::new();
    for (_, text) in sources {
        scan_structs(text, &mut structs);
    }
    if let Some(top) = structs.iter().find(|s| s.top) {
        info.about = top.about.clone();
        merge(&structs, top, &mut info, 0);
    }
    if info.is_empty() {
        info.flags = literal_flags(sources);
        let reads_args = sources.iter().any(|(_, t)| t.contains("env::args"));
        info.note = Some(match (reads_args, info.flags.is_empty()) {
            (true, false) => "no clap-style struct; the flags are those spelled in the source".into(),
            (true, true) => "no clap-style struct found; the program reads std::env::args directly".into(),
            (false, false) => "no clap-style struct; these flags are spelled in the source".into(),
            (false, true) => "no command line arguments found in the source".into(),
        });
    }
    info
}

#[cfg(test)]
fn scan_str(src: &str) -> HelpInfo {
    scan_sources(&[(PathBuf::from("main.rs"), src.to_string())])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scans_clap_derive() {
        let src = r#"
use clap::Parser;

/// Resize images
#[derive(Parser, Debug)]
#[command(version)]
struct Cli {
    /// Input image
    input: PathBuf,
    /// Output width
    #[arg(short, long, default_value_t = 100)]
    width: u32,
    #[arg(long = "keep-aspect", help = "Keep the aspect ratio")]
    keep: bool,
    #[arg(
        short = 'q',
        long,
    )]
    quality_level: Option<u8>,
    #[command(subcommand)]
    cmd: Option<Cmd>,
}

fn main() {}
"#;
        let h = scan_str(src);
        assert_eq!(h.about, "Resize images");
        assert_eq!(
            h.positionals,
            vec![Item {
                name: "<INPUT>".into(),
                desc: "Input image".into()
            }]
        );
        assert_eq!(h.flags.len(), 3);
        assert_eq!(h.flags[0].spec(), "-w, --width <WIDTH>");
        assert_eq!(h.flags[1].spec(), "    --keep-aspect");
        assert_eq!(h.flags[1].desc, "Keep the aspect ratio");
        assert_eq!(h.flags[2].spec(), "-q, --quality-level <QUALITY_LEVEL>");
    }

    #[test]
    fn notes_env_args() {
        let h = scan_str("fn main() { let a: Vec<String> = std::env::args().collect(); }");
        assert!(h.note.unwrap().contains("env::args"));
    }

    #[test]
    fn literal_flags_of_a_hand_written_parser() {
        let h = scan_str(r#"fn main() { for a in std::env::args() { if a == "--fast" || a == "--n-steps" || a == "--" {} } }"#);
        let longs: Vec<_> = h.flags.iter().filter_map(|f| f.long.clone()).collect();
        assert_eq!(longs, ["--fast", "--n-steps"]);
    }

    #[test]
    fn follows_flatten_into_modules() {
        let dir = std::env::temp_dir().join(format!("cargo-explore-src-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("common")).unwrap();
        std::fs::write(
            dir.join("ex.rs"),
            "mod common;\n#[derive(clap::Parser)]\nstruct Cli {\n    #[arg(long)]\n    fast: bool,\n    #[command(flatten)]\n    planner: common::ilqr::PlannerArgs,\n}\n",
        )
        .unwrap();
        std::fs::write(dir.join("common/mod.rs"), "pub mod ilqr;\n").unwrap();
        std::fs::write(
            dir.join("common/ilqr.rs"),
            "#[derive(clap::Args)]\npub struct PlannerArgs {\n    /// Threads to use\n    #[arg(long, default_value_t = 1)]\n    pub threads: usize,\n}\n",
        )
        .unwrap();
        let sources = read_sources(&dir.join("ex.rs"));
        assert_eq!(sources.len(), 3);
        assert_eq!(parser_of(&sources, &["clap".into()]), "clap, derive");
        let h = scan_sources(&sources);
        let specs: Vec<_> = h.flags.iter().map(|f| f.spec()).collect();
        assert_eq!(specs, ["    --fast", "    --threads <THREADS>"]);
        assert_eq!(h.flags[1].desc, "Threads to use");
        let _ = std::fs::remove_dir_all(dir);
    }
}
