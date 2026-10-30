//! The search index: incremental indexing, rebuilds, permissions and the query error path.

mod common;

use std::io::Write;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use botler_core::defs::Capability;
use botler_core::{Event, Provenance, Source};
use botler_hub::auth::Principal;
use botler_hub::ops::{self, Caller, OpError};
use botler_hub::search::{self, Indexed};
use botler_store::NewInstance;
use chrono::Utc;
use common::{Env, env};
use serde_json::{Value, json};
use tower::ServiceExt;

async fn instance(e: &Env, name: &str, definition: &str) {
    e.hub
        .store
        .create_instance(&NewInstance {
            name: name.into(),
            definition: definition.into(),
            persistent: false,
            session_file: "s.jsonl".into(),
            branch: None,
            parent: None,
            depth: 0,
            task: None,
        })
        .await
        .unwrap();
}

fn session_path(e: &Env, name: &str) -> std::path::PathBuf {
    e.hub.paths.sessions(name).join("s.jsonl")
}

fn user(id: &str, text: &str) -> String {
    json!({"type": "message", "id": id, "timestamp": "2026-10-30T10:00:00Z",
           "message": {"role": "user", "content": [{"type": "text", "text": text}]}})
    .to_string()
        + "\n"
}

fn assistant(id: &str, text: &str) -> String {
    json!({"type": "message", "id": id, "timestamp": "2026-10-30T10:00:01Z",
           "message": {"role": "assistant", "content": [{"type": "text", "text": text},
               {"type": "toolCall", "id": "c1", "name": "bash", "arguments": {"command": "ls"}}]}})
    .to_string()
        + "\n"
}

fn append(e: &Env, name: &str, text: &str) {
    let p = session_path(e, name);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    let mut f = std::fs::OpenOptions::new().create(true).append(true).open(p).unwrap();
    f.write_all(text.as_bytes()).unwrap();
}

fn agent(name: &str, caps: Vec<Capability>) -> Caller {
    Caller {
        principal: Principal::Agent { instance: name.into(), wake: "w".into() },
        capabilities: caps,
        runnable: None,
    }
}

fn user_caller() -> Caller {
    Caller { principal: Principal::User, capabilities: vec![], runnable: None }
}

async fn find(e: &Env, caller: &Caller, args: Value) -> Result<Vec<Value>, OpError> {
    let out = ops::call(&e.hub, caller, "search", args).await?;
    Ok(out["results"].as_array().unwrap().clone())
}

fn refs(hits: &[Value]) -> Vec<String> {
    hits.iter().map(|h| format!("{}:{}", h["source"].as_str().unwrap(), h["ref"].as_str().unwrap())).collect()
}

