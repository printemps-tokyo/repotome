//! repotome — pack a git repository (or any directory) into a single text
//! document for feeding to an LLM.
//!
//! The crate exposes a small library so the walking and rendering logic can be
//! unit- and integration-tested without going through the binary.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{Context, Result};
use globset::{Glob, GlobSet, GlobSetBuilder};
use ignore::WalkBuilder;

/// Output format for the packed document.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    /// Markdown with fenced code blocks (default).
    Md,
    /// A simple repomix-style XML wrapper.
    Xml,
}

/// Options controlling traversal and rendering.
#[derive(Debug, Clone)]
pub struct Options {
    /// Include only files whose repo-relative path matches one of these globs.
    pub include: Vec<String>,
    /// Additionally exclude files matching one of these globs.
    pub exclude: Vec<String>,
    /// Skip files larger than this many bytes (contents omitted).
    pub max_size: u64,
    /// Respect .gitignore / .ignore and skip the .git directory.
    pub respect_gitignore: bool,
    /// Include hidden files (dotfiles).
    pub hidden: bool,
    /// Render the directory-tree section.
    pub tree: bool,
    /// Render each file's contents. When false, only the summary and tree are
    /// produced (a quick map of the repo without the bodies).
    pub contents: bool,
    /// Output format.
    pub format: Format,
    /// Include an approximate token count in the summary.
    pub tokens: bool,
    /// Stop emitting file bodies once the running approximate token count would
    /// exceed this budget (the first file is always included).
    pub max_tokens: Option<usize>,
    /// Absolute path to skip during the walk (e.g. the --output file when it
    /// lives inside the target directory, so a previous run is not re-packed).
    pub skip_path: Option<std::path::PathBuf>,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            include: Vec::new(),
            exclude: Vec::new(),
            max_size: 1024 * 1024,
            respect_gitignore: true,
            hidden: false,
            tree: true,
            contents: true,
            format: Format::Md,
            tokens: false,
            max_tokens: None,
            skip_path: None,
        }
    }
}

/// What happened to a single file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EntryKind {
    /// A UTF-8 text file with its contents.
    Text(String),
    /// A binary / non-UTF-8 file (contents omitted).
    Binary,
    /// A file larger than `max_size` (contents omitted); carries its size.
    TooLarge(u64),
    /// A file that could not be read (permission denied, vanished, etc.).
    Unreadable,
}

/// One collected file, identified by its repo-relative path (forward slashes).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub rel_path: String,
    pub kind: EntryKind,
}

/// Parse a human size string ("1MiB", "500k", "1048576") into bytes.
/// Decimal units (MB) use 1000, binary units (MiB) use 1024.
pub fn parse_size(input: &str) -> Result<u64> {
    let s = input.trim();
    // Split off a trailing alphabetic unit suffix, leaving the numeric prefix.
    // Scanning from the end keeps scientific notation (e.g. "1e6") in the number.
    let unit_len = s
        .chars()
        .rev()
        .take_while(|c| c.is_ascii_alphabetic())
        .count();
    let (num, unit) = s.split_at(s.len() - unit_len);
    let value: f64 = num
        .trim()
        .parse()
        .with_context(|| format!("invalid size: {input:?}"))?;
    if !(value.is_finite() && value >= 0.0) {
        anyhow::bail!("invalid size value: {input:?}");
    }
    let mult: f64 = match unit.trim().to_ascii_lowercase().as_str() {
        "" | "b" => 1.0,
        "k" | "kb" => 1_000.0,
        "ki" | "kib" => 1_024.0,
        "m" | "mb" => 1_000_000.0,
        "mi" | "mib" => 1_048_576.0,
        "g" | "gb" => 1_000_000_000.0,
        "gi" | "gib" => 1_073_741_824.0,
        other => anyhow::bail!("unknown size unit: {other:?}"),
    };
    Ok((value * mult) as u64)
}

/// A rough token estimate: about 4 characters per token.
pub fn approx_tokens(chars: usize) -> usize {
    chars.div_ceil(4)
}

fn build_globset(patterns: &[String]) -> Result<Option<GlobSet>> {
    if patterns.is_empty() {
        return Ok(None);
    }
    let mut builder = GlobSetBuilder::new();
    for p in patterns {
        builder.add(Glob::new(p).with_context(|| format!("invalid glob: {p:?}"))?);
    }
    Ok(Some(builder.build()?))
}

const BINARY_SNIFF_BYTES: usize = 8192;

