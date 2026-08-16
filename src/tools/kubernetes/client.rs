use std::time::Duration;

use serde::Deserialize;
use serde_json::Value;
use tokio_util::sync::CancellationToken;
use url::Url;

use crate::{ConnectorClass, OpsCodexError, Result, tools::connector, workspace::KubernetesScope};

pub const READ_VERBS: &[&str] = &["get", "list", "watch"];
const WRITE_VERBS: &[&str] = &[
    "create",
    "update",
    "patch",
    "delete",
    "deletecollection",
    "apply",
    "bind",
];
const FORBIDDEN_KINDS: &[&str] = &["Secret"];
const FORBIDDEN_SUBRESOURCES: &[&str] = &["exec", "attach", "portforward", "proxy", "eviction"];
const MAX_SELECTOR_CHARS: usize = 256;
const MAX_SELECTOR_TERMS: usize = 8;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KubernetesPolicy {
    pub cluster_alias: String,
    pub allowed_namespaces: Vec<String>,
    pub allowed_kinds: Vec<String>,
}

impl KubernetesPolicy {
    pub fn from_scope(scope: &KubernetesScope) -> Self {
        Self {
            cluster_alias: scope.cluster_alias.clone(),
            allowed_namespaces: scope.allowed_namespaces.clone(),
            allowed_kinds: scope.allowed_kinds.clone(),
        }
    }

    pub fn allows_kind(&self, kind: &str) -> bool {
        let kind = normalize_kind(kind);
        !FORBIDDEN_KINDS
            .iter()
            .any(|item| item.eq_ignore_ascii_case(&kind))
            && self
                .allowed_kinds
                .iter()
                .any(|allowed| allowed.eq_ignore_ascii_case(&kind))
    }

    pub fn allows_namespace(&self, namespace: &str) -> bool {
        self.allowed_namespaces
            .iter()
            .any(|allowed| allowed == namespace)
    }
}

pub struct KubernetesClient {
    client: reqwest::Client,
    base: Url,
    policy: KubernetesPolicy,
    bearer: Option<String>,
}

impl KubernetesClient {
    pub fn new(
        client: reqwest::Client,
        base_url: impl AsRef<str>,
        policy: KubernetesPolicy,
    ) -> Result<Self> {
        let mut base = Url::parse(base_url.as_ref())
            .map_err(|error| OpsCodexError::Tool(format!("invalid Kubernetes API URL: {error}")))?;
        if !matches!(base.scheme(), "http" | "https") {
            return Err(OpsCodexError::Tool(
                "Kubernetes API URL must use http or https".into(),
            ));
        }
        if base.username() != "" || base.password().is_some() {
            return Err(OpsCodexError::Policy(
                "Kubernetes API URL cannot contain credentials".into(),
            ));
        }
        base.set_query(None);
        base.set_fragment(None);
        Ok(Self {
            client,
            base,
            policy,
            bearer: None,
        })
    }

    pub fn from_scope(client: reqwest::Client, scope: &KubernetesScope) -> Result<Self> {
        let configured = std::env::var(&scope.kubeconfig_env).map_err(|_| {
            OpsCodexError::connector(
                ConnectorClass::Auth,
                format!("environment variable {} is not set", scope.kubeconfig_env),
            )
        })?;
        if configured.starts_with("http://") || configured.starts_with("https://") {
            return Self::new(client, configured, KubernetesPolicy::from_scope(scope));
        }
        load_kubeconfig(client, &configured, scope)
    }

    pub fn with_bearer(mut self, token: impl Into<String>) -> Self {
        self.bearer = Some(token.into());
        self
    }

    pub fn cluster_alias(&self) -> &str {
        &self.policy.cluster_alias
    }

    pub fn policy(&self) -> &KubernetesPolicy {
        &self.policy
    }

    pub fn rejects_write_verb(verb: &str) -> bool {
        WRITE_VERBS
            .iter()
            .any(|item| item.eq_ignore_ascii_case(verb))
    }

    pub async fn get_resource(
        &self,
        kind: &str,
        namespace: Option<&str>,
        name: Option<&str>,
        label_selector: Option<&str>,
        limit: u32,
        cancellation: CancellationToken,
    ) -> Result<Value> {
        self.ensure_get()?;
        let kind = normalize_kind(kind);
        self.authorize(&kind, namespace)?;
        if let Some(selector) = label_selector {
            validate_selector(selector)?;
        }
        let path = resource_path(&kind, namespace, name)?;
        let mut url = self.join(&path)?;
        {
            let mut pairs = url.query_pairs_mut();
            if name.is_none() {
                pairs.append_pair("limit", &limit.clamp(1, 50).to_string());
            }
            if let Some(selector) = label_selector {
                pairs.append_pair("labelSelector", selector);
            }
        }
        let mut object = self.send_get(url, cancellation).await?;
        sanitize_object(&mut object);
        Ok(object)
    }

