use opscodex::config::Config;

#[test]
fn runtime_defaults_match_the_mvp_safety_limits() {
    let config = Config::default();

    assert_eq!(config.runtime.max_steps, 12);
    assert_eq!(config.runtime.max_concurrent_turns, 4);
    assert_eq!(config.runtime.tool_timeout_seconds, 30);
    assert_eq!(config.runtime.model_timeout_seconds, 120);
    assert_eq!(config.runtime.max_output_bytes, 64 * 1024);
    assert_eq!(config.loki.url, "http://localhost:3100");
    assert_eq!(config.tempo.url, "http://localhost:3200");
    assert!(!config.tools.exec);
}

#[test]
fn toml_overrides_selected_values_without_losing_defaults() -> anyhow::Result<()> {
    let config = Config::from_toml(
        r#"
        [model]
        model = "gpt-test"

        [prometheus]
        url = "http://prometheus:9090"

        [targets]
        allowed_hosts = ["order-service"]
        "#,
    )?;

    assert_eq!(config.model.model, "gpt-test");
    assert_eq!(config.prometheus.url, "http://prometheus:9090");
    assert_eq!(config.targets.allowed_hosts, ["order-service"]);
    assert_eq!(config.runtime.max_steps, 12);
    Ok(())
}

#[test]
fn zero_limits_are_rejected() {
    let error = Config::from_toml("[runtime]\nmax_steps = 0").unwrap_err();
    assert!(error.to_string().contains("max_steps"));
}

#[test]
fn reasoning_effort_is_optional_and_rejects_unknown_values() -> anyhow::Result<()> {
    assert_eq!(Config::default().model.reasoning_effort, None);

    let config = Config::from_toml("[model]\nreasoning_effort = \"none\"")?;
    assert_eq!(config.model.reasoning_effort.as_deref(), Some("none"));

    let error = Config::from_toml("[model]\nreasoning_effort = \"fast\"").unwrap_err();
    assert!(error.to_string().contains("reasoning_effort"));
    Ok(())
}

#[test]
fn workspace_ids_must_be_unique_and_valid() -> anyhow::Result<()> {
    let config = Config::from_toml(
        r#"
        [[workspaces]]
        id = "staging"
        kubeconfig_env = "STAGING_KUBECONFIG"
        "#,
    )?;
    assert_eq!(
        config.workspaces[0].kubeconfig_env.as_deref(),
        Some("STAGING_KUBECONFIG")
    );

    let duplicate = Config::from_toml(
        r#"
        [[workspaces]]
        id = "staging"
        [[workspaces]]
        id = "staging"
        "#,
    )
    .unwrap_err();
    assert!(duplicate.to_string().contains("duplicate workspace"));

    let invalid = Config::from_toml("[[workspaces]]\nid = \"prod/cluster\"").unwrap_err();
    assert!(invalid.to_string().contains("workspace id"));
    Ok(())
}

#[test]
fn extensions_and_skills_are_parsed_and_default_to_disabled_custom_tools() -> anyhow::Result<()> {
    assert!(!Config::default().extensions.allow_custom_tools);
    assert!(!Config::default().extensions.production_safe);

    let config = Config::from_toml(
        r#"
        [extensions]
        production_safe = true
        allow_custom_tools = true
        max_skill_context_bytes = 2048

        [[extension]]
        id = "acme-status"
        kind = "custom"
        trusted_local = true
        path = "/opt/acme/tool.yaml"
        effect = "observe"

        [[skills]]
        path = "skills/db-pool"
        workspaces = ["default"]
        "#,
    )?;
    assert!(config.extensions.production_safe);
    assert_eq!(config.extensions.max_skill_context_bytes, 2048);
    assert_eq!(config.extension[0].id, "acme-status");
    assert!(config.extension[0].enabled);
    assert_eq!(config.skills[0].path, "skills/db-pool");
    assert!(config.skills[0].enabled);

    let duplicate = Config::from_toml(
        r#"
        [[extension]]
        id = "dup"
        kind = "mcp"
        [[extension]]
        id = "dup"
        kind = "custom"
        "#,
    )
    .unwrap_err();
    assert!(duplicate.to_string().contains("duplicate extension"));
    Ok(())
}
