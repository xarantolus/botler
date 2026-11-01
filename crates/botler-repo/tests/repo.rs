use std::path::Path;
use std::process::Command;

use botler_repo::{Error, Merge, Repo};

fn write(root: &Path, rel: &str, body: &str) {
    let p = root.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, body).unwrap();
}

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_AUTHOR_NAME", "agent")
        .env("GIT_AUTHOR_EMAIL", "a@x")
        .env("GIT_COMMITTER_NAME", "agent")
        .env("GIT_COMMITTER_EMAIL", "a@x")
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

struct Fixture {
    _dir: tempfile::TempDir,
    repo: Repo,
    root: std::path::PathBuf,
}

async fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let tpl = dir.path().join("template");
    write(&tpl, "botler.toml", "timezone = \"UTC\"\n");
    write(&tpl, "agents/main/agent.toml", "kind = \"persistent\"\n");
    write(&tpl, "agents/main/AGENTS.md", "You are main.\n");
    write(&tpl, "jobs/poll/job.toml", "command = [\"python\", \"-m\", \"poll\"]\n");
    write(&tpl, "jobs/poll/Dockerfile", "FROM x\n");
    write(&tpl, "jobs/poll/src/poll.py", "print('not a definition')\n");
    let repo = Repo::new(dir.path().join("repo.git"), dir.path().join("worktrees"));
    repo.init_from(&tpl).await.unwrap();
    let root = dir.path().to_path_buf();
    Fixture { _dir: dir, repo, root }
}

/// An agent-style clone that commits on its own branch and pushes to the bare repo.
fn agent_commit(f: &Fixture, name: &str, branch: &str, rel: &str, body: &str) -> String {
    let clone = f.root.join(name);
    if !clone.exists() {
        git(&f.root, &["clone", "-q", f.repo.git_dir().to_str().unwrap(), name]);
    }
    git(&clone, &["fetch", "-q", "origin"]);
    git(&clone, &["checkout", "-q", "-B", branch, "origin/main"]);
    write(&clone, rel, body);
    git(&clone, &["add", "-A"]);
    git(&clone, &["commit", "-q", "-m", &format!("edit {rel}")]);
    git(&clone, &["push", "-q", "origin", &format!("{branch}:{branch}")]);
    git(&clone, &["rev-parse", "HEAD"])
}

#[tokio::test]
async fn init_loads_only_definitions() {
    let f = fixture().await;
    let main = f.repo.main().await.unwrap();
    let tree = f.repo.definition_tree(&main).await.unwrap();
    let paths: Vec<&str> = tree.paths().collect();
    assert!(paths.contains(&"agents/main/agent.toml"));
    assert!(paths.contains(&"jobs/poll/Dockerfile"));
    assert!(!paths.iter().any(|p| p.ends_with(".py")));
    assert!(!f.repo.git_dir().join("index").exists());
}

#[tokio::test]
async fn advance_main_is_compare_and_swap() {
    let f = fixture().await;
    let base = f.repo.main().await.unwrap();
    let head = agent_commit(&f, "a", "agents/main/x", "triggers/a.toml", "");
    assert_eq!(f.repo.changed_paths(&base, &head).await.unwrap(), vec!["triggers/a.toml"]);
    f.repo.advance_main(&base, &head).await.unwrap();
    assert_eq!(f.repo.main().await.unwrap(), head);
    // stale expectation
    assert!(matches!(
        f.repo.advance_main(&base, &head).await,
        Err(Error::MainMoved { .. }) | Err(Error::NotFastForward { .. })
    ));
}

#[tokio::test]
async fn rebase_and_conflict() {
    let f = fixture().await;
    let base = f.repo.main().await.unwrap();
    let one = agent_commit(&f, "a", "agents/a/one", "triggers/one.toml", "");
    let two = agent_commit(&f, "b", "agents/b/two", "triggers/two.toml", "");
    f.repo.advance_main(&base, &one).await.unwrap();
    let rebased = f.repo.rebase_onto_main(&two, "two").await.unwrap();
    assert_ne!(rebased, two);
    // The same change, rebased: the same patch id; another change: another one.
    let patch = f.repo.patch_id(&base, &two).await.unwrap();
    assert!(patch.is_some());
    assert_eq!(f.repo.patch_id(&one, &rebased).await.unwrap(), patch);
    assert_ne!(f.repo.patch_id(&base, &one).await.unwrap(), patch);
    assert_eq!(f.repo.patch_id(&one, &one).await.unwrap(), None);
    f.repo.advance_main(&one, &rebased).await.unwrap();

    let c1 = agent_commit(&f, "a", "agents/a/c1", "botler.toml", "timezone = \"Europe/Berlin\"\n");
    let c2 = agent_commit(&f, "b", "agents/b/c2", "botler.toml", "timezone = \"Asia/Tokyo\"\n");
    f.repo.advance_main(&rebased, &c1).await.unwrap();
    assert!(matches!(f.repo.rebase_onto_main(&c2, "c2").await, Err(Error::Conflict { .. })));
    // temporary worktrees are cleaned up even on conflict
    assert_eq!(std::fs::read_dir(f.root.join("worktrees")).unwrap().count(), 0);
}