    pub async fn list_events(
        &self,
        namespace: &str,
        involved_kind: Option<&str>,
        involved_name: Option<&str>,
        reason: Option<&str>,
        limit: u32,
        cancellation: CancellationToken,
    ) -> Result<Value> {
        self.ensure_get()?;
        self.authorize("Event", Some(namespace))?;
        let mut url = self.join(&format!("/api/v1/namespaces/{namespace}/events"))?;
        let mut selectors = Vec::new();
        if let Some(kind) = involved_kind {
            selectors.push(format!("involvedObject.kind={}", normalize_kind(kind)));
        }
        if let Some(name) = involved_name {
            selectors.push(format!("involvedObject.name={name}"));
        }
        if let Some(reason) = reason {
            selectors.push(format!("reason={reason}"));
        }
        {
            let mut pairs = url.query_pairs_mut();
            pairs.append_pair("limit", &limit.clamp(1, 50).to_string());
            if !selectors.is_empty() {
                pairs.append_pair("fieldSelector", &selectors.join(","));
            }
        }
        let mut object = self.send_get(url, cancellation).await?;
        sanitize_object(&mut object);
        Ok(object)
    }

    pub async fn pod_logs(
        &self,
        namespace: &str,
        pod: &str,
        container: Option<&str>,
        tail_lines: u32,
        since_seconds: Option<u64>,
        cancellation: CancellationToken,
    ) -> Result<String> {
        self.ensure_get()?;
        self.authorize("Pod", Some(namespace))?;
        let mut url = self.join(&format!("/api/v1/namespaces/{namespace}/pods/{pod}/log"))?;
        {
            let mut pairs = url.query_pairs_mut();
            pairs.append_pair("timestamps", "true");
            pairs.append_pair("tailLines", &tail_lines.clamp(1, 200).to_string());
            if let Some(container) = container {
                pairs.append_pair("container", container);
            }
            if let Some(since) = since_seconds {
                pairs.append_pair("sinceSeconds", &since.to_string());
            }
        }
        let (bytes, _) = self.send_get_bytes(url, cancellation).await?;
        let (text, _) = crate::evidence::redact_text(&String::from_utf8_lossy(&bytes));
        Ok(text)
    }

    fn ensure_get(&self) -> Result<()> {
        if READ_VERBS.contains(&"get") {
            Ok(())
        } else {
            Err(OpsCodexError::Policy(
                "Kubernetes connector is not read-only".into(),
            ))
        }
    }

    fn authorize(&self, kind: &str, namespace: Option<&str>) -> Result<()> {
        if !self.policy.allows_kind(kind) {
            return Err(OpsCodexError::Policy(format!(
                "Kubernetes kind `{kind}` is not allowlisted in cluster {}",
                self.policy.cluster_alias
            )));
        }
        if matches!(kind, "Node") {
            return Ok(());
        }
        let namespace = namespace.ok_or_else(|| {
            OpsCodexError::Policy(format!("Kubernetes kind `{kind}` requires a namespace"))
        })?;
        if kind == "Namespace" {
            if !self.policy.allows_namespace(namespace) {
                return Err(OpsCodexError::Policy(format!(
                    "namespace `{namespace}` is outside workspace allowlist"
                )));
            }
            return Ok(());
        }
        if !self.policy.allows_namespace(namespace) {
            return Err(OpsCodexError::Policy(format!(
                "namespace `{namespace}` is outside workspace allowlist"
            )));
        }
        Ok(())
    }

    fn join(&self, path: &str) -> Result<Url> {
        if FORBIDDEN_SUBRESOURCES.iter().any(|item| {
            path.split('/')
                .any(|segment| segment.eq_ignore_ascii_case(item))
        }) {
            return Err(OpsCodexError::Policy(
                "Kubernetes subresource is not permitted".into(),
            ));
        }
        let mut url = self.base.clone();
        let prefix = url.path().trim_end_matches('/');
        let suffix = if path.starts_with('/') {
            path.to_owned()
        } else {
            format!("/{path}")
        };
        url.set_path(&format!("{prefix}{suffix}"));
        Ok(url)
    }

