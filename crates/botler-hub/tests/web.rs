//! The web app's surface: the route table matches the router, the SPA is served, and the live
//! WebSocket authenticates and streams events.

mod common;

use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use botler_core::{Event, Provenance, Source};
use botler_hub::auth::Principal;
use chrono::Utc;
use common::env;
use futures::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio_tungstenite::tungstenite::Message;
use tower::ServiceExt;

/// Every route in the generated client's table is served (and requires the token), except
/// signing in, which is how a device gets its cookie.
#[tokio::test]
async fn route_table_matches_router() {
    let e = env().await;
    let app = botler_hub::api::router(e.hub.clone());
    let table = botler_hub::wire::route_table(&ts_rs::Config::new());
    assert!(table.len() > 20);
    for r in table.into_iter().filter(|r| r.name != "login") {
        let path = r.path.replace("{id}", "x").replace("{instance}", "x").replace("{name}", "x");
        let req = Request::builder().method(r.method).uri(&path).body(Body::empty()).unwrap();
        let resp = app.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED, "{} {} ({})", r.method, path, r.name);
    }
}

#[tokio::test]
async fn serves_the_app_and_the_legacy_page() {
    let e = env().await;
    let app = botler_hub::api::router(e.hub.clone());
    for (path, status) in [("/", StatusCode::OK), ("/chat/main", StatusCode::OK), ("/legacy", StatusCode::OK)] {
        let resp = app.clone().oneshot(Request::get(path).body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(resp.status(), status, "{path}");
    }
    let resp = app.clone().oneshot(Request::get("/api/v1/nope").body(Body::empty()).unwrap()).await.unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

/// The API and the app are compressed for clients that accept it.
#[tokio::test]
async fn responses_are_compressed() {
    let e = env().await;
    let app = botler_hub::api::router(e.hub.clone());
    let token = e.hub.tokens.issue(&Principal::User, chrono::Duration::hours(1), Utc::now());
    for path in ["/api/v1/status", "/"] {
        let req = Request::get(path)
            .header("authorization", format!("Bearer {token}"))
            .header("accept-encoding", "gzip")
            .body(Body::empty())
            .unwrap();
        let resp = app.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK, "{path}");
        assert_eq!(resp.headers().get("content-encoding").map(|v| v.to_str().unwrap()), Some("gzip"), "{path}");
    }
}

async fn serve(e: &common::Env) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = botler_hub::api::router(e.hub.clone());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    format!("ws://{addr}/api/v1/live")
}

async fn next_json<S>(ws: &mut S) -> Value
where
    S: futures::Stream<Item = Result<Message, tokio_tungstenite::tungstenite::Error>> + Unpin,
{
    loop {
        let msg = tokio::time::timeout(Duration::from_secs(5), ws.next()).await.expect("message in time");
        match msg.expect("stream open").expect("no socket error") {
            Message::Text(t) => return serde_json::from_str(&t).unwrap(),
            Message::Ping(_) | Message::Pong(_) => continue,
            other => panic!("unexpected {other:?}"),
        }
    }
}

