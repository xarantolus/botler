//! Content-addressed image tags, the pi-resources export and garbage collection (fake runtime).

mod common;

use std::collections::BTreeMap;

use botler_hub::images::{self, Recipe};
use botler_runtime::Containers;

async fn tags(e: &common::Env, recipes: &[Recipe]) -> Vec<String> {
    let commit = e.hub.applied().await.commit.clone();
    let mut out = Vec::new();
    for r in recipes {
        out.push(images::tag(&e.hub, r, &commit).await.unwrap());
    }
    out
}

fn job_files(names: &[&str]) -> Vec<(String, String)> {
    names.iter().flat_map(|n| common::job(n, "lifetime = \"oneshot\"\ncommand = [\"true\"]\n")).collect()
}

#[tokio::test]
async fn tags_follow_declared_inputs_only() {
    let e = common::env().await;
    images::ensure_base(&e.hub).await.unwrap();
    let files = job_files(&["a", "b"]);
    e.commit(&files.iter().map(|(p, b)| (p.as_str(), b.as_str())).collect::<Vec<_>>()).await;
    let recipes = [Recipe::agent("agent"), Recipe::agent("dev"), Recipe::job("a"), Recipe::job("b")];
    let before = tags(&e, &recipes).await;
    assert_eq!(before, tags(&e, &recipes).await, "same inputs, same tags");
    assert!(before[0].starts_with("localhost/botler/image-agent:"));

    e.commit(&[("agents/chat/AGENTS.md", "changed\n")]).await;
    assert_eq!(before, tags(&e, &recipes).await, "AGENTS.md is no image's input");

    e.commit(&[("jobs/a/src/main.py", "print(1)\n")]).await;
    let after = tags(&e, &recipes).await;
    assert_eq!(after[..2], before[..2]);
    assert_ne!(after[2], before[2], "job a changed");
    assert_eq!(after[3], before[3]);

    e.commit(&[("packages/shared/src/shared/x.py", "")]).await;
    let shared = tags(&e, &recipes).await;
    assert_eq!(shared[..2], after[..2]);
    assert!(shared[2] != after[2] && shared[3] != after[3], "shared packages feed every job");

    e.commit(&[("images/dev/Dockerfile", "FROM botler/agent-base:latest\nRUN true\n")]).await;
    let dev = tags(&e, &recipes).await;
    assert_eq!(dev[0], shared[0]);
    assert_ne!(dev[1], shared[1]);
}

#[tokio::test]
async fn unrelated_merge_builds_nothing_and_pi_export_is_shared() {
    let e = common::env().await;
    let first = e.hub.applied().await.commit.clone();
    let tag = images::ensure_agent_image(&e.hub, "agent", &first).await.unwrap();
    let builds = e.fake.builds().len();
    let pi = images::pi_resources(&e.hub, &first).await.unwrap().expect("template has pi/");
    assert!(pi.join("skills").is_dir());

    e.commit(&[("agents/chat/AGENTS.md", "changed\n")]).await;
    let second = e.hub.applied().await.commit.clone();
    assert_eq!(images::ensure_agent_image(&e.hub, "agent", &second).await.unwrap(), tag);
    assert_eq!(e.fake.builds().len(), builds, "no rebuild");
    assert_eq!(images::pi_resources(&e.hub, &second).await.unwrap().unwrap(), pi);

    e.commit(&[("pi/skills/x/SKILL.md", "x\n")]).await;
    let third = e.hub.applied().await.commit.clone();
    let pi3 = images::pi_resources(&e.hub, &third).await.unwrap().unwrap();
    assert_ne!(pi3, pi);
    assert!(pi3.join("skills/x/SKILL.md").is_file());
    // The build context held only the image's inputs and is gone afterwards.
    let exports: Vec<_> = std::fs::read_dir(e.hub.paths.exports()).unwrap().map(|d| d.unwrap().file_name()).collect();
    assert!(exports.iter().all(|n| n.to_string_lossy().starts_with("pi-")), "{exports:?}");
}

