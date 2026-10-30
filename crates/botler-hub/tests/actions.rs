//! Actions: approval-gated jobs with hub-class secrets, run once per approved set of arguments.

mod common;

use botler_core::event::kinds;
use botler_hub::auth::Principal;
use botler_hub::ops::{self, Caller, OpError};
use botler_hub::{jobs, router, supervisor, transactions};
use botler_runtime::{ContainerEvent, Mount};
use botler_store::{DialogKind, EventQuery, ProposalStatus, RunKind};
use serde_json::json;

const ACTION: &str = r#"lifetime = "action"
description = "Send an email from the user's account."
command = ["python3", "-m", "send"]
secrets = ["mail"]
args = { type = "object", properties = { to = { type = "string" }, subject = { type = "string" } }, required = ["to", "subject"], additionalProperties = false }
"#;

async fn setup() -> (common::Env, String, Caller) {
    let e = common::env().await;
    let main = e.hub.repo.main().await.unwrap();
    let secrets = e.hub.repo.read_file(&main, "secrets.toml").await.unwrap().unwrap();
    let secrets =
        format!("{secrets}\n[[group]]\nname = \"mail\"\nclass = \"hub\"\n[[group.secret]]\nname = \"SMTP_PASS\"\n");
    let mut files = common::job("send-mail", ACTION);
    files.push(("secrets.toml".into(), secrets));
    let refs: Vec<(&str, &str)> = files.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect();
    e.commit(&refs).await;
    e.hub.store.set_secret(&e.hub.secret_key, "SMTP_PASS", "hunter2").await.unwrap();
    let chat = router::new_instance(&e.hub, "chat", None, None).await.unwrap();
    let me = Caller {
        principal: Principal::Agent { instance: chat.clone(), wake: "w".into() },
        capabilities: vec![],
        runnable: None,
    };
    (e, chat, me)
}

async fn results(e: &common::Env) -> Vec<botler_core::Event> {
    let q = EventQuery { kind: Some(kinds::ACTION_RESULT.into()), limit: 10, ..Default::default() };
    e.hub.store.events(&q).await.unwrap()
}

#[tokio::test]
async fn actions_check_their_arguments_and_ask_the_user() {
    let (e, chat, me) = setup().await;
    let unknown = ops::call(&e.hub, &me, "invoke_action", json!({"name": "nope", "args": {}})).await;
    assert!(matches!(unknown, Err(OpError::BadRequest(m)) if m.contains("no action nope")));
    let bad = ops::call(&e.hub, &me, "invoke_action", json!({"name": "send-mail", "args": {"to": 1}})).await;
    assert!(matches!(&bad, Err(OpError::BadRequest(m)) if m.contains("do not match its schema")), "{bad:?}");
    // Actions are never run any other way.
    assert!(jobs::start(&e.hub, "send-mail", None, None).await.is_err());

    let args = json!({"to": "anna@example.org", "subject": "Dinner"});
    let out = ops::call(&e.hub, &me, "invoke_action", json!({"name": "send-mail", "args": args})).await.unwrap();
    let id = out["proposal_id"].as_str().unwrap().to_string();
    let p = e.hub.store.proposal(&id).await.unwrap();
    assert_eq!(
        (p.status, p.action.as_deref(), p.args.clone()),
        (ProposalStatus::AwaitingUser, Some("send-mail"), Some(args))
    );
    let d =
        e.hub.store.open_dialogs().await.unwrap().into_iter().find(|d| d.proposal_id.as_deref() == Some(&id)).unwrap();
    assert_eq!(d.kind, DialogKind::Approval);
    let detail = d.detail.unwrap();
    assert!(detail.contains("anna@example.org") && detail.contains("`mail`"), "{detail}");

    // Declining needs a reason, which the chat reads.
    assert!(transactions::on_approval(&e.hub, &id, false, None).await.is_err());
    transactions::on_approval(&e.hub, &id, false, Some("not tonight")).await.unwrap();
    let r = results(&e).await;
    assert_eq!(r[0].to.as_deref(), Some(chat.as_str()));
    assert_eq!(
        (r[0].payload["ok"].clone(), r[0].payload["result"]["declined"].clone()),
        (json!(false), json!("not tonight"))
    );
    assert!(transactions::on_approval(&e.hub, &id, true, None).await.is_err(), "decided once");
}

#[tokio::test]
async fn an_approved_action_runs_once_with_its_hub_secret_and_reports_back() {
    let (e, chat, me) = setup().await;
    let args = json!({"to": "anna@example.org", "subject": "Dinner"});
    let out = ops::call(&e.hub, &me, "invoke_action", json!({"name": "send-mail", "args": args})).await.unwrap();
    let id = out["proposal_id"].as_str().unwrap().to_string();
    transactions::on_approval(&e.hub, &id, true, None).await.unwrap();
    assert_eq!(e.hub.store.proposal(&id).await.unwrap().status, ProposalStatus::Approved);

    let run = e.hub.store.runs_for("send-mail", 5).await.unwrap().remove(0);
    assert_eq!(run.kind, RunKind::Action);
    let name = jobs::container_name(&e.hub, &run.id);
    let c = &e.fake.containers()[&name];
    assert_eq!(c.spec.env["SMTP_PASS"], "hunter2", "the hub-class secret, for this run only");
    assert_eq!(c.spec.env["BOTLER_ACTION_ARGS"], "/botler/args.json");
    let args_file = c.spec.mounts.iter().find_map(|m| match m {
        Mount::Bind { src, dst, .. } if dst == "/botler/args.json" => Some(src.clone()),
        _ => None,
    });
    let written: serde_json::Value = serde_json::from_slice(&std::fs::read(args_file.unwrap()).unwrap()).unwrap();
    assert_eq!(written, args);

    e.fake.exit(&name, 0);
    supervisor::on_event(&e.hub, &ContainerEvent { name: name.clone(), status: "died".into(), exit_code: Some(0) })
        .await
        .unwrap();
    let r = results(&e).await;
    assert_eq!(r.len(), 1);
    assert_eq!((r[0].to.as_deref(), &r[0].payload["ok"]), (Some(chat.as_str()), &json!(true)));
    assert_eq!(r[0].payload["result"], json!(format!("fake logs of {name}")), "the last line of output");
    assert!(e.hub.store.open_incidents().await.unwrap().is_empty());
}
