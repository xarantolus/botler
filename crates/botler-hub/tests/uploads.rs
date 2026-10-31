//! Uploads attached to messages, image input on models, and ntfy push for blocking
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

/// Uploads come back to the user: images inline with their type, anything else (SVG included)
/// as an attachment, never sniffed; only for the user.
#[tokio::test]
async fn uploads_are_served_back_safely() {
    let e = common::env().await;
    let get = |path: String, who: Principal| {
        let token = e.hub.tokens.issue(&who, chrono::Duration::hours(1), Utc::now());
        let req = Request::get(path).header("authorization", format!("Bearer {token}")).body(Body::empty()).unwrap();
        botler_hub::api::router(e.hub.clone()).oneshot(req)
    };
    let header = |r: &axum::response::Response, h: &str| r.headers()[h].to_str().unwrap().to_string();
    let png: &[u8] = b"\x89PNG\r\n\x1a\nfake";
    let (_, up) = api(&e, "PUT", "/api/v1/uploads?name=photo.png", Body::from(png)).await;
    let r = get(format!("/api/v1/uploads/{}", up["file"].as_str().unwrap()), Principal::User).await.unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    assert_eq!(
        (header(&r, "content-type"), header(&r, "content-disposition")),
        ("image/png".into(), "inline; filename=\"photo.png\"".into())
    );
    assert_eq!(
        (header(&r, "x-content-type-options"), header(&r, "content-security-policy")),
        ("nosniff".into(), "sandbox".into())
    );
    assert_eq!(axum::body::to_bytes(r.into_body(), 1 << 20).await.unwrap(), png);

    let (_, svg) = api(&e, "PUT", "/api/v1/uploads?name=x.svg", Body::from("<svg onload=alert(1)/>")).await;
    let r = get(format!("/api/v1/uploads/{}", svg["file"].as_str().unwrap()), Principal::User).await.unwrap();
    assert_eq!(header(&r, "content-type"), "application/octet-stream");
    assert_eq!(header(&r, "content-disposition"), "attachment; filename=\"x.svg\"");

    let agent = Principal::Agent { instance: "botler".into(), wake: "w".into() };
    let r = get(format!("/api/v1/uploads/{}", up["file"].as_str().unwrap()), agent).await.unwrap();
    assert_eq!(r.status(), StatusCode::FORBIDDEN);
    assert_eq!(
        get("/api/v1/uploads/0123/nope.png".into(), Principal::User).await.unwrap().status(),
        StatusCode::NOT_FOUND
    );
}

/// A GET as `who`, with the response's status, headers and body.
async fn fetch(e: &common::Env, path: &str, who: Principal) -> (StatusCode, axum::http::HeaderMap, Vec<u8>) {
    let token = e.hub.tokens.issue(&who, chrono::Duration::hours(1), Utc::now());
    let req = Request::get(path).header("authorization", format!("Bearer {token}")).body(Body::empty()).unwrap();
    let r = botler_hub::api::router(e.hub.clone()).oneshot(req).await.unwrap();
    let (status, headers) = (r.status(), r.headers().clone());
    (status, headers, axum::body::to_bytes(r.into_body(), 1 << 24).await.unwrap().to_vec())
}

