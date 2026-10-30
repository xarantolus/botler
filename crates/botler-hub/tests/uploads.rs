//! Uploads attached to messages, image input on model routes, and ntfy push for blocking
//! requests.

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use botler_core::event::kinds;
use botler_hub::agents;
use botler_hub::auth::Principal;
use botler_hub::ops::{self, Caller};
use botler_runtime::Mount;
use botler_store::EventQuery;
use chrono::Utc;
use serde_json::{Value, json};
use tower::ServiceExt;

async fn api(e: &common::Env, method: &str, path: &str, body: Body) -> (StatusCode, Value) {
    let token = e.hub.tokens.issue(&Principal::User, chrono::Duration::hours(1), Utc::now());
    let req = Request::builder()
        .method(method)
        .uri(path)
        .header("authorization", format!("Bearer {token}"))
        .header("content-type", "application/json")
        .body(body)
        .unwrap();
    let resp = botler_hub::api::router(e.hub.clone()).oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20).await.unwrap();
    (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

#[tokio::test]
async fn uploads_reach_only_the_chat_they_are_sent_to() {
    let e = common::env().await;
    e.hub.store.set_secret(&e.hub.secret_key, "OPENROUTER_API_KEY", "sk").await.unwrap();
    let (status, _) = api(&e, "PUT", "/api/v1/uploads?name=../x", Body::from("x")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let png: &[u8] = b"\x89PNG\r\n\x1a\nfake";
    let (status, up) = api(&e, "PUT", "/api/v1/uploads?name=photo.png", Body::from(png)).await;
    assert_eq!(status, StatusCode::OK, "{up}");
    let file = up["file"].as_str().unwrap().to_string();
    assert!(file.ends_with("/photo.png") && up["size"] == png.len());

    let missing = json!({"text": "look", "files": ["0123/nope.png"]});
    assert_eq!(api(&e, "POST", "/api/v1/messages", Body::from(missing.to_string())).await.0, StatusCode::NOT_FOUND);
    let msg = json!({"to": "botler", "text": "What is in this picture?", "files": [file]});
    assert_eq!(api(&e, "POST", "/api/v1/messages", Body::from(msg.to_string())).await.0, StatusCode::OK);

    let q = EventQuery { kind: Some(kinds::MESSAGE.into()), limit: 1, ..Default::default() };
    let ev = e.hub.store.events(&q).await.unwrap().remove(0);
    let path = format!("/inbox/files/{file}");
    assert_eq!(ev.payload["files"], json!([path]));
    assert_eq!(std::fs::read(e.hub.paths.inbox("botler").join(&file)).unwrap(), png);
    assert!(!e.hub.paths.inbox("chat").join(&file).exists(), "other chats do not see it");

    // The agent finds it at that path (its inbox mounted read-only) and is told about it.
    e.route().await;
    agents::dispatch_pending(&e.hub).await.unwrap();
    let c = &e.fake.containers()[&agents::container_name(&e.hub, "botler")];
    assert!(c.spec.mounts.iter().any(|m| matches!(m,
        Mount::Bind { src, dst, read_only: true } if dst == "/inbox/files" && *src == e.hub.paths.inbox("botler"))));
    let prompt = e.fake.prompts(&agents::container_name(&e.hub, "botler")).join("\n");
    assert!(prompt.contains("Attached files") && prompt.contains(&path), "{prompt}");
}

#[tokio::test]
async fn image_input_reaches_models_json() {
    let e = common::env().await;
    e.hub.store.set_secret(&e.hub.secret_key, "OPENROUTER_API_KEY", "sk").await.unwrap();
    let route = &e.hub.applied().await.defs.instance.models.routes["smart"];
    assert_eq!(route.input, Some(vec![botler_core::defs::ModelInput::Text, botler_core::defs::ModelInput::Image]));
    let toml = "version = 1\nkind = \"persistent\"\nimage = \"agent\"\nmodel = \"smart\"\n";
    e.commit(&[("agents/looker/agent.toml", toml), ("agents/looker/AGENTS.md", "Look at things.")]).await;
    let ev = botler_core::Event::new(
        kinds::MESSAGE,
        botler_core::Source::User,
        botler_core::Provenance::Trusted,
        Utc::now(),
    )
    .to("looker")
    .payload(json!({"text": "hi"}));
    e.hub.emit(ev).await.unwrap();
    e.route().await;
    agents::dispatch_pending(&e.hub).await.unwrap();
    let models: Value =
        serde_json::from_str(&std::fs::read_to_string(e.hub.paths.agent_dir("looker").join("models.json")).unwrap())
            .unwrap();
    assert_eq!(
        models["providers"]["openrouter"]["modelOverrides"]["anthropic/claude-sonnet-latest"]["input"],
        json!(["text", "image"])
    );
}

/// A blocking request pushes fixed text to `<ntfy_url>/<topic>`; the question stays home.
#[tokio::test]
async fn blocking_requests_are_pushed_with_hub_written_text() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let e = common::env().await;
    let ntfy = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = ntfy.local_addr().unwrap().port();
    let toml = e.hub.repo.read_file(&e.hub.repo.main().await.unwrap(), "botler.toml").await.unwrap().unwrap();
    let toml = format!("{toml}\n[push]\nntfy_url = \"http://127.0.0.1:{port}\"\ntopic_secret = \"NTFY_TOPIC\"\n");
    let secrets = e.hub.repo.read_file(&e.hub.repo.main().await.unwrap(), "secrets.toml").await.unwrap().unwrap();
    let secrets =
        format!("{secrets}\n[[group]]\nname = \"push\"\nclass = \"hub\"\n[[group.secret]]\nname = \"NTFY_TOPIC\"\n");
    e.commit(&[("botler.toml", toml.as_str()), ("secrets.toml", secrets.as_str())]).await;
    e.hub.store.set_secret(&e.hub.secret_key, "NTFY_TOPIC", "botler-test-topic").await.unwrap();

    let chat = botler_hub::router::new_instance(&e.hub, "chat", None, None).await.unwrap();
    let me = Caller {
        principal: Principal::Agent { instance: chat.clone(), wake: "w".into() },
        capabilities: vec![],
        runnable: None,
    };
    ops::call(&e.hub, &me, "ask_user", json!({"question": "Secret plans: may I?", "priority": "blocking"}))
        .await
        .unwrap();
    let (mut conn, _) = tokio::time::timeout(std::time::Duration::from_secs(10), ntfy.accept()).await.unwrap().unwrap();
    let mut buf = vec![0u8; 4096];
    let mut req = String::new();
    while !req.contains("\r\n\r\n") || !req.ends_with('.') {
        let n = tokio::time::timeout(std::time::Duration::from_secs(5), conn.read(&mut buf)).await.unwrap().unwrap();
        if n == 0 {
            break;
        }
        req.push_str(&String::from_utf8_lossy(&buf[..n]));
    }
    conn.write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\n\r\n{}").await.unwrap();
    assert!(req.starts_with("POST /botler-test-topic HTTP/1.1"), "{req}");
    assert!(req.contains("Title: botler") && req.ends_with("A question is waiting for your answer."), "{req}");
    assert!(!req.contains("Secret plans"), "agent-written text never leaves: {req}");
}
