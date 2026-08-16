use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;
use url::Url;

use crate::{
    ConnectorClass, OpsCodexError, Result,
    tools::{KubernetesClient, read_bounded_body},
};

#[derive(Clone, Debug, PartialEq)]
pub struct ExecutionOutcome {
    pub committed: bool,
    pub verified: bool,
    pub uncertain: bool,
    pub before: Value,
    pub after: Value,
    pub message: String,
}

pub async fn execute_demo_fault(
    client: &reqwest::Client,
    base_url: &str,
    dry_run: bool,
    cancellation: CancellationToken,
) -> Result<ExecutionOutcome> {
    let base = validate_loopback_url(base_url)?;
    let health = get_json(
        client,
        base.join("health").expect("health"),
        cancellation.clone(),
    )
    .await?;
    let before_mode = health
        .get("mode")
        .and_then(Value::as_str)
        .unwrap_or("unknown")
        .to_owned();
    if dry_run {
        return Ok(ExecutionOutcome {
            committed: false,
            verified: false,
            uncertain: false,
            before: health,
            after: json!({"would_set": "normal"}),
            message: format!("dry-run would POST /debug/fault/normal (current mode {before_mode})"),
        });
    }
    let reset = base
        .join("debug/fault/normal")
        .map_err(|error| OpsCodexError::Tool(format!("invalid demo fault URL: {error}")))?;
    let posted = match post_json(client, reset, cancellation.clone()).await {
        Ok(value) => value,
        Err(error) if matches!(error, OpsCodexError::Timeout(_) | OpsCodexError::Cancelled) => {
            return Ok(ExecutionOutcome {
                committed: false,
                verified: false,
                uncertain: true,
                before: health,
                after: json!({"error": error.to_string()}),
                message: "demo fault reset result is unknown".into(),
            });
        }
        Err(error) => return Err(error),
    };
    let after = get_json(client, base.join("health").expect("health"), cancellation).await?;
    let ok = after.get("status").and_then(Value::as_str) == Some("ok")
        && after.get("mode").and_then(Value::as_str) == Some("normal");
    Ok(ExecutionOutcome {
        committed: true,
        verified: ok,
        uncertain: false,
        before: health,
        after: json!({"reset": posted, "health": after}),
        message: if ok {
            "demo fault reset verified".into()
        } else {
            "demo fault reset did not reach healthy normal mode".into()
        },
    })
}

pub async fn read_k8s_snapshot(
    client: &KubernetesClient,
    kind: &str,
    namespace: &str,
    name: &str,
    cancellation: CancellationToken,
) -> Result<Value> {
    client
        .get_workload(kind, namespace, name, cancellation)
        .await
}

#[allow(clippy::too_many_arguments)]
pub async fn execute_k8s_scale(
    client: &KubernetesClient,
    kind: &str,
    namespace: &str,
    name: &str,
    replicas: u32,
    resource_version: &str,
    uid: &str,
    operation_id: &str,
    dry_run: bool,
    cancellation: CancellationToken,
) -> Result<ExecutionOutcome> {
    let before = client
        .get_workload(kind, namespace, name, cancellation.clone())
        .await?;
    let current_uid = metadata_string(&before, "uid");
    let current_rv = metadata_string(&before, "resourceVersion");
    if !dry_run && (current_uid != uid || current_rv != resource_version) {
        return Err(OpsCodexError::Policy(
            "kubernetes target changed; resourceVersion/UID precondition failed".into(),
        ));
    }
    let patch_rv = if resource_version.is_empty() {
        current_rv.as_str()
    } else {
        resource_version
    };
    match client
        .patch_replicas(
            kind,
            namespace,
            name,
            replicas,
            patch_rv,
            operation_id,
            dry_run,
            cancellation.clone(),
        )
        .await
    {
        Ok(patched) => {
            if dry_run {
                return Ok(ExecutionOutcome {
                    committed: false,
                    verified: false,
                    uncertain: false,
                    before,
                    after: patched,
                    message: format!("dry-run scale {kind}/{name} to {replicas}"),
                });
            }
            let after = client
                .get_workload(kind, namespace, name, cancellation)
                .await?;
            let desired = after
                .pointer("/spec/replicas")
                .and_then(Value::as_u64)
                .unwrap_or(0);
            let available = after
                .pointer("/status/availableReplicas")
                .and_then(Value::as_u64)
                .unwrap_or(0);
            let verified = desired == u64::from(replicas) && available == u64::from(replicas);
            Ok(ExecutionOutcome {
                committed: true,
                verified,
                uncertain: false,
                before,
                after,
                message: if verified {
                    format!("scaled {kind}/{name} to {replicas}")
                } else {
                    format!("scale of {kind}/{name} did not reach available replicas")
                },
            })
        }
        Err(error) if matches!(error, OpsCodexError::Timeout(_) | OpsCodexError::Cancelled) => {
            Ok(ExecutionOutcome {
                committed: false,
                verified: false,
                uncertain: true,
                before,
                after: json!({"error": error.to_string()}),
                message: "kubernetes scale result is unknown".into(),
            })
        }
        Err(error) => Err(error),
    }
}

fn validate_loopback_url(value: &str) -> Result<Url> {
    let mut url = Url::parse(value)
        .map_err(|error| OpsCodexError::Protocol(format!("invalid demo_fault_url: {error}")))?;
    if url.scheme() != "http" && url.scheme() != "https" {
        return Err(OpsCodexError::Policy(
            "demo fault URL must use http or https".into(),
        ));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(OpsCodexError::Policy(
            "demo fault URL cannot contain credentials".into(),
        ));
    }
    let host = url.host_str().unwrap_or_default();
    if !matches!(host, "127.0.0.1" | "localhost" | "::1") {
        return Err(OpsCodexError::Policy(
            "demo fault reset is limited to loopback hosts".into(),
        ));
    }
    if !url.path().ends_with('/') {
        url.set_path(&format!("{}/", url.path().trim_end_matches('/')));
    }
    Ok(url)
}

fn metadata_string(object: &Value, field: &str) -> String {
    object
        .pointer(&format!("/metadata/{field}"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

async fn get_json(
    client: &reqwest::Client,
    url: Url,
    cancellation: CancellationToken,
) -> Result<Value> {
    let request = client.get(url);
    send_json(request, cancellation).await
}

async fn post_json(
    client: &reqwest::Client,
    url: Url,
    cancellation: CancellationToken,
) -> Result<Value> {
    let request = client.post(url);
    send_json(request, cancellation).await
}

async fn send_json(
    request: reqwest::RequestBuilder,
    cancellation: CancellationToken,
) -> Result<Value> {
    let response = tokio::select! {
        biased;
        _ = cancellation.cancelled() => return Err(OpsCodexError::Cancelled),
        _ = tokio::time::sleep(std::time::Duration::from_secs(10)) => {
            return Err(OpsCodexError::Timeout("demo fault request timed out".into()));
        }
        response = request.send() => {
            response.map_err(|error| OpsCodexError::connector(ConnectorClass::Unavailable, error.to_string()))?
        }
    };
    let status = response.status();
    let (bytes, _) = read_bounded_body(response, cancellation, 16 * 1024).await?;
    if !status.is_success() {
        return Err(OpsCodexError::Tool(format!(
            "demo fault endpoint returned {status}"
        )));
    }
    serde_json::from_slice(&bytes)
        .map_err(|error| OpsCodexError::connector(ConnectorClass::MalformedData, error.to_string()))
}
