//! `request_visit`: open a page of a service the chat may use, answered with done or cancel.

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use botler_core::defs::{Resource, ResourceKind};
use botler_core::event::kinds;
use botler_hub::auth::Principal;
use botler_hub::ops::{self, Caller, OpError};
use botler_hub::{grants, router};
use botler_store::DialogKind;
use serde_json::json;
use tower::ServiceExt;

fn agent(name: &str) -> Caller {
    Caller {
        principal: Principal::Agent { instance: name.into(), wake: "w".into() },
        capabilities: vec![],
        runnable: None,
    }
}

const HA: &str =
    "lifetime = \"service\"\ncommand = [\"hass\"]\nport = 8123\n[[publish]]\nport = 8123\nhost_port = 18123\n";

#[tokio::test]
async fn visit_requests_need_a_usable_published_port_and_answer_done_or_cancel() {
    let e = common::env().await;
    let files = common::job("homeassistant", HA);
    let refs: Vec<(&str, &str)> = files.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect();
    e.commit(&refs).await;
    let chat = router::new_instance(&e.hub, "chat", None, None).await.unwrap();
    let me = agent(&chat);
    let args = json!({"host_port": 18123, "path": "/auth", "title": "Log in to Home Assistant", "instructions": "Sign in, then press Done."});

    // Unknown port, then a port of a service the chat may not use.
    let unknown =
        ops::call(&e.hub, &me, "request_visit", json!({"host_port": 1, "title": "x", "instructions": "y"})).await;
    assert!(matches!(&unknown, Err(OpError::BadRequest(m)) if m.contains("no service publishes")), "{unknown:?}");
    let denied = ops::call(&e.hub, &me, "request_visit", args.clone()).await;
    assert!(
        matches!(&denied, Err(OpError::Forbidden(m)) if m.contains("request_access(\"service:homeassistant\")")),
        "{denied:?}"
    );
    let bad_path = json!({"host_port": 18123, "path": "auth", "title": "x", "instructions": "y"});
    grants::give(&e.hub, &chat, &Resource::new(ResourceKind::Service, "homeassistant"), None, "user", false)
        .await
        .unwrap();
    assert!(matches!(ops::call(&e.hub, &me, "request_visit", bad_path).await, Err(OpError::BadRequest(_))));

    let out = ops::call(&e.hub, &me, "request_visit", args).await.unwrap();
    let id = out["dialog_id"].as_str().unwrap().to_string();
    let d = e.hub.store.dialog(&id).await.unwrap();
    assert_eq!((d.kind, d.ctas.clone()), (DialogKind::Visit, vec!["done".to_string(), "cancel".to_string()]));
    let spec: ops::VisitSpec = serde_json::from_value(d.spec.clone().unwrap()).unwrap();
    assert_eq!((spec.service.as_str(), spec.host_port, spec.path.as_str()), ("homeassistant", 18123, "/auth"));
    assert!(ops::OPS.iter().any(|o| o.name == "request_visit" && o.ends_turn), "ends the turn");

    // The inbox carries the typed target; only done or cancel answer it, as dialog.answered.
    let app = botler_hub::api::router(e.hub.clone());
    let token = e.hub.tokens.issue(&Principal::User, chrono::Duration::hours(1), chrono::Utc::now());
    let call = |method: &str, path: String, body: serde_json::Value| {
        Request::builder()
            .method(method)
            .uri(path)
            .header("authorization", format!("Bearer {token}"))
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap()
    };
    let resp = app.clone().oneshot(call("GET", "/api/v1/dialogs".into(), json!(null))).await.unwrap();
    let body: serde_json::Value =
        serde_json::from_slice(&axum::body::to_bytes(resp.into_body(), 1 << 20).await.unwrap()).unwrap();
    let open = body["open"].as_array().unwrap().iter().find(|d| d["id"] == id.as_str()).unwrap().clone();
    assert_eq!(open["visit"]["host_port"], 18123);
    assert_eq!(open["visit"]["path"], "/auth");
    let answer = |a: &str| call("POST", format!("/api/v1/dialogs/{id}/answer"), json!({"answer": a}));
    assert!(!app.clone().oneshot(answer("maybe")).await.unwrap().status().is_success());
    assert_eq!(app.clone().oneshot(answer("done")).await.unwrap().status(), StatusCode::OK);
    let q = botler_store::EventQuery { kind: Some(kinds::DIALOG_ANSWERED.into()), limit: 5, ..Default::default() };
    let answered = e.hub.store.events(&q).await.unwrap();
    assert_eq!((answered[0].to.as_deref(), &answered[0].payload["answer"]), (Some(chat.as_str()), &json!("done")));
}
