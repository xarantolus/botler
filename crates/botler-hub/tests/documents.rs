//! Documents agents write for the user: by name, replaced on rewrite (the previous body kept).

mod common;

use botler_hub::auth::Principal;
use botler_hub::ops::{self, Caller};
use botler_hub::router;
use serde_json::json;

#[tokio::test]
async fn agents_write_documents_the_user_reads() {
    let e = common::env().await;
    let chat = router::new_instance(&e.hub, "chat", None, None).await.unwrap();
    let agent = Caller {
        principal: Principal::Agent { instance: chat.clone(), wake: "w".into() },
        capabilities: vec![],
        runnable: None,
    };
    let user = Caller { principal: Principal::User, capabilities: vec![], runnable: None };
    let write = |body: &str| json!({"name": "flat-shortlist", "title": "Flat shortlist", "body": body});

    let out = ops::call(&e.hub, &agent, "write_document", write("- Riverside, €1400")).await.unwrap();
    assert_eq!((out["path"].as_str(), out["updated"].as_bool()), (Some("/documents/flat-shortlist"), Some(false)));
    let out =
        ops::call(&e.hub, &agent, "write_document", write("- Riverside, €1400\n- Old Town, €1300")).await.unwrap();
    assert_eq!(out["updated"], true);

    let doc = ops::call(&e.hub, &user, "read_document", json!({"name": "flat-shortlist"})).await.unwrap();
    // Its link reads it too.
    let link = json!({"name": "http://myhost:7420/documents/flat-shortlist"});
    assert_eq!(ops::call(&e.hub, &user, "read_document", link).await.unwrap(), doc);
    assert!(doc["body"].as_str().unwrap().contains("Old Town"));
    assert_eq!(doc["previous_body"], "- Riverside, €1400");
    assert_eq!(doc["author"], format!("agent:{chat}"));
    let list = ops::call(&e.hub, &user, "list_documents", json!({})).await.unwrap();
    assert_eq!(list[0]["name"], "flat-shortlist");

    let bad = ops::call(&e.hub, &agent, "write_document", json!({"name": "Bad Name", "title": "x", "body": ""})).await;
    assert!(bad.is_err());
    assert!(
        ops::call(&e.hub, &agent, "delete_document", json!({"name": "flat-shortlist"})).await.is_err(),
        "the user deletes"
    );
    let gone = ops::call(&e.hub, &user, "delete_document", json!({"name": "flat-shortlist"})).await.unwrap();
    assert_eq!(gone["deleted"], true);
}
