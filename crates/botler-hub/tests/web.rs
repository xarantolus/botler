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

/// Every route in the generated client's table is served (and requires the token).
#[tokio::test]
async fn route_table_matches_router() {
    let e = env().await;
    let app = botler_hub::api::router(e.hub.clone());
    let table = botler_hub::wire::route_table(&ts_rs::Config::new());
    assert!(table.len() > 20);
    for r in table {
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