#[tokio::test]
async fn garbage_collection_keeps_only_referenced() {
    let e = common::env().await;
    let files = job_files(&["a"]);
    e.commit(&files.iter().map(|(p, b)| (p.as_str(), b.as_str())).collect::<Vec<_>>()).await;
    let c1 = e.hub.applied().await.commit.clone();
    let old_job = images::ensure_job_image(&e.hub, "a", &c1).await.unwrap();
    let agent = images::ensure_agent_image(&e.hub, "agent", &c1).await.unwrap();
    let old_pi = images::pi_resources(&e.hub, &c1).await.unwrap().unwrap();

    e.commit(&[("jobs/a/src/main.py", "print(1)\n"), ("pi/prompts/p.md", "p\n")]).await;
    let c2 = e.hub.applied().await.commit.clone();
    let new_job = images::ensure_job_image(&e.hub, "a", &c2).await.unwrap();
    let new_pi = images::pi_resources(&e.hub, &c2).await.unwrap().unwrap();
    // A foreign image (no owner label) and a stale legacy export older than the grace period.
    e.fake
        .build(&e.hub.paths.assets().join("agent-base"), "Dockerfile", "localhost/other:1", &BTreeMap::new())
        .await
        .unwrap();
    let legacy = e.hub.paths.exports().join("0123abcd");
    std::fs::create_dir_all(&legacy).unwrap();
    let old = std::time::SystemTime::now() - std::time::Duration::from_secs(7200);
    for d in [&legacy, &old_pi.parent().unwrap().to_path_buf()] {
        std::fs::File::open(d).unwrap().set_modified(old).unwrap();
    }

    images::collect_garbage(&e.hub).await.unwrap();
    let left = e.fake.images(&BTreeMap::new()).await.unwrap();
    assert!(left.contains(&new_job) && left.contains(&agent), "{left:?}");
    assert!(!left.contains(&old_job), "{left:?}");
    assert!(left.iter().any(|t| t == "localhost/other:1"), "never touches foreign images");
    assert!(left.iter().any(|t| t.contains("agent-base")), "never touches the base image");
    assert!(new_pi.exists() && !old_pi.exists() && !legacy.exists());
}

/// Real Podman: `BOTLER_PODMAN_TESTS=1`. Needs the agent base image already built (it is never
/// built here, since its `latest` tag is shared with any other hub on this host). Removes only
/// the images it built, which carry its temporary data directory as owner.
#[tokio::test]
async fn podman_unrelated_merge_skips_build_and_gc_removes_stale() {
    if std::env::var("BOTLER_PODMAN_TESTS").as_deref() != Ok("1") {
        return;
    }
    let rt = std::sync::Arc::new(botler_runtime::Cli::detect(None).await.unwrap());
    let base = botler_hub::assets::agent_base_tag();
    if rt.image_id(&base).await.unwrap().is_none()
        || rt.image_id(botler_hub::assets::AGENT_BASE_LATEST).await.unwrap().is_none()
    {
        eprintln!("skipped: {base} or the latest base image is missing");
        return;
    }
    let e = common::env_on(rt.clone(), botler_runtime::fake::Fake::new()).await;
    let owner = BTreeMap::from([(images::OWNER_LABEL.to_string(), e.hub.paths.root.to_string_lossy().into_owned())]);
    let c1 = e.hub.applied().await.commit.clone();
    let dev1 = images::ensure_agent_image(&e.hub, "dev", &c1).await.unwrap();
    let agent1 = images::ensure_agent_image(&e.hub, "agent", &c1).await.unwrap();

    e.commit(&[("agents/chat/AGENTS.md", "changed\n")]).await;
    let c2 = e.hub.applied().await.commit.clone();
    let started = std::time::Instant::now();
    assert_eq!(images::ensure_agent_image(&e.hub, "dev", &c2).await.unwrap(), dev1);
    assert_eq!(images::ensure_agent_image(&e.hub, "agent", &c2).await.unwrap(), agent1);
    eprintln!("AGENTS.md-only merge: both images ensured in {:?} without a build", started.elapsed());

    e.commit(&[("images/agent/Dockerfile", "FROM botler/agent-base:latest\nLABEL test=changed\n")]).await;
    let c3 = e.hub.applied().await.commit.clone();
    let agent3 = images::ensure_agent_image(&e.hub, "agent", &c3).await.unwrap();
    assert_ne!(agent3, agent1);
    images::collect_garbage(&e.hub).await.unwrap();
    let left = rt.images(&owner).await.unwrap();
    assert!(left.contains(&agent3) && left.contains(&dev1) && !left.contains(&agent1), "{left:?}");

    for t in left {
        rt.remove_image(&t).await.unwrap();
    }
}
