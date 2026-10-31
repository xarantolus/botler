//! A chat's session tree on the fake runtime: the tree from the JSONL, rewinding and labelling
//! through the extension's commands (only while idle), and forking a new chat.

mod common;

use botler_core::event::kinds;
use botler_core::{Event, Provenance, Source};
use botler_hub::agents;
use botler_hub::auth::Principal;
use botler_hub::ops::{self, Caller, OpError};
use chrono::Utc;
use common::{Env, env};
use serde_json::{Value, json};

fn user() -> Caller {
    Caller { principal: Principal::User, capabilities: vec![], runnable: None }
}

fn msg(id: &str, parent: Option<&str>, role: &str, text: &str) -> Value {
    json!({"type": "message", "id": id, "parentId": parent, "timestamp": "2026-10-30T10:00:00Z",
           "message": {"role": role, "content": [{"type": "text", "text": text}]}})
}

/// `u1 → a1 → u2 → a2`, a label on `a1`, then a second branch `a1 → u3 → a3` (the leaf).
fn session() -> String {
    let lines = [
        json!({"type": "session", "version": 3, "id": "s-old", "cwd": "/home/agent"}),
        msg("u1", None, "user", "plan the trip"),
        msg("a1", Some("u1"), "assistant", "Here is a plan."),
        msg("u2", Some("a1"), "user", "by train"),
        msg("a2", Some("u2"), "assistant", "Trains booked."),
        json!({"type": "label", "id": "l1", "parentId": "a2", "targetId": "a1", "label": "checkpoint"}),
        msg("u3", Some("a1"), "user", "by plane instead"),
        msg("a3", Some("u3"), "assistant", "Flights booked."),
    ];
    lines.iter().map(|l| format!("{l}\n")).collect()
}

async fn setup() -> Env {
    let e = env().await;
    e.hub.store.set_secret(&e.hub.secret_key, "OPENROUTER_API_KEY", "sk").await.unwrap();
    let inst = e.hub.store.instance("botler").await;
    if inst.is_err() {
        let ev = Event::new(kinds::MESSAGE, Source::User, Provenance::Trusted, Utc::now())
            .to("botler")
            .payload(json!({"text": "hi"}));
        e.hub.emit(ev).await.unwrap();
        e.route().await;
    }
    let file = e.hub.store.instance("botler").await.unwrap().session_file;
    let dir = e.hub.paths.sessions("botler");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join(file), session()).unwrap();
    e
}

async fn call(e: &Env, op: &str, args: Value) -> Result<Value, OpError> {
    ops::call(&e.hub, &user(), op, args).await
}

#[tokio::test]
async fn the_tree_shows_branches_labels_and_the_leaf() {
    let e = setup().await;
    let t = call(&e, "session_tree", json!({"chat": "botler"})).await.unwrap();
    assert_eq!(t["leaf"], "a3");
    let node = |id: &str| t["nodes"].as_array().unwrap().iter().find(|n| n["id"] == id).unwrap().clone();
    for (id, on) in [("u1", true), ("a1", true), ("u3", true), ("a3", true), ("u2", false), ("a2", false)] {
        assert_eq!(node(id)["on_branch"], on, "{id}");
    }
    assert_eq!(node("a1")["label"], "checkpoint");
    assert_eq!((node("a2")["kind"].as_str(), node("a2")["text"].as_str()), (Some("assistant"), Some("Trains booked.")));
    assert_eq!(node("l1")["kind"], "label");
    assert!(t["nodes"].as_array().unwrap().iter().all(|n| n["id"] != "s-old"), "the header is no entry");
    // Agents do not get it.
    let agent = Caller {
        principal: Principal::Agent { instance: "botler".into(), wake: "w".into() },
        capabilities: vec![],
        runnable: None,
    };
    assert!(matches!(
        ops::call(&e.hub, &agent, "session_tree", json!({"chat": "botler"})).await,
        Err(OpError::Forbidden(_))
    ));
}

