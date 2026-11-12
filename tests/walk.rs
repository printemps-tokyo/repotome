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
fn summary_names_top_token_files() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(root.join("src/big.rs"), "b".repeat(400)).unwrap(); // 100 tokens
    fs::write(root.join("src/gen.rs"), "g".repeat(120)).unwrap(); // 30 tokens
    fs::write(root.join("note.txt"), "n".repeat(40)).unwrap(); // 10 tokens

    let opts = Options {
        tokens: true,
        ..Options::default()
    };
    let entries = collect(root, &opts).unwrap();
    let doc = render("proj", &entries, &opts);
    assert!(doc.contains("- Top files: src/big.rs ~100t, src/gen.rs ~30t, note.txt ~10t"));

    // Without --tokens the summary keeps its original shape.
    let plain = render("proj", &entries, &Options::default());
    assert!(!plain.contains("Top files:"));
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

#[test]
fn an_exclude_preset_drops_lock_files_and_build_output() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    fs::create_dir_all(root.join("dist")).unwrap();
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(root.join("package.json"), "{}\n").unwrap();
    fs::write(root.join("package-lock.json"), "{}\n").unwrap();
    fs::write(root.join("dist/app.js"), "x\n").unwrap();
    fs::write(root.join("src/app.min.js"), "x\n").unwrap();
    fs::write(root.join("src/app.ts"), "export {}\n").unwrap();

    let opts = Options {
        exclude: repotome::preset_globs(&["auto".into()], root).unwrap(),
        ..Options::default()
    };
    let paths: Vec<String> = collect(root, &opts)
        .unwrap()
        .into_iter()
        .map(|e| e.rel_path)
        .collect();
    assert_eq!(paths, vec!["package.json", "src/app.ts"]);
}

#[test]
fn split_tokens_writes_numbered_parts() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("proj");
    fs::create_dir_all(&root).unwrap();
    fs::write(root.join("a.txt"), "a".repeat(400)).unwrap();
    fs::write(root.join("b.txt"), "b".repeat(400)).unwrap();
    let out = dir.path().join("pack.md");
    let status = std::process::Command::new(env!("CARGO_BIN_EXE_repotome"))
        .arg(&root)
        .args(["--split-tokens", "150", "-o"])
        .arg(&out)
        .status()
        .unwrap();
    assert!(status.success());
    let one = fs::read_to_string(dir.path().join("pack-1.md")).unwrap();
    let two = fs::read_to_string(dir.path().join("pack-2.md")).unwrap();
    assert!(one.contains("(part 1 of 2)") && one.contains("aaaa") && !one.contains("bbbb"));
    assert!(two.contains("(part 2 of 2)") && two.contains("bbbb"));
    assert!(!out.exists());

    // Without --output there is nowhere to put the parts.
    let o = std::process::Command::new(env!("CARGO_BIN_EXE_repotome"))
        .arg(&root)
        .args(["--split-tokens", "150"])
        .output()
        .unwrap();
    assert!(!o.status.success());
}

#[test]
fn a_repository_nested_in_another_follows_its_own_ignore_rules() {
    let dir = tempfile::tempdir().unwrap();
    let outer = dir.path();
    // An outer repository that ignores everything (like a workspace of repos).
    fs::create_dir_all(outer.join(".git")).unwrap();
    fs::write(outer.join(".gitignore"), "*\n!.gitignore\n").unwrap();
    let inner = outer.join("inner");
    fs::create_dir_all(inner.join(".git")).unwrap();
    fs::create_dir_all(inner.join("src")).unwrap();
    fs::write(inner.join(".gitignore"), "build/\n").unwrap();
    fs::write(inner.join("src/a.txt"), "hi\n").unwrap();
    fs::create_dir_all(inner.join("build")).unwrap();
    fs::write(inner.join("build/out.txt"), "x\n").unwrap();

    let paths: Vec<String> = collect(&inner, &Options::default())
        .unwrap()
        .into_iter()
        .map(|e| e.rel_path)
        .collect();
    assert_eq!(paths, vec!["src/a.txt"]);

    // A subdirectory of the inner repository still sees the inner rules.
    let sub: Vec<String> = collect(&inner.join("src"), &Options::default())
        .unwrap()
        .into_iter()
        .map(|e| e.rel_path)
        .collect();
    assert_eq!(sub, vec!["a.txt"]);
}
