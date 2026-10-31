//! Boot-time migration of a data directory from before the front door was renamed.

mod common;

use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;

use botler_core::FRONT_DOOR;
use botler_core::defs::Target;
use botler_hub::Hub;
use botler_hub::config::{HubConfig, Paths};
use botler_runtime::fake::Fake;
use botler_store::{NewInstance, NewTimer, Store};
use chrono::{Duration, Utc};
use serde_json::json;

async fn open(paths: &Paths) -> Arc<Hub> {
    Hub::open(paths.clone(), HubConfig::new(SocketAddr::from(([127, 0, 0, 1], 0))), Arc::new(Fake::new()))
        .await
        .unwrap()
}

/// A data directory as an older hub left it: `agents/main` in the monorepo, instance `main`.
async fn old_data_dir(root: &Path) -> Paths {
    let tpl = root.join("template");
    common::copy_dir(Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../../template")), &tpl);
    std::fs::rename(tpl.join("agents/botler"), tpl.join("agents/main")).unwrap();
    let prompt = std::fs::read_to_string(tpl.join("agents/main/AGENTS.md")).unwrap();
    std::fs::write(tpl.join("agents/main/AGENTS.md"), prompt.replace("# You are `botler`", "# You are `main`"))
        .unwrap();
    std::fs::write(
        tpl.join("triggers/digest.toml"),
        "version = 1\n[[trigger]]\nname = \"digest\"\ncron = \"0 7 * * *\"\ntarget = { resume = \"main\" }\n",
    )
    .unwrap();
    let paths = Paths::new(root.join("data"));
    paths.ensure().unwrap();
    botler_repo::Repo::new(paths.repo(), paths.worktrees()).init_from(&tpl).await.unwrap();

    let store = Store::open(&paths.db()).await.unwrap();
    store
        .create_instance(&NewInstance {
            name: "main".into(),
            definition: "main".into(),
            persistent: true,
            session_file: "main.jsonl".into(),
            branch: None,
            parent: None,
            depth: 0,
            task: None,
        })
        .await
        .unwrap();
    store
        .create_timer(&NewTimer {
            fire_at: Utc::now() + Duration::hours(1),
            target: Target::Resume("main".into()),
            prompt: Some("check".into()),
            payload: json!({}),
            created_by: "agent:main".into(),
        })
        .await
        .unwrap();
    store.pool().close().await;
    std::fs::create_dir_all(root.join("data/sessions/main")).unwrap();
    std::fs::write(root.join("data/sessions/main/main.jsonl"), "{\"type\":\"session\",\"cwd\":\"/workspace\"}\n")
        .unwrap();
    std::fs::create_dir_all(root.join("data/scratch-spaces/chat-main")).unwrap();
    std::fs::write(root.join("data/scratch-spaces/chat-main/notes.md"), "notes").unwrap();
    paths
}

#[tokio::test]
async fn old_front_door_is_renamed_in_repo_store_and_directories() {
    let dir = tempfile::tempdir().unwrap();
    let paths = old_data_dir(dir.path()).await;
    let hub = open(&paths).await;

    // Monorepo: one hub commit moved the definition and retargeted the trigger.
    let head = hub.repo.main().await.unwrap();
    assert!(hub.repo.read_file(&head, "agents/main/agent.toml").await.unwrap().is_none());
    let prompt = hub.repo.read_file(&head, "agents/botler/AGENTS.md").await.unwrap().unwrap();
    assert!(prompt.starts_with("# You are `botler`"), "{prompt}");
    let trigger = hub.repo.read_file(&head, "triggers/digest.toml").await.unwrap().unwrap();
    assert!(trigger.contains("resume = \"botler\""), "{trigger}");
    assert!(hub.applied().await.defs.agents.contains_key(FRONT_DOOR), "the renamed main is applied");
    assert!(hub.repo.log_subjects(&format!("{head}~1"), &head).await.unwrap()[0].contains("Rename the front-door"));

    // Runtime state and directories.
    let i = hub.store.instance(FRONT_DOOR).await.unwrap();
    assert_eq!(i.definition, FRONT_DOOR);
    assert!(hub.store.find_instance("main").await.unwrap().is_none());
    assert_eq!(hub.store.pending_timers(None).await.unwrap()[0].target, Target::Resume(FRONT_DOOR.into()));
    assert!(paths.sessions(FRONT_DOOR).join(&i.session_file).exists());
    assert_eq!(std::fs::read_to_string(paths.home(FRONT_DOOR).join("notes.md")).unwrap(), "notes");
    assert!(!paths.sessions("main").exists() && !paths.home("main").exists());
    drop(hub);

    // A second boot changes nothing.
    let hub = open(&paths).await;
    assert_eq!(hub.repo.main().await.unwrap(), head);
    assert!(hub.store.find_instance(FRONT_DOOR).await.unwrap().is_some());
    std::fs::remove_dir_all(paths.run_dir().unwrap()).unwrap();
}

#[tokio::test]
async fn fresh_data_dir_is_untouched() {
    let e = common::env().await;
    assert!(e.hub.repo.rev("refs/heads/main~1").await.unwrap().is_none(), "no migration commit on a fresh repo");
    assert!(e.hub.store.find_instance("main").await.unwrap().is_none());
}

/// The template's `[models]` before models were named by id.
const ROUTES_TOML: &str = "version = 1\n\n[models]\ndefault = \"standard\"\n\n[models.routes.standard]\nprovider = \"openrouter\"\nmodel = \"deepseek/deepseek-v4.1-flash\"\napi_key_secret = \"OPENROUTER_API_KEY\"\n\n[models.routes.smart]\nprovider = \"openrouter\"\nmodel = \"anthropic/claude-sonnet-latest\"\napi_key_secret = \"OPENROUTER_API_KEY\"\ninput = [\"text\", \"image\"]\n\n[defaults]\nidle_timeout = \"15m\"\n";

#[tokio::test]
async fn agents_naming_routes_name_model_ids_and_the_template_upgrade_merges() {
    let dir = tempfile::tempdir().unwrap();
    let tpl = dir.path().join("template");
    common::copy_dir(Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../../template")), &tpl);
    std::fs::write(tpl.join("botler.toml"), ROUTES_TOML).unwrap();
    let chat = std::fs::read_to_string(tpl.join("agents/chat/agent.toml")).unwrap();
    std::fs::write(
        tpl.join("agents/chat/agent.toml"),
        chat.replacen("idle_timeout", "model = \"standard\"\nidle_timeout", 1),
    )
    .unwrap();
    let paths = Paths::new(dir.path().join("data"));
    paths.ensure().unwrap();
    let repo = botler_repo::Repo::new(paths.repo(), paths.worktrees());
    repo.init_from(&tpl).await.unwrap();
    // An agent-written definition on the smart route.
    let base = repo.main().await.unwrap();
    let looker = "version = 1\nkind = \"persistent\"\nmodel = \"smart\" # sees images\n";
    let head = repo.commit_files(&base, &[("agents/looker/agent.toml", Some(looker))], "looker").await.unwrap();
    repo.advance_main(&base, &head).await.unwrap();

    let hub = open(&paths).await;
    let main = hub.repo.main().await.unwrap();
    let read = |p: &'static str| {
        let (repo, main) = (&hub.repo, &main);
        async move { repo.read_file(main, p).await.unwrap().unwrap() }
    };
    assert_eq!(read("agents/chat/agent.toml").await, chat, "the default route's line is dropped, as in the template");
    assert!(
        read("agents/looker/agent.toml").await.contains("model = \"anthropic/claude-sonnet-latest\" # sees images")
    );
    let defs = &hub.applied().await.defs;
    assert_eq!(defs.model_for(&defs.agents["chat"].spec).unwrap().id, "deepseek/deepseek-v4.1-flash");

    // The template upgrade that replaces the routes merges without conflicts.
    let tree = botler_hub::template::embedded_tree(&hub.repo, &paths.worktrees()).await.unwrap();
    let up = botler_hub::template::upgrade(&hub, &tree).await.unwrap();
    assert!(matches!(up, botler_hub::template::Upgrade::Proposed { .. }), "{up:?}");
    drop(hub);
    // Nothing left to migrate on the next boot.
    let hub = open(&paths).await;
    assert_eq!(hub.repo.main().await.unwrap(), main);
    std::fs::remove_dir_all(paths.run_dir().unwrap()).unwrap();
}
