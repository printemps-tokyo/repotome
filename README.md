# repotome

> Pack a git repository into a single text document for LLMs. Fast Rust CLI.

[![CI](https://github.com/printemps-tokyo/repotome/actions/workflows/ci.yml/badge.svg)](https://github.com/printemps-tokyo/repotome/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](./LICENSE)

`repotome` walks a repository (respecting `.gitignore`), renders a directory
tree, and concatenates every text file into one document you can paste into an
LLM. Binary and oversized files are listed but their contents are omitted. It is
a single static binary — no Node, no Python.

## Why

Feeding a codebase to an LLM means flattening it into one well-structured text
blob. Doing that by hand is tedious and easy to get wrong (committing secrets,
including `node_modules`, blowing the context window on a lockfile). `repotome`
does it in one command, honoring `.gitignore` and skipping binaries.

## Install

```bash
cargo install --git https://github.com/printemps-tokyo/repotome
```

Or download a prebuilt binary from the [Releases](https://github.com/printemps-tokyo/repotome/releases) page.

## Usage

```bash
repotome                       # pack the current directory to stdout
repotome ./my-project --tokens # include an approximate token count
repotome . -o context.md       # write to a file
repotome . --include '*.rs' --include '*.toml'
repotome . --exclude 'target/**' --max-size 256k
repotome . --format xml        # repomix-style XML wrapper
```

Example output (Markdown):

````text
# Repository: my-project

## Summary

- Files: 2 text, 1 skipped
- Text size: 412 bytes
- Approx tokens: ~103

## Structure

```
src/
  main.rs
README.md
logo.png  (binary, omitted)
```

## Files

### `src/main.rs`

```rust
fn main() { println!("hello"); }
```
...
````

The one-line run summary (files included/skipped, bytes, tokens) is printed to
stderr, so piping stdout stays clean.

## Options

| Option | Description |
| --- | --- |
| `[PATH]` | Directory to pack (default `.`) |
| `-o, --output <FILE>` | Write to a file instead of stdout |
| `--include <GLOB>` | Include only files matching this glob (repeatable) |
| `--exclude <GLOB>` | Exclude files matching this glob (repeatable) |
| `--max-size <SIZE>` | Skip files larger than this, e.g. `1MiB`, `500k` (default `1MiB`) |
| `--no-gitignore` | Do not respect `.gitignore` / `.ignore` |
| `--hidden` | Include hidden files (dotfiles) |
| `--no-tree` | Omit the directory-tree section |
| `--no-contents` | Omit file bodies; produce only the summary and tree |
| `--copy` | Copy the output to the system clipboard |
| `--format <md\|xml>` | Output format (default `md`) |
| `--tokens` | Include an approximate token count in the summary |

`--copy` uses `pbcopy` (macOS), `wl-copy` / `xclip` (Linux), or `clip`
(Windows); with `--copy` and no `--output`, stdout is suppressed.

## Notes

- `.gitignore` (and `.ignore`) are honored even when the target is not inside a
  git repository; the `.git` directory is always skipped. Use `--no-gitignore`
  to disable, `--hidden` to include dotfiles.
- A `.repotomeignore` file (same syntax as `.gitignore`) is always honored, for
  excludes you want only when packing — e.g. lockfiles, fixtures, or generated
  code that bloats the context.
- A file is treated as binary if it contains a NUL byte or is not valid UTF-8;
  such files are listed in the tree but their contents are omitted. Files that
  cannot be read (permissions, etc.) are listed as `(unreadable, omitted)`.
- `--include` / `--exclude` globs match against the repo-relative path, and `*`
  spans `/` (so `--include '*.rs'` matches Rust files at any depth). The
  `--output` file is skipped automatically when it lives inside the target.
- The token count is a rough heuristic (about 4 characters per token), not a
  tokenizer — treat it as a ballpark.
- Markdown output picks a code fence longer than any run of backticks inside a
  file, so files that themselves contain fences are embedded safely.

## Library

```rust
use repotome::{collect, render, Options};

let opts = Options::default();
let entries = collect(std::path::Path::new("."), &opts)?;
let document = render("my-project", &entries, &opts);
println!("{document}");
# Ok::<(), anyhow::Error>(())
```

## License

[MIT](./LICENSE) (c) printemps.tokyo
