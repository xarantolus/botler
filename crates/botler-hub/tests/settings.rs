//! The Settings page's API: what it reads, and saves committed on main as the user.

mod common;

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use botler_hub::auth::Principal;
use botler_hub::model_catalog::{Catalog, CatalogModel};
use chrono::Utc;
use serde_json::{Value, json};
use tower::ServiceExt;

async fn api(e: &common::Env, method: &str, body: Option<Value>) -> Value {
    let token = e.hub.tokens.issue(&Principal::User, chrono::Duration::hours(1), Utc::now());
    let req = Request::builder()
        .method(method)
        .uri("/api/v1/settings")
        .header("authorization", format!("Bearer {token}"))
        .header("content-type", "application/json")
        .body(body.map_or(Body::empty(), |b| Body::from(b.to_string())))
        .unwrap();
    let resp = botler_hub::api::router(e.hub.clone()).oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    serde_json::from_slice(&axum::body::to_bytes(resp.into_body(), 1 << 22).await.unwrap()).unwrap()
}

async fn file(e: &common::Env, path: &str) -> String {
    let main = e.hub.repo.main().await.unwrap();
    e.hub.repo.read_file(&main, path).await.unwrap().unwrap()
}

fn model(id: &str) -> CatalogModel {
    CatalogModel {
        id: id.into(),
        name: id.into(),
        context_length: None,
        max_output: None,
        image: false,
        price_in: None,
        price_out: None,
        price_cache_read: None,
        price_cache_write: None,
    }
}

#[tokio::test]
async fn edits_are_committed_as_the_user_and_keep_the_files_layout() {
    let e = common::env().await;
    let ids = ["deepseek/deepseek-v4.1-flash", "qwen/qwen3.8-flash", "~anthropic/claude-sonnet-latest"];
    *e.hub.catalog.write().await =
        Some(Arc::new(Catalog { fetched_at: Utc::now(), models: ids.iter().map(|i| model(i)).collect() }));
    let s = api(&e, "GET", None).await;
    assert_eq!(s["models"]["default"], "deepseek/deepseek-v4.1-flash");
    assert_eq!(s["models"]["models"].as_array().unwrap().len(), 3);
    let chat = s["agents"].as_array().unwrap().iter().find(|a| a["name"] == "chat").unwrap().clone();
    assert_eq!(chat["effective_model"], "deepseek/deepseek-v4.1-flash");
    assert_eq!(s["defaults"]["idle_timeout"], "15m");
    assert!(s["defaults_schema"]["properties"]["idle_timeout"].is_object(), "{}", s["defaults_schema"]);
    assert!(s["warnings"].as_array().unwrap().is_empty(), "{}", s["warnings"]);
    let loaded = s["commit"].as_str().unwrap().to_string();

    // Main moves meanwhile: a field the page did not touch keeps main's value.
    let toml = file(&e, "botler.toml").await.replace("# timezone = \"Europe/Berlin\"", "timezone = \"Europe/Berlin\"");
    e.commit(&[("botler.toml", &toml)]).await;

    // Problems are reported per field, and nothing is saved.
    let mut defaults = s["defaults"].clone();
    defaults["idle_timeout"] = json!("soon");
    let mut agent = chat["edit"].clone();
    agent["model"] = json!("qwen/qwen-flash");
    let bad = json!({ "commit": loaded, "agents": { "chat": agent }, "deployment": { "defaults": defaults } });
    let r = api(&e, "PUT", Some(bad)).await;
    assert_eq!(r["saved"], false);
    let fields: Vec<&str> = r["errors"].as_array().unwrap().iter().map(|e| e["field"].as_str().unwrap()).collect();
    assert_eq!(fields, ["agents.chat.model", "defaults.idle_timeout"], "{r}");
    assert!(r["errors"][0]["message"].as_str().unwrap().contains("closest: qwen/qwen3.8-flash"), "{r}");

    let mut models = s["models"].clone();
    models["models"].as_array_mut().unwrap().remove(1);
    models["default"] = json!("qwen/qwen3.8-flash");
    let mut defaults = s["defaults"].clone();
    defaults["idle_timeout"] = json!("1200s");
    let mut agent = chat["edit"].clone();
    agent["model"] = json!("~anthropic/claude-sonnet-latest");
    agent["tools"] = json!([]);
    let edit = json!({
        "commit": loaded,
        "models": models,
        "agents": { "chat": agent },
        "deployment": { "defaults": defaults },
    });
    let r = api(&e, "PUT", Some(edit)).await;
    assert_eq!(r["saved"], true, "{r}");
    assert_eq!(r["commit"], e.hub.repo.main().await.unwrap());

    let toml = file(&e, "botler.toml").await;
    assert!(toml.contains("timezone = \"Europe/Berlin\""), "main's own change survives: {toml}");
    assert!(toml.contains("default = \"qwen/qwen3.8-flash\"") && toml.contains("idle_timeout = \"20m\""), "{toml}");
    assert!(toml.contains("# The models we typically use") && !toml.contains("Cheapest"), "{toml}");
    let chat_toml = file(&e, "agents/chat/agent.toml").await;
    assert!(
        chat_toml.contains("model = \"~anthropic/claude-sonnet-latest\"") && !chat_toml.contains("tools"),
        "{chat_toml}"
    );
    assert!(chat_toml.contains("# The chat's persistent home"), "{chat_toml}");
    let log = std::process::Command::new("git")
        .arg("--git-dir")
        .arg(e.hub.repo.git_dir())
        .args(["log", "-1", "--format=%an|%s", "main"])
        .output()
        .unwrap();
    let log = String::from_utf8(log.stdout).unwrap();
    assert!(log.starts_with("the user|settings: ") && log.trim_end().ends_with("(by the user)"), "{log}");
    let applied = e.hub.applied().await;
    assert_eq!(
        applied.defs.model_for(&applied.defs.agents["chat"].spec).unwrap().id,
        "~anthropic/claude-sonnet-latest"
    );
    drop(applied);

    // Saving the same again changes nothing.
    let s = api(&e, "GET", None).await;
    let same = json!({ "commit": s["commit"], "models": s["models"], "deployment": { "defaults": s["defaults"], "push": null } });
    let r = api(&e, "PUT", Some(same)).await;
    assert_eq!((&r["saved"], &r["commit"]), (&json!(true), &s["commit"]), "{r}");

    // A mirror set here gets main right away (and after every merge).
    let bare = std::env::temp_dir().join(format!("botler-mirror-{}", uuid::Uuid::now_v7()));
    let ok = std::process::Command::new("git").args(["init", "-q", "--bare"]).arg(&bare).status().unwrap();
    assert!(ok.success());
    let url = bare.to_string_lossy().to_string();
    let edit =
        json!({ "commit": s["commit"], "deployment": { "defaults": s["defaults"], "push": null, "mirror": url } });
    let r = api(&e, "PUT", Some(edit)).await;
    assert_eq!(r["saved"], true, "{r}");
    assert!(file(&e, "botler.toml").await.contains(&format!("[mirror]\nurl = \"{url}\"")));
    assert_eq!(api(&e, "GET", None).await["mirror"], json!(url));
    let pushed = std::process::Command::new("git").args(["--git-dir", &url, "rev-parse", "main"]).output().unwrap();
    assert_eq!(String::from_utf8_lossy(&pushed.stdout).trim(), e.hub.repo.main().await.unwrap());
    std::fs::remove_dir_all(&bare).unwrap();
}