#[tokio::test]
async fn session_files_are_indexed_incrementally() {
    let e = env().await;
    instance(&e, "chat-a", "chat").await;
    append(&e, "chat-a", "{\"type\":\"session\",\"version\":3,\"cwd\":\"/home/agent\"}\n");
    append(
        &e,
        "chat-a",
        &user("u1", "[from user | provenance: trusted | message | delivery d1 | event e1]\nplease fix the antenna"),
    );
    append(&e, "chat-a", &assistant("a1", "Rotated the antenna by ninety degrees"));
    let first = search::index_pending(&e.hub).await.unwrap();
    // The user's message, the assistant's text and its tool call (a document of its own).
    assert_eq!(first.entries, 3);
    assert!(first.repo > 0, "the applied monorepo is indexed");

    // Nothing new: nothing indexed.
    assert_eq!(search::index_pending(&e.hub).await.unwrap(), Indexed::default());

    // Only the appended entry; a half-written line waits until it is complete.
    let line = user("u2", "what about the gearbox");
    append(&e, "chat-a", &line);
    let partial = assistant("a2", "gearbox replaced");
    append(&e, "chat-a", &partial[..20]);
    assert_eq!(search::index_pending(&e.hub).await.unwrap().entries, 1);
    append(&e, "chat-a", &partial[20..]);
    assert_eq!(search::index_pending(&e.hub).await.unwrap().entries, 2);

    let hits = find(&e, &user_caller(), json!({"query": "antenna"})).await.unwrap();
    let mut found = refs(&hits);
    found.sort();
    assert_eq!(found, ["session:a1", "session:u1"], "{hits:?}");
    let u1 = hits.iter().find(|h| h["ref"] == "u1").unwrap();
    assert_eq!(u1["owner"], "chat-a");
    assert_eq!(u1["title"], "from user");
    assert!(u1["snippet"].as_str().unwrap().contains("**antenna**"), "{u1}");
    assert!(!u1["snippet"].as_str().unwrap().contains("provenance"), "headers are not indexed: {u1}");
    // Stemming; a tool call is found as itself (`<entry>#<call>`).
    let rotate = json!({"query": "rotate", "sources": ["session"]});
    assert_eq!(refs(&find(&e, &user_caller(), rotate).await.unwrap()), ["session:a1"]);
    let calls = refs(&find(&e, &user_caller(), json!({"query": "ls"})).await.unwrap());
    assert!(calls.contains(&"session:a1#c1".to_string()), "{calls:?}");
    assert_eq!(refs(&find(&e, &user_caller(), json!({"query": "gearbox replaced"})).await.unwrap()), ["session:a2"]);

    // A rewritten file (a new working directory in its header) is read again without duplicates.
    assert!(botler_hub::sessions::retarget_cwd(&session_path(&e, "chat-a"), "/workspace").await.unwrap());
    assert_eq!(search::index_pending(&e.hub).await.unwrap().entries, 6);
    assert_eq!(find(&e, &user_caller(), json!({"query": "antenna"})).await.unwrap().len(), 2);
}

#[tokio::test]
async fn compacted_history_stays_searchable_and_readable() {
    let e = env().await;
    instance(&e, "chat-a", "chat").await;
    append(&e, "chat-a", &user("u1", "the router password lives in the blue binder"));
    append(&e, "chat-a", &assistant("a1", "noted"));
    append(
        &e,
        "chat-a",
        &(json!({"type": "compaction", "id": "c1", "parentId": "a1", "summary": "User talked about networking.",
                 "firstKeptEntryId": "a1", "timestamp": "2026-10-30T11:00:00Z"})
        .to_string()
            + "\n"),
    );
    for i in 0..8 {
        append(&e, "chat-a", &user(&format!("later{i}"), &format!("unrelated message {i}")));
    }
    search::index_pending(&e.hub).await.unwrap();

    let me = agent("chat-a", vec![]);
    let hits = find(&e, &me, json!({"query": "blue binder", "scope": "self"})).await.unwrap();
    assert_eq!(refs(&hits), ["session:u1"]);
    assert_eq!(refs(&find(&e, &me, json!({"query": "networking", "scope": "self"})).await.unwrap()), ["session:c1"]);

    // The hit's ref reads the context around it, from before the compaction.
    let ctx =
        ops::call(&e.hub, &me, "read_session", json!({"instance": "chat-a", "around": "u1", "n": 1})).await.unwrap();
    let ids: Vec<&str> = ctx["messages"].as_array().unwrap().iter().map(|m| m["id"].as_str().unwrap()).collect();
    assert_eq!(ids, ["u1", "a1"]);
    let ctx = ops::call(&e.hub, &me, "read_session", json!({"instance": "chat-a", "around": "later3", "n": 1}))
        .await
        .unwrap();
    let ids: Vec<&str> = ctx["messages"].as_array().unwrap().iter().map(|m| m["id"].as_str().unwrap()).collect();
    assert_eq!(ids, ["later2", "later3", "later4"]);
    let missing = ops::call(&e.hub, &me, "read_session", json!({"instance": "chat-a", "around": "nope"})).await;
    assert!(matches!(missing, Err(OpError::BadRequest(_))), "{missing:?}");
}

