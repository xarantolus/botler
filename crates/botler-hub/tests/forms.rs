//! Forms in questions: `ask_user(form)` checks the schema, the answer's values are checked
//! against it, and file fields reach the agent's inbox.

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use botler_core::event::kinds;
use botler_hub::auth::Principal;
use botler_hub::ops::{self, Caller, OpError};
use botler_hub::router;
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
async fn a_form_question_takes_typed_values_and_files() {
    let e = common::env().await;
    let chat = router::new_instance(&e.hub, "chat", None, None).await.unwrap();
    let me = Caller {
        principal: Principal::Agent { instance: chat.clone(), wake: "w".into() },
        capabilities: vec![],
        runnable: None,
    };
    let form = json!({
        "required": ["folder", "every"],
        "properties": {
            "folder": {"type": "string", "enum": ["INBOX", "Archive"], "default": "INBOX"},
            "every": {"type": "integer", "minimum": 5, "title": "Minutes"},
            "sample": {"type": "string", "format": "file"}
        }
    });

    let bad =
        ops::call(&e.hub, &me, "ask_user", json!({"question": "Q", "form": {"properties": {"x": {"type": "object"}}}}))
            .await;
    assert!(matches!(&bad, Err(OpError::BadRequest(m)) if m.contains("field `x`")), "{bad:?}");
    let both = ops::call(&e.hub, &me, "ask_user", json!({"question": "Q", "options": ["a"], "form": form})).await;
    assert!(matches!(both, Err(OpError::BadRequest(_))));
    let asked =
        ops::call(&e.hub, &me, "ask_user", json!({"question": "Watch which folder?", "form": form})).await.unwrap();
    let id = asked["dialog_id"].as_str().unwrap();

    let (_, up) = api(&e, "PUT", "/api/v1/uploads?name=mail.eml", Body::from("From: a")).await;
    let upload = up["file"].as_str().unwrap();
    let answer = |values: Value| json!({"answer": "", "values": values}).to_string();
    let path = format!("/api/v1/dialogs/{id}/answer");
    let (status, err) = api(&e, "POST", &path, Body::from(answer(json!({"folder": "INBOX", "every": 1})))).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{err}");
    let (status, _) = api(&e, "POST", &path, Body::from(json!({"answer": "INBOX"}).to_string())).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "a form needs values");
    let (status, out) =
        api(&e, "POST", &path, Body::from(answer(json!({"folder": "Archive", "every": 15, "sample": upload})))).await;
    assert_eq!(status, StatusCode::OK, "{out}");

    let q = EventQuery { kind: Some(kinds::DIALOG_ANSWERED.into()), limit: 5, ..Default::default() };
    let ev = &e.hub.store.events(&q).await.unwrap()[0];
    assert_eq!(ev.payload["answer"], "folder: Archive · Minutes: 15 · sample: mail.eml", "in the form's order");
    assert_eq!(ev.payload["values"]["every"], 15);
    let inbox = format!("/inbox/files/{upload}");
    assert_eq!(ev.payload["values"]["sample"], inbox.as_str());
    assert!(e.hub.paths.inbox(&chat).join(upload).exists(), "the file is in the chat's inbox");
    let (status, _) = api(&e, "POST", &path, Body::from(answer(json!({"folder": "INBOX", "every": 5})))).await;
    assert_eq!(status, StatusCode::CONFLICT, "answered once");
}
