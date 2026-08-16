use opscodex::{
    config::Config,
    runtime::{ThreadId, WorkspaceId},
    store::JsonlStore,
    workspace::WorkspaceCatalog,
};
use tempfile::tempdir;

#[test]
fn legacy_config_synthesizes_a_default_workspace() -> anyhow::Result<()> {
    let config = Config::from_toml(
        r#"
        [prometheus]
        url = "http://prometheus.local:9090"
        "#,
    )?;
    let catalog = WorkspaceCatalog::from_config(&config)?;
    let default = catalog.require(&WorkspaceId::default())?;
    assert_eq!(default.prometheus_url, "http://prometheus.local:9090");
    assert_eq!(default.environment, "local");
    let summary = serde_json::to_string(&default.summary())?;
    assert!(!summary.contains("token"));
    Ok(())
}

#[test]
fn workspace_entries_do_not_serialize_secret_values() -> anyhow::Result<()> {
    let config = Config::from_toml(
        r#"
        [[workspaces]]
        id = "staging"
        display_name = "Staging"
        environment = "staging"
        kubeconfig_env = "STAGING_KUBE_TOKEN"
        allowed_namespaces = ["checkout"]
        "#,
    )?;
    let catalog = WorkspaceCatalog::from_config(&config)?;
    let staging = catalog.require(&WorkspaceId::new("staging"))?;
    let encoded = serde_json::to_string(&staging.summary())?;
    assert!(encoded.contains("kubernetes"));
    assert!(!encoded.contains("STAGING_KUBE_TOKEN"));
    assert!(!encoded.contains("token"));
    assert_eq!(
        staging.kubernetes.as_ref().unwrap().kubeconfig_env,
        "STAGING_KUBE_TOKEN"
    );
    Ok(())
}

#[tokio::test]
async fn cross_workspace_thread_access_is_denied() -> anyhow::Result<()> {
    let directory = tempdir()?;
    let store = JsonlStore::new(directory.path()).await?;
    let thread_id = ThreadId::new();
    store
        .create_thread(thread_id.clone(), WorkspaceId::new("staging"))
        .await?;
    let error = store
        .get_thread_in(&WorkspaceId::new("production"), &thread_id)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("cross-workspace"));
    Ok(())
}

#[test]
fn invalid_workspace_ids_are_rejected() {
    assert!(WorkspaceId::new("").validate().is_err());
    assert!(WorkspaceId::new("prod/cluster").validate().is_err());
    assert!(WorkspaceId::new("ok-stage_1").validate().is_ok());
}