#[tokio::test]
async fn results_follow_capabilities() {
    let e = env().await;
    instance(&e, "chat-a", "chat").await;
    instance(&e, "dev-b", "dev").await;
    append(&e, "chat-a", &user("ua", "zebra crossing for chat a"));
    append(&e, "dev-b", &user("ub", "zebra stripes for dev b"));
    let mem = e.hub.paths.root.join("memory");
    for (path, text) in [
        ("agents/chat/notes.md", "zebra facts for chat agents"),
        ("agents/dev/notes.md", "zebra facts for dev agents"),
        ("user/profile.md", "the user likes zebra prints"),
        (".git/zebra.md", "not memory"),
    ] {
        std::fs::create_dir_all(mem.join(path).parent().unwrap()).unwrap();
        std::fs::write(mem.join(path), text).unwrap();
    }
    e.hub
        .emit(
            Event::new("note.taken", Source::Agent("dev-b".into()), Provenance::External, Utc::now())
                .payload(json!({"text": "a zebra was seen"})),
        )
        .await
        .unwrap();
    let indexed = search::index_pending(&e.hub).await.unwrap();
    assert_eq!((indexed.memory, indexed.entries), (3, 2));

    let q = json!({"query": "zebra", "limit": 30});
    let all = refs(&find(&e, &user_caller(), q.clone()).await.unwrap());
    assert_eq!(all.len(), 6, "the user sees everything: {all:?}");

    let a = refs(&find(&e, &agent("chat-a", vec![]), q.clone()).await.unwrap());
    let mut want: Vec<String> =
        vec!["session:ua".into(), "memory:agents/chat/notes.md".into(), "memory:user/profile.md".into()];
    want.push(all.iter().find(|r| r.starts_with("event:")).unwrap().clone());
    let (mut a_sorted, mut want_sorted) = (a.clone(), want.clone());
    a_sorted.sort();
    want_sorted.sort();
    assert_eq!(a_sorted, want_sorted, "no other session or namespace without the capability");

    let reader = refs(&find(&e, &agent("chat-a", vec![Capability::ReadSessions]), q.clone()).await.unwrap());
    assert!(reader.contains(&"session:ub".to_string()), "{reader:?}");
    assert!(!reader.contains(&"memory:agents/dev/notes.md".to_string()), "{reader:?}");

    let own = refs(
        &find(&e, &agent("chat-a", vec![Capability::ReadSessions]), json!({"query": "zebra", "scope": "self"}))
            .await
            .unwrap(),
    );
    let mut own_sorted = own.clone();
    own_sorted.sort();
    assert_eq!(own_sorted, ["memory:agents/chat/notes.md", "memory:user/profile.md", "session:ua"]);

    let only = refs(&find(&e, &user_caller(), json!({"query": "zebra", "sources": ["memory"]})).await.unwrap());
    assert!(only.iter().all(|r| r.starts_with("memory:")) && only.len() == 3, "{only:?}");

    // Reading another session needs the capability; your own does not.
    let denied = ops::call(&e.hub, &agent("chat-a", vec![]), "read_session", json!({"instance": "dev-b"})).await;
    assert!(matches!(denied, Err(OpError::Forbidden(_))), "{denied:?}");
    ops::call(&e.hub, &agent("chat-a", vec![]), "read_session", json!({"instance": "chat-a"})).await.unwrap();

    // Removed memory files leave the index.
    std::fs::remove_file(mem.join("user/profile.md")).unwrap();
    search::index_pending(&e.hub).await.unwrap();
    let left = refs(&find(&e, &user_caller(), json!({"query": "zebra", "sources": ["memory"]})).await.unwrap());
    assert_eq!(left.len(), 2, "{left:?}");
}

