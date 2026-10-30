//! Memory: namespaced Markdown files the hub alone writes, with optimistic concurrency, git
//! history and read-only mounts in agent containers.

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use botler_hub::auth::Principal;
use botler_hub::memory::{MemoryError, sha256};
use botler_hub::ops::{self, Caller, OpError};
use botler_hub::{HubError, agents, router};
use botler_runtime::Mount;
use chrono::Utc;
use common::env;
use serde_json::{Value, json};
use tower::ServiceExt;

fn user() -> Caller {
    Caller { principal: Principal::User, capabilities: vec![], runnable: None }
}

async fn agent(e: &common::Env, definition: &str) -> Caller {
    let name = router::new_instance(&e.hub, definition, None, None).await.unwrap();
    Caller { principal: Principal::Agent { instance: name, wake: "w".into() }, capabilities: vec![], runnable: None }
}

fn memory_err(r: Result<Value, OpError>) -> MemoryError {
    match r {
        Err(OpError::Hub(HubError::Memory(m))) => m,
        other => panic!("expected a memory error, got {other:?}"),
    }
}

#[tokio::test]
async fn concurrent_stale_writers_get_one_success_and_conflicts_with_the_current_content() {
    let e = env().await;
    let path = "user/shared.md";
    let mut sha =
        ops::call(&e.hub, &user(), "memory_write", json!({"path": path, "content": "v0\n"})).await.unwrap()["sha256"]
            .as_str()
            .unwrap()
            .to_string();
    for round in 0..3 {
        let tasks: Vec<_> = (0..8)
            .map(|i| {
                let hub = e.hub.clone();
                let sha = sha.clone();
                tokio::spawn(async move {
                    let content = format!("round {round} writer {i}\n");
                    let r = ops::call(
                        &hub,
                        &user(),
                        "memory_write",
                        json!({"path": path, "content": content, "expected_sha256": sha}),
                    )
                    .await;
                    (content, r)
                })
            })
            .collect();
        let mut won = Vec::new();
        let mut conflicts = Vec::new();
        for t in tasks {
            let (content, r) = t.await.unwrap();
            match r {
                Ok(v) => won.push((content, v["sha256"].as_str().unwrap().to_string())),
                Err(OpError::Hub(HubError::Memory(MemoryError::Conflict(c)))) => conflicts.push(c),
                Err(other) => panic!("{other}"),
            }
        }
        assert_eq!(won.len(), 1, "round {round}: exactly one writer wins");
        assert_eq!(conflicts.len(), 7);
        let (content, new_sha) = won.pop().unwrap();
        for c in conflicts {
            let current = c.current.expect("the conflict carries the current version");
            assert_eq!(current.content, content);
            assert_eq!(current.sha256, new_sha);
        }
        assert_eq!(std::fs::read_to_string(e.hub.paths.memory().join(path)).unwrap(), content);
        sha = new_sha;
    }
}

#[tokio::test]
async fn create_only_without_expected_version_and_append_and_delete() {
    let e = env().await;
    let u = user();
    let w = |content: &str, expected: Option<&str>| json!({"path": "user/a.md", "content": content, "expected_sha256": expected});
    ops::call(&e.hub, &u, "memory_write", w("one\n", None)).await.unwrap();
    // Creating again is a conflict that shows what is there.
    let MemoryError::Conflict(c) = memory_err(ops::call(&e.hub, &u, "memory_write", w("two\n", None)).await) else {
        panic!("not a conflict")
    };
    assert_eq!(c.current.unwrap().content, "one\n");

    let appended =
        ops::call(&e.hub, &u, "memory_append", json!({"path": "user/a.md", "text": "- more"})).await.unwrap();
    let read = ops::call(&e.hub, &u, "memory_read", json!({"path": "user/a.md"})).await.unwrap();
    assert_eq!(read["content"], "one\n- more\n");
    assert_eq!(read["sha256"], appended["sha256"]);
    assert_eq!(read["sha256"], sha256("one\n- more\n").as_str());

    let stale =
        ops::call(&e.hub, &u, "memory_delete", json!({"path": "user/a.md", "expected_sha256": sha256("one\n")}));
    assert!(matches!(memory_err(stale.await), MemoryError::Conflict(_)));
    ops::call(&e.hub, &u, "memory_delete", json!({"path": "user/a.md", "expected_sha256": read["sha256"]}))
        .await
        .unwrap();
    assert!(matches!(
        memory_err(ops::call(&e.hub, &u, "memory_read", json!({"path": "user/a.md"})).await),
        MemoryError::NotFound(_)
    ));
}

