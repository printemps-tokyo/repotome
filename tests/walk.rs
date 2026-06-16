//! Integration tests that build a temporary directory tree and pack it.

use std::fs;

use repotome::{collect, render, EntryKind, Options};

fn find<'a>(entries: &'a [repotome::Entry], path: &str) -> Option<&'a repotome::Entry> {
    entries.iter().find(|e| e.rel_path == path)
}

#[test]
fn packs_a_directory_respecting_rules() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();

    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(root.join("src/main.rs"), "fn main() {}\n").unwrap();
    fs::write(root.join("README.md"), "# hello\n").unwrap();
    fs::write(root.join(".gitignore"), "ignored.txt\n").unwrap();
    fs::write(root.join("ignored.txt"), "should not appear\n").unwrap();
    // Binary file (contains a NUL byte).
    fs::write(root.join("blob.bin"), [0u8, 1, 2, 3]).unwrap();
    // Oversized file.
    fs::write(root.join("big.txt"), "x".repeat(2048)).unwrap();

    let opts = Options {
        max_size: 1024,
        ..Options::default()
    };
    let entries = collect(root, &opts).unwrap();

    // .gitignore is respected.
    assert!(find(&entries, "ignored.txt").is_none());
    // .gitignore file itself is hidden by default.
    assert!(find(&entries, ".gitignore").is_none());

    // Text files are read.
    match &find(&entries, "src/main.rs").unwrap().kind {
        EntryKind::Text(t) => assert!(t.contains("fn main")),
        other => panic!("expected text, got {other:?}"),
    }

    // Binary file is listed but omitted.
    assert_eq!(find(&entries, "blob.bin").unwrap().kind, EntryKind::Binary);

    // Oversized file is skipped (TooLarge).
    match find(&entries, "big.txt").unwrap().kind {
        EntryKind::TooLarge(n) => assert_eq!(n, 2048),
        ref other => panic!("expected too-large, got {other:?}"),
    }

    // Deterministic order.
    let mut sorted = entries.clone();
    sorted.sort_by(|a, b| a.rel_path.cmp(&b.rel_path));
    assert_eq!(entries, sorted);

    let doc = render("proj", &entries, &opts);
    assert!(doc.contains("# Repository: proj"));
    assert!(doc.contains("### `src/main.rs`"));
    assert!(doc.contains("blob.bin  (binary, omitted)"));
    assert!(doc.contains("big.txt  (skipped: 2048 bytes > max)"));
}

#[test]
fn repotomeignore_excludes_files() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    fs::write(root.join("keep.rs"), "fn keep() {}\n").unwrap();
    fs::write(root.join("drop.rs"), "fn drop() {}\n").unwrap();
    fs::write(root.join(".repotomeignore"), "drop.rs\n").unwrap();

    let entries = collect(root, &Options::default()).unwrap();
    assert!(find(&entries, "keep.rs").is_some());
    assert!(find(&entries, "drop.rs").is_none());
}

#[test]
fn include_glob_filters() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    fs::write(root.join("a.rs"), "fn a() {}\n").unwrap();
    fs::write(root.join("b.py"), "def b(): pass\n").unwrap();

    let opts = Options {
        include: vec!["*.rs".to_string()],
        ..Options::default()
    };
    let entries = collect(root, &opts).unwrap();
    assert!(find(&entries, "a.rs").is_some());
    assert!(find(&entries, "b.py").is_none());
}