/// Walk `root` and collect entries according to `opts`.
pub fn collect(root: &Path, opts: &Options) -> Result<Vec<Entry>> {
    let include = build_globset(&opts.include)?;
    let exclude = build_globset(&opts.exclude)?;

    let mut walker = WalkBuilder::new(root);
    walker
        .hidden(!opts.hidden)
        .git_ignore(opts.respect_gitignore)
        .git_global(opts.respect_gitignore)
        .git_exclude(opts.respect_gitignore)
        .ignore(opts.respect_gitignore)
        .parents(opts.respect_gitignore)
        // Honor .gitignore even when the target is not inside a git repo.
        .require_git(false)
        // A repotome-specific ignore file, always honored (gitignore syntax).
        .add_custom_ignore_filename(".repotomeignore")
        .filter_entry(|e| e.file_name() != ".git");

    let mut entries = Vec::new();
    for result in walker.build() {
        let dent = match result {
            Ok(d) => d,
            Err(_) => continue,
        };
        if !dent.file_type().is_some_and(|t| t.is_file()) {
            continue;
        }
        let path = dent.path();
        if let Some(skip) = &opts.skip_path {
            // Cheap pre-filter on the file name, then confirm via canonicalize
            // (the walker yields root-joined, not canonicalized, paths).
            if dent.file_name() == skip.file_name().unwrap_or_default()
                && path.canonicalize().ok().as_deref() == Some(skip.as_path())
            {
                continue;
            }
        }
        let rel = match path.strip_prefix(root) {
            Ok(r) => r,
            Err(_) => path,
        };
        let rel_path = rel.to_string_lossy().replace('\\', "/");
        if rel_path.is_empty() {
            continue;
        }
        if let Some(set) = &include {
            if !set.is_match(&rel_path) {
                continue;
            }
        }
        if let Some(set) = &exclude {
            if set.is_match(&rel_path) {
                continue;
            }
        }

        let meta = match dent.metadata() {
            Ok(m) => m,
            Err(_) => {
                entries.push(Entry {
                    rel_path,
                    kind: EntryKind::Unreadable,
                });
                continue;
            }
        };
        if meta.len() > opts.max_size {
            entries.push(Entry {
                rel_path,
                kind: EntryKind::TooLarge(meta.len()),
            });
            continue;
        }

        let bytes = match std::fs::read(path) {
            Ok(b) => b,
            Err(_) => {
                entries.push(Entry {
                    rel_path,
                    kind: EntryKind::Unreadable,
                });
                continue;
            }
        };
        let kind = if is_binary(&bytes) {
            EntryKind::Binary
        } else {
            match String::from_utf8(bytes) {
                Ok(text) => EntryKind::Text(text),
                Err(_) => EntryKind::Binary,
            }
        };
        entries.push(Entry { rel_path, kind });
    }

    entries.sort_by(|a, b| a.rel_path.cmp(&b.rel_path));
    Ok(entries)
}

fn is_binary(bytes: &[u8]) -> bool {
    bytes.iter().take(BINARY_SNIFF_BYTES).any(|&b| b == 0)
}

/// Guess a code-fence info string (language) from a file extension.
pub fn lang_for_path(path: &str) -> &'static str {
    let ext = Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    match ext.as_str() {
        "rs" => "rust",
        "ts" | "mts" | "cts" => "ts",
        "tsx" => "tsx",
        "js" | "mjs" | "cjs" => "js",
        "jsx" => "jsx",
        "py" => "python",
        "rb" => "ruby",
        "go" => "go",
        "java" => "java",
        "c" | "h" => "c",
        "cpp" | "cc" | "hpp" => "cpp",
        "cs" => "csharp",
        "php" => "php",
        "sh" | "bash" | "zsh" => "bash",
        "md" | "markdown" => "markdown",
        "json" => "json",
        "toml" => "toml",
        "yaml" | "yml" => "yaml",
        "html" | "htm" => "html",
        "css" => "css",
        "sql" => "sql",
        "xml" => "xml",
        _ => "",
    }
}

/// Build a fence of backticks longer than any run found in `content`.
pub fn fence_for(content: &str) -> String {
    let mut max_run = 0usize;
    let mut run = 0usize;
    for ch in content.chars() {
        if ch == '`' {
            run += 1;
            max_run = max_run.max(run);
        } else {
            run = 0;
        }
    }
    "`".repeat(max_run.max(2) + 1)
}