#[tokio::test]
async fn agents_write_only_their_namespaces_and_the_user_writes_all() {
    let e = env().await;
    e.commit(&[("agents/station/agent.toml", "workspace = \"none\"\nhome = \"groundstation\"\n")]).await;
    let dev = agent(&e, "dev").await;
    let station = agent(&e, "station").await;
    let write = |path: &str| json!({"path": path, "content": "x\n"});
    for ok in ["agents/dev/notes.md", "user/github.md"] {
        ops::call(&e.hub, &dev, "memory_write", write(ok)).await.unwrap_or_else(|e| panic!("{ok}: {e}"));
    }
    ops::call(&e.hub, &station, "memory_write", write("projects/groundstation/INDEX.md")).await.unwrap();
    for no in ["agents/botler/notes.md", "projects/groundstation/x.md", "agents/station/x.md"] {
        let err = memory_err(ops::call(&e.hub, &dev, "memory_write", write(no)).await);
        assert!(matches!(err, MemoryError::Forbidden { .. }), "{no}: {err}");
        assert!(err.to_string().contains("agents/dev, user"), "{err}");
    }
    let err =
        memory_err(ops::call(&e.hub, &dev, "memory_append", json!({"path": "agents/station/x.md", "text": "y"})).await);
    assert!(matches!(err, MemoryError::Forbidden { .. }));
    // Agents read through their mounts; the read ops are for callers without one.
    assert!(matches!(
        ops::call(&e.hub, &dev, "memory_read", json!({"path": "user/github.md"})).await,
        Err(OpError::Forbidden(_))
    ));
    for any in ["agents/botler/notes.md", "projects/groundstation/x.md", "agents/station/x.md"] {
        ops::call(&e.hub, &user(), "memory_write", write(any)).await.unwrap();
    }
    let listed = ops::call(&e.hub, &user(), "memory_list", json!({"prefix": "agents"})).await.unwrap();
    let paths: Vec<&str> = listed.as_array().unwrap().iter().map(|f| f["path"].as_str().unwrap()).collect();
    assert_eq!(paths, ["agents/botler/notes.md", "agents/dev/notes.md", "agents/station/x.md"]);
    let all = ops::call(&e.hub, &user(), "memory_list", json!({})).await.unwrap();
    assert!(!all.to_string().contains(".git"), "{all}");
}

#[tokio::test]
async fn invalid_paths_and_sizes_are_refused() {
    let e = env().await;
    for bad in ["user/../agents/x/a.md", "/etc/passwd.md", "user/a.txt", "agents/dev/.git/HEAD.md", "elsewhere/a.md"] {
        let err = memory_err(ops::call(&e.hub, &user(), "memory_write", json!({"path": bad, "content": "x"})).await);
        assert!(matches!(err, MemoryError::InvalidPath { .. }), "{bad}: {err}");
    }
    let big = "x".repeat(botler_hub::memory::MAX_BYTES + 1);
    let err =
        memory_err(ops::call(&e.hub, &user(), "memory_write", json!({"path": "user/big.md", "content": big})).await);
    assert!(matches!(err, MemoryError::TooLarge { .. }), "{err}");
}

#[tokio::test]
async fn history_records_the_author() {
    let e = env().await;
    let dev = agent(&e, "dev").await;
    let Principal::Agent { instance, .. } = &dev.principal else { unreachable!() };
    let written =
        ops::call(&e.hub, &dev, "memory_write", json!({"path": "agents/dev/a.md", "content": "1\n"})).await.unwrap();
    ops::call(
        &e.hub,
        &user(),
        "memory_write",
        json!({"path": "agents/dev/a.md", "content": "2\n", "expected_sha256": written["sha256"]}),
    )
    .await
    .unwrap();
    let h = ops::call(&e.hub, &user(), "memory_history", json!({"path": "agents/dev/a.md"})).await.unwrap();
    let authors: Vec<&str> = h.as_array().unwrap().iter().map(|c| c["author"].as_str().unwrap()).collect();
    assert_eq!(authors, ["user", instance.as_str()]);
    assert_eq!(h[1]["message"], "write agents/dev/a.md");
}

