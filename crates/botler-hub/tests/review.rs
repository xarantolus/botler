//! The review diff: what a proposal changes, per file, as the user approves it.

use botler_core::defs::Definitions;
use botler_hub::review::{self, FILE_LINES, FileStatus};
use botler_repo::Repo;

async fn repo(dir: &std::path::Path) -> (Repo, String) {
    let template = dir.join("template");
    std::fs::create_dir_all(template.join("jobs/x")).unwrap();
    std::fs::write(template.join("README.md"), "hello\nworld\n").unwrap();
    std::fs::write(template.join("jobs/x/old.py"), "print('moved')\n".repeat(20)).unwrap();
    std::fs::write(template.join("gone.txt"), "bye\n").unwrap();
    let repo = Repo::new(dir.join("monorepo.git"), dir.join("wt"));
    let root = repo.init_from(&template).await.unwrap();
    (repo, root)
}

#[tokio::test]
async fn files_hunks_and_flags() {
    let dir = tempfile::tempdir().unwrap();
    let (repo, base) = repo(dir.path()).await;
    let big: String = (0..FILE_LINES + 100).map(|i| format!("line {i}\n")).collect();
    let head = repo
        .commit_changes(
            &base,
            &[("jobs/x/old.py", "jobs/x/new.py")],
            &[
                ("README.md", Some("hello\nthere\n")),
                ("gate/run", Some("#!/bin/sh\nexit 0\n")),
                ("gone.txt", None),
                ("logo.png", Some("\u{0}\u{1}\u{2}binary")),
                ("big.txt", Some(&big)),
            ],
            "change",
        )
        .await
        .unwrap();
    let d = review::diff(&repo, &base, &head, &Definitions::default()).await.unwrap();
    let file = |p: &str| d.files.iter().find(|f| f.path == p).unwrap_or_else(|| panic!("{p} in {:?}", d.files));

    let readme = file("README.md");
    assert_eq!((readme.status, readme.additions, readme.deletions), (FileStatus::Modified, 1, 1));
    assert_eq!(readme.hunks.len(), 1);
    assert_eq!(readme.hunks[0].old_start, 1);
    assert_eq!(readme.hunks[0].lines, [" hello", "-world", "+there"]);
    assert!(!readme.protected && !readme.binary);

    let gate = file("gate/run");
    assert_eq!((gate.status, gate.additions), (FileStatus::Added, 2));
    assert!(gate.protected, "gate/ is always protected");

    assert_eq!(file("gone.txt").status, FileStatus::Deleted);
    assert_eq!(file("gone.txt").deletions, 1);

    let moved = file("jobs/x/new.py");
    assert_eq!(moved.status, FileStatus::Renamed);
    assert_eq!(moved.old_path.as_deref(), Some("jobs/x/old.py"));
    assert!(moved.hunks.is_empty());

    let logo = file("logo.png");
    assert!(logo.binary && logo.hunks.is_empty());
    assert_eq!(logo.status, FileStatus::Added);

    let big = file("big.txt");
    let shown: usize = big.hunks.iter().map(|h| h.lines.len()).sum();
    assert_eq!(shown, FILE_LINES);
    assert_eq!(big.omitted, 100);
    assert_eq!(big.additions as usize, FILE_LINES + 100, "counts cover the whole file");
}

#[tokio::test]
async fn compares_against_the_merge_base() {
    let dir = tempfile::tempdir().unwrap();
    let (repo, root) = repo(dir.path()).await;
    let branch = repo.commit_files(&root, &[("a.txt", Some("a\n"))], "branch").await.unwrap();
    // main moved on after the branch started: its change is not part of the proposal.
    let main = repo.commit_files(&root, &[("b.txt", Some("b\n"))], "main").await.unwrap();
    let d = review::diff(&repo, &main, &branch, &Definitions::default()).await.unwrap();
    let paths: Vec<&str> = d.files.iter().map(|f| f.path.as_str()).collect();
    assert_eq!(paths, ["a.txt"]);
}