#[tokio::test]
async fn revert_and_export() {
    let f = fixture().await;
    let base = f.repo.main().await.unwrap();
    let head = agent_commit(&f, "a", "agents/a/x", "triggers/x.toml", "[[trigger]]\n");
    f.repo.advance_main(&base, &head).await.unwrap();
    let rev = f.repo.revert_commit(&head).await.unwrap();
    f.repo.advance_main(&head, &rev).await.unwrap();
    let tree = f.repo.definition_tree(&rev).await.unwrap();
    assert!(!tree.paths().any(|p| p == "triggers/x.toml"));

    let out = f.root.join("export");
    f.repo.export(&rev, &out).await.unwrap();
    assert!(out.join("jobs/poll/src/poll.py").exists());

    let part = f.root.join("partial");
    f.repo.export_paths(&rev, &["jobs/poll", "botler.toml"], &part).await.unwrap();
    assert!(part.join("jobs/poll/src/poll.py").exists() && part.join("botler.toml").exists());
    assert!(!part.join("agents").exists());

    // Exports of the same content side by side (`x.partial-<id>`) do not share an archive.
    let dirs: Vec<_> = (0..4).map(|i| f.root.join(format!("pi-tree.partial-{i}"))).collect();
    let paths = ["jobs/poll"];
    let (a, b, c, d) = tokio::join!(
        f.repo.export_paths(&rev, &paths, &dirs[0]),
        f.repo.export_paths(&rev, &paths, &dirs[1]),
        f.repo.export_paths(&rev, &paths, &dirs[2]),
        f.repo.export_paths(&rev, &paths, &dirs[3]),
    );
    for r in [a, b, c, d] {
        r.unwrap();
    }
    assert!(dirs.iter().all(|d| d.join("jobs/poll/src/poll.py").exists()));
}

#[tokio::test]
async fn path_ids_are_tree_hashes() {
    let f = fixture().await;
    let base = f.repo.main().await.unwrap();
    let ids = f.repo.path_ids(&base, &["jobs/poll", "botler.toml", "missing", "agents/main"]).await.unwrap();
    assert_eq!(ids.keys().collect::<Vec<_>>(), ["agents/main", "botler.toml", "jobs/poll"]);
    let head = agent_commit(&f, "a", "agents/main/x", "agents/main/AGENTS.md", "changed\n");
    let after = f.repo.path_ids(&head, &["jobs/poll", "botler.toml", "agents/main"]).await.unwrap();
    assert_eq!(after["jobs/poll"], ids["jobs/poll"]);
    assert_ne!(after["agents/main"], ids["agents/main"]);
}

#[tokio::test]
async fn failed_init_leaves_no_repo() {
    let dir = tempfile::tempdir().unwrap();
    let repo = Repo::new(dir.path().join("repo.git"), dir.path().join("worktrees"));
    assert!(repo.init_from(&dir.path().join("no-such-template")).await.is_err());
    assert!(!dir.path().join("repo.git").exists());
}

#[tokio::test]
async fn hook_restricts_pushes_to_own_namespace() {
    let f = fixture().await;
    let clone = f.root.join("c");
    git(&f.root, &["clone", "-q", f.repo.git_dir().to_str().unwrap(), "c"]);
    write(&clone, "x.txt", "x");
    git(&clone, &["add", "-A"]);
    git(&clone, &["commit", "-q", "-m", "x"]);
    let push = |refspec: &str| {
        Command::new("git")
            .args(["push", "-q", "origin", refspec])
            .current_dir(&clone)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("REMOTE_USER", "alice")
            .output()
            .unwrap()
            .status
            .success()
    };
    assert!(push("HEAD:refs/heads/agents/alice/x"));
    assert!(!push("HEAD:refs/heads/main"));
    assert!(!push("HEAD:refs/heads/agents/bob/x"));
    assert_eq!(f.repo.agent_branches("alice").await.unwrap().len(), 1);
}

#[tokio::test]
async fn pin_is_namespaced() {
    let f = fixture().await;
    let main = f.repo.main().await.unwrap();
    f.repo.pin("refs/proposals/p1", &main).await.unwrap();
    assert!(f.repo.pin("refs/heads/main", &main).await.is_err());
}

#[tokio::test]
async fn hub_commits_files_on_main() {
    let f = fixture().await;
    let base = f.repo.main().await.unwrap();
    let head = f
        .repo
        .commit_files(&base, &[("secrets.toml", Some("x = 1\n")), ("jobs/poll/src/poll.py", None)], "hub edit")
        .await
        .unwrap();
    f.repo.advance_main(&base, &head).await.unwrap();
    assert_eq!(f.repo.read_file(&head, "secrets.toml").await.unwrap().as_deref(), Some("x = 1\n"));
    assert_eq!(f.repo.read_file(&head, "jobs/poll/src/poll.py").await.unwrap(), None);
    assert_eq!(f.repo.read_file(&head, "nope.txt").await.unwrap(), None);
}

