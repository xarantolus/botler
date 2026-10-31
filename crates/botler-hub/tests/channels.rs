//! Channels: pairing with one-time codes, unpaired senders ignored, the user's messages delivered
//! with `via`, replies and questions back in the outbox, and only channel services may call.

mod common;

use botler_hub::auth::Principal;
use botler_hub::channels;
use botler_hub::ops::{self, Caller, OpError};
use botler_hub::router;
use botler_store::{NewRun, RunKind};
use serde_json::{Value, json};

const CHANNEL: &str = "lifetime = \"service\"\ncommand = [\"bot\"]\nchannel = true\n";

async fn caller(e: &common::Env, job: &str) -> Caller {
    let run = e
        .hub
        .store
        .create_run(&NewRun {
            runnable: job.into(),
            kind: RunKind::Service,
            trigger_id: None,
            event_id: None,
            commit_id: None,
        })
        .await
        .unwrap();
    Caller::resolve(&e.hub, Principal::Run { run_id: run.id }).await.unwrap()
}

async fn inbound(e: &common::Env, me: &Caller, sender: &str, text: &str, reply_ref: Option<&str>) -> Value {
    ops::call(&e.hub, me, "channel_inbound", json!({"sender": sender, "text": text, "reply_ref": reply_ref}))
        .await
        .unwrap()
}

async fn outbox(e: &common::Env, me: &Caller) -> Vec<Value> {
    let out = ops::call(&e.hub, me, "channel_outbox", json!({})).await.unwrap();
    let msgs = out["messages"].as_array().unwrap().clone();
    for m in &msgs {
        ops::call(&e.hub, me, "channel_ack", json!({"id": m["id"]})).await.unwrap();
    }
    msgs
}

#[tokio::test]
async fn a_paired_sender_talks_as_the_user_and_hears_back() {
    let e = common::env().await;
    let mut files = common::job("tg", CHANNEL);
    files.extend(common::job("plain", "lifetime = \"service\"\ncommand = [\"x\"]\n"));
    let refs: Vec<(&str, &str)> = files.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect();
    e.commit(&refs).await;
    let me = caller(&e, "tg").await;

    // Only channel services may call.
    let plain = caller(&e, "plain").await;
    let refused = ops::call(&e.hub, &plain, "channel_outbox", json!({})).await;
    assert!(matches!(&refused, Err(OpError::Forbidden(m)) if m.contains("channel = true")), "{refused:?}");

    // A stranger is ignored, a wrong code too, and nothing goes back.
    assert_eq!(inbound(&e, &me, "42", "hello", None).await["status"], "ignored");
    assert_eq!(inbound(&e, &me, "42", "pair 0000000x", None).await["status"], "ignored");
    assert_eq!(e.hub.store.channel_strangers().await.unwrap()[0].count, 2);
    assert!(outbox(&e, &me).await.is_empty());

    // Pairing binds the sender (once: the code is used up).
    let chat = router::new_instance(&e.hub, "chat", None, None).await.unwrap();
    let (code, _) = channels::create_code(&e.hub, &chat).await.unwrap();
    assert_eq!(
        inbound(&e, &me, "42", &format!("pair {}", code.to_lowercase()), None).await,
        json!({"status": "paired", "to": chat})
    );
    assert_eq!(inbound(&e, &me, "43", &format!("pair {code}"), None).await["status"], "ignored", "single use");
    let sent = outbox(&e, &me).await;
    assert!(sent[0]["text"].as_str().unwrap().starts_with("Paired"), "{sent:?}");
    assert_eq!(sent[0]["sender"], "42");
    assert!(outbox(&e, &me).await.is_empty(), "acknowledged messages do not come again");

    // A message is the user's, with `via`; the turn's reply goes back.
    let got = inbound(&e, &me, "42", "what's on today?", None).await;
    assert_eq!(got["status"], "delivered");
    let ev = e.hub.store.event(got["event_id"].as_str().unwrap()).await.unwrap();
    assert_eq!((ev.source.clone(), ev.to.as_deref()), (botler_core::Source::User, Some(chat.as_str())));
    assert_eq!(ev.payload["via"], json!({"channel": "tg", "sender": "42"}));
    e.route().await;
    let d = e.hub.store.deliveries_for_event(&ev.id.to_string()).await.unwrap();
    e.hub.store.mark_delivered(&d[0].id).await.unwrap();
    channels::turn_ended(&e.hub, &chat, "Two meetings.").await.unwrap();
    assert_eq!(outbox(&e, &me).await[0]["text"], "Two meetings.");

    // A question goes out too and a reply with one of its choices answers it.
    let agent = Caller {
        principal: Principal::Agent { instance: chat.clone(), wake: "w".into() },
        capabilities: vec![],
        runnable: None,
    };
    let asked = ops::call(&e.hub, &agent, "ask_user", json!({"question": "Move the 3pm?", "options": ["Yes", "No"]}))
        .await
        .unwrap();
    let q = outbox(&e, &me).await;
    assert_eq!((q[0]["question"].as_bool(), q[0]["text"].as_str().unwrap().contains("Yes / No")), (Some(true), true));
    let answered = inbound(&e, &me, "42", "yes", None).await;
    assert_eq!(answered, json!({"status": "answered", "dialog_id": asked["dialog_id"]}));
    assert_eq!(e.hub.store.dialog(asked["dialog_id"].as_str().unwrap()).await.unwrap().answer.as_deref(), Some("Yes"));

    // `/chat` switches where messages go; revoking makes the sender a stranger again.
    inbound(&e, &me, "42", "/chat botler", None).await;
    assert!(outbox(&e, &me).await[0]["text"].as_str().unwrap().contains("now go to"));
    let b = e.hub.store.channel_binding("tg", "42").await.unwrap().unwrap();
    assert_eq!(b.target, "botler");
    e.hub.store.revoke_channel_binding(&b.id).await.unwrap();
    assert_eq!(inbound(&e, &me, "42", "hi", None).await["status"], "ignored");
}
