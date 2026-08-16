use std::net::{IpAddr, Ipv4Addr, SocketAddr};

use opscodex::{
    config::Config,
    ops::{self, CheckStatus},
    store::{EventStore, SqliteStore},
};
use tempfile::tempdir;

fn config_with_sqlite(path: &std::path::Path) -> Config {
    let mut config = Config::default();
    config.store.sqlite_path = Some(path.display().to_string());
    config
}

#[test]
fn loopback_bind_hosts_are_accepted_and_unspecified_is_rejected() {
    assert!(ops::deny_non_loopback_bind("127.0.0.1").is_ok());
    assert!(ops::deny_non_loopback_bind("localhost").is_ok());
    assert!(ops::deny_non_loopback_bind("::1").is_ok());
    assert!(ops::deny_non_loopback_bind("[::1]").is_ok());
    assert!(ops::deny_non_loopback_bind("0.0.0.0").is_err());
    assert!(ops::deny_non_loopback_bind("192.168.1.10").is_err());
}

#[test]
fn listen_addr_parses_ipv4_and_ipv6_loopback() -> anyhow::Result<()> {
    assert_eq!(
        ops::parse_listen_addr("127.0.0.1", 3000)?,
        SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 3000)
    );
    let ipv6 = ops::parse_listen_addr("::1", 3000)?;
    assert!(ipv6.ip().is_loopback());
    assert_eq!(ipv6.port(), 3000);
    assert!(ops::parse_listen_addr("0.0.0.0", 3000).is_err());
    Ok(())
}

#[tokio::test]
async fn doctor_is_degraded_when_sqlite_is_missing() -> anyhow::Result<()> {
    let directory = tempdir()?;
    let config = config_with_sqlite(&directory.path().join("state.sqlite3"));
    let report = ops::doctor(&config).await?;
    assert_eq!(report.status, CheckStatus::Degraded);
    assert!(report.is_ok());
    assert!(
        report
            .checks
            .iter()
            .any(|check| check.name == "store" && check.status == CheckStatus::Degraded)
    );
    Ok(())
}

#[tokio::test]
async fn doctor_reports_error_when_production_safe_enables_exec() -> anyhow::Result<()> {
    let directory = tempdir()?;
    let mut config = config_with_sqlite(&directory.path().join("state.sqlite3"));
    config.extensions.production_safe = true;
    config.tools.exec = true;
    let report = ops::doctor(&config).await?;
    assert_eq!(report.status, CheckStatus::Error);
    assert!(!report.is_ok());
    Ok(())
}

#[tokio::test]
async fn verify_backup_and_audit_round_trip_a_sqlite_store() -> anyhow::Result<()> {
    let directory = tempdir()?;
    let sqlite = directory.path().join("state.sqlite3");
    let config = config_with_sqlite(&sqlite);
    assert!(ops::verify_store(&config).await.is_err());

    let store = SqliteStore::open(sqlite.clone()).await?;
    store
        .create_thread(
            opscodex::runtime::ThreadId::new(),
            opscodex::runtime::WorkspaceId::default(),
        )
        .await?;
    drop(store);

    let detail = ops::verify_store(&config).await?;
    assert!(detail.contains("integrity ok"));
    assert!(detail.contains("1 thread"));

    let audit = ops::verify_audit(&config).await?;
    assert!(audit.contains("audit record"));

    let backup_dir = directory.path().join("backup");
    let backup = ops::backup_store(&config, &backup_dir).await?;
    assert!(backup.ends_with("state.sqlite3"));
    let restored = SqliteStore::open(backup.clone()).await?;
    restored.integrity_check().await?;
    assert_eq!(restored.list_threads().await?.len(), 1);
    assert!(ops::backup_store(&config, &backup).await.is_err());
    Ok(())
}