/// A new template snapshot on top of the current one, from the template with `edits` applied.
async fn snapshot(f: &Fixture, edits: &[(&str, &str)]) -> String {
    let tpl = f.root.join("template");
    for (rel, body) in edits {
        write(&tpl, rel, body);
    }
    let tree = f.repo.write_tree_from(&tpl).await.unwrap();
    let parent = f.repo.rev(botler_repo::TEMPLATE_REF).await.unwrap().unwrap();
    let snap = f.repo.commit_tree(&tree, Some(&parent), "template v2").await.unwrap();
    f.repo.pin(botler_repo::TEMPLATE_REF, &snap).await.unwrap();
    snap
}

const PROMPT: &str = "# main\n\nline 2\nline 3\nline 4\nline 5\nline 6\n";

#[tokio::test]
async fn init_is_the_first_template_snapshot() {
    let f = fixture().await;
    let main = f.repo.main().await.unwrap();
    assert_eq!(f.repo.rev(botler_repo::TEMPLATE_REF).await.unwrap().as_deref(), Some(main.as_str()));
    assert_eq!(f.repo.roots(&main).await.unwrap(), [(main.clone(), botler_repo::HUB_EMAIL.to_string())]);
    // The same directory gives the same tree, so an unchanged template is no upgrade.
    assert_eq!(f.repo.write_tree_from(&f.root.join("template")).await.unwrap(), f.repo.tree(&main).await.unwrap());
    let snap = snapshot(&f, &[("botler.toml", "timezone = \"Europe/Berlin\"\n")]).await;
    assert_ne!(f.repo.tree(&snap).await.unwrap(), f.repo.tree(&main).await.unwrap());
    assert_eq!(f.repo.log_subjects(&main, &snap).await.unwrap().len(), 1, "linear: parent is the old snapshot");
    // The throwaway index is gone.
    assert_eq!(std::fs::read_dir(f.root.join("worktrees")).unwrap().count(), 0);
}

#[tokio::test]
async fn template_merge_keeps_agent_edits() {
    let f = fixture().await;
    write(&f.root.join("template"), "agents/main/AGENTS.md", PROMPT);
    let s1 = snapshot(&f, &[]).await;
    let base = f.repo.main().await.unwrap();
    let merged = match f.repo.merge_into_main(&s1, "upgrade 1").await.unwrap() {
        Merge::Clean(c) => c,
        m => panic!("{m:?}"),
    };
    f.repo.advance_main(&base, &merged).await.unwrap();
    // The agent edits the top, the template the bottom of the same file.
    let edit = agent_commit(&f, "a", "agents/a/x", "agents/main/AGENTS.md", &PROMPT.replace("line 2", "agent line"));
    f.repo.advance_main(&merged, &edit).await.unwrap();
    let s2 = snapshot(&f, &[("agents/main/AGENTS.md", &PROMPT.replace("line 6", "template line"))]).await;
    let Merge::Clean(up) = f.repo.merge_into_main(&s2, "upgrade 2").await.unwrap() else { panic!("conflict") };
    let text = f.repo.read_file(&up, "agents/main/AGENTS.md").await.unwrap().unwrap();
    assert!(text.contains("agent line") && text.contains("template line"), "{text}");
    assert_eq!(f.repo.main().await.unwrap(), edit, "main is not moved");
    assert!(f.repo.is_ancestor(&edit, &up).await.unwrap() && f.repo.is_ancestor(&s2, &up).await.unwrap());
}

#[tokio::test]
async fn conflicting_template_merge_is_committed_with_markers() {
    let f = fixture().await;
    let base = f.repo.main().await.unwrap();
    let edit = agent_commit(&f, "a", "agents/a/x", "botler.toml", "timezone = \"Asia/Tokyo\"\n");
    f.repo.advance_main(&base, &edit).await.unwrap();
    let snap = snapshot(&f, &[("botler.toml", "timezone = \"Europe/Berlin\"\n")]).await;
    match f.repo.merge_into_main(&snap, "upgrade").await.unwrap() {
        Merge::Conflicted { commit, paths } => {
            assert_eq!(paths, ["botler.toml"]);
            let text = f.repo.read_file(&commit, "botler.toml").await.unwrap().unwrap();
            assert!(text.contains("<<<<<<<") && text.contains("Asia/Tokyo") && text.contains("Europe/Berlin"));
            assert!(f.repo.is_ancestor(&snap, &commit).await.unwrap());
        }
        m => panic!("{m:?}"),
    }
    assert_eq!(std::fs::read_dir(f.root.join("worktrees")).unwrap().count(), 0);
}
