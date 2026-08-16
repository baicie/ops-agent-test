use opscodex::runbook::{RunbookCatalog, parse_runbook};

#[test]
fn runbook_parses_front_matter_and_hashes_content() -> anyhow::Result<()> {
    let runbook = parse_runbook(
        r#"---
id: order-service-db-pool
title: Order service DB pool exhaustion
services: [order-service]
signals: [db_pool_waiting]
tags: [database]
version: 1
---

Increase the pool only after checking downstream latency.
"#,
    )?;
    assert_eq!(runbook.meta.id, "order-service-db-pool");
    assert_eq!(runbook.meta.version, 1);
    assert_eq!(runbook.meta.hash.len(), 64);
    Ok(())
}

#[test]
fn runbook_catalog_search_and_path_traversal_are_enforced() -> anyhow::Result<()> {
    let mut catalog = RunbookCatalog::default();
    catalog.insert(parse_runbook(
        r#"---
id: order-service-db-pool
title: Order service DB pool exhaustion
services: [order-service]
signals: [database_pool_exhausted]
version: 1
---
body
"#,
    )?)?;
    let matches = catalog.search("pool", Some("order-service"));
    assert_eq!(matches[0].id, "order-service-db-pool");
    assert!(catalog.read("../secret", None).is_err());
    assert!(catalog.read("missing", None).is_err());
    Ok(())
}

#[test]
fn duplicate_runbook_versions_are_rejected() -> anyhow::Result<()> {
    let mut catalog = RunbookCatalog::default();
    let runbook = parse_runbook(
        r#"---
id: same
title: Same
version: 1
---
a
"#,
    )?;
    catalog.insert(runbook.clone())?;
    assert!(catalog.insert(runbook).is_err());
    Ok(())
}

#[test]
fn runbook_catalog_loads_workspace_directory() -> anyhow::Result<()> {
    let catalog = RunbookCatalog::load(Some(&std::path::PathBuf::from("runbooks")))?;
    let matches = catalog.search("pool", Some("order-service"));
    assert_eq!(matches[0].id, "order-service-db-pool");
    assert_eq!(matches[0].version, 1);
    assert_eq!(matches[0].hash.len(), 64);
    let read = catalog.read("order-service-db-pool", Some(1))?;
    assert!(read.body.contains("never"));
    assert!(read.body.contains("executed automatically"));
    Ok(())
}
