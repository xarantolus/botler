//! The host's own configuration: the optional config file, secret files, and the monorepo
//! `botler serve` creates on its first start.

mod common;

use std::net::SocketAddr;
use std::sync::Arc;

use botler_hub::Hub;
use botler_hub::config::{HubConfig, Paths};
use botler_hub::host::{self, HostConfig};
use common::env;

#[tokio::test]
async fn init_writes_the_mirror_into_the_new_monorepo() {
    let dir = tempfile::tempdir().unwrap();
    let paths = Paths::new(dir.path().join("data"));
    host::init(&paths, Some("git@example.org:me/monorepo.git")).await.unwrap();
    let hub = Hub::open(
        paths.clone(),
        HubConfig::new(SocketAddr::from(([127, 0, 0, 1], 0))),
        Arc::new(botler_runtime::fake::Fake::new()),
    )
    .await
    .unwrap();
    let mirror = hub.applied().await.defs.instance.mirror.clone().map(|m| m.url);
    assert_eq!(mirror.as_deref(), Some("git@example.org:me/monorepo.git"));
    let again = host::init(&paths, None).await.unwrap_err().to_string();
    assert!(again.contains("already holds a botler monorepo"), "{again}");
    if let Ok(run) = hub.paths.run_dir() {
        let _ = std::fs::remove_dir_all(run);
    }
}

#[tokio::test]
async fn secret_files_set_declared_secrets_and_reject_others() {
    let e = env().await;
    let dir = tempfile::tempdir().unwrap();
    assert!(host::import_secrets(&e.hub, &dir.path().join("missing")).await.unwrap().is_empty());

    std::fs::write(dir.path().join("OPENROUTER_API_KEY"), "sk-or-test\n").unwrap();
    std::fs::write(dir.path().join(".hidden"), "x").unwrap();
    assert_eq!(host::import_secrets(&e.hub, dir.path()).await.unwrap(), ["OPENROUTER_API_KEY"]);
    let value = e.hub.store.secret(&e.hub.secret_key, "OPENROUTER_API_KEY").await.unwrap();
    assert_eq!(value.as_deref(), Some("sk-or-test"));

    std::fs::write(dir.path().join("NOT_DECLARED"), "x").unwrap();
    let err = host::import_secrets(&e.hub, dir.path()).await.unwrap_err().to_string();
    assert!(err.contains("no secret NOT_DECLARED is declared"), "{err}");
}

#[test]
fn the_config_file_is_optional_and_strict() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    assert!(HostConfig::load(&path).unwrap().listen.is_none());
    std::fs::write(&path, "listen = \"0.0.0.0:7420\"\nmirror = \"git@example.org:m.git\"\n").unwrap();
    let c = HostConfig::load(&path).unwrap();
    assert_eq!(c.listen, Some(SocketAddr::from(([0, 0, 0, 0], 7420))));
    std::fs::write(&path, "lisen = \"0.0.0.0:7420\"\n").unwrap();
    let err = HostConfig::load(&path).unwrap_err().to_string();
    assert!(err.contains("unknown field `lisen`"), "{err}");
}

#[test]
fn the_service_unit_runs_this_binary_with_podman_on_its_path() {
    let unit = host::systemd_unit(std::path::Path::new("/home/me/apps/botler/bin/botler"));
    assert!(unit.contains("\nExecStart=/home/me/apps/botler/bin/botler serve\n"), "{unit}");
    assert!(unit.contains("\nEnvironment=PATH=/home/me/apps/botler/bin:%h/.cargo/bin:"), "{unit}");
    assert!(unit.contains("\nKillMode=mixed\n") && unit.contains("\nWantedBy=default.target\n"), "{unit}");
}