/// A screenshot in a tool result is a reference in the session view (never its bytes) and
/// served from the session file by its entry and place.
#[tokio::test]
async fn tool_result_images_are_served_from_the_session() {
    use base64::Engine;
    let e = common::env().await;
    let png: &[u8] = b"\x89PNG\r\n\x1a\nscreenshot";
    let data = base64::engine::general_purpose::STANDARD.encode(png);
    let chat = botler_hub::router::new_instance(&e.hub, "chat", None, None).await.unwrap();
    let file = e.hub.store.instance(&chat).await.unwrap().session_file;
    let lines = [
        json!({"type": "session", "version": 3, "cwd": "/home/agent"}),
        json!({"type": "message", "id": "a1", "message": {"role": "assistant", "content": [
            {"type": "toolCall", "id": "c1", "name": "screenshot", "arguments": {}}]}}),
        json!({"type": "message", "id": "r1", "parentId": "a1", "message": {"role": "toolResult", "toolCallId": "c1",
            "toolName": "screenshot", "content": [
                {"type": "text", "text": "the page"},
                {"type": "image", "data": data, "mimeType": "image/png"},
                {"type": "image", "data": "PHN2Zy8+", "mimeType": "image/svg+xml"}]}}),
    ];
    let body: String = lines.iter().map(|l| format!("{l}\n")).collect();
    std::fs::create_dir_all(e.hub.paths.sessions(&chat)).unwrap();
    std::fs::write(e.hub.paths.sessions(&chat).join(&file), body).unwrap();

    let (status, _, view) = fetch(&e, &format!("/api/v1/sessions/{chat}"), Principal::User).await;
    assert_eq!(status, StatusCode::OK);
    let view = String::from_utf8(view).unwrap();
    assert!(!view.contains(&data), "no image bytes in the view");
    let view: Value = serde_json::from_str(&view).unwrap();
    assert_eq!(view["items"][1]["images"], json!([{"index": 1, "mime": "image/png"}]), "SVG is not shown");

    let (status, headers, bytes) = fetch(&e, &format!("/api/v1/sessions/{chat}/images/r1/1"), Principal::User).await;
    assert_eq!((status, bytes.as_slice()), (StatusCode::OK, png));
    assert_eq!(headers["content-type"], "image/png");
    assert_eq!(headers["x-content-type-options"], "nosniff");
    assert!(headers["cache-control"].to_str().unwrap().contains("immutable"));
    let at_file = format!("/api/v1/sessions/{chat}/images/r1/1?file={file}");
    assert_eq!(fetch(&e, &at_file, Principal::User).await.0, StatusCode::OK);
    for missing in ["r1/0", "r1/2", "r1/9", "a1/0", "nope/1"] {
        let path = format!("/api/v1/sessions/{chat}/images/{missing}");
        assert_eq!(fetch(&e, &path, Principal::User).await.0, StatusCode::NOT_FOUND, "{missing}");
    }
    let other = format!("/api/v1/sessions/{chat}/images/r1/1?file=../../hub.db");
    assert_eq!(fetch(&e, &other, Principal::User).await.0, StatusCode::NOT_FOUND);
    let agent = Principal::Agent { instance: chat.clone(), wake: "w".into() };
    assert_eq!(fetch(&e, &format!("/api/v1/sessions/{chat}/images/r1/1"), agent).await.0, StatusCode::FORBIDDEN);
}