/// Tree label for an entry. With `show_tokens`, text files show their
/// approximate token count (e.g. "~120t"); other kinds keep their status note.
fn entry_label(kind: &EntryKind, show_tokens: bool) -> Option<String> {
    match kind {
        EntryKind::Text(content) => {
            show_tokens.then(|| format!("~{}t", approx_tokens(content.chars().count())))
        }
        EntryKind::Binary => Some("(binary, omitted)".to_string()),
        EntryKind::TooLarge(n) => Some(format!("(skipped: {n} bytes > max)")),
        EntryKind::Unreadable => Some("(unreadable, omitted)".to_string()),
    }
}

/// Render an indented directory tree from the collected entries.
fn render_tree(entries: &[Entry], show_tokens: bool) -> String {
    // Nested map: directory name -> subtree; files carry an optional label.
    #[derive(Default)]
    struct Node {
        dirs: BTreeMap<String, Node>,
        files: BTreeMap<String, Option<String>>,
    }
    let mut root = Node::default();
    for e in entries {
        let parts: Vec<&str> = e.rel_path.split('/').collect();
        let mut node = &mut root;
        for dir in &parts[..parts.len() - 1] {
            node = node.dirs.entry((*dir).to_string()).or_default();
        }
        if let Some(name) = parts.last() {
            node.files
                .insert((*name).to_string(), entry_label(&e.kind, show_tokens));
        }
    }

    fn walk(node: &Node, depth: usize, out: &mut String) {
        let indent = "  ".repeat(depth);
        for (name, child) in &node.dirs {
            out.push_str(&format!("{indent}{name}/\n"));
            walk(child, depth + 1, out);
        }
        for (name, label) in &node.files {
            match label {
                Some(l) => out.push_str(&format!("{indent}{name}  {l}\n")),
                None => out.push_str(&format!("{indent}{name}\n")),
            }
        }
    }

    let mut out = String::new();
    walk(&root, 0, &mut out);
    if out.is_empty() {
        out.push_str("(empty)\n");
    }
    out
}

/// Escape text content for XML (`&`, `<`, `>`).
fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// Escape a value for an XML attribute (adds `"` and `'` to the content set).
fn xml_attr_escape(s: &str) -> String {
    xml_escape(s).replace('"', "&quot;").replace('\'', "&apos;")
}

/// How many leading text files fit a token budget, and how many are dropped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BudgetPlan {
    /// Number of text files (in order) whose bodies are included.
    pub included: usize,
    /// Number of text files whose bodies are omitted for the budget.
    pub omitted: usize,
}

/// Decide which text files fit `max_tokens` (entries are consumed in order).
/// The first text file is always included; once a file would exceed the budget,
/// it and every later text file are omitted.
pub fn plan_budget(entries: &[Entry], max_tokens: Option<usize>) -> BudgetPlan {
    let text_total = entries
        .iter()
        .filter(|e| matches!(e.kind, EntryKind::Text(_)))
        .count();
    let Some(budget) = max_tokens else {
        return BudgetPlan {
            included: text_total,
            omitted: 0,
        };
    };

    let mut used = 0usize;
    let mut included = 0usize;
    for e in entries {
        if let EntryKind::Text(content) = &e.kind {
            let cost = approx_tokens(content.chars().count());
            if included == 0 || used + cost <= budget {
                used += cost;
                included += 1;
            } else {
                break;
            }
        }
    }
    BudgetPlan {
        included,
        omitted: text_total - included,
    }
}

/// Counts used in the summary line.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Stats {
    pub text_files: usize,
    pub skipped_files: usize,
    pub text_bytes: usize,
    pub chars: usize,
}

/// Compute summary statistics over the entries.
pub fn stats(entries: &[Entry]) -> Stats {
    let mut s = Stats::default();
    for e in entries {
        match &e.kind {
            EntryKind::Text(t) => {
                s.text_files += 1;
                s.text_bytes += t.len();
                s.chars += t.chars().count();
            }
            _ => s.skipped_files += 1,
        }
    }
    s
}

/// Render the collected entries into the final document string.
pub fn render(root_name: &str, entries: &[Entry], opts: &Options) -> String {
    let st = stats(entries);
    match opts.format {
        Format::Md => render_md(root_name, entries, opts, &st),
        Format::Xml => render_xml(root_name, entries, opts, &st),
    }
}

fn summary_lines(st: &Stats, opts: &Options) -> Vec<String> {
    let mut lines = vec![
        format!(
            "- Files: {} text, {} skipped",
            st.text_files, st.skipped_files
        ),
        format!("- Text size: {} bytes", st.text_bytes),
    ];
    if opts.tokens {
        lines.push(format!("- Approx tokens: ~{}", approx_tokens(st.chars)));
    }
    lines
}