#[tokio::test]
async fn live_rejects_missing_and_bad_tokens() {
    let e = env().await;
    let url = serve(&e).await;

    // No auth message: the first message is something else.
    let (mut ws, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
    ws.send(Message::text(json!({ "type": "watch", "instance": "main" }).to_string())).await.unwrap();
    let m = next_json(&mut ws).await;
    assert_eq!(m["kind"], "error", "{m}");
    match tokio::time::timeout(Duration::from_secs(5), ws.next()).await.unwrap() {
        Some(Ok(Message::Close(Some(f)))) => assert_eq!(u16::from(f.code), botler_hub::live::CLOSE_UNAUTHORIZED),
        other => panic!("expected close, got {other:?}"),
    }

    // A forged token.
    let (mut ws, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
    ws.send(Message::text(json!({ "type": "auth", "token": "user.x.1.sig" }).to_string())).await.unwrap();
    assert_eq!(next_json(&mut ws).await["kind"], "error");

    // An agent token is not enough.
    let agent = e.hub.tokens.issue(
        &Principal::Agent { instance: "main".into(), wake: "w".into() },
        chrono::Duration::hours(1),
        Utc::now(),
    );
    let (mut ws, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
    ws.send(Message::text(json!({ "type": "auth", "token": agent }).to_string())).await.unwrap();
    assert_eq!(next_json(&mut ws).await["kind"], "error");
}

#[tokio::test]
async fn live_streams_emitted_events() {
    let e = env().await;
    let url = serve(&e).await;
    let token = e.hub.tokens.issue(&Principal::User, chrono::Duration::hours(1), Utc::now());
    let (mut ws, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
    ws.send(Message::text(json!({ "type": "auth", "token": token }).to_string())).await.unwrap();
    assert_eq!(next_json(&mut ws).await["kind"], "ready");

    let ev = Event::new("test.ping", Source::Hub, Provenance::Trusted, Utc::now()).payload(json!({ "n": 1 }));
    let ev = e.hub.emit(ev).await.unwrap();
    let m = next_json(&mut ws).await;
    assert_eq!(m["kind"], "event");
    assert_eq!(m["event"]["id"], ev.id.to_string());
    assert_eq!(m["event"]["type"], "test.ping");

    // Pi events reach only a client watching that chat.
    ws.send(Message::text(json!({ "type": "watch", "instance": "main" }).to_string())).await.unwrap();
    // The watch message is processed before the next broadcast only once the server has read it.
    tokio::time::sleep(Duration::from_millis(100)).await;
    e.hub
        .live
        .send(botler_hub::hub::Live::Pi { instance: "other".into(), event: json!({ "type": "agent_start" }) })
        .unwrap();
    e.hub
        .live
        .send(botler_hub::hub::Live::Pi { instance: "main".into(), event: json!({ "type": "agent_end" }) })
        .unwrap();
    let m = next_json(&mut ws).await;
    assert_eq!((m["kind"].as_str(), m["instance"].as_str()), (Some("pi"), Some("main")));
    assert_eq!(m["event"]["type"], "agent_end");
}

/// While the agent works, a queued message waits in the hub (listed in the session, cancellable)
/// until its run ends; a steered one goes to pi at once (listed, not cancellable, until the run
/// settles: pi adds it after the running tool calls).
#[tokio::test]
async fn queued_messages_wait_for_the_turn_and_can_be_taken_back() {
    let e = env().await;
    e.hub.store.set_secret(&e.hub.secret_key, "OPENROUTER_API_KEY", "sk-test").await.unwrap();
    let app = botler_hub::api::router(e.hub.clone());
    let token = e.hub.tokens.issue(&Principal::User, chrono::Duration::hours(1), Utc::now());
    let req = |method: &str, uri: &str, body: Option<Value>| {
        let app = app.clone();
        let req = Request::builder()
            .method(method)
            .uri(uri)
            .header("authorization", format!("Bearer {token}"))
            .header("content-type", "application/json")
            .body(body.map_or_else(Body::empty, |b| Body::from(b.to_string())))
            .unwrap();
        async move {
            let resp = app.oneshot(req).await.unwrap();
            let status = resp.status();
            let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
            (status, serde_json::from_slice::<Value>(&bytes).unwrap_or(Value::Null))
        }
    };
    let pump = || async {
        for _ in 0..3 {
            e.route().await;
            botler_hub::agents::dispatch_pending(&e.hub).await.unwrap();
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    };
    let chat = botler_hub::router::new_instance(&e.hub, "chat", None, None).await.unwrap();
    let container = botler_hub::agents::container_name(&e.hub, &chat);
    let session = format!("/api/v1/sessions/{chat}");
    e.fake.set_hold_turns(true);

    let say = |text: &str, mode: &str| json!({ "to": chat, "text": text, "mode": mode });
    assert_eq!(req("POST", "/api/v1/messages", Some(say("first", "queue"))).await.0, StatusCode::OK);
    pump().await;
    assert_eq!(e.fake.prompts(&container).len(), 1, "an idle agent gets a queued message right away");

    req("POST", "/api/v1/messages", Some(say("later", "queue"))).await;
    req("POST", "/api/v1/messages", Some(say("and this", "queue"))).await;
    req("POST", "/api/v1/messages", Some(say("stop!", "steer"))).await;
    pump().await;
    assert_eq!(e.fake.prompt_behaviors(&container), [None, Some("steer".to_string())]);
    let (_, view) = req("GET", &session, None).await;
    let queued = view["queued"].as_array().unwrap();
    // The steer is pi's (it adds it once the running tool calls are done): listed until the
    // session has it, first, and no longer cancellable.
    let listed = |q: &Value| (q["text"].as_str().unwrap().to_string(), q["mode"].clone(), q["handed_over"].clone());
    assert_eq!(
        queued.iter().map(listed).collect::<Vec<_>>(),
        [
            ("stop!".to_string(), json!("steer"), json!(true)),
            ("later".to_string(), json!("queue"), json!(false)),
            ("and this".to_string(), json!("queue"), json!(false)),
        ]
    );
    let steer = queued[0]["delivery"].as_str().unwrap().to_string();
    assert_eq!(req("DELETE", &format!("/api/v1/messages/{steer}"), None).await.0, StatusCode::CONFLICT);

    // Take one back; the other is delivered when the turn ends.
    let id = queued[2]["delivery"].as_str().unwrap().to_string();
    assert_eq!(req("DELETE", &format!("/api/v1/messages/{id}"), None).await.0, StatusCode::OK);
    assert!(e.fake.pi_event(&container, json!({ "type": "agent_settled" })));
    pump().await;
    let prompts = e.fake.prompts(&container);
    assert_eq!(prompts.len(), 3);
    assert!(prompts[2].contains("later") && !prompts.iter().any(|p| p.contains("and this")));
    assert_eq!(req("GET", &session, None).await.1["queued"], json!([]));

    // Delivered (or already taken back) messages cannot be taken back.
    let delivered = e.hub.store.last_delivered(&chat).await.unwrap().unwrap().id;
    assert_eq!(req("DELETE", &format!("/api/v1/messages/{delivered}"), None).await.0, StatusCode::CONFLICT);
    assert_eq!(req("DELETE", &format!("/api/v1/messages/{id}"), None).await.0, StatusCode::CONFLICT);
}
