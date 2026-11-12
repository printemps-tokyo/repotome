//! Command-line entry point for repotome.

use std::io::Write;
use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::Parser;

use repotome::{collect, parse_size, preset_globs, render, render_parts, stats, Format, Options};

/// Pack a git repository into a single text document for LLMs.
#[derive(Parser, Debug)]
#[command(name = "repotome", version, about, long_about = None)]
struct Cli {
    /// Directory to pack.
    #[arg(default_value = ".")]
    path: PathBuf,

    /// Write output to a file instead of stdout.
    #[arg(short, long)]
    output: Option<PathBuf>,

    /// Include only files matching this glob (repeatable).
    #[arg(long)]
    include: Vec<String>,

    /// Exclude files matching this glob (repeatable).
    #[arg(long)]
    exclude: Vec<String>,

    /// Exclude a language's lock files, dependencies and build output:
    /// node, python, rust, go, php, ruby, java, or auto (by the files at the
    /// root). Repeatable or comma-separated.
    #[arg(long, value_delimiter = ',', value_name = "NAME")]
    exclude_preset: Vec<String>,

    /// Skip files larger than this size (e.g. 1MiB, 500k).
    #[arg(long, default_value = "1MiB")]
    max_size: String,

    /// Do not respect .gitignore / .ignore.
    #[arg(long)]
    no_gitignore: bool,

    /// Include hidden files (dotfiles).
    #[arg(long)]
    hidden: bool,

    /// Omit the directory-tree section.
    #[arg(long)]
    no_tree: bool,

    /// Omit file contents; produce only the summary and directory tree.
    #[arg(long)]
    no_contents: bool,

    /// Output just the collected file paths, one per line.
    #[arg(long)]
    paths: bool,

    /// Copy the output to the system clipboard (in addition to stdout/--output).
    #[arg(long)]
    copy: bool,

    /// Output format.
    #[arg(long, value_parser = ["md", "xml"], default_value = "md")]
    format: String,

    /// Include an approximate token count in the summary.
    #[arg(long)]
    tokens: bool,

    /// Stop emitting file bodies once this approximate token budget is reached.
    #[arg(long, conflicts_with = "split_tokens")]
    max_tokens: Option<usize>,

    /// Split the output into parts of about this many tokens each, written
    /// next to --output as NAME-1.EXT, NAME-2.EXT, ...
    #[arg(long, value_name = "TOKENS", requires = "output", value_parser = clap::value_parser!(u64).range(1..))]
    split_tokens: Option<u64>,
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    let mut exclude = cli.exclude;
    exclude.extend(preset_globs(&cli.exclude_preset, &cli.path)?);
    let opts = Options {
        include: cli.include,
        exclude,
        max_size: parse_size(&cli.max_size)?,
        respect_gitignore: !cli.no_gitignore,
        hidden: cli.hidden,
        tree: !cli.no_tree,
        contents: !cli.no_contents,
        paths: cli.paths,
        format: if cli.format == "xml" {
            Format::Xml
        } else {
            Format::Md
        },
        tokens: cli.tokens,
        max_tokens: cli.max_tokens,
        // Don't re-pack the output file if it lives inside the target directory.
        skip_path: cli.output.as_ref().and_then(|p| p.canonicalize().ok()),
    };

    if !cli.path.is_dir() {
        anyhow::bail!("not a directory: {}", cli.path.display());
    }
    let root_name = cli
        .path
        .canonicalize()
        .ok()
        .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
        .unwrap_or_else(|| cli.path.display().to_string());

    let entries = collect(&cli.path, &opts).context("failed to walk the directory")?;

    if let (Some(budget), Some(output)) = (cli.split_tokens, &cli.output) {
        if cli.paths || cli.copy {
            anyhow::bail!("--split-tokens cannot be combined with --paths or --copy");
        }
        let parts = render_parts(
            &root_name,
            &entries,
            &opts,
            usize::try_from(budget).unwrap_or(usize::MAX),
        );
        let names = part_paths(output, parts.len());
        for (doc, path) in parts.iter().zip(&names) {
            std::fs::write(path, doc)
                .with_context(|| format!("failed to write {}", path.display()))?;
        }
        let st = stats(&entries);
        eprintln!(
            "repotome: {} text file(s), {} skipped, ~{} tokens in {} part(s): {}",
            st.text_files,
            st.skipped_files,
            st.tokens,
            names.len(),
            names
                .iter()
                .map(|p| p.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        );
        return Ok(());
    }

    let document = render(&root_name, &entries, &opts);

    if cli.copy {
        copy_to_clipboard(&document).context("failed to copy to clipboard")?;
        eprintln!("repotome: copied {} bytes to clipboard", document.len());
    }

    match &cli.output {
        Some(path) => {
            std::fs::write(path, &document)
                .with_context(|| format!("failed to write {}", path.display()))?;
        }
        None if !cli.copy => {
            std::io::stdout().write_all(document.as_bytes())?;
        }
        None => {}
    }

    let st = stats(&entries);
    eprintln!(
        "repotome: {} text file(s), {} skipped, {} bytes{}",
        st.text_files,
        st.skipped_files,
        st.text_bytes,
        if opts.tokens {
            format!(", ~{} tokens", st.tokens)
        } else {
            String::new()
        }
    );
    Ok(())
}

/// NAME-1.EXT, NAME-2.EXT, ... next to `output` (just NAME-1 without an
/// extension). One part still gets the -1 suffix, so the names do not
/// depend on how the content happened to split.
fn part_paths(output: &std::path::Path, n: usize) -> Vec<PathBuf> {
    let stem = output
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let ext = output
        .extension()
        .map(|e| format!(".{}", e.to_string_lossy()))
        .unwrap_or_default();
    (1..=n)
        .map(|k| output.with_file_name(format!("{stem}-{k}{ext}")))
        .collect()
}

/// Copy `text` to the system clipboard using whichever helper is available
/// (pbcopy on macOS, wl-copy / xclip on Linux, clip on Windows).
fn copy_to_clipboard(text: &str) -> Result<()> {
    use std::process::{Command, Stdio};

    let candidates: &[(&str, &[&str])] = if cfg!(target_os = "macos") {
        &[("pbcopy", &[])]
    } else if cfg!(target_os = "windows") {
        &[("clip", &[])]
    } else {
        &[("wl-copy", &[]), ("xclip", &["-selection", "clipboard"])]
    };

    let mut last_err = anyhow::anyhow!("no clipboard helper found");
    for (bin, args) in candidates {
        match Command::new(bin).args(*args).stdin(Stdio::piped()).spawn() {
            Ok(mut child) => {
                child
                    .stdin
                    .take()
                    .context("clipboard helper has no stdin")?
                    .write_all(text.as_bytes())?;
                let status = child.wait()?;
                if status.success() {
                    return Ok(());
                }
                last_err = anyhow::anyhow!("{bin} exited with {status}");
            }
            Err(e) => last_err = anyhow::anyhow!("could not run {bin}: {e}"),
        }
    }
    Err(last_err)
}
