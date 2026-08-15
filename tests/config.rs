use opscodex::config::Config;

#[test]
fn runtime_defaults_match_the_mvp_safety_limits() {
    let config = Config::default();

    assert_eq!(config.runtime.max_steps, 12);
    assert_eq!(config.runtime.max_concurrent_turns, 4);
    assert_eq!(config.runtime.tool_timeout_seconds, 30);
    assert_eq!(config.runtime.model_timeout_seconds, 120);
    assert_eq!(config.runtime.max_output_bytes, 64 * 1024);
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