fn render_md(root_name: &str, entries: &[Entry], opts: &Options, st: &Stats) -> String {
    let mut out = String::new();
    out.push_str(&format!("# Repository: {root_name}\n\n"));
    out.push_str("## Summary\n\n");
    for line in summary_lines(st, opts) {
        out.push_str(&line);
        out.push('\n');
    }
    out.push('\n');

    if opts.tree {
        out.push_str("## Structure\n\n```\n");
        out.push_str(&render_tree(entries, opts.tokens));
        out.push_str("```\n\n");
    }

    if !opts.contents {
        return out;
    }

    let plan = plan_budget(entries, opts.max_tokens);
    out.push_str("## Files\n\n");
    let mut emitted = 0usize;
    for e in entries {
        if let EntryKind::Text(content) = &e.kind {
            if emitted >= plan.included {
                break;
            }
            emitted += 1;
            let fence = fence_for(content);
            // Wrap the path in inline code unless it contains a backtick, which
            // inline code cannot represent; fall back to a plain heading then.
            if e.rel_path.contains('`') {
                out.push_str(&format!("### {}\n\n", e.rel_path));
            } else {
                out.push_str(&format!("### `{}`\n\n", e.rel_path));
            }
            out.push_str(&format!("{fence}{}\n", lang_for_path(&e.rel_path)));
            out.push_str(content);
            if !content.ends_with('\n') {
                out.push('\n');
            }
            out.push_str(&format!("{fence}\n\n"));
        }
    }
    if plan.omitted > 0 {
        out.push_str(&format!(
            "_{} more file(s) omitted (token budget)_\n",
            plan.omitted
        ));
    }
    out
}