#[tokio::test]
async fn http_conflicts_carry_the_current_version() {
    let e = env().await;
    let app = botler_hub::api::router(e.hub.clone());
    let token = e.hub.tokens.issue(&Principal::User, chrono::Duration::hours(1), Utc::now());
    let post = |body: Value| {
        Request::post("/api/v1/ops/memory_write")
            .header("authorization", format!("Bearer {token}"))
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap()
    };
    let resp = app.clone().oneshot(post(json!({"path": "user/p.md", "content": "a\n"}))).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let resp = app.clone().oneshot(post(json!({"path": "user/p.md", "content": "b\n"}))).await.unwrap();
    assert_eq!(resp.status(), StatusCode::CONFLICT);
    let body: Value = serde_json::from_slice(&axum::body::to_bytes(resp.into_body(), 1 << 20).await.unwrap()).unwrap();
    assert_eq!(body["conflict"]["current"]["content"], "a\n");
    assert_eq!(body["conflict"]["current"]["sha256"], sha256("a\n").as_str());
    let resp = app.oneshot(post(json!({"path": "user/../x.md", "content": "b\n"}))).await.unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn agents_get_their_namespaces_read_only_and_their_indexes_in_the_prompt() {
    let e = env().await;
    e.hub.store.set_secret(&e.hub.secret_key, "OPENROUTER_API_KEY", "sk-test").await.unwrap();
    e.commit(&[
        ("agents/station/agent.toml", "workspace = \"none\"\nhome = \"groundstation\"\n"),
        ("agents/station/AGENTS.md", "station agent"),
    ])
    .await;
    let u = user();
    let w = |path: &str, content: &str| json!({"path": path, "content": content});
    ops::call(&e.hub, &u, "memory_write", w("agents/station/INDEX.md", "- antenna.md: the antenna\n")).await.unwrap();
    ops::call(&e.hub, &u, "memory_write", w("user/INDEX.md", "- github.md: use secret group github-ssh\n"))
        .await
        .unwrap();

    let chat = router::spawn(&e.hub, "station", None, Some("check the antenna"), None, None).await.unwrap();
    agents::dispatch_pending(&e.hub).await.unwrap();
    let c = &e.fake.containers()[&agents::container_name(&chat)];
    let mem = e.hub.paths.memory();
    for (dst, src) in [
        ("/memory/self", mem.join("agents/station")),
        ("/memory/project", mem.join("projects/groundstation")),
        ("/memory/user", mem.join("user")),
    ] {
        let found = c.spec.mounts.iter().find_map(|m| match m {
            Mount::Bind { src, dst: d, read_only } if d == dst => Some((src.clone(), *read_only)),
            _ => None,
        });
        assert_eq!(found, Some((src.clone(), true)), "{dst}");
        assert!(src.is_dir());
        assert!(!src.join(".git").exists());
    }
    assert!(!c.spec.mounts.iter().any(|m| matches!(m, Mount::Bind { src, .. } if src == &mem)));

    let prompt = std::fs::read_to_string(e.hub.paths.agent_dir(&chat).join("botler.md")).unwrap();
    assert!(prompt.contains("## Memory\n"), "{prompt}");
    assert!(prompt.contains("`projects/groundstation/`"), "{prompt}");
    assert!(prompt.contains("### /memory/self/INDEX.md\n- antenna.md: the antenna"), "{prompt}");
    assert!(prompt.contains("### /memory/user/INDEX.md\n- github.md"), "{prompt}");

    // Agents without a shared home get no project namespace.
    let dev = router::spawn(&e.hub, "dev", None, Some("x"), None, None).await.unwrap();
    agents::dispatch_pending(&e.hub).await.unwrap();
    let c = &e.fake.containers()[&agents::container_name(&dev)];
    assert!(!c.spec.mounts.iter().any(|m| matches!(m, Mount::Bind { dst, .. } if dst == "/memory/project")));
}