#[tokio::test]
async fn query_errors_are_bad_requests() {
    let e = env().await;
    search::index_pending(&e.hub).await.unwrap();
    for raw in [r#""unterminated"#, "AND OR", "foo NEAR("] {
        match find(&e, &user_caller(), json!({"query": raw, "raw": true})).await {
            Err(OpError::BadRequest(m)) => assert!(m.contains("invalid search syntax"), "{m}"),
            other => panic!("{raw}: {other:?}"),
        }
        // As plain words the same input is fine.
        find(&e, &user_caller(), json!({"query": raw})).await.unwrap();
    }
    assert!(matches!(find(&e, &user_caller(), json!({"query": "  "})).await, Err(OpError::BadRequest(_))));
    assert!(matches!(
        find(&e, &user_caller(), json!({"query": "x", "scope": "self"})).await,
        Err(OpError::BadRequest(_))
    ));
    // Raw syntax works when valid.
    let hits =
        find(&e, &user_caller(), json!({"query": "botler OR zzzz", "raw": true, "sources": ["repo"]})).await.unwrap();
    assert!(!hits.is_empty());
}

#[tokio::test]
async fn deleting_the_index_rebuilds_it() {
    let e = env().await;
    instance(&e, "chat-a", "chat").await;
    append(&e, "chat-a", &user("u1", "quokka sighting"));
    e.hub.emit(Event::new("seen.quokka", Source::User, Provenance::Trusted, Utc::now())).await.unwrap();
    let first = search::index_pending(&e.hub).await.unwrap();
    assert!(first.events >= 1 && first.entries == 1 && first.repo > 0, "{first:?}");

    std::fs::remove_file(e.hub.paths.search_db()).unwrap();
    let again = search::index_pending(&e.hub).await.unwrap();
    assert_eq!(again, first, "everything is indexed again");
    assert!(e.hub.paths.search_db().exists());
    let hits = find(&e, &user_caller(), json!({"query": "quokka"})).await.unwrap();
    assert_eq!(hits.len(), 2, "{hits:?}");

    // A corrupt file is replaced when the hub opens it.
    let paths = e.hub.paths.clone();
    std::fs::write(paths.search_db(), b"not a database").unwrap();
    let reopened = search::Index::open(&paths.search_db()).await;
    assert!(reopened.is_ok(), "{reopened:?}");
}

/// Over HTTP, through the hub: the user's search route and an agent's op on the fake runtime.
#[tokio::test]
async fn search_route_and_op() {
    let e = env().await;
    instance(&e, "chat-a", "chat").await;
    append(&e, "chat-a", &user("u1", "the wombat migration is done"));
    search::index_pending(&e.hub).await.unwrap();
    let app = botler_hub::api::router(e.hub.clone());
    let token = e.hub.tokens.issue(&Principal::User, chrono::Duration::hours(1), Utc::now());
    let get =
        |uri: String| Request::get(uri).header("authorization", format!("Bearer {token}")).body(Body::empty()).unwrap();
    let resp =
        app.clone().oneshot(get("/api/v1/search?q=wombat%20migration&sources=session,event".into())).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body: Value =
        serde_json::from_slice(&axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap()).unwrap();
    assert_eq!(body["results"][0]["ref"], "u1");
    assert_eq!(body["results"][0]["source"], "session");

    let resp = app.clone().oneshot(get("/api/v1/search?q=%22x&raw=true".into())).await.unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let resp = app.clone().oneshot(get("/api/v1/search?q=x&sources=nope".into())).await.unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

    // Agents see the op; jobs do not.
    let tools = |c: &Caller| ops::OPS.iter().filter(|o| o.who.admits(c)).map(|o| o.name).collect::<Vec<_>>();
    assert!(tools(&agent("chat-a", vec![])).contains(&"search"));
    let run =
        Caller { principal: Principal::Run { run_id: "r".into() }, capabilities: vec![], runnable: Some("j".into()) };
    assert!(!tools(&run).contains(&"search"));
}