/// A job's `Settings` model is a form on the page; saved values are committed in its
/// settings.toml (defaults left out) and reach its runs as `/botler/settings.json`.
#[tokio::test]
async fn runnable_settings_are_edited_committed_and_delivered() {
    let e = common::env().await;
    let schema = json!({
        "type": "object", "additionalProperties": false,
        "properties": {
            "folders": {"type": "array", "items": {"type": "string"}, "default": ["INBOX"]},
            "every_minutes": {"type": "integer", "minimum": 1, "default": 5}
        }
    });
    let manifest = json!([{"job": "watch", "description": "What to watch.", "schema": schema}]).to_string();
    let mut files = common::job("watch", "lifetime = \"service\"\ncommand = [\"w\"]\n");
    files.push(("settings.manifest.json".into(), manifest));
    let refs: Vec<(&str, &str)> = files.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect();
    e.commit(&refs).await;

    let s = api(&e, "GET", None).await;
    let r = s["runnables"].as_array().unwrap().iter().find(|r| r["job"] == "watch").unwrap().clone();
    assert_eq!((r["description"].as_str(), &r["values"]), (Some("What to watch."), &json!({})));

    let save = |values: Value| json!({"commit": s["commit"], "runnables": {"watch": values}});
    let bad = api(&e, "PUT", Some(save(json!({"every_minutes": 0, "folders": ["INBOX"]})))).await;
    assert_eq!(bad["saved"], false);
    assert_eq!(bad["errors"][0]["field"], "runnables.watch.every_minutes", "{bad}");
    let ok = api(&e, "PUT", Some(save(json!({"every_minutes": 15, "folders": ["INBOX"]})))).await;
    assert_eq!(ok["saved"], true, "{ok}");
    assert_eq!(file(&e, "jobs/watch/settings.toml").await.trim(), "every_minutes = 15", "defaults are not written");

    // An unknown key in the file is a load error for that job.
    let main = e.hub.repo.main().await.unwrap();
    let typo = botler_hub::hub::load_at(&e.hub.repo, &main).await.unwrap();
    assert_eq!(typo.defs.jobs["watch"].settings.as_ref().unwrap().values, json!({"every_minutes": 15}));
    let head = e.push("typo", &[("jobs/watch/settings.toml", "every_minute = 15\n")]).await;
    let err = botler_hub::hub::load_at(&e.hub.repo, &head).await.unwrap_err().to_string();
    assert!(err.contains("settings.toml") || err.contains("definitions"), "{err}");

    botler_hub::services::reconcile(&e.hub).await.unwrap();
    let c = &e.fake.containers()[&botler_hub::services::container_name(&e.hub, "watch")];
    let mount = c.spec.mounts.iter().find_map(|m| match m {
        botler_runtime::Mount::Bind { src, dst, .. } if dst == "/botler/settings.json" => Some(src.clone()),
        _ => None,
    });
    let delivered: Value = serde_json::from_slice(&std::fs::read(mount.expect("settings mounted")).unwrap()).unwrap();
    assert_eq!(delivered, json!({"every_minutes": 15}));
}
