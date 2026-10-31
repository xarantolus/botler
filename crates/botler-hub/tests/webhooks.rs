//! Webhooks (`POST /hooks/<name>`, verified by the trigger's secret) and `replay_event`.

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use botler_core::event::kinds;
use botler_core::{Event, Provenance, Source};
use botler_hub::auth::Principal;
use botler_hub::ops::{self, Caller};
use botler_hub::router;
use botler_runtime::Mount;
use botler_store::{EventQuery, RunKind};
use chrono::Utc;
use hmac::{Hmac, Mac};
use serde_json::json;
use sha2::Sha256;
use tower::ServiceExt;

const TRIGGER: &str = "version = 1\n[[trigger]]\nname = \"push\"\nwebhook = \"github\"\nwebhook_secret = \"HOOK_SECRET\"\ntarget = { resume = \"botler\" }\nprompt = \"Someone pushed.\"\n";

async fn setup() -> common::Env {
    let e = common::env().await;
    let main = e.hub.repo.main().await.unwrap();
    let secrets = e.hub.repo.read_file(&main, "secrets.toml").await.unwrap().unwrap();
    let secrets =
        format!("{secrets}\n[[group]]\nname = \"hooks\"\nclass = \"hub\"\n[[group.secret]]\nname = \"HOOK_SECRET\"\n");
    e.commit(&[("secrets.toml", secrets.as_str()), ("triggers/github.toml", TRIGGER)]).await;
    e.hub.store.set_secret(&e.hub.secret_key, "HOOK_SECRET", "s3cret").await.unwrap();
    e
}

async fn post(e: &common::Env, path: &str, headers: &[(&str, &str)], body: Vec<u8>) -> StatusCode {
    let mut req = Request::post(path).header("content-type", "application/json");
    for (k, v) in headers {
        req = req.header(*k, *v);
    }
    let app = botler_hub::api::router(e.hub.clone());
    app.oneshot(req.body(Body::from(body)).unwrap()).await.unwrap().status()
}

fn sign(body: &[u8]) -> String {
    let mut mac = Hmac::<Sha256>::new_from_slice(b"s3cret").unwrap();
    mac.update(body);
    format!("sha256={}", hex::encode(mac.finalize().into_bytes()))
}

#[tokio::test]
async fn webhooks_verify_callers_and_become_external_events() {
    let e = setup().await;
    let body = br#"{"ref":"refs/heads/main"}"#.to_vec();
    assert_eq!(post(&e, "/hooks/nope", &[], body.clone()).await, StatusCode::NOT_FOUND);
    assert_eq!(post(&e, "/hooks/github", &[], body.clone()).await, StatusCode::UNAUTHORIZED);
    assert_eq!(
        post(&e, "/hooks/github", &[("authorization", "Bearer wrong")], body.clone()).await,
        StatusCode::UNAUTHORIZED
    );
    let forged = sign(b"something else");
    assert_eq!(
        post(&e, "/hooks/github", &[("x-hub-signature-256", &forged)], body.clone()).await,
        StatusCode::UNAUTHORIZED
    );
    // The user's token is no webhook credential either.
    let user = e.hub.tokens.issue(&Principal::User, chrono::Duration::hours(1), Utc::now());
    let bearer = format!("Bearer {user}");
    assert_eq!(post(&e, "/hooks/github", &[("authorization", &bearer)], body.clone()).await, StatusCode::UNAUTHORIZED);

    assert_eq!(
        post(&e, "/hooks/github", &[("authorization", "Bearer s3cret")], body.clone()).await,
        StatusCode::ACCEPTED
    );
    let sig = sign(&body);
    assert_eq!(post(&e, "/hooks/github", &[("x-hub-signature-256", &sig)], body.clone()).await, StatusCode::ACCEPTED);

    let q = EventQuery { kind: Some(kinds::WEBHOOK_RECEIVED.into()), limit: 5, ..Default::default() };
    let evs = e.hub.store.events(&q).await.unwrap();
    assert_eq!(evs.len(), 2);
    assert_eq!((evs[0].provenance, evs[0].source.clone()), (Provenance::External, Source::Webhook("github".into())));
    assert_eq!(evs[0].payload["body"]["ref"], "refs/heads/main");
    let delivered = e.hub.store.pending_deliveries("botler").await.unwrap();
    assert!(
        delivered.iter().any(|d| d.event_id == evs[0].id.to_string() && d.prompt.as_deref() == Some("Someone pushed."))
    );

    let big = vec![b'x'; botler_hub::webhooks::MAX_BODY + 1];
    assert_eq!(
        post(&e, "/hooks/github", &[("authorization", "Bearer s3cret")], big).await,
        StatusCode::PAYLOAD_TOO_LARGE
    );
    let mut limited = false;
    for _ in 0..70 {
        if post(&e, "/hooks/github", &[("authorization", "Bearer s3cret")], b"{}".to_vec()).await
            == StatusCode::TOO_MANY_REQUESTS
        {
            limited = true;
            break;
        }
    }
    assert!(limited, "calls are rate limited per webhook");
}

#[tokio::test]
async fn replay_event_reruns_a_job_on_a_past_event_as_a_dry_run() {
    let e = common::env().await;
    let files = common::job("digest", "command = [\"python3\", \"-m\", \"digest\"]\n");
    let refs: Vec<(&str, &str)> = files.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect();
    e.commit(&refs).await;
    let ev = Event::new("mail.received", Source::Runnable("poller".into()), Provenance::External, Utc::now())
        .payload(json!({"subject": "Invoice"}));
    let ev = e.hub.emit(ev).await.unwrap();
    let chat = router::new_instance(&e.hub, "chat", None, None).await.unwrap();
    let me = Caller {
        principal: Principal::Agent { instance: chat.clone(), wake: "w".into() },
        capabilities: vec![],
        runnable: None,
    };
    let foreign = json!({"event_id": ev.id.to_string(), "job": "digest", "branch": "agents/other/x"});
    assert!(ops::call(&e.hub, &me, "replay_event", foreign).await.is_err(), "only your own branches");
    let out =
        ops::call(&e.hub, &me, "replay_event", json!({"event_id": ev.id.to_string(), "job": "digest"})).await.unwrap();
    let run = e.hub.store.run(out["run_id"].as_str().unwrap()).await.unwrap();
    assert_eq!((run.kind, run.event_id.clone()), (RunKind::Trial, Some(ev.id.to_string())));
    // The image builds in the background; then the container gets the event.
    let name = botler_hub::jobs::container_name(&e.hub, &run.id);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    while !e.fake.containers().contains_key(&name) {
        assert!(std::time::Instant::now() < deadline, "the replay never started");
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    let c = &e.fake.containers()[&name];
    let file = c.spec.mounts.iter().find_map(|m| match m {
        Mount::Bind { src, dst, .. } if dst == "/botler/event.json" => Some(src.clone()),
        _ => None,
    });
    let replayed: serde_json::Value = serde_json::from_slice(&std::fs::read(file.unwrap()).unwrap()).unwrap();
    assert_eq!(replayed["payload"]["subject"], "Invoice");
    assert!(e.hub.store.trial(&run.id).await.unwrap().is_some(), "a trial: effects recorded, not carried out");
}