fn render_xml(root_name: &str, entries: &[Entry], opts: &Options, st: &Stats) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "<repotome path=\"{}\">\n",
        xml_attr_escape(root_name)
    ));

    out.push_str("  <summary>\n");
    for line in summary_lines(st, opts) {
        out.push_str(&format!(
            "    {}\n",
            xml_escape(line.trim_start_matches("- "))
        ));
    }
    out.push_str("  </summary>\n");

    if opts.tree {
        out.push_str("  <structure>\n");
        out.push_str(&xml_escape(&render_tree(entries, opts.tokens)));
        out.push_str("  </structure>\n");
    }

    if !opts.contents {
        out.push_str("</repotome>\n");
        return out;
    }

    let plan = plan_budget(entries, opts.max_tokens);
    let mut emitted = 0usize;
    for e in entries {
        if let EntryKind::Text(content) = &e.kind {
            if emitted >= plan.included {
                break;
            }
            emitted += 1;
            out.push_str(&format!(
                "  <file path=\"{}\">\n",
                xml_attr_escape(&e.rel_path)
            ));
            out.push_str(&xml_escape(content));
            if !content.ends_with('\n') {
                out.push('\n');
            }
            out.push_str("  </file>\n");
        }
    }
    if plan.omitted > 0 {
        out.push_str(&format!(
            "  <omitted reason=\"token-budget\">{}</omitted>\n",
            plan.omitted
        ));
    }
    out.push_str("</repotome>\n");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_size_units() {
        assert_eq!(parse_size("1048576").unwrap(), 1_048_576);
        assert_eq!(parse_size("1MiB").unwrap(), 1_048_576);
        assert_eq!(parse_size("500k").unwrap(), 500_000);
        assert_eq!(parse_size("2MB").unwrap(), 2_000_000);
        assert_eq!(parse_size("1.5MiB").unwrap(), 1_572_864);
        // Scientific notation stays in the numeric part (no false unit split).
        assert_eq!(parse_size("1e6").unwrap(), 1_000_000);
        assert!(parse_size("abc").is_err());
        assert!(parse_size("1XB").is_err());
    }

    #[test]
    fn xml_attribute_escaping_handles_quotes() {
        let entries = vec![Entry {
            rel_path: "we\"ird'.txt".to_string(),
            kind: EntryKind::Text("hi\n".to_string()),
        }];
        let opts = Options {
            format: Format::Xml,
            ..Options::default()
        };
        let out = render("proj", &entries, &opts);
        assert!(out.contains("path=\"we&quot;ird&apos;.txt\""));
        assert!(!out.contains("path=\"we\"ird"));
    }

    #[test]
    fn approx_tokens_rounds_up() {
        assert_eq!(approx_tokens(0), 0);
        assert_eq!(approx_tokens(1), 1);
        assert_eq!(approx_tokens(4), 1);
        assert_eq!(approx_tokens(5), 2);
    }

    #[test]
    fn lang_detection() {
        assert_eq!(lang_for_path("src/main.rs"), "rust");
        assert_eq!(lang_for_path("a/b.py"), "python");
        assert_eq!(lang_for_path("x.unknownext"), "");
    }

    #[test]
    fn fence_grows_past_inner_backticks() {
        assert_eq!(fence_for("no ticks"), "```");
        assert_eq!(fence_for("a ``` b"), "````");
        assert_eq!(fence_for("a ```` b"), "`````");
    }

    #[test]
    fn render_md_includes_contents_and_omits_binary() {
        let entries = vec![
            Entry {
                rel_path: "a.rs".to_string(),
                kind: EntryKind::Text("fn main() {}\n".to_string()),
            },
            Entry {
                rel_path: "img.png".to_string(),
                kind: EntryKind::Binary,
            },
        ];
        let out = render("demo", &entries, &Options::default());
        assert!(out.contains("# Repository: demo"));
        assert!(out.contains("### `a.rs`"));
        assert!(out.contains("fn main() {}"));
        assert!(out.contains("img.png  (binary, omitted)"));
        // Binary file has no content block.
        assert!(!out.contains("### `img.png`"));
    }

    #[test]
    fn tree_shows_per_file_tokens_when_enabled() {
        let entries = vec![Entry {
            rel_path: "a.rs".into(),
            kind: EntryKind::Text("aaaaaaaa\n".into()), // 9 chars -> ceil(9/4)=3 tokens
        }];
        // Without --tokens: no per-file annotation.
        let plain = render("demo", &entries, &Options::default());
        assert!(plain.contains("a.rs\n"));
        assert!(!plain.contains("~3t"));
        // With --tokens: the tree annotates the file.
        let opts = Options {
            tokens: true,
            ..Options::default()
        };
        let withtok = render("demo", &entries, &opts);
        assert!(withtok.contains("a.rs  ~3t"));
    }

    #[test]
    fn plan_budget_includes_until_the_cap() {
        // Each file is ~5 chars -> ceil(5/4) = 2 tokens.
        let entries = vec![
            Entry {
                rel_path: "a".into(),
                kind: EntryKind::Text("aaaa\n".into()),
            },
            Entry {
                rel_path: "b".into(),
                kind: EntryKind::Text("bbbb\n".into()),
            },
            Entry {
                rel_path: "c".into(),
                kind: EntryKind::Text("cccc\n".into()),
            },
        ];
        assert_eq!(
            plan_budget(&entries, None),
            BudgetPlan {
                included: 3,
                omitted: 0
            }
        );
        // Budget 3 tokens: first file (2) fits; second would make 4 > 3 -> stop.
        assert_eq!(
            plan_budget(&entries, Some(3)),
            BudgetPlan {
                included: 1,
                omitted: 2
            }
        );
        // Budget 4 tokens: two files fit (2+2), third omitted.
        assert_eq!(
            plan_budget(&entries, Some(4)),
            BudgetPlan {
                included: 2,
                omitted: 1
            }
        );
        // The first file is always included even if it alone exceeds the budget.
        assert_eq!(
            plan_budget(&entries, Some(1)),
            BudgetPlan {
                included: 1,
                omitted: 2
            }
        );
    }

    #[test]
    fn render_md_truncates_at_token_budget() {
        let entries = vec![
            Entry {
                rel_path: "a.txt".into(),
                kind: EntryKind::Text("aaaa\n".into()),
            },
            Entry {
                rel_path: "b.txt".into(),
                kind: EntryKind::Text("bbbb\n".into()),
            },
        ];
        let opts = Options {
            max_tokens: Some(2),
            ..Options::default()
        };
        let out = render("demo", &entries, &opts);
        assert!(out.contains("### `a.txt`"));
        assert!(!out.contains("### `b.txt`"));
        assert!(out.contains("1 more file(s) omitted (token budget)"));
    }

    #[test]
    fn no_contents_omits_file_bodies_but_keeps_tree() {
        let entries = vec![Entry {
            rel_path: "a.rs".to_string(),
            kind: EntryKind::Text("fn main() {}\n".to_string()),
        }];
        let opts = Options {
            contents: false,
            ..Options::default()
        };
        let md = render("demo", &entries, &opts);
        assert!(md.contains("## Structure"));
        assert!(md.contains("a.rs"));
        assert!(!md.contains("## Files"));
        assert!(!md.contains("fn main() {}"));

        let xml = render(
            "demo",
            &entries,
            &Options {
                format: Format::Xml,
                contents: false,
                ..Options::default()
            },
        );
        assert!(xml.contains("<structure>"));
        assert!(!xml.contains("<file "));
        assert!(xml.trim_end().ends_with("</repotome>"));
    }
}