    async fn send_get(&self, url: Url, cancellation: CancellationToken) -> Result<Value> {
        let (bytes, _) = self.send_get_bytes(url, cancellation).await?;
        serde_json::from_slice(&bytes).map_err(|error| {
            OpsCodexError::connector(ConnectorClass::MalformedData, error.to_string())
        })
    }

    async fn send_get_bytes(
        &self,
        url: Url,
        cancellation: CancellationToken,
    ) -> Result<(Vec<u8>, bool)> {
        let mut request = self.client.get(url);
        if let Some(token) = &self.bearer {
            request = request.bearer_auth(token);
        }
        let response = tokio::select! {
            biased;
            _ = cancellation.cancelled() => return Err(OpsCodexError::Cancelled),
            _ = tokio::time::sleep(Duration::from_secs(15)) => {
                return Err(OpsCodexError::Timeout("kubernetes request timed out".into()));
            }
            response = request.send() => {
                response.map_err(|error| OpsCodexError::connector(ConnectorClass::Unavailable, error.to_string()))?
            }
        };
        let status = response.status();
        let (bytes, truncated) =
            crate::tools::read_bounded_body(response, cancellation, 64 * 1024).await?;
        if !status.is_success() {
            let body = String::from_utf8_lossy(&bytes);
            return Err(connector::http_status_error("kubernetes", status, &body));
        }
        Ok((bytes, truncated))
    }
}

fn load_kubeconfig(
    client: reqwest::Client,
    source: &str,
    scope: &KubernetesScope,
) -> Result<KubernetesClient> {
    let path = std::path::Path::new(source);
    let contents = if path.exists() {
        std::fs::read_to_string(path).map_err(|error| {
            OpsCodexError::connector(
                ConnectorClass::Auth,
                format!("kubeconfig read failed: {error}"),
            )
        })?
    } else {
        source.to_owned()
    };
    let parsed: KubeConfig = serde_yaml::from_str(&contents).map_err(|error| {
        OpsCodexError::connector(ConnectorClass::Auth, format!("invalid kubeconfig: {error}"))
    })?;
    let context_name = scope
        .context
        .clone()
        .or(parsed.current_context)
        .ok_or_else(|| {
            OpsCodexError::connector(ConnectorClass::Auth, "kubeconfig context missing")
        })?;
    let context = parsed
        .contexts
        .iter()
        .find(|item| item.name == context_name)
        .ok_or_else(|| {
            OpsCodexError::connector(
                ConnectorClass::Auth,
                format!("kubeconfig context `{context_name}` not found"),
            )
        })?;
    let cluster = parsed
        .clusters
        .iter()
        .find(|item| item.name == context.context.cluster)
        .ok_or_else(|| {
            OpsCodexError::connector(ConnectorClass::Auth, "kubeconfig cluster not found")
        })?;
    let user = parsed
        .users
        .iter()
        .find(|item| item.name == context.context.user)
        .ok_or_else(|| {
            OpsCodexError::connector(ConnectorClass::Auth, "kubeconfig user not found")
        })?;
    let mut kube = KubernetesClient::new(
        client,
        &cluster.cluster.server,
        KubernetesPolicy::from_scope(scope),
    )?;
    if let Some(token) = &user.user.token {
        kube = kube.with_bearer(token);
    }
    Ok(kube)
}

fn resource_path(kind: &str, namespace: Option<&str>, name: Option<&str>) -> Result<String> {
    let name_suffix = name.map(|name| format!("/{name}")).unwrap_or_default();
    match kind {
        "Namespace" => {
            let name = name
                .ok_or_else(|| OpsCodexError::Policy("k8s_get Namespace requires name".into()))?;
            Ok(format!("/api/v1/namespaces/{name}"))
        }
        "Node" => {
            let name =
                name.ok_or_else(|| OpsCodexError::Policy("k8s_get Node requires name".into()))?;
            Ok(format!("/api/v1/nodes/{name}"))
        }
        "Pod" => namespaced("/api/v1/namespaces", namespace, "pods", &name_suffix),
        "Service" => namespaced("/api/v1/namespaces", namespace, "services", &name_suffix),
        "Event" => namespaced("/api/v1/namespaces", namespace, "events", &name_suffix),
        "Deployment" => namespaced(
            "/apis/apps/v1/namespaces",
            namespace,
            "deployments",
            &name_suffix,
        ),
        "StatefulSet" => namespaced(
            "/apis/apps/v1/namespaces",
            namespace,
            "statefulsets",
            &name_suffix,
        ),
        "DaemonSet" => namespaced(
            "/apis/apps/v1/namespaces",
            namespace,
            "daemonsets",
            &name_suffix,
        ),
        "Job" => namespaced("/apis/batch/v1/namespaces", namespace, "jobs", &name_suffix),
        "EndpointSlice" => namespaced(
            "/apis/discovery.k8s.io/v1/namespaces",
            namespace,
            "endpointslices",
            &name_suffix,
        ),
        other => Err(OpsCodexError::Policy(format!(
            "Kubernetes kind `{other}` is not a discovered GVK"
        ))),
    }
}

