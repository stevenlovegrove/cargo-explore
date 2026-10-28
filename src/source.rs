//! Fallback flag discovery by scanning a target's source for clap/structopt
//! derive structs. Cheap and build-free, but only approximate.

use crate::help::{Flag, HelpInfo, Item, Source};
use std::path::Path;

pub fn scan(path: &Path) -> HelpInfo {
    match std::fs::read_to_string(path) {
        Ok(src) => scan_str(&src),
        Err(e) => HelpInfo {
            source: Source::SourceScan,
            note: Some(format!("could not read {}: {e}", path.display())),
            ..Default::default()
        },
    }
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

pub fn scan_str(src: &str) -> HelpInfo {
    let mut info = HelpInfo {
        source: Source::SourceScan,
        ..Default::default()
    };
    let mut pending_derive_parser = false;
    let mut in_struct = false;
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

        if !in_struct {
            if t.starts_with("#[derive")
                && (t.contains("Parser")
                    || t.contains("StructOpt")
                    || t.contains("Args")
                    || t.contains("FromArgs"))
            {
                pending_derive_parser = true;
                continue;
            }
            if let Some(d) = t.strip_prefix("///") {
                struct_docs.push(d.trim().to_string());
                continue;
            }
            if pending_derive_parser
                && (t.starts_with("pub struct") || t.starts_with("struct"))
                && t.contains('{')
            {
                in_struct = true;
                depth = 1;
                if info.about.is_empty() {
                    info.about = struct_docs.join(" ");
                }
                docs.clear();
                attrs.clear();
                continue;
            }
            if !t.starts_with("#[") {
                pending_derive_parser = false;
                struct_docs.clear();
            }
            continue;
        }

        // Inside a Parser/Args struct.
        depth += t.matches('{').count() as i32 - t.matches('}').count() as i32;
        if depth <= 0 {
            in_struct = false;
            pending_derive_parser = false;
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
            let skip = has_word(&attrs, "subcommand")
                || has_word(&attrs, "flatten")
                || has_word(&attrs, "skip");
            if !skip
                && name
                    .chars()
                    .all(|c| c.is_alphanumeric() || c == '_' || c == '#')
            {
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
                    info.flags.push(Flag {
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
                    info.positionals.push(Item {
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

    if info.is_empty() {
        let note = if src.contains("env::args") {
            "no clap-style struct found; the program reads std::env::args directly"
        } else {
            "no command line arguments found in the source"
        };
        info.note = Some(note.into());
    }
    info
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
}
