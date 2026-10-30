use std::path::Path;
use std::process::Command;

use botler_repo::{Error, Repo};

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
    let repo = Repo::new(dir.path().join("repo.git"), dir.path().join("scratch"));
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
    f.repo.advance_main(&one, &rebased).await.unwrap();

    let c1 = agent_commit(&f, "a", "agents/a/c1", "botler.toml", "timezone = \"Europe/Berlin\"\n");
    let c2 = agent_commit(&f, "b", "agents/b/c2", "botler.toml", "timezone = \"Asia/Tokyo\"\n");
    f.repo.advance_main(&rebased, &c1).await.unwrap();
    assert!(matches!(f.repo.rebase_onto_main(&c2, "c2").await, Err(Error::Conflict { .. })));
    // scratch worktrees are cleaned up even on conflict
    assert_eq!(std::fs::read_dir(f.root.join("scratch")).unwrap().count(), 0);
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