#[tokio::test]
async fn goto_and_label_go_through_the_extension_only_while_idle() {
    let e = setup().await;
    // A sleeping chat is woken for it.
    call(&e, "goto_entry", json!({"chat": "botler", "entry": "a2"})).await.unwrap();
    call(&e, "label_entry", json!({"chat": "botler", "entry": "a1", "label": "keep"})).await.unwrap();
    call(&e, "label_entry", json!({"chat": "botler", "entry": "a1"})).await.unwrap();
    let container = agents::container_name(&e.hub, "botler");
    assert_eq!(e.fake.commands(&container), ["/botler-goto a2", "/botler-label a1 keep", "/botler-label a1"]);
    assert!(e.fake.prompts(&container).is_empty(), "commands start no turn");
    // What the extension writes after navigating pins the branch, for pi and for the transcript.
    let inst = e.hub.store.instance("botler").await.unwrap();
    let path = e.hub.paths.sessions("botler").join(&inst.session_file);
    let nav =
        json!({"type": "custom", "customType": "botler.nav", "id": "n1", "parentId": "a2", "data": {"target": "a2"}});
    std::fs::write(&path, format!("{}{nav}\n", session())).unwrap();
    let items = botler_hub::sessions::read(&e.hub, "botler", None).await.unwrap();
    let texts: Vec<String> = items
        .iter()
        .filter_map(|i| match i {
            botler_hub::sessions::Item::Assistant { text, .. } => Some(text.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(texts, ["Here is a plan.", "Trains booked."]);
    assert_eq!(call(&e, "session_tree", json!({"chat": "botler"})).await.unwrap()["leaf"], "n1");
    assert!(matches!(
        call(&e, "goto_entry", json!({"chat": "botler", "entry": "nope"})).await,
        Err(OpError::BadRequest(m)) if m.contains("no entry nope")
    ));
    // Working: refused, nothing sent.
    e.fake.set_hold_turns(true);
    let ev = Event::new(kinds::MESSAGE, Source::User, Provenance::Trusted, Utc::now())
        .to("botler")
        .payload(json!({"text": "go"}));
    e.hub.emit(ev).await.unwrap();
    e.route().await;
    agents::dispatch_pending(&e.hub).await.unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    assert!(matches!(
        call(&e, "goto_entry", json!({"chat": "botler", "entry": "a1"})).await,
        Err(OpError::BadRequest(m)) if m.contains("working")
    ));
    assert_eq!(e.fake.commands(&container).len(), 3);
}

#[tokio::test]
async fn forking_copies_the_branch_up_to_an_assistant_entry_into_a_new_chat() {
    let e = setup().await;
    assert!(matches!(
        call(&e, "fork_chat", json!({"chat": "botler", "entry": "u2"})).await,
        Err(OpError::BadRequest(m)) if m.contains("assistant")
    ));
    let out = call(&e, "fork_chat", json!({"chat": "botler", "entry": "a2"})).await.unwrap();
    let fork = out["instance"].as_str().unwrap();
    let inst = e.hub.store.instance(fork).await.unwrap();
    assert_eq!(inst.definition, "botler");
    let text = std::fs::read_to_string(e.hub.paths.sessions(fork).join(&inst.session_file)).unwrap();
    let ids: Vec<String> =
        text.lines().map(|l| serde_json::from_str::<Value>(l).unwrap()["id"].as_str().unwrap().to_string()).collect();
    assert_eq!(&ids[1..], ["u1", "a1", "u2", "a2"]);
    assert_ne!(ids[0], "s-old", "a session of its own");
    let items = botler_hub::sessions::read(&e.hub, fork, None).await.unwrap();
    assert_eq!(items.len(), 4);
    // The original is untouched.
    assert_eq!(call(&e, "session_tree", json!({"chat": "botler"})).await.unwrap()["leaf"], "a3");
}
