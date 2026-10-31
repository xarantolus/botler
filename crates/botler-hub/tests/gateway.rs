//! The model gateway: a job calls the models its job.toml lists through the hub, which holds the
//! key, meters each call under `job:<name>` and stops at the budget.

mod common;

use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use axum::routing::post;
use botler_hub::auth::Principal;
use botler_store::{NewRun, RunKind};
use serde_json::{Value, json};
use tower::ServiceExt;

/// What the stand-in provider received: the Authorization header and the body.
type Seen = Arc<Mutex<Vec<(Option<String>, Value)>>>;

#[tokio::test]
async fn jobs_call_listed_models_metered_and_within_budget() {
    // A stand-in provider that records what it got and charges $0.40 a call.
    let seen: Seen = Arc::default();
    let s2 = seen.clone();
    let provider = axum::Router::new().route(
        "/v1/chat/completions",
        post(move |headers: axum::http::HeaderMap, axum::Json(body): axum::Json<Value>| {
            let s = s2.clone();
            async move {
                let auth = headers.get(header::AUTHORIZATION).map(|v| v.to_str().unwrap().to_owned());
                s.lock().unwrap().push((auth, body));
                axum::Json(json!({
                    "choices": [{"message": {"role": "assistant", "content": "hi"}}],
                    "usage": {"total_tokens": 12, "cost": 0.4}
                }))
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move { axum::serve(listener, provider).await.unwrap() });

    let e = common::env().await;
    e.hub.store.set_secret(&e.hub.secret_key, "OPENROUTER_API_KEY", "sk-hub").await.unwrap();
    let main = e.hub.repo.main().await.unwrap();
    let toml = e.hub.repo.read_file(&main, "botler.toml").await.unwrap().unwrap();
    let toml = toml.replacen("[models]\n", &format!("[models]\nbase_url = \"http://127.0.0.1:{port}/v1\"\n"), 1);
    let mut files = common::job("digest", "command = [\"d\"]\nmodels = [\"default\"]\n");
    files.push(("botler.toml".into(), toml));
    let refs: Vec<(&str, &str)> = files.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect();
    e.commit(&refs).await;

    let run = e
        .hub
        .store
        .create_run(&NewRun {
            runnable: "digest".into(),
            kind: RunKind::Oneshot,
            trigger_id: None,
            event_id: None,
            commit_id: None,
        })
        .await
        .unwrap();
    let token = e.hub.tokens.issue(&Principal::Run { run_id: run.id }, chrono::Duration::hours(1), chrono::Utc::now());
    let chat = |body: Value| {
        let req = Request::post("/v1/chat/completions")
            .header(header::AUTHORIZATION, format!("Bearer {token}"))
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body.to_string()))
            .unwrap();
        let app = botler_hub::api::router(e.hub.clone());
        async move {
            let resp = app.oneshot(req).await.unwrap();
            let status = resp.status();
            let v: Value =
                serde_json::from_slice(&axum::body::to_bytes(resp.into_body(), 1 << 20).await.unwrap()).unwrap();
            (status, v)
        }
    };
    let msg = json!([{"role": "user", "content": "hello"}]);

    let (status, v) = chat(json!({"model": "openai/gpt-5", "messages": msg})).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{v}");
    assert!(v["error"]["message"].as_str().unwrap().contains("models = [default]"), "{v}");
    assert_eq!(chat(json!({"messages": msg, "stream": true})).await.0, StatusCode::BAD_REQUEST);

    let (status, v) = chat(json!({"messages": msg})).await;
    assert_eq!((status, v["choices"][0]["message"]["content"].as_str()), (StatusCode::OK, Some("hi")));
    {
        let seen = seen.lock().unwrap();
        let (auth, body) = &seen[0];
        assert_eq!(auth.as_deref(), Some("Bearer sk-hub"), "the hub's key, never the job's");
        assert_eq!(body["model"], "deepseek/deepseek-v4.1-flash");
        assert_eq!(body["usage"], json!({"include": true}));
    }
    let used =
        e.hub.store.usage_since(Some("job:digest"), chrono::Utc::now() - chrono::Duration::hours(1)).await.unwrap();
    assert_eq!((used.tokens, used.cost_usd), (12, 0.4));

    // The template's default budget is $1 a day: the third call is refused.
    assert_eq!(chat(json!({"messages": msg})).await.0, StatusCode::OK);
    assert_eq!(chat(json!({"messages": msg})).await.0, StatusCode::OK);
    let (status, v) = chat(json!({"messages": msg})).await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS, "{v}");
    assert_eq!(seen.lock().unwrap().len(), 3);

    // Agents use their own model, not the gateway.
    let user = e.hub.tokens.issue(&Principal::User, chrono::Duration::hours(1), chrono::Utc::now());
    let req = Request::post("/v1/chat/completions")
        .header(header::AUTHORIZATION, format!("Bearer {user}"))
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(json!({"messages": msg}).to_string()))
        .unwrap();
    assert_eq!(botler_hub::api::router(e.hub.clone()).oneshot(req).await.unwrap().status(), StatusCode::FORBIDDEN);
}
