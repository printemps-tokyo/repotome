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

    /// Output format.
    #[arg(long, value_parser = ["md", "xml"], default_value = "md")]
    format: String,

    /// Include an approximate token count in the summary.
    #[arg(long)]
    tokens: bool,
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
        format: if cli.format == "xml" {
            Format::Xml
        } else {
            Format::Md
        },
        tokens: cli.tokens,
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

    match &cli.output {
        Some(path) => {
            std::fs::write(path, &document)
                .with_context(|| format!("failed to write {}", path.display()))?;
        }
        None => {
            std::io::stdout().write_all(document.as_bytes())?;
        }
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
