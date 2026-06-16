//! Command-line entry point for repotome.

use std::io::Write;
use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::Parser;

use repotome::{collect, parse_size, render, stats, Format, Options};

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
    #[arg(long)]
    max_tokens: Option<usize>,
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    let opts = Options {
        include: cli.include,
        exclude: cli.exclude,
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
            format!(", ~{} tokens", repotome::approx_tokens(st.chars))
        } else {
            String::new()
        }
    );
    Ok(())
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
