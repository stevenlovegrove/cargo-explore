//! Parsing `--help` output of typical Rust command line programs (clap 2/3/4,
//! structopt, argh, getopts, pico-args style hand-written usage text).

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Flag {
    pub short: Option<String>,
    pub long: Option<String>,
    /// Value placeholder, e.g. `<FILE>`; `None` for boolean switches.
    pub value: Option<String>,
    pub desc: String,
    pub possible_values: Vec<String>,
}

impl Flag {
    /// The spelling inserted into the argument line.
    pub fn spelling(&self) -> &str {
        self.long
            .as_deref()
            .or(self.short.as_deref())
            .unwrap_or_default()
    }

    pub fn spec(&self) -> String {
        let mut s = match (&self.short, &self.long) {
            (Some(sh), Some(l)) => format!("{sh}, {l}"),
            (Some(sh), None) => sh.clone(),
            (None, Some(l)) => format!("    {l}"),
            (None, None) => String::new(),
        };
        if let Some(v) = &self.value {
            s.push(' ');
            s.push_str(v);
        }
        s
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Item {
    pub name: String,
    pub desc: String,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Source {
    #[default]
    Help,
    SourceScan,
}

#[derive(Clone, Debug, Default)]
pub struct HelpInfo {
    pub about: String,
    pub usage: String,
    pub flags: Vec<Flag>,
    pub positionals: Vec<Item>,
    pub subcommands: Vec<Item>,
    pub raw: String,
    pub source: Source,
    /// Extra remark shown to the user, e.g. why a fallback was used.
    pub note: Option<String>,
}

impl HelpInfo {
    pub fn is_empty(&self) -> bool {
        self.flags.is_empty() && self.positionals.is_empty() && self.subcommands.is_empty()
    }

    /// Whether the text looks like usage output at all (as opposed to a
    /// program that ignored `--help` and just ran).
    pub fn looks_like_help(&self) -> bool {
        !self.usage.is_empty() || !self.is_empty()
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Section {
    Preamble,
    Flags,
    Positionals,
    Subcommands,
    Other,
}

fn classify_header(name: &str) -> Section {
    let n = name.to_ascii_lowercase();
    if n.contains("command") {
        Section::Subcommands
    } else if n.contains("option") || n.contains("flag") || n.contains("switch") {
        Section::Flags
    } else if n.contains("arg") || n.contains("positional") {
        Section::Positionals
    } else {
        Section::Other
    }
}

fn indent_of(line: &str) -> usize {
    line.len() - line.trim_start().len()
}

/// Splits an entry line into its spec and description at the first run of
/// two or more spaces (or a tab).
fn split_spec(s: &str) -> (&str, &str) {
    let bytes = s.as_bytes();
    for i in 0..bytes.len() {
        if bytes[i] == b'\t' || (bytes[i] == b' ' && bytes.get(i + 1) == Some(&b' ')) {
            return (s[..i].trim_end(), s[i..].trim());
        }
    }
    (s.trim_end(), "")
}

fn parse_flag_spec(spec: &str) -> Flag {
    let mut f = Flag::default();
    for tok in spec
        .split(|c: char| c == ',' || c.is_whitespace())
        .filter(|t| !t.is_empty())
    {
        if let Some(rest) = tok.strip_prefix("--") {
            let (name, val) = match rest.split_once('=') {
                Some((n, v)) => (n, Some(v)),
                None => (rest, None),
            };
            let name = name.trim_end_matches("...");
            if !name.is_empty() && f.long.is_none() {
                f.long = Some(format!("--{name}"));
            }
            if let Some(v) = val {
                f.value = Some(v.to_string());
            }
        } else if tok.starts_with('-') && tok.len() > 1 && f.short.is_none() && f.long.is_none() {
            let short: String = tok.chars().take(2).collect();
            f.short = Some(short);
            if tok.len() > 2 {
                let rest = tok[2..].trim_start_matches('=').trim_end_matches("...");
                if !rest.is_empty() {
                    f.value = Some(rest.to_string());
                }
            }
        } else if (tok.starts_with('<')
            || tok.starts_with('[')
            || tok
                .chars()
                .all(|c| c.is_ascii_uppercase() || c == '_' || c == '-' || c == '.'))
            && f.value.is_none()
            && tok != "..."
        {
            f.value = Some(tok.to_string());
        }
    }
    f
}

/// Pulls `[possible values: a, b]` out of a description.
fn possible_values(desc: &str) -> Vec<String> {
    let Some(start) = desc.find("[possible values:") else {
        return Vec::new();
    };
    let rest = &desc[start + "[possible values:".len()..];
    let end = rest.find(']').unwrap_or(rest.len());
    rest[..end]
        .split(',')
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .collect()
}

fn append_desc(desc: &mut String, more: &str) {
    let more = more.trim();
    if more.is_empty() {
        return;
    }
    if !desc.is_empty() {
        desc.push(' ');
    }
    desc.push_str(more);
}

fn looks_like_flag(s: &str) -> bool {
    let mut c = s.chars();
    c.next() == Some('-')
        && c.next()
            .is_some_and(|c| c == '-' || c.is_ascii_alphanumeric())
}

enum Entry {
    Flag(Flag),
    Positional(Item),
    Subcommand(Item),
}

pub fn parse(text: &str) -> HelpInfo {
    let text = crate::cargo::strip_ansi(text);
    let mut info = HelpInfo {
        raw: text.clone(),
        ..Default::default()
    };
    let mut section = Section::Preamble;
    let mut entry_indent: Option<usize> = None;
    let mut current: Option<Entry> = None;
    let mut in_usage = false;
    let mut about_done = false;

    let flush = |current: &mut Option<Entry>, info: &mut HelpInfo| match current.take() {
        Some(Entry::Flag(mut f)) => {
            f.possible_values = possible_values(&f.desc);
            info.flags.push(f)
        }
        Some(Entry::Positional(p)) => info.positionals.push(p),
        Some(Entry::Subcommand(s)) => info.subcommands.push(s),
        None => {}
    };

    for line in text.lines() {
        let trimmed = line.trim();
        let indent = indent_of(line);

        // Usage line(s): "Usage: prog [OPTIONS]" or "USAGE:\n    prog [FLAGS]".
        let lower = trimmed.to_ascii_lowercase();
        if indent == 0 && lower.starts_with("usage") {
            flush(&mut current, &mut info);
            let rest = trimmed[5..].trim_start_matches(':').trim();
            info.usage = rest.to_string();
            in_usage = true;
            section = Section::Other;
            entry_indent = None;
            about_done = true;
            continue;
        }
        if in_usage {
            if trimmed.is_empty() {
                in_usage = false;
                continue;
            }
            if indent > 0 {
                append_desc(&mut info.usage, trimmed);
                continue;
            }
            in_usage = false;
        }

        // Section header: unindented and ending with ':' ("Options:", "ARGS:").
        if indent == 0 && trimmed.ends_with(':') && !trimmed.starts_with('-') && trimmed.len() < 40
        {
            flush(&mut current, &mut info);
            section = classify_header(trimmed.trim_end_matches(':'));
            entry_indent = None;
            about_done = true;
            continue;
        }

        if trimmed.is_empty() {
            continue;
        }

        if section == Section::Preamble {
            if !about_done && indent == 0 && !trimmed.starts_with('-') {
                append_desc(&mut info.about, trimmed);
                continue;
            }
            // Help text without section headers (e.g. hand-written usage):
            // treat indented dash-lines as flags.
            if trimmed.starts_with('-') {
                section = Section::Flags;
            } else {
                continue;
            }
        }

        let is_entry_start = match entry_indent {
            None => indent > 0 || section != Section::Preamble,
            // Long-only flags are indented past the short ones ("      --color").
            Some(ei) => indent <= ei || (indent <= ei + 4 && looks_like_flag(trimmed)),
        };

        if is_entry_start {
            flush(&mut current, &mut info);
            entry_indent.get_or_insert(indent);
            let (spec, desc) = split_spec(trimmed);
            let effective = match section {
                Section::Other | Section::Preamble if spec.starts_with('-') => Section::Flags,
                s => s,
            };
            current = match effective {
                Section::Flags if spec.starts_with('-') => {
                    let mut f = parse_flag_spec(spec);
                    f.desc = desc.to_string();
                    Some(Entry::Flag(f))
                }
                Section::Positionals => Some(Entry::Positional(Item {
                    name: spec.to_string(),
                    desc: desc.to_string(),
                })),
                Section::Subcommands => {
                    let name = spec
                        .split_whitespace()
                        .next()
                        .unwrap_or(spec)
                        .trim_end_matches(',');
                    Some(Entry::Subcommand(Item {
                        name: name.to_string(),
                        desc: desc.to_string(),
                    }))
                }
                _ => None,
            };
        } else if let Some(entry) = current.as_mut() {
            let desc = match entry {
                Entry::Flag(f) => &mut f.desc,
                Entry::Positional(p) => &mut p.desc,
                Entry::Subcommand(s) => &mut s.desc,
            };
            append_desc(desc, trimmed);
        }
    }
    flush(&mut current, &mut info);
    info
}

#[cfg(test)]
mod tests {
    use super::*;

    const CLAP4: &str = "\
A tiny grep

Usage: minigrep [OPTIONS] <PATTERN> [FILES]...
       minigrep --version

Commands:
  index  Build an index
  help   Print this message or the help of the given subcommand(s)

Arguments:
  <PATTERN>   Pattern to search for
  [FILES]...  Files to search

Options:
  -i, --ignore-case          Case insensitive
  -n, --max-count <NUM>      Stop after NUM matches [default: 10]
      --color <WHEN>         When to color [default: auto] [possible values: auto, always, never]
  -h, --help                 Print help
  -V, --version              Print version
";

    #[test]
    fn clap4_short_help() {
        let h = parse(CLAP4);
        assert_eq!(h.about, "A tiny grep");
        assert!(h.usage.starts_with("minigrep [OPTIONS] <PATTERN>"));
        assert_eq!(h.subcommands.len(), 2);
        assert_eq!(
            h.subcommands[0],
            Item {
                name: "index".into(),
                desc: "Build an index".into()
            }
        );
        assert_eq!(h.positionals.len(), 2);
        assert_eq!(h.positionals[1].name, "[FILES]...");
        assert_eq!(h.flags.len(), 5);
        assert_eq!(h.flags[1].short.as_deref(), Some("-n"));
        assert_eq!(h.flags[1].long.as_deref(), Some("--max-count"));
        assert_eq!(h.flags[1].value.as_deref(), Some("<NUM>"));
        assert_eq!(h.flags[2].short, None);
        assert_eq!(h.flags[2].possible_values, vec!["auto", "always", "never"]);
        assert_eq!(h.flags[0].value, None);
    }

    #[test]
    fn clap4_long_help() {
        let text = "\
Usage: app [OPTIONS]

Options:
  -c, --config <FILE>
          Path to the config file.

          Can be given more than once.

  -v, --verbose...
          More output

  -h, --help
          Print help (see a summary with '-h')
";
        let h = parse(text);
        assert_eq!(h.flags.len(), 3);
        assert_eq!(
            h.flags[0].desc,
            "Path to the config file. Can be given more than once."
        );
        assert_eq!(h.flags[1].long.as_deref(), Some("--verbose"));
        assert_eq!(h.flags[1].value, None);
    }

    #[test]
    fn clap2_structopt_style() {
        let text = "\
myapp 0.1.0
Does things

USAGE:
    myapp [FLAGS] [OPTIONS] <input>

FLAGS:
    -d, --debug      Activate debug mode
    -h, --help       Prints help information

OPTIONS:
    -o, --output <output>    Output file
        --speed=<speed>      Set speed

ARGS:
    <input>    Input file
";
        let h = parse(text);
        assert_eq!(h.about, "myapp 0.1.0 Does things");
        assert_eq!(h.usage, "myapp [FLAGS] [OPTIONS] <input>");
        assert_eq!(h.flags.len(), 4);
        assert_eq!(h.flags[3].long.as_deref(), Some("--speed"));
        assert_eq!(h.flags[3].value.as_deref(), Some("<speed>"));
        assert_eq!(
            h.positionals,
            vec![Item {
                name: "<input>".into(),
                desc: "Input file".into()
            }]
        );
    }

    #[test]
    fn getopts_and_argh_style() {
        let getopts = "\
Usage: prog FILE [options]

Options:
    -o NAME             set output file name
    -h, --help          print this help menu
";
        let h = parse(getopts);
        assert_eq!(h.flags[0].short.as_deref(), Some("-o"));
        assert_eq!(h.flags[0].value.as_deref(), Some("NAME"));

        let argh = "\
Usage: goup [--height <height>] [--pilot-nickname <pilot-nickname>]

Reach new heights.

Options:
  --height          how high to go
  --pilot-nickname  an optional nickname for the pilot
  --help, help      display usage information
";
        let h = parse(argh);
        assert_eq!(h.flags.len(), 3);
        assert_eq!(h.flags[1].long.as_deref(), Some("--pilot-nickname"));
    }

    #[test]
    fn non_help_output() {
        let h = parse("Hello, world!\nthe answer is 42\n");
        assert!(!h.looks_like_help());
    }
}