/// Agents send files of any type from their home: shown in their own chat (`attach_files`) or
/// sent with a message to another chat, which shows them with the message.
#[tokio::test]
async fn agents_send_files_from_their_home() {
    let e = common::env().await;
    let chat = botler_hub::router::new_instance(&e.hub, "chat", None, None).await.unwrap();
    let me = Caller {
        principal: Principal::Agent { instance: chat.clone(), wake: "w".into() },
        capabilities: vec![],
        runnable: None,
    };
    let home = e.hub.paths.home(&chat);
    std::fs::create_dir_all(home.join("out")).unwrap();
    std::fs::write(home.join("out/demo.mp4"), b"mp4 bytes").unwrap();
    std::fs::write(home.join("out/report.pdf"), b"%PDF-1.7").unwrap();
    std::fs::write(e.hub.paths.root.join("hub-secret.txt"), b"secret").unwrap();
    std::os::unix::fs::symlink(e.hub.paths.root.join("hub-secret.txt"), home.join("out/escape.txt")).unwrap();

    let out =
        ops::call(&e.hub, &me, "attach_files", json!({"files": ["/home/agent/out/demo.mp4", "~/out/report.pdf"]}))
            .await
            .unwrap();
    let files = out["files"].as_array().unwrap();
    assert_eq!((files[0]["mime"].as_str(), files[0]["size"].as_u64()), (Some("video/mp4"), Some(9)));
    assert_eq!(files[1]["mime"], "application/pdf");
    let stored = e.hub.paths.uploads().join(files[0]["file"].as_str().unwrap());
    assert_eq!(std::fs::read(stored).unwrap(), b"mp4 bytes");

    for (path, why) in [
        ("/tmp/shot.png", "not in your home"),
        ("/home/agent/../../etc/passwd", "not in your home"),
        ("out/escape.txt", "not in your home"),
        ("out/missing.png", "no file"),
        ("out", "not a file"),
    ] {
        let err = ops::call(&e.hub, &me, "attach_files", json!({"files": [path]})).await.unwrap_err();
        assert!(err.to_string().contains(why), "{path}: {err}");
    }
    let user = Caller { principal: Principal::User, capabilities: vec![], runnable: None };
    assert!(ops::call(&e.hub, &user, "attach_files", json!({"files": ["x"]})).await.is_err());

    // To another chat: the files land in its inbox and show with the message there.
    let other = botler_hub::router::new_instance(&e.hub, "chat", None, None).await.unwrap();
    let sent =
        ops::call(&e.hub, &me, "send_message", json!({"to": other, "text": "the report", "files": ["out/report.pdf"]}))
            .await
            .unwrap();
    let file = sent["files"][0]["file"].as_str().unwrap().to_string();
    assert!(file.ends_with("/report.pdf"), "{sent}");
    assert!(e.hub.paths.inbox(&other).join(&file).is_file());
    let ev = e.hub.store.event(sent["event_id"].as_str().unwrap()).await.unwrap();
    assert_eq!(ev.payload["files"], json!([format!("/inbox/files/{file}")]));
    let session = e.hub.store.instance(&other).await.unwrap().session_file;
    let entry = json!({"type": "message", "id": "u1", "message": {"role": "user", "content": format!(
        "[from {chat} | provenance: trusted | message | delivery d | event {}]\nthe report", ev.id)}});
    std::fs::create_dir_all(e.hub.paths.sessions(&other)).unwrap();
    std::fs::write(e.hub.paths.sessions(&other).join(session), format!("{entry}\n")).unwrap();
    let items = botler_hub::sessions::read(&e.hub, &other, None).await.unwrap();
    let botler_hub::sessions::Item::User { files, message_text, .. } = &items[0] else { panic!("{items:?}") };
    assert_eq!((files.len(), files[0].file.as_str(), files[0].mime), (1, file.as_str(), "application/pdf"));
    assert_eq!(*message_text, None, "only the user's own text replaces the delivered text");
}

#[tokio::test]
async fn image_input_reaches_models_json() {
    let e = common::env().await;
    e.hub.store.set_secret(&e.hub.secret_key, "OPENROUTER_API_KEY", "sk").await.unwrap();
    // Image input comes from OpenRouter's catalog.
    let sonnet = botler_hub::model_catalog::CatalogModel {
        id: "~anthropic/claude-sonnet-latest".into(),
        name: "Sonnet".into(),
        context_length: Some(1_000_000),
        max_output: Some(128_000),
        image: true,
        price_in: Some(2.0),
        price_out: Some(10.0),
        price_cache_read: None,
        price_cache_write: None,
    };
    *e.hub.catalog.write().await =
        Some(std::sync::Arc::new(botler_hub::model_catalog::Catalog { fetched_at: Utc::now(), models: vec![sonnet] }));
    let toml = "version = 1\nkind = \"persistent\"\nimage = \"agent\"\nmodel = \"~anthropic/claude-sonnet-latest\"\n";
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
    let model = &models["providers"]["openrouter"]["modelOverrides"]["~anthropic/claude-sonnet-latest"];
    assert_eq!(model["input"], json!(["text", "image"]));
    assert_eq!((model["maxTokens"].as_u64(), model["contextWindow"].as_u64()), (Some(16384), Some(1_000_000)));
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