fn namespaced(
    prefix: &str,
    namespace: Option<&str>,
    resource: &str,
    name_suffix: &str,
) -> Result<String> {
    let namespace = namespace.ok_or_else(|| {
        OpsCodexError::Policy(format!(
            "Kubernetes resource `{resource}` requires a namespace"
        ))
    })?;
    Ok(format!("{prefix}/{namespace}/{resource}{name_suffix}"))
}

fn normalize_kind(kind: &str) -> String {
    match kind.to_ascii_lowercase().as_str() {
        "namespaces" => "Namespace".into(),
        "pods" | "pod" => "Pod".into(),
        "services" | "svc" | "service" => "Service".into(),
        "deployments" | "deploy" | "deployment" => "Deployment".into(),
        "statefulsets" | "sts" | "statefulset" => "StatefulSet".into(),
        "daemonsets" | "ds" | "daemonset" => "DaemonSet".into(),
        "jobs" | "job" => "Job".into(),
        "nodes" | "node" => "Node".into(),
        "events" | "event" => "Event".into(),
        "endpointslices" | "endpointslice" => "EndpointSlice".into(),
        _ => kind.to_owned(),
    }
}

fn validate_selector(selector: &str) -> Result<()> {
    if selector.len() > MAX_SELECTOR_CHARS {
        return Err(OpsCodexError::Policy(
            "label selector exceeds workspace limit".into(),
        ));
    }
    let terms = selector
        .split(',')
        .filter(|item| !item.trim().is_empty())
        .count();
    if terms > MAX_SELECTOR_TERMS {
        return Err(OpsCodexError::Policy(
            "label selector has too many terms".into(),
        ));
    }
    Ok(())
}

fn sanitize_object(value: &mut Value) {
    match value {
        Value::Object(map) => {
            map.remove("data");
            map.remove("stringData");
            map.remove("managedFields");
            if let Some(Value::Object(metadata)) = map.get_mut("metadata") {
                metadata.remove("managedFields");
                if let Some(Value::Object(annotations)) = metadata.get_mut("annotations") {
                    annotations.retain(|key, _| {
                        let lower = key.to_ascii_lowercase();
                        !lower.contains("token")
                            && !lower.contains("secret")
                            && !lower.contains("password")
                            && !lower.contains(
                                "kubeadm.kubernetes.io/kube-apiserver.advertise-address.endpoint",
                            )
                    });
                }
            }
            if let Some(items) = map.get_mut("items").and_then(Value::as_array_mut) {
                for item in items {
                    sanitize_object(item);
                }
            }
            if let Some(spec) = map.get_mut("spec") {
                sanitize_object(spec);
            }
            if let Some(status) = map.get_mut("status") {
                sanitize_object(status);
            }
        }
        Value::Array(items) => {
            for item in items {
                sanitize_object(item);
            }
        }
        _ => {}
    }
}

#[derive(Deserialize)]
struct KubeConfig {
    #[serde(default)]
    clusters: Vec<NamedCluster>,
    #[serde(default)]
    contexts: Vec<NamedContext>,
    #[serde(default)]
    users: Vec<NamedUser>,
    #[serde(rename = "current-context")]
    current_context: Option<String>,
}

#[derive(Deserialize)]
struct NamedCluster {
    name: String,
    cluster: Cluster,
}

#[derive(Deserialize)]
struct Cluster {
    server: String,
}

#[derive(Deserialize)]
struct NamedContext {
    name: String,
    context: ContextRef,
}

#[derive(Deserialize)]
struct ContextRef {
    cluster: String,
    user: String,
}

#[derive(Deserialize)]
struct NamedUser {
    name: String,
    user: User,
}

#[derive(Deserialize)]
struct User {
    token: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_verbs_are_rejected() {
        for verb in WRITE_VERBS {
            assert!(KubernetesClient::rejects_write_verb(verb));
        }
        assert!(!KubernetesClient::rejects_write_verb("get"));
    }
}
