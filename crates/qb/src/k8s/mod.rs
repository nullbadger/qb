use {
    anyhow::{
        Context,
        Result,
    },
    jiff::Timestamp,
    k8s_openapi::{
        api::{
            apps::v1::{
                DaemonSet,
                Deployment,
                ReplicaSet,
                StatefulSet,
            },
            autoscaling::v1::HorizontalPodAutoscaler,
            batch::v1::{
                CronJob,
                Job,
            },
            core::v1::{
                ConfigMap,
                Endpoints,
                Namespace,
                Node,
                PersistentVolume,
                PersistentVolumeClaim,
                Pod,
                Secret,
                Service,
                ServiceAccount,
            },
            events::v1::Event,
            networking::v1::{
                Ingress,
                NetworkPolicy,
            },
            rbac::v1::{
                ClusterRole,
                ClusterRoleBinding,
                Role,
                RoleBinding,
            },
            storage::v1::StorageClass,
        },
        apiextensions_apiserver::pkg::apis::apiextensions::v1::CustomResourceDefinition,
        apimachinery::pkg::apis::meta::v1::Time,
    },
    kube::{
        api::{
            Api,
            ListParams,
        },
        config::{
            KubeConfigOptions,
            Kubeconfig,
        },
        Client,
    },
    serde_json::Value,
    std::time::Duration,
};

const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);

fn shell_quote(value: &str) -> String {
    if value.is_empty()
        || value
            .chars()
            .any(|c| !(c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '/' | ':' | '=' | '@' | '+')))
    {
        format!("'{}'", value.replace('\'', "'\\''"))
    } else {
        value.to_string()
    }
}

fn sanitize_terminal_text(message: &str, max_chars: usize) -> String {
    let mut out = String::new();
    let mut chars = message.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\u{1b}' {
            if chars.peek() == Some(&'[') {
                chars.next();
                for next in chars.by_ref() {
                    if next.is_ascii_alphabetic() {
                        break;
                    }
                }
            }
            continue;
        }
        if ch.is_control() {
            out.push(' ');
        } else {
            out.push(ch);
        }
        if out.chars().count() >= max_chars {
            break;
        }
    }
    let collapsed = out.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() > max_chars {
        collapsed.chars().take(max_chars).collect()
    } else {
        collapsed
    }
}

/// Why a cluster call failed. Auth is a typed kube failure, not a text search.
#[derive(Clone, Debug)]
pub enum ClusterFailure {
    /// Exec plugin failed, or the apiserver rejected the token (401 /
    /// Unauthorized).
    AuthExpired {
        detail: String,
    },
    Other {
        detail: String,
    },
}

impl ClusterFailure {
    pub fn other(detail: impl Into<String>) -> Self {
        Self::Other {
            detail: sanitize_terminal_text(&detail.into(), 240),
        }
    }

    pub fn from_anyhow(err: anyhow::Error) -> Self {
        Self::classify(auth_in_chain(&err), sanitize_terminal_text(&format!("{err:#}"), 240))
    }

    pub fn from_kube(err: &kube::Error) -> Self {
        Self::classify(kube_is_auth(err), sanitize_terminal_text(&err.to_string(), 240))
    }

    pub fn is_auth(&self) -> bool {
        matches!(self, Self::AuthExpired { .. })
    }

    pub fn detail(&self) -> &str {
        match self {
            | Self::AuthExpired { detail } | Self::Other { detail } => detail,
        }
    }

    pub fn context(self, prefix: &str) -> Self {
        let detail = sanitize_terminal_text(&format!("{prefix}: {}", self.detail()), 240);
        match self {
            | Self::AuthExpired { .. } => Self::AuthExpired { detail },
            | Self::Other { .. } => Self::Other { detail },
        }
    }

    fn classify(auth: bool, detail: String) -> Self {
        if auth {
            Self::AuthExpired { detail }
        } else {
            Self::Other { detail }
        }
    }
}

fn auth_in_chain(err: &anyhow::Error) -> bool {
    err.chain().any(|cause| {
        cause.downcast_ref::<kube::Error>().is_some_and(kube_is_auth)
            || cause.downcast_ref::<kube::client::AuthError>().is_some()
    })
}

fn kube_is_auth(err: &kube::Error) -> bool {
    match err {
        | kube::Error::Auth(_) => true,
        | kube::Error::Api(status) => is_unauthorized(status),
        | _ => false,
    }
}

fn is_unauthorized(status: &kube::core::Status) -> bool {
    status.code == 401 || status.reason == kube::core::response::reason::UNAUTHORIZED
}

fn apply_timeouts(config: &mut kube::Config) {
    config.connect_timeout = Some(REQUEST_TIMEOUT);
    config.read_timeout = Some(REQUEST_TIMEOUT);
    config.write_timeout = Some(REQUEST_TIMEOUT);
}

// ---------------------------------------------------------------------------
// Custom Resource Definition info (for dynamic CR support)
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct CrdInfo {
    pub group: String,
    pub version: String,
    pub kind: String,
    pub plural: String,
    pub scope: CrdScope,
    pub display_name: String,
    pub columns: Vec<CrdColumn>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CrdScope {
    Namespaced,
    Cluster,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct CrdColumn {
    pub name: String,
    pub json_path: String,
    pub type_: String,
}

impl CrdInfo {
    pub fn api_resource(&self) -> kube::api::ApiResource {
        kube::api::ApiResource {
            group: self.group.clone(),
            version: self.version.clone(),
            api_version: if self.group.is_empty() {
                self.version.clone()
            } else {
                format!("{}/{}", self.group, self.version)
            },
            kind: self.kind.clone(),
            plural: self.plural.clone(),
        }
    }

    pub fn is_cluster_scoped(&self) -> bool {
        self.scope == CrdScope::Cluster
    }

    pub fn column_headers(&self) -> Vec<String> {
        let mut headers = vec!["NAME".to_string()];
        for col in &self.columns {
            headers.push(col.name.to_uppercase());
        }
        headers.push("AGE".to_string());
        headers
    }

    pub fn matches_gvk(&self, api_version: &str, kind: &str) -> bool {
        self.kind == kind && self.api_resource().api_version == api_version
    }
}

// ---------------------------------------------------------------------------
// Resource type definitions
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ResourceType {
    // Workloads
    Deployment,
    StatefulSet,
    DaemonSet,
    ReplicaSet,
    Pod,
    CronJob,
    Job,
    HorizontalPodAutoscaler,
    // Config
    ConfigMap,
    Secret,
    // Network
    Service,
    Ingress,
    Endpoints,
    NetworkPolicy,
    // Storage
    PersistentVolumeClaim,
    PersistentVolume,
    StorageClass,
    // RBAC
    ServiceAccount,
    Role,
    RoleBinding,
    ClusterRole,
    ClusterRoleBinding,
    // Cluster
    Node,
    Namespace,
    Event,
    CustomResourceDefinition,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Category {
    Workloads,
    Config,
    Network,
    Storage,
    Rbac,
    Cluster,
}

impl ResourceType {
    pub fn display_name(&self) -> &'static str {
        match self {
            | Self::Deployment => "Deployments",
            | Self::StatefulSet => "StatefulSets",
            | Self::DaemonSet => "DaemonSets",
            | Self::ReplicaSet => "ReplicaSets",
            | Self::Pod => "Pods",
            | Self::CronJob => "CronJobs",
            | Self::Job => "Jobs",
            | Self::HorizontalPodAutoscaler => "HPAs",
            | Self::ConfigMap => "ConfigMaps",
            | Self::Secret => "Secrets",
            | Self::Service => "Services",
            | Self::Ingress => "Ingresses",
            | Self::Endpoints => "Endpoints",
            | Self::NetworkPolicy => "NetworkPolicies",
            | Self::PersistentVolumeClaim => "PVCs",
            | Self::PersistentVolume => "PVs",
            | Self::StorageClass => "StorageClasses",
            | Self::ServiceAccount => "ServiceAccounts",
            | Self::Role => "Roles",
            | Self::RoleBinding => "RoleBindings",
            | Self::ClusterRole => "ClusterRoles",
            | Self::ClusterRoleBinding => "ClusterRoleBindings",
            | Self::Node => "Nodes",
            | Self::Namespace => "Namespaces",
            | Self::Event => "Events",
            | Self::CustomResourceDefinition => "CRDs",
        }
    }

    pub fn singular_name(&self) -> &'static str {
        match self {
            | Self::Deployment => "deployment",
            | Self::StatefulSet => "statefulset",
            | Self::DaemonSet => "daemonset",
            | Self::ReplicaSet => "replicaset",
            | Self::Pod => "pod",
            | Self::CronJob => "cronjob",
            | Self::Job => "job",
            | Self::HorizontalPodAutoscaler => "hpa",
            | Self::ConfigMap => "configmap",
            | Self::Secret => "secret",
            | Self::Service => "service",
            | Self::Ingress => "ingress",
            | Self::Endpoints => "endpoints",
            | Self::NetworkPolicy => "networkpolicy",
            | Self::PersistentVolumeClaim => "pvc",
            | Self::PersistentVolume => "pv",
            | Self::StorageClass => "storageclass",
            | Self::ServiceAccount => "serviceaccount",
            | Self::Role => "role",
            | Self::RoleBinding => "rolebinding",
            | Self::ClusterRole => "clusterrole",
            | Self::ClusterRoleBinding => "clusterrolebinding",
            | Self::Node => "node",
            | Self::Namespace => "namespace",
            | Self::Event => "event",
            | Self::CustomResourceDefinition => "crd",
        }
    }

    pub fn from_singular_name(name: &str) -> Option<Self> {
        match name {
            | "deployment" => Some(Self::Deployment),
            | "statefulset" => Some(Self::StatefulSet),
            | "daemonset" => Some(Self::DaemonSet),
            | "replicaset" => Some(Self::ReplicaSet),
            | "pod" => Some(Self::Pod),
            | "cronjob" => Some(Self::CronJob),
            | "job" => Some(Self::Job),
            | "hpa" => Some(Self::HorizontalPodAutoscaler),
            | "configmap" => Some(Self::ConfigMap),
            | "secret" => Some(Self::Secret),
            | "service" => Some(Self::Service),
            | "ingress" => Some(Self::Ingress),
            | "endpoints" => Some(Self::Endpoints),
            | "networkpolicy" => Some(Self::NetworkPolicy),
            | "pvc" => Some(Self::PersistentVolumeClaim),
            | "pv" => Some(Self::PersistentVolume),
            | "storageclass" => Some(Self::StorageClass),
            | "serviceaccount" => Some(Self::ServiceAccount),
            | "role" => Some(Self::Role),
            | "rolebinding" => Some(Self::RoleBinding),
            | "clusterrole" => Some(Self::ClusterRole),
            | "clusterrolebinding" => Some(Self::ClusterRoleBinding),
            | "node" => Some(Self::Node),
            | "namespace" => Some(Self::Namespace),
            | "event" => Some(Self::Event),
            | "crd" | "customresourcedefinition" => Some(Self::CustomResourceDefinition),
            | _ => None,
        }
    }

    pub fn column_headers(&self) -> Vec<&'static str> {
        match self {
            | Self::Deployment => vec!["NAME", "READY", "UP-TO-DATE", "AVAILABLE", "AGE"],
            | Self::StatefulSet => vec!["NAME", "READY", "AGE"],
            | Self::DaemonSet => vec!["NAME", "DESIRED", "CURRENT", "READY", "UP-TO-DATE", "AGE"],
            | Self::ReplicaSet => vec!["NAME", "DESIRED", "CURRENT", "READY", "AGE"],
            | Self::Pod => vec!["NAME", "READY", "STATUS", "RESTARTS", "AGE"],
            | Self::CronJob => vec!["NAME", "SCHEDULE", "SUSPEND", "ACTIVE", "AGE"],
            | Self::Job => vec!["NAME", "COMPLETIONS", "DURATION", "AGE"],
            | Self::HorizontalPodAutoscaler => vec!["NAME", "REFERENCE", "MIN", "MAX", "REPLICAS", "AGE"],
            | Self::ConfigMap => vec!["NAME", "DATA", "AGE"],
            | Self::Secret => vec!["NAME", "TYPE", "DATA", "AGE"],
            | Self::Service => vec!["NAME", "TYPE", "CLUSTER-IP", "PORT(S)", "AGE"],
            | Self::Ingress => vec!["NAME", "CLASS", "HOSTS", "ADDRESS", "AGE"],
            | Self::Endpoints => vec!["NAME", "ENDPOINTS", "AGE"],
            | Self::NetworkPolicy => vec!["NAME", "POD-SELECTOR", "AGE"],
            | Self::PersistentVolumeClaim => vec!["NAME", "STATUS", "VOLUME", "CAPACITY", "AGE"],
            | Self::PersistentVolume => vec!["NAME", "CAPACITY", "RECLAIM POLICY", "STATUS", "CLAIM", "AGE"],
            | Self::StorageClass => vec!["NAME", "PROVISIONER", "RECLAIM POLICY", "AGE"],
            | Self::ServiceAccount => vec!["NAME", "SECRETS", "AGE"],
            | Self::Role => vec!["NAME", "AGE"],
            | Self::RoleBinding => vec!["NAME", "ROLE", "AGE"],
            | Self::ClusterRole => vec!["NAME", "AGE"],
            | Self::ClusterRoleBinding => vec!["NAME", "ROLE", "AGE"],
            | Self::Node => vec!["NAME", "STATUS", "ROLES", "VERSION", "AGE"],
            | Self::Namespace => vec!["NAME", "STATUS", "AGE"],
            | Self::Event => vec!["NAME", "TYPE", "REASON", "OBJECT", "AGE"],
            | Self::CustomResourceDefinition => vec!["NAME", "GROUP", "VERSIONS", "SCOPE", "AGE"],
        }
    }

    pub fn supports_logs(&self) -> bool {
        matches!(
            self,
            Self::Deployment
                | Self::StatefulSet
                | Self::DaemonSet
                | Self::ReplicaSet
                | Self::Pod
                | Self::CronJob
                | Self::Job
        )
    }

    pub fn supports_scale(&self) -> bool {
        matches!(self, Self::Deployment | Self::StatefulSet | Self::ReplicaSet)
    }

    pub fn supports_exec(&self) -> bool {
        matches!(self, Self::Pod)
    }

    pub fn supports_restart(&self) -> bool {
        matches!(self, Self::Deployment | Self::StatefulSet | Self::DaemonSet)
    }

    pub fn supports_port_forward(&self) -> bool {
        matches!(
            self,
            Self::Service
                | Self::Deployment
                | Self::StatefulSet
                | Self::DaemonSet
                | Self::ReplicaSet
                | Self::Pod
                | Self::Job
                | Self::CronJob
        )
    }

    pub fn is_cluster_scoped(&self) -> bool {
        matches!(
            self,
            Self::Node
                | Self::Namespace
                | Self::PersistentVolume
                | Self::StorageClass
                | Self::ClusterRole
                | Self::ClusterRoleBinding
                | Self::CustomResourceDefinition
        )
    }

    /// Returns the kube `ApiResource` descriptor for dynamic API operations.
    pub fn api_resource(&self) -> kube::api::ApiResource {
        use kube::api::ApiResource;
        match self {
            | Self::Deployment => ApiResource::erase::<Deployment>(&()),
            | Self::StatefulSet => ApiResource::erase::<StatefulSet>(&()),
            | Self::DaemonSet => ApiResource::erase::<DaemonSet>(&()),
            | Self::ReplicaSet => ApiResource::erase::<ReplicaSet>(&()),
            | Self::Pod => ApiResource::erase::<Pod>(&()),
            | Self::CronJob => ApiResource::erase::<CronJob>(&()),
            | Self::Job => ApiResource::erase::<Job>(&()),
            | Self::HorizontalPodAutoscaler => ApiResource::erase::<HorizontalPodAutoscaler>(&()),
            | Self::ConfigMap => ApiResource::erase::<ConfigMap>(&()),
            | Self::Secret => ApiResource::erase::<Secret>(&()),
            | Self::Service => ApiResource::erase::<Service>(&()),
            | Self::Ingress => ApiResource::erase::<Ingress>(&()),
            | Self::Endpoints => ApiResource::erase::<Endpoints>(&()),
            | Self::NetworkPolicy => ApiResource::erase::<NetworkPolicy>(&()),
            | Self::PersistentVolumeClaim => ApiResource::erase::<PersistentVolumeClaim>(&()),
            | Self::PersistentVolume => ApiResource::erase::<PersistentVolume>(&()),
            | Self::StorageClass => ApiResource::erase::<StorageClass>(&()),
            | Self::ServiceAccount => ApiResource::erase::<ServiceAccount>(&()),
            | Self::Role => ApiResource::erase::<Role>(&()),
            | Self::RoleBinding => ApiResource::erase::<RoleBinding>(&()),
            | Self::ClusterRole => ApiResource::erase::<ClusterRole>(&()),
            | Self::ClusterRoleBinding => ApiResource::erase::<ClusterRoleBinding>(&()),
            | Self::Node => ApiResource::erase::<Node>(&()),
            | Self::Namespace => ApiResource::erase::<Namespace>(&()),
            | Self::Event => ApiResource::erase::<Event>(&()),
            | Self::CustomResourceDefinition => ApiResource::erase::<CustomResourceDefinition>(&()),
        }
    }

    pub fn all_by_category() -> Vec<(Category, Vec<ResourceType>)> {
        vec![
            (Category::Workloads, vec![
                Self::Deployment,
                Self::StatefulSet,
                Self::DaemonSet,
                Self::ReplicaSet,
                Self::Pod,
                Self::CronJob,
                Self::Job,
                Self::HorizontalPodAutoscaler,
            ]),
            (Category::Network, vec![
                Self::Service,
                Self::Ingress,
                Self::Endpoints,
                Self::NetworkPolicy,
            ]),
            (Category::Config, vec![Self::ConfigMap, Self::Secret]),
            (Category::Storage, vec![
                Self::PersistentVolumeClaim,
                Self::PersistentVolume,
                Self::StorageClass,
            ]),
            (Category::Rbac, vec![
                Self::ServiceAccount,
                Self::Role,
                Self::RoleBinding,
                Self::ClusterRole,
                Self::ClusterRoleBinding,
            ]),
            (Category::Cluster, vec![
                Self::Node,
                Self::Namespace,
                Self::Event,
                Self::CustomResourceDefinition,
            ]),
        ]
    }

    pub fn category(self) -> Category {
        match self {
            | Self::Deployment
            | Self::StatefulSet
            | Self::DaemonSet
            | Self::ReplicaSet
            | Self::Pod
            | Self::CronJob
            | Self::Job
            | Self::HorizontalPodAutoscaler => Category::Workloads,
            | Self::ConfigMap | Self::Secret => Category::Config,
            | Self::Service | Self::Ingress | Self::Endpoints | Self::NetworkPolicy => Category::Network,
            | Self::PersistentVolumeClaim | Self::PersistentVolume | Self::StorageClass => Category::Storage,
            | Self::ServiceAccount | Self::Role | Self::RoleBinding | Self::ClusterRole | Self::ClusterRoleBinding => {
                Category::Rbac
            },
            | Self::Node | Self::Namespace | Self::Event | Self::CustomResourceDefinition => Category::Cluster,
        }
    }

    /// Abbreviated label for compact tables (favorites, port-forward type
    /// column).
    pub fn short_label(self) -> &'static str {
        match self {
            | Self::Deployment => "Deploy",
            | Self::StatefulSet => "StatefulSet",
            | Self::DaemonSet => "DaemonSet",
            | Self::ReplicaSet => "ReplicaSet",
            | Self::Pod => "Pod",
            | Self::CronJob => "CronJob",
            | Self::Job => "Job",
            | Self::HorizontalPodAutoscaler => "HPA",
            | Self::ConfigMap => "ConfigMap",
            | Self::Secret => "Secret",
            | Self::Service => "Service",
            | Self::Ingress => "Ingress",
            | Self::Endpoints => "Endpoints",
            | Self::NetworkPolicy => "NetPolicy",
            | Self::PersistentVolumeClaim => "PVC",
            | Self::PersistentVolume => "PV",
            | Self::StorageClass => "StorageClass",
            | Self::ServiceAccount => "SA",
            | Self::Role => "Role",
            | Self::RoleBinding => "RoleBinding",
            | Self::ClusterRole => "ClusterRole",
            | Self::ClusterRoleBinding => "CRBinding",
            | Self::Node => "Node",
            | Self::Namespace => "Namespace",
            | Self::Event => "Event",
            | Self::CustomResourceDefinition => "CRD",
        }
    }

    /// Match a builtin type by the `apiVersion` and `kind` written in YAML.
    pub fn from_gvk(api_version: &str, kind: &str) -> Option<Self> {
        Self::all_by_category()
            .into_iter()
            .flat_map(|(_, types)| types)
            .find(|rt| {
                let ar = rt.api_resource();
                ar.kind == kind && ar.api_version == api_version
            })
    }
}

impl Category {
    pub fn display_name(&self) -> &'static str {
        match self {
            | Self::Workloads => "Workloads",
            | Self::Config => "Config",
            | Self::Network => "Network",
            | Self::Storage => "Storage",
            | Self::Rbac => "RBAC",
            | Self::Cluster => "Cluster",
        }
    }
}

// ---------------------------------------------------------------------------
// Resource kind — builtin type or a discovered custom resource.
// One value, so callers cannot pair a dummy ResourceType with a CrdInfo.
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum ResourceKind {
    Builtin(ResourceType),
    Custom(CrdInfo),
}

impl ResourceKind {
    pub fn builtin(&self) -> Option<ResourceType> {
        match self {
            | Self::Builtin(rt) => Some(*rt),
            | Self::Custom(_) => None,
        }
    }

    pub fn crd(&self) -> Option<&CrdInfo> {
        match self {
            | Self::Custom(crd) => Some(crd),
            | Self::Builtin(_) => None,
        }
    }

    pub fn display_name(&self) -> &str {
        match self {
            | Self::Builtin(rt) => rt.display_name(),
            | Self::Custom(crd) => crd.display_name.as_str(),
        }
    }

    pub fn column_headers(&self) -> Vec<String> {
        match self {
            | Self::Builtin(rt) => rt.column_headers().into_iter().map(str::to_string).collect(),
            | Self::Custom(crd) => crd.column_headers(),
        }
    }

    pub fn is_event(&self) -> bool {
        matches!(self.builtin(), Some(ResourceType::Event))
    }

    pub fn is_cluster_scoped(&self) -> bool {
        match self {
            | Self::Builtin(rt) => rt.is_cluster_scoped(),
            | Self::Custom(crd) => crd.is_cluster_scoped(),
        }
    }

    pub fn api_resource(&self) -> kube::api::ApiResource {
        match self {
            | Self::Builtin(rt) => rt.api_resource(),
            | Self::Custom(crd) => crd.api_resource(),
        }
    }

    pub fn favorite_key(&self) -> FavoriteType {
        match self {
            | Self::Builtin(rt) => FavoriteType::builtin(*rt),
            | Self::Custom(crd) => FavoriteType::custom(&crd.group, &crd.plural),
        }
    }
}

/// Resource identity as stored in config.
///
/// Builtins use `ResourceType::singular_name` (`deployment`). Custom resources
/// use `cr:{group}/{plural}` so a favorite survives without embedding the full
/// CRD. Unknown strings are preserved so a newer config is not dropped on save.
#[derive(Clone, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(transparent)]
pub struct FavoriteType(String);

impl FavoriteType {
    pub fn builtin(rt: ResourceType) -> Self {
        Self(rt.singular_name().to_string())
    }

    pub fn custom(group: &str, plural: &str) -> Self {
        Self(format!("cr:{group}/{plural}"))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn builtin_type(&self) -> Option<ResourceType> {
        ResourceType::from_singular_name(&self.0)
    }

    pub fn custom_ref(&self) -> Option<(&str, &str)> {
        self.0.strip_prefix("cr:")?.split_once('/')
    }

    pub fn short_label(&self) -> std::borrow::Cow<'_, str> {
        if let Some(rt) = self.builtin_type() {
            return std::borrow::Cow::Borrowed(rt.short_label());
        }
        if let Some((_, plural)) = self.custom_ref() {
            return std::borrow::Cow::Borrowed(plural);
        }
        std::borrow::Cow::Borrowed("Resource")
    }
}

impl std::fmt::Display for FavoriteType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

// ---------------------------------------------------------------------------
// Resource entry (one row in the table)
// ---------------------------------------------------------------------------

pub struct ResourceEntry {
    pub name: String,
    pub namespace: String,
    pub columns: Vec<String>,
    /// Optional sort key (e.g. ISO 8601 timestamp for Events). Not displayed.
    pub sort_key: Option<String>,
}

// ---------------------------------------------------------------------------
// Pod info (for log view pod/container selection)
// ---------------------------------------------------------------------------

pub struct PodInfo {
    pub name: String,
    pub containers: Vec<String>,
}

pub struct RelatedResource {
    pub resource_type: ResourceType,
    pub name: String,
    pub namespace: String,
    pub info: String,
    pub category: &'static str,
    pub sort_key: String,
}

pub struct RelatedEvent {
    pub type_: String,
    pub reason: String,
    pub message: String,
    pub count: i32,
    pub last_seen: String,
}

// ---------------------------------------------------------------------------
// Cluster stats
// ---------------------------------------------------------------------------

pub struct ResourceCapacity {
    pub capacity: String,
    pub allocatable: String,
}

pub struct ResourceUsage {
    pub cpu_cores: f64,
    pub memory_bytes: f64,
}

pub struct NodeStats {
    pub name: String,
    pub status: String,
    pub unschedulable: bool,
    pub roles: String,
    pub version: String,
    pub os_arch: String,
    pub cpu: ResourceCapacity,
    pub mem: ResourceCapacity,
    pub pods: ResourceCapacity,
    pub age: String,
    pub usage: Option<ResourceUsage>,
}

pub struct NodeCounts {
    pub total: usize,
    pub ready: usize,
    pub not_ready: usize,
    pub cordoned: usize,
}

pub struct PodCounts {
    pub total: usize,
    pub running: usize,
    pub pending: usize,
    pub succeeded: usize,
    pub failed: usize,
    pub crash_loop: usize,
    pub error: usize,
}

pub struct WorkloadHealth {
    pub deployments_ready: usize,
    pub deployments_total: usize,
    pub statefulsets_ready: usize,
    pub statefulsets_total: usize,
    pub daemonsets_ready: usize,
    pub daemonsets_desired: usize,
}

pub struct ContainerCounts {
    pub total: usize,
    pub ready: usize,
    pub total_restarts: i64,
}

pub struct ClusterResources {
    pub cpu_capacity: f64,
    pub cpu_allocatable: f64,
    pub mem_capacity_bytes: f64,
    pub mem_allocatable_bytes: f64,
    pub pod_capacity: usize,
}

pub struct TopRestartingPod {
    pub name: String,
    pub namespace: String,
    pub restart_count: i64,
}

pub struct WarningEventDetail {
    pub reason: String,
    pub message: String,
    pub namespace: String,
    pub count: i32,
}

pub struct PressureDetail {
    pub disk_pressure: usize,
    pub memory_pressure: usize,
    pub pid_pressure: usize,
}

// ---------------------------------------------------------------------------
// Metrics-server data (metrics.k8s.io/v1beta1)
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct NodeMetricsEntry {
    pub name: String,
    pub cpu_cores: f64,
    pub memory_bytes: f64,
}

#[derive(Clone, Default)]
pub struct PodResourceSpec {
    pub cpu_request: f64,
    pub cpu_limit: f64,
    pub mem_request: f64,
    pub mem_limit: f64,
}

#[derive(Clone)]
pub struct PodMetricsEntry {
    pub namespace: String,
    pub cpu_cores: f64,
    pub memory_bytes: f64,
    /// Aggregated from pod spec containers[].resources
    pub spec: PodResourceSpec,
    /// Top-level owner (Deployment, StatefulSet, DaemonSet, Job, etc.)
    pub owner: String,
    pub owner_kind: String,
}

#[derive(Clone)]
pub struct WorkloadMetrics {
    pub name: String,
    pub namespace: String,
    pub kind: String,
    pub pod_count: usize,
    pub cpu_usage: f64,
    pub mem_usage: f64,
    pub cpu_request: f64,
    pub cpu_limit: f64,
    pub mem_request: f64,
    pub mem_limit: f64,
}

#[derive(Clone)]
pub struct MetricsData {
    pub available: bool,
    pub node_metrics: Vec<NodeMetricsEntry>,
    pub pod_metrics: Vec<PodMetricsEntry>,
    pub workload_metrics: Vec<WorkloadMetrics>,
    /// Aggregate cluster usage (sum of all nodes)
    pub total_cpu_usage: f64,
    pub total_memory_usage: f64,
}

pub struct ClusterStatsData {
    pub server_version: String,
    pub nodes: NodeCounts,
    pub node_list: Vec<NodeStats>,
    pub namespace_count: usize,
    pub pods: PodCounts,
    pub deployment_count: usize,
    pub service_count: usize,
    pub recent_warnings: usize,
    pub statefulset_count: usize,
    pub daemonset_count: usize,
    pub job_count: usize,
    pub cronjob_count: usize,
    pub configmap_count: usize,
    pub secret_count: usize,
    pub pvc_count: usize,
    pub ingress_count: usize,
    pub hpa_count: usize,
    pub workload_health: WorkloadHealth,
    pub containers: ContainerCounts,
    pub cluster_resources: ClusterResources,
    pub top_restarting: Vec<TopRestartingPod>,
    pub warning_details: Vec<WarningEventDetail>,
    pub pressure: PressureDetail,
}

// ---------------------------------------------------------------------------
// Kubernetes client
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct KubeClients {
    /// Standard client with connect/read/write timeouts.
    pub default: Client,
    /// Client with only connect timeout — no read timeout so long-lived
    /// log follow streams aren't killed.
    pub streaming: Client,
}

#[derive(Clone)]
pub struct KubeconfigState {
    pub config: Kubeconfig,
    pub path: Option<String>,
}

#[derive(Clone)]
pub struct KubeContext {
    pub name: String,
    pub namespace: Option<String>,
}

#[derive(Clone)]
pub struct KubeClient {
    pub client: KubeClients,
    pub kubeconfig: KubeconfigState,
    pub context: KubeContext,
}

impl KubeClient {
    pub async fn new(
        kubeconfig_path: Option<String>,
        context_name: Option<String>,
        namespace: Option<String>,
    ) -> Result<Self> {
        // Always resolve the kubeconfig path so it can be persisted per profile
        let path = kubeconfig_path.or_else(|| {
            std::env::var("KUBECONFIG")
                .ok()
                .or_else(|| dirs::home_dir().map(|h| h.join(".kube/config").to_string_lossy().into_owned()))
        });
        let config = match &path {
            | Some(p) => Kubeconfig::read_from(p).context("Failed to read kubeconfig")?,
            | None => Kubeconfig::read().context("Failed to read default kubeconfig")?,
        };

        let name = context_name
            .or_else(|| config.current_context.clone())
            .unwrap_or_default();

        let options = KubeConfigOptions {
            context: Some(name.clone()),
            ..Default::default()
        };
        let client = Self::build_clients(&config, &options).await?;

        Ok(Self {
            client,
            kubeconfig: KubeconfigState { config, path },
            context: KubeContext { name, namespace },
        })
    }

    pub fn contexts(&self) -> Vec<String> {
        self.kubeconfig.config.contexts.iter().map(|c| c.name.clone()).collect()
    }

    async fn build_clients(kubeconfig: &Kubeconfig, options: &KubeConfigOptions) -> Result<KubeClients> {
        let mut silenced = kubeconfig.clone();
        Self::silence_exec_plugins(&mut silenced);
        let mut config = kube::Config::from_custom_kubeconfig(silenced.clone(), options)
            .await
            .context("Failed to build kube config from kubeconfig")?;
        apply_timeouts(&mut config);
        let default = Client::try_from(config)?;

        let mut stream_config = kube::Config::from_custom_kubeconfig(silenced, options)
            .await
            .context("Failed to build kube config from kubeconfig")?;
        stream_config.connect_timeout = Some(REQUEST_TIMEOUT);
        stream_config.read_timeout = None;
        stream_config.write_timeout = Some(REQUEST_TIMEOUT);
        let streaming = Client::try_from(stream_config)?;

        Ok(KubeClients { default, streaming })
    }

    /// Credential plugins (gcloud, gke-gcloud-auth-plugin) inherit this process
    /// when kube marks them interactive. Their prompts and ANSI errors are what
    /// tear down the alternate screen. Force non-interactive and drop stderr.
    /// The user's kubeconfig file is not modified.
    pub fn silence_exec_plugins(config: &mut Kubeconfig) {
        // Also covers the legacy gcp auth-provider command, which inherits this
        // process.
        std::env::set_var("CLOUDSDK_CORE_DISABLE_PROMPTS", "1");
        for named in &mut config.auth_infos {
            let Some(exec) = named.auth_info.as_mut().and_then(|info| info.exec.as_mut()) else {
                continue;
            };
            exec.interactive_mode = Some(kube::config::ExecInteractiveMode::Never);
            let Some(command) = exec.command.clone().filter(|cmd| !cmd.is_empty()) else {
                continue;
            };
            if command == "sh" {
                continue;
            }
            let mut parts = vec![shell_quote(&command)];
            if let Some(args) = &exec.args {
                parts.extend(args.iter().map(|arg| shell_quote(arg)));
            }
            // ponytail: sh redirect. kube still inherits stderr unless the child closes it.
            let script = format!("CLOUDSDK_CORE_DISABLE_PROMPTS=1 exec {} 2>/dev/null", parts.join(" "));
            exec.command = Some("sh".into());
            exec.args = Some(vec!["-c".into(), script]);
        }
    }

    /// One line, no ANSI, no controls. Safe to draw in the TUI.
    pub fn display_error(message: &str) -> String {
        sanitize_terminal_text(message, 240)
    }

    pub async fn switch_context(&mut self, ctx: &str) -> Result<()> {
        let options = KubeConfigOptions {
            context: Some(ctx.to_string()),
            ..Default::default()
        };
        self.client = Self::build_clients(&self.kubeconfig.config, &options).await?;
        self.context.namespace = None;
        self.context.name = ctx.to_string();
        Ok(())
    }

    pub async fn list_namespaces(&self) -> Result<Vec<String>> {
        let api: Api<Namespace> = Api::all(self.client.default.clone());
        let list = api.list(&ListParams::default()).await?;
        let mut names: Vec<String> = list.items.iter().filter_map(|ns| ns.metadata.name.clone()).collect();
        names.sort();
        Ok(names)
    }

    pub async fn fetch_cluster_stats_with_client(c: &Client) -> Result<ClusterStatsData> {
        let lp = ListParams::default();

        // Server version
        let server_version = match c.apiserver_version().await {
            | Ok(info) => format!("{}.{}", info.major, info.minor),
            | Err(_) => "unknown".into(),
        };

        // Parallel API calls
        let node_api: Api<Node> = Api::all(c.clone());
        let ns_api: Api<Namespace> = Api::all(c.clone());
        let pod_api: Api<Pod> = Api::all(c.clone());
        let dep_api: Api<Deployment> = Api::all(c.clone());
        let svc_api: Api<Service> = Api::all(c.clone());
        let sts_api: Api<StatefulSet> = Api::all(c.clone());
        let ds_api: Api<DaemonSet> = Api::all(c.clone());
        let job_api: Api<Job> = Api::all(c.clone());
        let cj_api: Api<CronJob> = Api::all(c.clone());
        let cm_api: Api<ConfigMap> = Api::all(c.clone());
        let secret_api: Api<Secret> = Api::all(c.clone());
        let pvc_api: Api<PersistentVolumeClaim> = Api::all(c.clone());
        let ing_api: Api<Ingress> = Api::all(c.clone());
        let hpa_api: Api<HorizontalPodAutoscaler> = Api::all(c.clone());
        let event_api: Api<Event> = Api::all(c.clone());

        let (
            node_res,
            ns_res,
            pod_res,
            dep_res,
            svc_res,
            sts_res,
            ds_res,
            job_res,
            cj_res,
            cm_res,
            secret_res,
            pvc_res,
            ing_res,
            hpa_res,
            event_res,
        ) = tokio::join!(
            node_api.list(&lp),
            ns_api.list(&lp),
            pod_api.list(&lp),
            dep_api.list(&lp),
            svc_api.list(&lp),
            sts_api.list(&lp),
            ds_api.list(&lp),
            job_api.list(&lp),
            cj_api.list(&lp),
            cm_api.list(&lp),
            secret_api.list(&lp),
            pvc_api.list(&lp),
            ing_api.list(&lp),
            hpa_api.list(&lp),
            event_api.list(&lp),
        );

        let node_list = node_res?;
        let pod_list = pod_res?;
        let dep_list = dep_res?;
        let sts_list = sts_res?;
        let ds_list = ds_res?;

        let ns_count = ns_res.map(|l| l.items.len()).unwrap_or(0);
        let service_count = svc_res.map(|l| l.items.len()).unwrap_or(0);
        let job_count = job_res.map(|l| l.items.len()).unwrap_or(0);
        let cronjob_count = cj_res.map(|l| l.items.len()).unwrap_or(0);
        let configmap_count = cm_res.map(|l| l.items.len()).unwrap_or(0);
        let secret_count = secret_res.map(|l| l.items.len()).unwrap_or(0);
        let pvc_count = pvc_res.map(|l| l.items.len()).unwrap_or(0);
        let ingress_count = ing_res.map(|l| l.items.len()).unwrap_or(0);
        let hpa_count = hpa_res.map(|l| l.items.len()).unwrap_or(0);

        // ── Nodes ────────────────────────────────────────────────
        let mut nodes_ready = 0usize;
        let mut nodes_not_ready = 0usize;
        let mut node_stats = Vec::new();
        let mut total_cpu_cap = 0.0f64;
        let mut total_cpu_alloc = 0.0f64;
        let mut total_mem_cap = 0.0f64;
        let mut total_mem_alloc = 0.0f64;
        let mut total_pod_cap = 0usize;
        let mut pressure = PressureDetail {
            disk_pressure: 0,
            memory_pressure: 0,
            pid_pressure: 0,
        };

        for n in &node_list.items {
            let status = n.status.as_ref();
            let conditions = status.and_then(|s| s.conditions.as_ref());
            let is_ready = conditions
                .and_then(|c| c.iter().find(|c| c.type_ == "Ready"))
                .map(|c| c.status == "True")
                .unwrap_or(false);
            if is_ready {
                nodes_ready += 1;
            } else {
                nodes_not_ready += 1;
            }

            // Pressure per type
            if let Some(conds) = conditions {
                for cond in conds {
                    if cond.status == "True" {
                        match cond.type_.as_str() {
                            | "DiskPressure" => pressure.disk_pressure += 1,
                            | "MemoryPressure" => pressure.memory_pressure += 1,
                            | "PIDPressure" => pressure.pid_pressure += 1,
                            | _ => {},
                        }
                    }
                }
            }

            let roles = n
                .metadata
                .labels
                .as_ref()
                .map(|labels| {
                    labels
                        .keys()
                        .filter_map(|k| {
                            k.strip_prefix("node-role.kubernetes.io/")
                                .map(|r| if r.is_empty() { "worker" } else { r })
                        })
                        .collect::<Vec<_>>()
                        .join(",")
                })
                .unwrap_or_default();

            let info = status.and_then(|s| s.node_info.as_ref());
            let capacity = status.and_then(|s| s.capacity.as_ref());
            let allocatable = status.and_then(|s| s.allocatable.as_ref());

            let get_res = |map: Option<
                &std::collections::BTreeMap<String, k8s_openapi::apimachinery::pkg::api::resource::Quantity>,
            >,
                           key: &str|
             -> String {
                map.and_then(|m| m.get(key))
                    .map(|q| q.0.clone())
                    .unwrap_or_else(|| "-".into())
            };

            // Aggregate cluster resources
            let cpu_cap_raw = get_res(capacity, "cpu");
            let cpu_alloc_raw = get_res(allocatable, "cpu");
            let mem_cap_raw = get_res(capacity, "memory");
            let mem_alloc_raw = get_res(allocatable, "memory");
            let pod_cap_raw = get_res(capacity, "pods");
            let pod_alloc_raw = get_res(allocatable, "pods");

            total_cpu_cap += parse_cpu_cores(&cpu_cap_raw);
            total_cpu_alloc += parse_cpu_cores(&cpu_alloc_raw);
            total_mem_cap += parse_memory_bytes(&mem_cap_raw);
            total_mem_alloc += parse_memory_bytes(&mem_alloc_raw);
            total_pod_cap += pod_cap_raw.parse::<usize>().unwrap_or(0);

            let unschedulable = n.spec.as_ref().and_then(|s| s.unschedulable).unwrap_or(false);
            let node_status = match (is_ready, unschedulable) {
                | (true, false) => "Ready".to_string(),
                | (true, true) => "Ready,SchedulingDisabled".to_string(),
                | (false, false) => "NotReady".to_string(),
                | (false, true) => "NotReady,SchedulingDisabled".to_string(),
            };

            node_stats.push(NodeStats {
                name: meta_name(&n.metadata),
                status: node_status,
                unschedulable,
                roles: if roles.is_empty() { "<none>".into() } else { roles },
                version: info.map(|i| i.kubelet_version.clone()).unwrap_or_default(),
                os_arch: info
                    .map(|i| format!("{}/{}", i.operating_system, i.architecture))
                    .unwrap_or_default(),
                cpu: ResourceCapacity {
                    capacity: cpu_cap_raw,
                    allocatable: cpu_alloc_raw,
                },
                mem: ResourceCapacity {
                    capacity: format_memory_gb(&mem_cap_raw),
                    allocatable: format_memory_gb(&mem_alloc_raw),
                },
                pods: ResourceCapacity {
                    capacity: pod_cap_raw,
                    allocatable: pod_alloc_raw,
                },
                age: format_age(n.metadata.creation_timestamp.as_ref()),
                usage: None, // Filled in by metrics fetch
            });
        }

        let nodes_cordoned = node_stats.iter().filter(|n| n.unschedulable).count();

        // ── Pods ─────────────────────────────────────────────────
        let pod_count = pod_list.items.len();
        let mut pods_running = 0usize;
        let mut pods_pending = 0usize;
        let mut pods_succeeded = 0usize;
        let mut pods_failed = 0usize;
        let mut pods_crash_loop = 0usize;
        let mut pods_error = 0usize;
        let mut total_containers = 0usize;
        let mut ready_containers = 0usize;
        let mut total_restarts = 0i64;
        let mut restart_pods: Vec<TopRestartingPod> = Vec::new();

        for p in &pod_list.items {
            match p.status.as_ref().and_then(|s| s.phase.as_deref()) {
                | Some("Running") => pods_running += 1,
                | Some("Pending") => pods_pending += 1,
                | Some("Succeeded") => pods_succeeded += 1,
                | Some("Failed") => pods_failed += 1,
                | _ => {},
            }
            let container_statuses = p.status.as_ref().and_then(|s| s.container_statuses.as_ref());
            if let Some(statuses) = container_statuses {
                let mut pod_restarts = 0i64;
                for cs in statuses {
                    total_containers += 1;
                    if cs.ready {
                        ready_containers += 1;
                    }
                    let r = cs.restart_count as i64;
                    total_restarts += r;
                    pod_restarts += r;
                    if let Some(waiting) = cs.state.as_ref().and_then(|s| s.waiting.as_ref()) {
                        match waiting.reason.as_deref() {
                            | Some("CrashLoopBackOff") => pods_crash_loop += 1,
                            | Some("ImagePullBackOff") | Some("ErrImagePull") | Some("CreateContainerConfigError") => {
                                pods_error += 1
                            },
                            | _ => {},
                        }
                    }
                }
                if pod_restarts > 0 {
                    restart_pods.push(TopRestartingPod {
                        name: meta_name(&p.metadata),
                        namespace: meta_ns(&p.metadata),
                        restart_count: pod_restarts,
                    });
                }
            }
            // Also count init containers
            if let Some(init_statuses) = p.status.as_ref().and_then(|s| s.init_container_statuses.as_ref()) {
                for cs in init_statuses {
                    total_containers += 1;
                    if cs.ready {
                        ready_containers += 1;
                    }
                }
            }
        }

        // Top 5 restarting pods
        restart_pods.sort_by(|a, b| b.restart_count.cmp(&a.restart_count));
        restart_pods.truncate(5);

        // ── Workload health ──────────────────────────────────────
        let deployment_count = dep_list.items.len();
        let deployments_ready = dep_list
            .items
            .iter()
            .filter(|d| {
                let desired = d.spec.as_ref().and_then(|s| s.replicas).unwrap_or(1);
                let available = d.status.as_ref().and_then(|s| s.available_replicas).unwrap_or(0);
                available >= desired
            })
            .count();

        let statefulset_count = sts_list.items.len();
        let statefulsets_ready = sts_list
            .items
            .iter()
            .filter(|s| {
                let desired = s.spec.as_ref().and_then(|sp| sp.replicas).unwrap_or(1);
                let ready = s.status.as_ref().and_then(|st| st.ready_replicas).unwrap_or(0);
                ready >= desired
            })
            .count();

        let daemonset_count = ds_list.items.len();
        let daemonsets_ready = ds_list
            .items
            .iter()
            .filter(|d| {
                let desired = d.status.as_ref().map(|s| s.desired_number_scheduled).unwrap_or(0);
                let ready = d.status.as_ref().map(|s| s.number_ready).unwrap_or(0);
                ready >= desired
            })
            .count();

        // ── Events ───────────────────────────────────────────────
        let (recent_warnings, warning_details) = if let Ok(events) = event_res {
            let cutoff = Timestamp::now() - jiff::SignedDuration::from_hours(1);
            let recent: Vec<_> = events
                .items
                .iter()
                .filter(|e| {
                    e.type_.as_deref() == Some("Warning")
                        && e.event_time
                            .as_ref()
                            .map(|t| t.0)
                            .or(e.deprecated_last_timestamp.as_ref().map(|t| t.0))
                            .map(|t| t >= cutoff)
                            .unwrap_or(false)
                })
                .collect();
            let count = recent.len();

            // Deduplicate by (reason, message), aggregate counts
            let mut event_map: std::collections::HashMap<(String, String), WarningEventDetail> =
                std::collections::HashMap::new();
            for e in &recent {
                let reason = e.reason.clone().unwrap_or_default();
                let msg = e.note.clone().unwrap_or_default();
                let ns = e.metadata.namespace.clone().unwrap_or_default();
                let key = (reason.clone(), msg.clone());
                let entry = event_map.entry(key).or_insert(WarningEventDetail {
                    reason,
                    message: msg,
                    namespace: ns,
                    count: 0,
                });
                entry.count += 1;
            }
            let mut details: Vec<_> = event_map.into_values().collect();
            details.sort_by(|a, b| b.count.cmp(&a.count));
            details.truncate(8);

            (count, details)
        } else {
            (0, Vec::new())
        };

        Ok(ClusterStatsData {
            server_version,
            nodes: NodeCounts {
                total: node_list.items.len(),
                ready: nodes_ready,
                not_ready: nodes_not_ready,
                cordoned: nodes_cordoned,
            },
            node_list: node_stats,
            namespace_count: ns_count,
            pods: PodCounts {
                total: pod_count,
                running: pods_running,
                pending: pods_pending,
                succeeded: pods_succeeded,
                failed: pods_failed,
                crash_loop: pods_crash_loop,
                error: pods_error,
            },
            deployment_count,
            service_count,
            recent_warnings,
            statefulset_count,
            daemonset_count,
            job_count,
            cronjob_count,
            configmap_count,
            secret_count,
            pvc_count,
            ingress_count,
            hpa_count,
            workload_health: WorkloadHealth {
                deployments_ready,
                deployments_total: deployment_count,
                statefulsets_ready,
                statefulsets_total: statefulset_count,
                daemonsets_ready,
                daemonsets_desired: daemonset_count,
            },
            containers: ContainerCounts {
                total: total_containers,
                ready: ready_containers,
                total_restarts,
            },
            cluster_resources: ClusterResources {
                cpu_capacity: total_cpu_cap,
                cpu_allocatable: total_cpu_alloc,
                mem_capacity_bytes: total_mem_cap,
                mem_allocatable_bytes: total_mem_alloc,
                pod_capacity: total_pod_cap,
            },
            top_restarting: restart_pods,
            warning_details,
            pressure,
        })
    }

    /// Fetch metrics from the metrics-server API (metrics.k8s.io/v1beta1).
    /// Returns `MetricsData { available: false, .. }` if metrics-server is not
    /// installed — never fails hard so the overview keeps working without it.
    pub async fn fetch_metrics_with_client(c: &Client) -> MetricsData {
        let empty = MetricsData {
            available: false,
            node_metrics: Vec::new(),
            pod_metrics: Vec::new(),
            workload_metrics: Vec::new(),
            total_cpu_usage: 0.0,
            total_memory_usage: 0.0,
        };

        let node_ar = kube::api::ApiResource {
            group: "metrics.k8s.io".into(),
            version: "v1beta1".into(),
            api_version: "metrics.k8s.io/v1beta1".into(),
            kind: "NodeMetrics".into(),
            plural: "nodes".into(),
        };
        let pod_ar = kube::api::ApiResource {
            group: "metrics.k8s.io".into(),
            version: "v1beta1".into(),
            api_version: "metrics.k8s.io/v1beta1".into(),
            kind: "PodMetrics".into(),
            plural: "pods".into(),
        };

        let node_api: Api<kube::api::DynamicObject> = Api::all_with(c.clone(), &node_ar);
        let pod_metrics_api: Api<kube::api::DynamicObject> = Api::all_with(c.clone(), &pod_ar);
        let pod_spec_api: Api<Pod> = Api::all(c.clone());

        let lp = ListParams::default();
        let (node_res, metrics_res, spec_res) =
            tokio::join!(node_api.list(&lp), pod_metrics_api.list(&lp), pod_spec_api.list(&lp),);

        let node_list = match node_res {
            | Ok(list) => list,
            | Err(_) => return empty,
        };
        let metrics_list = match metrics_res {
            | Ok(list) => list,
            | Err(_) => return empty,
        };
        let spec_list = spec_res.ok();

        // Build pod spec lookup: name+ns → (requests, limits, owner)
        let mut spec_map: std::collections::HashMap<(String, String), (PodResourceSpec, String, String)> =
            std::collections::HashMap::new();
        if let Some(pods) = &spec_list {
            for p in &pods.items {
                let name = meta_name(&p.metadata);
                let ns = meta_ns(&p.metadata);
                let mut spec = PodResourceSpec::default();

                if let Some(pod_spec) = &p.spec {
                    for container in &pod_spec.containers {
                        if let Some(res) = &container.resources {
                            if let Some(req) = &res.requests {
                                if let Some(cpu) = req.get("cpu") {
                                    spec.cpu_request += parse_cpu_cores(&cpu.0);
                                }
                                if let Some(mem) = req.get("memory") {
                                    spec.mem_request += parse_memory_bytes(&mem.0);
                                }
                            }
                            if let Some(lim) = &res.limits {
                                if let Some(cpu) = lim.get("cpu") {
                                    spec.cpu_limit += parse_cpu_cores(&cpu.0);
                                }
                                if let Some(mem) = lim.get("memory") {
                                    spec.mem_limit += parse_memory_bytes(&mem.0);
                                }
                            }
                        }
                    }
                }

                // Resolve owner — walk up: Pod → ReplicaSet → Deployment
                let (owner_name, owner_kind) = resolve_top_owner(p);

                spec_map.insert((name, ns), (spec, owner_name, owner_kind));
            }
        }

        // Node metrics
        let mut total_cpu = 0.0f64;
        let mut total_mem = 0.0f64;

        let node_metrics: Vec<NodeMetricsEntry> = node_list
            .items
            .iter()
            .map(|obj| {
                let name = obj.metadata.name.clone().unwrap_or_default();
                let usage = obj.data.get("usage");
                let cpu = usage
                    .and_then(|u| u.get("cpu"))
                    .and_then(|v| v.as_str())
                    .map(parse_cpu_cores)
                    .unwrap_or(0.0);
                let mem = usage
                    .and_then(|u| u.get("memory"))
                    .and_then(|v| v.as_str())
                    .map(parse_memory_bytes)
                    .unwrap_or(0.0);
                total_cpu += cpu;
                total_mem += mem;
                NodeMetricsEntry {
                    name,
                    cpu_cores: cpu,
                    memory_bytes: mem,
                }
            })
            .collect();

        // Pod metrics + spec enrichment
        let pod_metrics: Vec<PodMetricsEntry> = metrics_list
            .items
            .iter()
            .map(|obj| {
                let name = obj.metadata.name.clone().unwrap_or_default();
                let namespace = obj.metadata.namespace.clone().unwrap_or_default();
                let containers = obj.data.get("containers").and_then(|c| c.as_array());
                let (mut cpu, mut mem) = (0.0f64, 0.0f64);
                if let Some(cs) = containers {
                    for c in cs {
                        let usage = c.get("usage");
                        cpu += usage
                            .and_then(|u| u.get("cpu"))
                            .and_then(|v| v.as_str())
                            .map(parse_cpu_cores)
                            .unwrap_or(0.0);
                        mem += usage
                            .and_then(|u| u.get("memory"))
                            .and_then(|v| v.as_str())
                            .map(parse_memory_bytes)
                            .unwrap_or(0.0);
                    }
                }

                let key = (name.clone(), namespace.clone());
                let (spec, owner, owner_kind) = spec_map
                    .remove(&key)
                    .unwrap_or_else(|| (PodResourceSpec::default(), name.clone(), "Pod".into()));

                PodMetricsEntry {
                    namespace,
                    cpu_cores: cpu,
                    memory_bytes: mem,
                    spec,
                    owner,
                    owner_kind,
                }
            })
            .collect();

        // Workload-level aggregation
        let mut wl_map: std::collections::HashMap<(String, String, String), WorkloadMetrics> =
            std::collections::HashMap::new();
        for pm in &pod_metrics {
            let key = (pm.owner.clone(), pm.namespace.clone(), pm.owner_kind.clone());
            let wl = wl_map.entry(key).or_insert(WorkloadMetrics {
                name: pm.owner.clone(),
                namespace: pm.namespace.clone(),
                kind: pm.owner_kind.clone(),
                pod_count: 0,
                cpu_usage: 0.0,
                mem_usage: 0.0,
                cpu_request: 0.0,
                cpu_limit: 0.0,
                mem_request: 0.0,
                mem_limit: 0.0,
            });
            wl.pod_count += 1;
            wl.cpu_usage += pm.cpu_cores;
            wl.mem_usage += pm.memory_bytes;
            wl.cpu_request += pm.spec.cpu_request;
            wl.cpu_limit += pm.spec.cpu_limit;
            wl.mem_request += pm.spec.mem_request;
            wl.mem_limit += pm.spec.mem_limit;
        }
        let mut workload_metrics: Vec<WorkloadMetrics> = wl_map.into_values().collect();
        workload_metrics.sort_by(|a, b| {
            b.cpu_usage
                .partial_cmp(&a.cpu_usage)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        MetricsData {
            available: true,
            node_metrics,
            pod_metrics,
            workload_metrics,
            total_cpu_usage: total_cpu,
            total_memory_usage: total_mem,
        }
    }

    pub async fn list_resources(&self, rt: ResourceType) -> Result<Vec<ResourceEntry>> {
        match rt {
            | ResourceType::Deployment => self.list_typed::<Deployment>(Self::map_deployment).await,
            | ResourceType::StatefulSet => self.list_typed::<StatefulSet>(Self::map_statefulset).await,
            | ResourceType::DaemonSet => self.list_typed::<DaemonSet>(Self::map_daemonset).await,
            | ResourceType::ReplicaSet => self.list_typed::<ReplicaSet>(Self::map_replicaset).await,
            | ResourceType::Pod => self.list_typed::<Pod>(Self::map_pod).await,
            | ResourceType::CronJob => self.list_typed::<CronJob>(Self::map_cronjob).await,
            | ResourceType::Job => self.list_typed::<Job>(Self::map_job).await,
            | ResourceType::HorizontalPodAutoscaler => self.list_typed::<HorizontalPodAutoscaler>(Self::map_hpa).await,
            | ResourceType::ConfigMap => self.list_typed::<ConfigMap>(Self::map_configmap).await,
            | ResourceType::Secret => self.list_typed::<Secret>(Self::map_secret).await,
            | ResourceType::Service => self.list_typed::<Service>(Self::map_service).await,
            | ResourceType::Ingress => self.list_typed::<Ingress>(Self::map_ingress).await,
            | ResourceType::Endpoints => self.list_typed::<Endpoints>(Self::map_endpoints).await,
            | ResourceType::NetworkPolicy => self.list_typed::<NetworkPolicy>(Self::map_network_policy).await,
            | ResourceType::PersistentVolumeClaim => self.list_typed::<PersistentVolumeClaim>(Self::map_pvc).await,
            | ResourceType::PersistentVolume => self.list_cluster::<PersistentVolume>(Self::map_pv).await,
            | ResourceType::StorageClass => self.list_cluster::<StorageClass>(Self::map_storage_class).await,
            | ResourceType::ServiceAccount => self.list_typed::<ServiceAccount>(Self::map_service_account).await,
            | ResourceType::Role => self.list_typed::<Role>(Self::map_role).await,
            | ResourceType::RoleBinding => self.list_typed::<RoleBinding>(Self::map_role_binding).await,
            | ResourceType::ClusterRole => self.list_cluster::<ClusterRole>(Self::map_cluster_role).await,
            | ResourceType::ClusterRoleBinding => {
                self.list_cluster::<ClusterRoleBinding>(Self::map_cluster_role_binding)
                    .await
            },
            | ResourceType::Node => self.list_cluster::<Node>(Self::map_node).await,
            | ResourceType::Namespace => self.list_cluster::<Namespace>(Self::map_namespace).await,
            | ResourceType::Event => self.list_typed::<Event>(Self::map_event).await,
            | ResourceType::CustomResourceDefinition => {
                self.list_cluster::<CustomResourceDefinition>(Self::map_crd).await
            },
        }
    }

    pub async fn get_resource(&self, rt: ResourceType, ns: &str, name: &str) -> Result<Value> {
        match rt {
            // Cluster-scoped
            | ResourceType::PersistentVolume => self.get_value_cluster::<PersistentVolume>(name).await,
            | ResourceType::StorageClass => self.get_value_cluster::<StorageClass>(name).await,
            | ResourceType::ClusterRole => self.get_value_cluster::<ClusterRole>(name).await,
            | ResourceType::ClusterRoleBinding => self.get_value_cluster::<ClusterRoleBinding>(name).await,
            | ResourceType::Node => self.get_value_cluster::<Node>(name).await,
            | ResourceType::Namespace => self.get_value_cluster::<Namespace>(name).await,
            // Namespaced
            | ResourceType::Deployment => self.get_value::<Deployment>(ns, name).await,
            | ResourceType::StatefulSet => self.get_value::<StatefulSet>(ns, name).await,
            | ResourceType::DaemonSet => self.get_value::<DaemonSet>(ns, name).await,
            | ResourceType::ReplicaSet => self.get_value::<ReplicaSet>(ns, name).await,
            | ResourceType::Pod => self.get_value::<Pod>(ns, name).await,
            | ResourceType::CronJob => self.get_value::<CronJob>(ns, name).await,
            | ResourceType::Job => self.get_value::<Job>(ns, name).await,
            | ResourceType::HorizontalPodAutoscaler => self.get_value::<HorizontalPodAutoscaler>(ns, name).await,
            | ResourceType::ConfigMap => self.get_value::<ConfigMap>(ns, name).await,
            | ResourceType::Secret => self.get_value::<Secret>(ns, name).await,
            | ResourceType::Service => self.get_value::<Service>(ns, name).await,
            | ResourceType::Ingress => self.get_value::<Ingress>(ns, name).await,
            | ResourceType::Endpoints => self.get_value::<Endpoints>(ns, name).await,
            | ResourceType::NetworkPolicy => self.get_value::<NetworkPolicy>(ns, name).await,
            | ResourceType::PersistentVolumeClaim => self.get_value::<PersistentVolumeClaim>(ns, name).await,
            | ResourceType::ServiceAccount => self.get_value::<ServiceAccount>(ns, name).await,
            | ResourceType::Role => self.get_value::<Role>(ns, name).await,
            | ResourceType::RoleBinding => self.get_value::<RoleBinding>(ns, name).await,
            | ResourceType::Event => self.get_value::<Event>(ns, name).await,
            | ResourceType::CustomResourceDefinition => self.get_value_cluster::<CustomResourceDefinition>(name).await,
        }
    }

    // -- Edit / replace resource -----------------------------------------------

    fn dynamic_api(
        &self,
        ar: &kube::api::ApiResource,
        cluster_scoped: bool,
        ns: &str,
    ) -> Api<kube::api::DynamicObject> {
        if cluster_scoped {
            Api::all_with(self.client.default.clone(), ar)
        } else {
            Api::namespaced_with(self.client.default.clone(), ns, ar)
        }
    }

    /// Replace a resource with edited YAML. Uses the kube dynamic API so it
    /// works for any resource type without per-type match arms.
    pub async fn replace_kind_yaml(&self, kind: &ResourceKind, ns: &str, name: &str, yaml: &str) -> Result<Value> {
        let obj: kube::api::DynamicObject = serde_yaml::from_str(yaml).context("Invalid YAML")?;
        let ar = kind.api_resource();
        let api = self.dynamic_api(&ar, kind.is_cluster_scoped(), ns);
        let pp = kube::api::PostParams::default();
        let result = api.replace(name, &pp, &obj).await.context("Failed to apply resource")?;
        Ok(serde_json::to_value(&result)?)
    }

    pub async fn patch_kind_metadata(
        &self,
        kind: &ResourceKind,
        ns: &str,
        name: &str,
        labels: Option<&serde_json::Map<String, Value>>,
        annotations: Option<&serde_json::Map<String, Value>>,
    ) -> Result<Value> {
        let ar = kind.api_resource();
        let api = self.dynamic_api(&ar, kind.is_cluster_scoped(), ns);
        let mut metadata = serde_json::Map::new();
        if let Some(l) = labels {
            metadata.insert("labels".into(), Value::Object(l.clone()));
        }
        if let Some(a) = annotations {
            metadata.insert("annotations".into(), Value::Object(a.clone()));
        }
        let patch = serde_json::json!({ "metadata": metadata });
        let pp = kube::api::PatchParams::default();
        let result = api
            .patch(name, &pp, &kube::api::Patch::Merge(&patch))
            .await
            .context("Failed to patch metadata")?;
        let val = serde_json::to_value(&result)?;
        Ok(val)
    }

    pub async fn restart_workload(&self, rt: ResourceType, ns: &str, name: &str) -> Result<()> {
        let ar = rt.api_resource();
        let api: Api<kube::api::DynamicObject> = Api::namespaced_with(self.client.default.clone(), ns, &ar);
        let now = Timestamp::now().to_string();
        let patch = serde_json::json!({
            "spec": {
                "template": {
                    "metadata": {
                        "annotations": {
                            "kubectl.kubernetes.io/restartedAt": now
                        }
                    }
                }
            }
        });
        let pp = kube::api::PatchParams::default();
        api.patch(name, &pp, &kube::api::Patch::Merge(&patch))
            .await
            .context("Failed to restart workload")?;
        Ok(())
    }

    /// Create a resource from YAML.
    ///
    /// Builtin types and already-discovered CRDs supply the plural and scope.
    /// Anything else is resolved with a single discovery call — guessing
    /// `kind + "s"` sends Ingress, StorageClass, and most CRDs to the wrong
    /// URL, and always using the namespaced client rejects cluster-scoped
    /// creates.
    pub async fn create_resource_yaml(&self, yaml: &str, crds: &[CrdInfo]) -> Result<Value> {
        let value: Value = serde_yaml::from_str(yaml).context("Invalid YAML")?;

        let api_version = value.get("apiVersion").and_then(|v| v.as_str()).unwrap_or("");
        let kind = value.get("kind").and_then(|v| v.as_str()).unwrap_or("");
        let namespace = value
            .get("metadata")
            .and_then(|m| m.get("namespace"))
            .and_then(|n| n.as_str())
            .unwrap_or("");

        if kind.is_empty() || api_version.is_empty() {
            anyhow::bail!("YAML must contain apiVersion and kind");
        }

        let (ar, cluster_scoped) = if let Some(rt) = ResourceType::from_gvk(api_version, kind) {
            (rt.api_resource(), rt.is_cluster_scoped())
        } else if let Some(crd) = crds.iter().find(|c| c.matches_gvk(api_version, kind)) {
            (crd.api_resource(), crd.is_cluster_scoped())
        } else {
            let (group, version) = api_version.split_once('/').unwrap_or(("", api_version));
            let gvk = kube::api::GroupVersionKind::gvk(group, version, kind);
            let (ar, caps) = kube::discovery::pinned_kind(&self.client.default, &gvk)
                .await
                .with_context(|| format!("Failed to discover {kind} ({api_version})"))?;
            (ar, caps.scope == kube::discovery::Scope::Cluster)
        };

        let obj: kube::api::DynamicObject = serde_yaml::from_str(yaml).context("Invalid YAML for DynamicObject")?;
        let api = if cluster_scoped {
            Api::all_with(self.client.default.clone(), &ar)
        } else {
            let effective_ns = if namespace.is_empty() {
                self.context.namespace.as_deref().unwrap_or("default")
            } else {
                namespace
            };
            Api::namespaced_with(self.client.default.clone(), effective_ns, &ar)
        };

        let pp = kube::api::PostParams::default();
        let result = api.create(&pp, &obj).await.context("Failed to create resource")?;
        Ok(serde_json::to_value(&result)?)
    }

    pub async fn delete_kind(&self, kind: &ResourceKind, ns: &str, name: &str) -> Result<()> {
        let ar = kind.api_resource();
        let api = self.dynamic_api(&ar, kind.is_cluster_scoped(), ns);
        let dp = kube::api::DeleteParams::default();
        api.delete(name, &dp).await.context("Failed to delete resource")?;
        Ok(())
    }

    pub async fn list_kind(&self, kind: &ResourceKind) -> Result<Vec<ResourceEntry>> {
        match kind {
            | ResourceKind::Builtin(rt) => self.list_resources(*rt).await,
            | ResourceKind::Custom(crd) => self.list_custom_resources(crd).await,
        }
    }

    pub async fn get_kind(&self, kind: &ResourceKind, ns: &str, name: &str) -> Result<Value> {
        match kind {
            | ResourceKind::Builtin(rt) => self.get_resource(*rt, ns, name).await,
            | ResourceKind::Custom(crd) => self.get_custom_resource(crd, ns, name).await,
        }
    }

    pub async fn scale_resource(&self, rt: ResourceType, ns: &str, name: &str, replicas: u32) -> Result<()> {
        let ar = rt.api_resource();
        let api: Api<kube::api::DynamicObject> = Api::namespaced_with(self.client.default.clone(), ns, &ar);
        let patch = serde_json::json!({ "spec": { "replicas": replicas } });
        let pp = kube::api::PatchParams::default();
        api.patch(name, &pp, &kube::api::Patch::Merge(&patch))
            .await
            .context("Failed to scale resource")?;
        Ok(())
    }

    /// Generate a default job name for a manually triggered CronJob.
    pub fn default_trigger_job_name(cronjob_name: &str) -> String {
        let ts = Timestamp::now().strftime("%Y%m%d%H%M%S").to_string();
        format!("{}-manual-{}", cronjob_name, ts)
    }

    /// Create a Job from a CronJob's jobTemplate (manual trigger).
    pub async fn trigger_cronjob(&self, ns: &str, name: &str, job_name: &str) -> Result<String> {
        let cj_api: Api<CronJob> = Api::namespaced(self.client.default.clone(), ns);
        let cj = cj_api.get(name).await.context("Failed to get CronJob")?;

        let job_template = cj
            .spec
            .as_ref()
            .map(|s| &s.job_template)
            .ok_or_else(|| anyhow::anyhow!("CronJob has no spec"))?;

        let mut job_meta = job_template.metadata.clone().unwrap_or_default();
        job_meta.name = Some(job_name.to_string());
        job_meta.namespace = Some(ns.to_string());
        // Add owner reference so the job shows up as related
        job_meta.owner_references = Some(vec![k8s_openapi::apimachinery::pkg::apis::meta::v1::OwnerReference {
            api_version: "batch/v1".to_string(),
            kind: "CronJob".to_string(),
            name: name.to_string(),
            uid: cj.metadata.uid.clone().unwrap_or_default(),
            controller: Some(true),
            block_owner_deletion: Some(true),
        }]);
        // Annotate as manually triggered
        let annotations = job_meta.annotations.get_or_insert_with(Default::default);
        annotations.insert("cronjob.kubernetes.io/instantiate".to_string(), "manual".to_string());

        let job = Job {
            metadata: job_meta,
            spec: job_template.spec.clone(),
            ..Default::default()
        };

        let job_api: Api<Job> = Api::namespaced(self.client.default.clone(), ns);
        let pp = kube::api::PostParams::default();
        job_api
            .create(&pp, &job)
            .await
            .context("Failed to create Job from CronJob")?;

        Ok(job_name.to_string())
    }

    /// Cordon a node (mark as unschedulable).
    pub async fn cordon_node(&self, name: &str) -> Result<()> {
        let api: Api<Node> = Api::all(self.client.default.clone());
        let patch = serde_json::json!({ "spec": { "unschedulable": true } });
        let pp = kube::api::PatchParams::default();
        api.patch(name, &pp, &kube::api::Patch::Merge(&patch))
            .await
            .context("Failed to cordon node")?;
        Ok(())
    }

    /// Uncordon a node (mark as schedulable).
    pub async fn uncordon_node(&self, name: &str) -> Result<()> {
        let api: Api<Node> = Api::all(self.client.default.clone());
        let patch = serde_json::json!({ "spec": { "unschedulable": false } });
        let pp = kube::api::PatchParams::default();
        api.patch(name, &pp, &kube::api::Patch::Merge(&patch))
            .await
            .context("Failed to uncordon node")?;
        Ok(())
    }

    /// Drain a node: cordon it, then evict all non-DaemonSet, non-mirror pods.
    pub async fn drain_node(&self, name: &str) -> Result<usize> {
        // Step 1: cordon
        self.cordon_node(name).await?;

        // Step 2: list pods on this node
        let pod_api: Api<Pod> = Api::all(self.client.default.clone());
        let lp = ListParams::default().fields(&format!("spec.nodeName={}", name));
        let pods = pod_api.list(&lp).await.context("Failed to list pods on node")?;

        let mut evicted = 0;
        for pod in &pods.items {
            let meta = &pod.metadata;
            let pod_name = meta.name.as_deref().unwrap_or("");
            let ns = meta.namespace.as_deref().unwrap_or("default");

            // Skip DaemonSet-owned pods
            if let Some(owners) = &meta.owner_references {
                if owners.iter().any(|o| o.kind == "DaemonSet") {
                    continue;
                }
            }

            // Skip mirror pods (created by kubelet)
            if let Some(annotations) = &meta.annotations {
                if annotations.contains_key("kubernetes.io/config.mirror") {
                    continue;
                }
            }

            // Evict the pod
            let ns_api: Api<Pod> = Api::namespaced(self.client.default.clone(), ns);
            let ep = kube::api::EvictParams::default();
            match ns_api.evict(pod_name, &ep).await {
                | Ok(_) => evicted += 1,
                | Err(_) => {
                    // Continue — some pods may have PDBs blocking eviction
                },
            }
        }

        Ok(evicted)
    }

    pub async fn fetch_related_events(&self, ns: &str, resource_name: &str) -> Result<Vec<RelatedEvent>> {
        let api: Api<Event> = if ns.is_empty() {
            Api::all(self.client.default.clone())
        } else {
            Api::namespaced(self.client.default.clone(), ns)
        };
        let lp = ListParams::default().fields(&format!("regarding.name={}", resource_name));
        let list = api.list(&lp).await.context("Failed to fetch events")?;
        let mut events: Vec<RelatedEvent> = list
            .items
            .iter()
            .map(|e| {
                let last_seen = e
                    .event_time
                    .as_ref()
                    .map(|t| {
                        let dur = jiff::Timestamp::now().duration_since(t.0);
                        format_duration(dur)
                    })
                    .or_else(|| e.deprecated_last_timestamp.as_ref().map(|t| format_age(Some(t))))
                    .unwrap_or_else(|| "-".into());
                let count = e.series.as_ref().map(|s| s.count).or(e.deprecated_count).unwrap_or(1);
                RelatedEvent {
                    type_: e.type_.clone().unwrap_or_default(),
                    reason: e.reason.clone().unwrap_or_default(),
                    message: e.note.clone().unwrap_or_default(),
                    count,
                    last_seen,
                }
            })
            .collect();
        events.reverse();
        Ok(events)
    }

    pub async fn fetch_related_resources(
        &self,
        rt: ResourceType,
        ns: &str,
        name: &str,
        value: &Value,
    ) -> Vec<RelatedResource> {
        let mut related = Vec::new();

        // Owner references (universal — all resource types)
        Self::add_owner_refs(&mut related, value, ns);

        match rt {
            | ResourceType::Deployment => {
                self.add_replicasets_for_deployment(&mut related, ns, value).await;
                self.add_pods_by_selector(&mut related, ns, value).await;
                self.add_hpa_for_workload(&mut related, ns, name).await;
                Self::add_service_account(&mut related, value, ns, true);
            },
            | ResourceType::StatefulSet => {
                self.add_pods_by_selector(&mut related, ns, value).await;
                self.add_hpa_for_workload(&mut related, ns, name).await;
                Self::add_service_account(&mut related, value, ns, true);
                if let Some(templates) = value
                    .get("spec")
                    .and_then(|s| s.get("volumeClaimTemplates"))
                    .and_then(|v| v.as_array())
                {
                    for tpl in templates {
                        if let Some(pvc_name) = tpl.get("metadata").and_then(|m| m.get("name")).and_then(|n| n.as_str())
                        {
                            Self::push_unique(
                                &mut related,
                                ResourceType::PersistentVolumeClaim,
                                pvc_name,
                                ns,
                                "template",
                                "Storage",
                            );
                        }
                    }
                }
            },
            | ResourceType::DaemonSet => {
                self.add_pods_by_selector(&mut related, ns, value).await;
                Self::add_service_account(&mut related, value, ns, true);
            },
            | ResourceType::ReplicaSet => {
                self.add_pods_by_selector(&mut related, ns, value).await;
                Self::add_service_account(&mut related, value, ns, true);
            },
            | ResourceType::Pod => {
                Self::add_service_account(&mut related, value, ns, false);
                if let Some(node) = value
                    .get("spec")
                    .and_then(|s| s.get("nodeName"))
                    .and_then(|n| n.as_str())
                {
                    Self::push_unique(&mut related, ResourceType::Node, node, "", "scheduled", "Cluster");
                }
            },
            | ResourceType::Service => {
                Self::push_unique(&mut related, ResourceType::Endpoints, name, ns, "endpoints", "Network");
                self.add_pods_by_service_selector(&mut related, ns, value).await;
            },
            | ResourceType::Ingress => {
                if let Some(rules) = value
                    .get("spec")
                    .and_then(|s| s.get("rules"))
                    .and_then(|r| r.as_array())
                {
                    for rule in rules {
                        if let Some(paths) = rule.get("http").and_then(|h| h.get("paths")).and_then(|p| p.as_array()) {
                            for path in paths {
                                if let Some(svc) = path
                                    .get("backend")
                                    .and_then(|b| b.get("service"))
                                    .and_then(|s| s.get("name"))
                                    .and_then(|n| n.as_str())
                                {
                                    Self::push_unique(
                                        &mut related,
                                        ResourceType::Service,
                                        svc,
                                        ns,
                                        "backend",
                                        "Network",
                                    );
                                }
                            }
                        }
                    }
                }
                if let Some(tls) = value.get("spec").and_then(|s| s.get("tls")).and_then(|t| t.as_array()) {
                    for entry in tls {
                        if let Some(secret) = entry.get("secretName").and_then(|s| s.as_str()) {
                            Self::push_unique(&mut related, ResourceType::Secret, secret, ns, "tls", "Config");
                        }
                    }
                }
            },
            | ResourceType::Job => {
                self.add_pods_by_label(&mut related, ns, &format!("job-name={}", name))
                    .await;
                Self::add_service_account(&mut related, value, ns, true);
            },
            | ResourceType::CronJob => {
                let job_api: Api<Job> = Api::namespaced(self.client.default.clone(), ns);
                if let Ok(jobs) = job_api.list(&ListParams::default()).await {
                    for job in &jobs.items {
                        let is_owned = job
                            .metadata
                            .owner_references
                            .as_ref()
                            .map(|refs| refs.iter().any(|r| r.kind == "CronJob" && r.name == name))
                            .unwrap_or(false);
                        if is_owned {
                            let ts = job
                                .metadata
                                .creation_timestamp
                                .as_ref()
                                .map(|t| t.0.to_string())
                                .unwrap_or_default();
                            let status = if job.status.as_ref().and_then(|s| s.succeeded).unwrap_or(0) > 0 {
                                "Succeeded"
                            } else if job.status.as_ref().and_then(|s| s.active).unwrap_or(0) > 0 {
                                "Active"
                            } else {
                                "Unknown"
                            };
                            related.push(RelatedResource {
                                resource_type: ResourceType::Job,
                                name: meta_name(&job.metadata),
                                namespace: ns.to_string(),
                                info: status.to_string(),
                                category: ResourceType::Job.display_name(),
                                sort_key: ts,
                            });
                        }
                    }
                }
                // SA from jobTemplate
                if let Some(sa) = value
                    .get("spec")
                    .and_then(|s| s.get("jobTemplate"))
                    .and_then(|j| j.get("spec"))
                    .and_then(|s| s.get("template"))
                    .and_then(|t| t.get("spec"))
                    .and_then(|s| s.get("serviceAccountName"))
                    .and_then(|n| n.as_str())
                {
                    Self::push_unique(
                        &mut related,
                        ResourceType::ServiceAccount,
                        sa,
                        ns,
                        "serviceAccount",
                        "RBAC",
                    );
                }
            },
            | ResourceType::HorizontalPodAutoscaler => {
                if let Some(target_ref) = value.get("spec").and_then(|s| s.get("scaleTargetRef")) {
                    let kind = target_ref.get("kind").and_then(|k| k.as_str()).unwrap_or("");
                    let tname = target_ref.get("name").and_then(|n| n.as_str()).unwrap_or("");
                    let trt = match kind {
                        | "Deployment" => Some(ResourceType::Deployment),
                        | "StatefulSet" => Some(ResourceType::StatefulSet),
                        | "ReplicaSet" => Some(ResourceType::ReplicaSet),
                        | _ => None,
                    };
                    if let Some(trt) = trt {
                        Self::push_unique(&mut related, trt, tname, ns, "scale target", "Workloads");
                    }
                }
            },
            | ResourceType::PersistentVolumeClaim => {
                if let Some(vol) = value
                    .get("spec")
                    .and_then(|s| s.get("volumeName"))
                    .and_then(|v| v.as_str())
                {
                    Self::push_unique(
                        &mut related,
                        ResourceType::PersistentVolume,
                        vol,
                        "",
                        "bound",
                        "Storage",
                    );
                }
                if let Some(sc) = value
                    .get("spec")
                    .and_then(|s| s.get("storageClassName"))
                    .and_then(|v| v.as_str())
                {
                    Self::push_unique(
                        &mut related,
                        ResourceType::StorageClass,
                        sc,
                        "",
                        "storageClass",
                        "Storage",
                    );
                }
            },
            | ResourceType::PersistentVolume => {
                if let Some(claim) = value.get("spec").and_then(|s| s.get("claimRef")) {
                    let cn = claim.get("name").and_then(|n| n.as_str()).unwrap_or("");
                    let cns = claim.get("namespace").and_then(|n| n.as_str()).unwrap_or("");
                    if !cn.is_empty() {
                        Self::push_unique(
                            &mut related,
                            ResourceType::PersistentVolumeClaim,
                            cn,
                            cns,
                            "claim",
                            "Storage",
                        );
                    }
                }
                if let Some(sc) = value
                    .get("spec")
                    .and_then(|s| s.get("storageClassName"))
                    .and_then(|v| v.as_str())
                {
                    Self::push_unique(
                        &mut related,
                        ResourceType::StorageClass,
                        sc,
                        "",
                        "storageClass",
                        "Storage",
                    );
                }
            },
            | ResourceType::ServiceAccount => {
                // Token secrets
                if let Some(secrets) = value.get("secrets").and_then(|s| s.as_array()) {
                    for secret in secrets {
                        if let Some(sname) = secret.get("name").and_then(|n| n.as_str()) {
                            Self::push_unique(&mut related, ResourceType::Secret, sname, ns, "token", "x");
                        }
                    }
                }
                // RoleBindings referencing this SA
                let rb_api: Api<RoleBinding> = Api::namespaced(self.client.default.clone(), ns);
                if let Ok(list) = rb_api.list(&ListParams::default()).await {
                    for rb in &list.items {
                        let refs_sa = rb
                            .subjects
                            .as_ref()
                            .map(|subjects| subjects.iter().any(|s| s.kind == "ServiceAccount" && s.name == name))
                            .unwrap_or(false);
                        if refs_sa {
                            Self::push_unique(
                                &mut related,
                                ResourceType::RoleBinding,
                                &meta_name(&rb.metadata),
                                ns,
                                "binds SA",
                                "x",
                            );
                        }
                    }
                }
                // ClusterRoleBindings referencing this SA
                let crb_api: Api<ClusterRoleBinding> = Api::all(self.client.default.clone());
                if let Ok(list) = crb_api.list(&ListParams::default()).await {
                    for crb in &list.items {
                        let refs_sa = crb
                            .subjects
                            .as_ref()
                            .map(|subjects| {
                                subjects.iter().any(|s| {
                                    s.kind == "ServiceAccount" && s.name == name && s.namespace.as_deref() == Some(ns)
                                })
                            })
                            .unwrap_or(false);
                        if refs_sa {
                            Self::push_unique(
                                &mut related,
                                ResourceType::ClusterRoleBinding,
                                &meta_name(&crb.metadata),
                                "",
                                "binds SA",
                                "x",
                            );
                        }
                    }
                }
                // Workloads using this SA
                self.add_workloads_using_sa(&mut related, ns, name).await;
            },
            | ResourceType::RoleBinding | ResourceType::ClusterRoleBinding => {
                if let Some(role_ref) = value.get("roleRef") {
                    let kind = role_ref.get("kind").and_then(|k| k.as_str()).unwrap_or("");
                    let rname = role_ref.get("name").and_then(|n| n.as_str()).unwrap_or("");
                    let rrt = match kind {
                        | "Role" => Some(ResourceType::Role),
                        | "ClusterRole" => Some(ResourceType::ClusterRole),
                        | _ => None,
                    };
                    if let Some(rrt) = rrt {
                        Self::push_unique(&mut related, rrt, rname, ns, "bound role", "RBAC");
                    }
                }
                if let Some(subjects) = value.get("subjects").and_then(|s| s.as_array()) {
                    for subj in subjects {
                        if subj.get("kind").and_then(|k| k.as_str()) == Some("ServiceAccount") {
                            let sa_name = subj.get("name").and_then(|n| n.as_str()).unwrap_or("");
                            let sa_ns = subj.get("namespace").and_then(|n| n.as_str()).unwrap_or(ns);
                            Self::push_unique(
                                &mut related,
                                ResourceType::ServiceAccount,
                                sa_name,
                                sa_ns,
                                "subject",
                                "RBAC",
                            );
                        }
                    }
                }
            },
            | ResourceType::NetworkPolicy => {
                self.add_pods_by_network_policy_selector(&mut related, ns, value).await;
            },
            | ResourceType::Secret | ResourceType::ConfigMap => {
                self.add_workloads_referencing(&mut related, rt, ns, name).await;
            },
            | _ => {},
        }

        // Extract mounted volumes + env refs from pod templates
        let template_spec = match rt {
            | ResourceType::CronJob => {
                value
                    .get("spec")
                    .and_then(|s| s.get("jobTemplate"))
                    .and_then(|j| j.get("spec"))
                    .and_then(|s| s.get("template"))
                    .and_then(|t| t.get("spec"))
            },
            | ResourceType::Deployment
            | ResourceType::StatefulSet
            | ResourceType::DaemonSet
            | ResourceType::Job
            | ResourceType::ReplicaSet => {
                value
                    .get("spec")
                    .and_then(|s| s.get("template"))
                    .and_then(|t| t.get("spec"))
            },
            | ResourceType::Pod => value.get("spec"),
            | _ => None,
        };
        if let Some(spec) = template_spec {
            if let Some(volumes) = spec.get("volumes").and_then(|v| v.as_array()) {
                Self::extract_volume_refs(&mut related, volumes, ns);
            }
            if let Some(containers) = spec.get("containers").and_then(|c| c.as_array()) {
                Self::extract_env_refs(&mut related, containers, ns);
            }
            if let Some(init_containers) = spec.get("initContainers").and_then(|c| c.as_array()) {
                Self::extract_env_refs(&mut related, init_containers, ns);
            }
        }

        // Sort: by resource type category order, then by sort_key (timestamp)
        let type_order = |c: &str| -> u8 {
            match c {
                // Workloads
                | "Deployments" => 0,
                | "ReplicaSets" => 1,
                | "StatefulSets" => 2,
                | "DaemonSets" => 3,
                | "Pods" => 4,
                | "Jobs" => 5,
                | "CronJobs" => 6,
                | "HPAs" => 7,
                // Network
                | "Services" => 10,
                | "Ingresses" => 11,
                | "Endpoints" => 12,
                | "NetworkPolicies" => 13,
                // Config
                | "ConfigMaps" => 20,
                | "Secrets" => 21,
                // Storage
                | "PVCs" => 30,
                | "PVs" => 31,
                | "StorageClasses" => 32,
                // RBAC
                | "ServiceAccounts" => 40,
                | "Roles" => 41,
                | "RoleBindings" => 42,
                | "ClusterRoles" => 43,
                | "ClusterRoleBindings" => 44,
                // Cluster
                | "Nodes" => 50,
                | "Namespaces" => 51,
                | _ => 99,
            }
        };
        related.sort_by(|a, b| {
            type_order(a.category).cmp(&type_order(b.category)).then_with(|| {
                if a.sort_key.is_empty() && b.sort_key.is_empty() {
                    a.name.cmp(&b.name)
                } else {
                    b.sort_key.cmp(&a.sort_key)
                }
            })
        });

        related
    }

    fn add_owner_refs(related: &mut Vec<RelatedResource>, value: &Value, ns: &str) {
        if let Some(owners) = value
            .get("metadata")
            .and_then(|m| m.get("ownerReferences"))
            .and_then(|o| o.as_array())
        {
            for owner in owners {
                let kind = owner.get("kind").and_then(|k| k.as_str()).unwrap_or("");
                let oname = owner.get("name").and_then(|n| n.as_str()).unwrap_or("");
                let owner_rt = match kind {
                    | "Deployment" => Some(ResourceType::Deployment),
                    | "ReplicaSet" => Some(ResourceType::ReplicaSet),
                    | "StatefulSet" => Some(ResourceType::StatefulSet),
                    | "DaemonSet" => Some(ResourceType::DaemonSet),
                    | "Job" => Some(ResourceType::Job),
                    | "CronJob" => Some(ResourceType::CronJob),
                    | "Node" => Some(ResourceType::Node),
                    | _ => None,
                };
                if let Some(ort) = owner_rt {
                    related.push(RelatedResource {
                        resource_type: ort,
                        name: oname.to_string(),
                        namespace: ns.to_string(),
                        info: "owner".to_string(),
                        category: ort.display_name(),
                        sort_key: String::new(),
                    });
                }
            }
        }
    }

    async fn add_replicasets_for_deployment(&self, related: &mut Vec<RelatedResource>, ns: &str, value: &Value) {
        let selector = value
            .get("spec")
            .and_then(|s| s.get("selector"))
            .and_then(|s| s.get("matchLabels"))
            .and_then(|m| m.as_object());
        if let Some(labels) = selector {
            let sel = labels
                .iter()
                .map(|(k, v)| format!("{}={}", k, v.as_str().unwrap_or("")))
                .collect::<Vec<_>>()
                .join(",");
            let api: Api<ReplicaSet> = Api::namespaced(self.client.default.clone(), ns);
            if let Ok(list) = api.list(&ListParams::default().labels(&sel)).await {
                for rs in &list.items {
                    let rs_name = meta_name(&rs.metadata);
                    let replicas = rs.status.as_ref().map(|s| s.replicas).unwrap_or(0);
                    let ready = rs.status.as_ref().and_then(|s| s.ready_replicas).unwrap_or(0);
                    let revision = rs
                        .metadata
                        .annotations
                        .as_ref()
                        .and_then(|a| a.get("deployment.kubernetes.io/revision"))
                        .map(|s| s.as_str())
                        .unwrap_or("0");
                    let ts = rs
                        .metadata
                        .creation_timestamp
                        .as_ref()
                        .map(|t| t.0.to_string())
                        .unwrap_or_default();
                    related.push(RelatedResource {
                        resource_type: ResourceType::ReplicaSet,
                        name: rs_name,
                        namespace: ns.to_string(),
                        info: format!("rev:{} ready:{}/{}", revision, ready, replicas),
                        category: ResourceType::ReplicaSet.display_name(),
                        sort_key: ts,
                    });
                }
            }
        }
    }

    async fn add_pods_by_selector(&self, related: &mut Vec<RelatedResource>, ns: &str, value: &Value) {
        let selector = value
            .get("spec")
            .and_then(|s| s.get("selector"))
            .and_then(|s| s.get("matchLabels"))
            .and_then(|m| m.as_object());
        if let Some(labels) = selector {
            let sel = labels
                .iter()
                .map(|(k, v)| format!("{}={}", k, v.as_str().unwrap_or("")))
                .collect::<Vec<_>>()
                .join(",");
            self.add_pods_by_label(related, ns, &sel).await;
        }
    }

    async fn add_pods_by_service_selector(&self, related: &mut Vec<RelatedResource>, ns: &str, value: &Value) {
        if let Some(selector) = value
            .get("spec")
            .and_then(|s| s.get("selector"))
            .and_then(|s| s.as_object())
        {
            let sel = selector
                .iter()
                .map(|(k, v)| format!("{}={}", k, v.as_str().unwrap_or("")))
                .collect::<Vec<_>>()
                .join(",");
            self.add_pods_by_label(related, ns, &sel).await;
        }
    }

    async fn add_pods_by_network_policy_selector(&self, related: &mut Vec<RelatedResource>, ns: &str, value: &Value) {
        if let Some(selector) = value
            .get("spec")
            .and_then(|s| s.get("podSelector"))
            .and_then(|s| s.get("matchLabels"))
            .and_then(|m| m.as_object())
        {
            let sel = selector
                .iter()
                .map(|(k, v)| format!("{}={}", k, v.as_str().unwrap_or("")))
                .collect::<Vec<_>>()
                .join(",");
            self.add_pods_by_label(related, ns, &sel).await;
        }
    }

    async fn add_pods_by_label(&self, related: &mut Vec<RelatedResource>, ns: &str, label_sel: &str) {
        let api: Api<Pod> = Api::namespaced(self.client.default.clone(), ns);
        if let Ok(list) = api.list(&ListParams::default().labels(label_sel)).await {
            for pod in &list.items {
                let phase = pod
                    .status
                    .as_ref()
                    .and_then(|s| s.phase.as_deref())
                    .unwrap_or("Unknown");
                let ts = pod
                    .metadata
                    .creation_timestamp
                    .as_ref()
                    .map(|t| t.0.to_string())
                    .unwrap_or_default();
                related.push(RelatedResource {
                    resource_type: ResourceType::Pod,
                    name: meta_name(&pod.metadata),
                    namespace: ns.to_string(),
                    info: phase.to_string(),
                    category: ResourceType::Pod.display_name(),
                    sort_key: ts,
                });
            }
        }
    }

    async fn add_hpa_for_workload(&self, related: &mut Vec<RelatedResource>, ns: &str, name: &str) {
        let api: Api<HorizontalPodAutoscaler> = Api::namespaced(self.client.default.clone(), ns);
        if let Ok(list) = api.list(&ListParams::default()).await {
            for hpa in &list.items {
                let target = hpa
                    .spec
                    .as_ref()
                    .map(|s| s.scale_target_ref.name.as_str())
                    .unwrap_or("");
                if target == name {
                    let min = hpa.spec.as_ref().and_then(|s| s.min_replicas).unwrap_or(0);
                    let max = hpa.spec.as_ref().map(|s| s.max_replicas).unwrap_or(0);
                    related.push(RelatedResource {
                        resource_type: ResourceType::HorizontalPodAutoscaler,
                        name: meta_name(&hpa.metadata),
                        namespace: ns.to_string(),
                        info: format!("min:{} max:{}", min, max),
                        category: ResourceType::HorizontalPodAutoscaler.display_name(),
                        sort_key: String::new(),
                    });
                }
            }
        }
    }

    fn add_service_account(related: &mut Vec<RelatedResource>, value: &Value, ns: &str, from_template: bool) {
        let spec = if from_template {
            value
                .get("spec")
                .and_then(|s| s.get("template"))
                .and_then(|t| t.get("spec"))
        } else {
            value.get("spec")
        };
        if let Some(sa) = spec.and_then(|s| s.get("serviceAccountName")).and_then(|n| n.as_str()) {
            Self::push_unique(related, ResourceType::ServiceAccount, sa, ns, "serviceAccount", "RBAC");
        }
        // Also check serviceAccount field (deprecated but still used)
        if let Some(sa) = spec.and_then(|s| s.get("serviceAccount")).and_then(|n| n.as_str()) {
            Self::push_unique(related, ResourceType::ServiceAccount, sa, ns, "serviceAccount", "RBAC");
        }
    }

    fn push_unique(
        related: &mut Vec<RelatedResource>,
        rt: ResourceType,
        name: &str,
        ns: &str,
        info: &str,
        _category: &'static str,
    ) {
        if !related.iter().any(|r| r.resource_type == rt && r.name == name) {
            related.push(RelatedResource {
                resource_type: rt,
                name: name.to_string(),
                namespace: ns.to_string(),
                info: info.to_string(),
                category: rt.display_name(),
                sort_key: String::new(),
            });
        }
    }

    /// Find workloads (Deployments, StatefulSets, DaemonSets, Jobs, CronJobs)
    /// that reference a given Secret or ConfigMap by name.
    async fn add_workloads_referencing(
        &self,
        related: &mut Vec<RelatedResource>,
        ref_type: ResourceType,
        ns: &str,
        ref_name: &str,
    ) {
        let is_secret = ref_type == ResourceType::Secret;

        // Helper: check if a pod spec references the target name
        let spec_references = |spec: &Value| -> bool {
            // Check volumes
            if let Some(volumes) = spec.get("volumes").and_then(|v| v.as_array()) {
                for vol in volumes {
                    if is_secret {
                        if vol
                            .get("secret")
                            .and_then(|s| s.get("secretName"))
                            .and_then(|n| n.as_str())
                            == Some(ref_name)
                        {
                            return true;
                        }
                        if let Some(sources) = vol
                            .get("projected")
                            .and_then(|p| p.get("sources"))
                            .and_then(|s| s.as_array())
                        {
                            for src in sources {
                                if src.get("secret").and_then(|s| s.get("name")).and_then(|n| n.as_str())
                                    == Some(ref_name)
                                {
                                    return true;
                                }
                            }
                        }
                    } else {
                        if vol
                            .get("configMap")
                            .and_then(|c| c.get("name"))
                            .and_then(|n| n.as_str())
                            == Some(ref_name)
                        {
                            return true;
                        }
                        if let Some(sources) = vol
                            .get("projected")
                            .and_then(|p| p.get("sources"))
                            .and_then(|s| s.as_array())
                        {
                            for src in sources {
                                if src
                                    .get("configMap")
                                    .and_then(|c| c.get("name"))
                                    .and_then(|n| n.as_str())
                                    == Some(ref_name)
                                {
                                    return true;
                                }
                            }
                        }
                    }
                }
            }
            // Check containers env refs
            for container_key in &["containers", "initContainers"] {
                if let Some(containers) = spec.get(container_key).and_then(|c| c.as_array()) {
                    for container in containers {
                        if let Some(env_from) = container.get("envFrom").and_then(|e| e.as_array()) {
                            for src in env_from {
                                let ref_field = if is_secret { "secretRef" } else { "configMapRef" };
                                if src.get(ref_field).and_then(|r| r.get("name")).and_then(|n| n.as_str())
                                    == Some(ref_name)
                                {
                                    return true;
                                }
                            }
                        }
                        if let Some(env) = container.get("env").and_then(|e| e.as_array()) {
                            for var in env {
                                if let Some(vf) = var.get("valueFrom") {
                                    let ref_field = if is_secret { "secretKeyRef" } else { "configMapKeyRef" };
                                    if vf.get(ref_field).and_then(|r| r.get("name")).and_then(|n| n.as_str())
                                        == Some(ref_name)
                                    {
                                        return true;
                                    }
                                }
                            }
                        }
                    }
                }
            }
            false
        };

        // Search Deployments
        let dep_api: Api<Deployment> = Api::namespaced(self.client.default.clone(), ns);
        if let Ok(list) = dep_api.list(&ListParams::default()).await {
            for dep in &list.items {
                let val = serde_json::to_value(dep).unwrap_or_default();
                if let Some(spec) = val
                    .get("spec")
                    .and_then(|s| s.get("template"))
                    .and_then(|t| t.get("spec"))
                {
                    if spec_references(spec) {
                        Self::push_unique(
                            related,
                            ResourceType::Deployment,
                            &meta_name(&dep.metadata),
                            ns,
                            "references",
                            "Workloads",
                        );
                    }
                }
            }
        }
        // Search StatefulSets
        let ss_api: Api<StatefulSet> = Api::namespaced(self.client.default.clone(), ns);
        if let Ok(list) = ss_api.list(&ListParams::default()).await {
            for ss in &list.items {
                let val = serde_json::to_value(ss).unwrap_or_default();
                if let Some(spec) = val
                    .get("spec")
                    .and_then(|s| s.get("template"))
                    .and_then(|t| t.get("spec"))
                {
                    if spec_references(spec) {
                        Self::push_unique(
                            related,
                            ResourceType::StatefulSet,
                            &meta_name(&ss.metadata),
                            ns,
                            "references",
                            "Workloads",
                        );
                    }
                }
            }
        }
        // Search DaemonSets
        let ds_api: Api<DaemonSet> = Api::namespaced(self.client.default.clone(), ns);
        if let Ok(list) = ds_api.list(&ListParams::default()).await {
            for ds in &list.items {
                let val = serde_json::to_value(ds).unwrap_or_default();
                if let Some(spec) = val
                    .get("spec")
                    .and_then(|s| s.get("template"))
                    .and_then(|t| t.get("spec"))
                {
                    if spec_references(spec) {
                        Self::push_unique(
                            related,
                            ResourceType::DaemonSet,
                            &meta_name(&ds.metadata),
                            ns,
                            "references",
                            "Workloads",
                        );
                    }
                }
            }
        }
        // Search Jobs
        let job_api: Api<Job> = Api::namespaced(self.client.default.clone(), ns);
        if let Ok(list) = job_api.list(&ListParams::default()).await {
            for job in &list.items {
                let val = serde_json::to_value(job).unwrap_or_default();
                if let Some(spec) = val
                    .get("spec")
                    .and_then(|s| s.get("template"))
                    .and_then(|t| t.get("spec"))
                {
                    if spec_references(spec) {
                        Self::push_unique(
                            related,
                            ResourceType::Job,
                            &meta_name(&job.metadata),
                            ns,
                            "references",
                            "Workloads",
                        );
                    }
                }
            }
        }
        // Search CronJobs
        let cj_api: Api<CronJob> = Api::namespaced(self.client.default.clone(), ns);
        if let Ok(list) = cj_api.list(&ListParams::default()).await {
            for cj in &list.items {
                let val = serde_json::to_value(cj).unwrap_or_default();
                if let Some(spec) = val
                    .get("spec")
                    .and_then(|s| s.get("jobTemplate"))
                    .and_then(|j| j.get("spec"))
                    .and_then(|s| s.get("template"))
                    .and_then(|t| t.get("spec"))
                {
                    if spec_references(spec) {
                        Self::push_unique(
                            related,
                            ResourceType::CronJob,
                            &meta_name(&cj.metadata),
                            ns,
                            "references",
                            "Workloads",
                        );
                    }
                }
            }
        }
    }

    async fn add_workloads_using_sa(&self, related: &mut Vec<RelatedResource>, ns: &str, sa_name: &str) {
        let check_sa = |val: &Value| -> bool {
            val.get("spec")
                .and_then(|s| s.get("template"))
                .and_then(|t| t.get("spec"))
                .and_then(|s| s.get("serviceAccountName").or_else(|| s.get("serviceAccount")))
                .and_then(|n| n.as_str())
                == Some(sa_name)
        };
        let dep_api: Api<Deployment> = Api::namespaced(self.client.default.clone(), ns);
        if let Ok(list) = dep_api.list(&ListParams::default()).await {
            for dep in &list.items {
                let val = serde_json::to_value(dep).unwrap_or_default();
                if check_sa(&val) {
                    Self::push_unique(
                        related,
                        ResourceType::Deployment,
                        &meta_name(&dep.metadata),
                        ns,
                        "uses SA",
                        "x",
                    );
                }
            }
        }
        let ss_api: Api<StatefulSet> = Api::namespaced(self.client.default.clone(), ns);
        if let Ok(list) = ss_api.list(&ListParams::default()).await {
            for ss in &list.items {
                let val = serde_json::to_value(ss).unwrap_or_default();
                if check_sa(&val) {
                    Self::push_unique(
                        related,
                        ResourceType::StatefulSet,
                        &meta_name(&ss.metadata),
                        ns,
                        "uses SA",
                        "x",
                    );
                }
            }
        }
        let ds_api: Api<DaemonSet> = Api::namespaced(self.client.default.clone(), ns);
        if let Ok(list) = ds_api.list(&ListParams::default()).await {
            for ds in &list.items {
                let val = serde_json::to_value(ds).unwrap_or_default();
                if check_sa(&val) {
                    Self::push_unique(
                        related,
                        ResourceType::DaemonSet,
                        &meta_name(&ds.metadata),
                        ns,
                        "uses SA",
                        "x",
                    );
                }
            }
        }
        let job_api: Api<Job> = Api::namespaced(self.client.default.clone(), ns);
        if let Ok(list) = job_api.list(&ListParams::default()).await {
            for job in &list.items {
                let val = serde_json::to_value(job).unwrap_or_default();
                if check_sa(&val) {
                    Self::push_unique(
                        related,
                        ResourceType::Job,
                        &meta_name(&job.metadata),
                        ns,
                        "uses SA",
                        "x",
                    );
                }
            }
        }
    }

    fn extract_volume_refs(related: &mut Vec<RelatedResource>, volumes: &[Value], ns: &str) {
        for vol in volumes {
            if let Some(name) = vol
                .get("secret")
                .and_then(|s| s.get("secretName"))
                .and_then(|n| n.as_str())
            {
                Self::push_unique(related, ResourceType::Secret, name, ns, "mounted", "Config");
            }
            if let Some(name) = vol
                .get("configMap")
                .and_then(|c| c.get("name"))
                .and_then(|n| n.as_str())
            {
                Self::push_unique(related, ResourceType::ConfigMap, name, ns, "mounted", "Config");
            }
            if let Some(name) = vol
                .get("persistentVolumeClaim")
                .and_then(|p| p.get("claimName"))
                .and_then(|n| n.as_str())
            {
                Self::push_unique(
                    related,
                    ResourceType::PersistentVolumeClaim,
                    name,
                    ns,
                    "mounted",
                    "Storage",
                );
            }
            if let Some(name) = vol
                .get("projected")
                .and_then(|p| p.get("sources"))
                .and_then(|s| s.as_array())
            {
                for src in name {
                    if let Some(sname) = src.get("secret").and_then(|s| s.get("name")).and_then(|n| n.as_str()) {
                        Self::push_unique(related, ResourceType::Secret, sname, ns, "projected", "Config");
                    }
                    if let Some(cname) = src
                        .get("configMap")
                        .and_then(|c| c.get("name"))
                        .and_then(|n| n.as_str())
                    {
                        Self::push_unique(related, ResourceType::ConfigMap, cname, ns, "projected", "Config");
                    }
                }
            }
        }
    }

    fn extract_env_refs(related: &mut Vec<RelatedResource>, containers: &[Value], ns: &str) {
        for container in containers {
            if let Some(env_from) = container.get("envFrom").and_then(|e| e.as_array()) {
                for src in env_from {
                    if let Some(name) = src
                        .get("secretRef")
                        .and_then(|s| s.get("name"))
                        .and_then(|n| n.as_str())
                    {
                        Self::push_unique(related, ResourceType::Secret, name, ns, "envFrom", "Config");
                    }
                    if let Some(name) = src
                        .get("configMapRef")
                        .and_then(|c| c.get("name"))
                        .and_then(|n| n.as_str())
                    {
                        Self::push_unique(related, ResourceType::ConfigMap, name, ns, "envFrom", "Config");
                    }
                }
            }
            if let Some(env) = container.get("env").and_then(|e| e.as_array()) {
                for var in env {
                    if let Some(vf) = var.get("valueFrom") {
                        if let Some(name) = vf
                            .get("secretKeyRef")
                            .and_then(|s| s.get("name"))
                            .and_then(|n| n.as_str())
                        {
                            Self::push_unique(related, ResourceType::Secret, name, ns, "env", "Config");
                        }
                        if let Some(name) = vf
                            .get("configMapKeyRef")
                            .and_then(|c| c.get("name"))
                            .and_then(|n| n.as_str())
                        {
                            Self::push_unique(related, ResourceType::ConfigMap, name, ns, "env", "Config");
                        }
                    }
                }
            }
        }
    }

    // -- Log support -----------------------------------------------------------

    pub async fn find_pods(&self, rt: ResourceType, ns: &str, name: &str) -> Result<Vec<PodInfo>> {
        let api: Api<Pod> = Api::namespaced(self.client.default.clone(), ns);
        match rt {
            | ResourceType::Pod => {
                let pod = api.get(name).await?;
                let containers = pod
                    .spec
                    .as_ref()
                    .map(|s| s.containers.iter().map(|c| c.name.clone()).collect())
                    .unwrap_or_default();
                Ok(vec![PodInfo {
                    name: name.to_string(),
                    containers,
                }])
            },
            | ResourceType::Job => {
                let lp = ListParams::default().labels(&format!("job-name={}", name));
                self.pods_from_list(&api, &lp).await
            },
            | ResourceType::CronJob => {
                // Find latest Job owned by this CronJob, then find its pods
                let job_api: Api<Job> = Api::namespaced(self.client.default.clone(), ns);
                let jobs = job_api.list(&ListParams::default()).await?;
                let latest = jobs
                    .items
                    .iter()
                    .filter(|j| {
                        j.metadata
                            .owner_references
                            .as_ref()
                            .map(|refs| refs.iter().any(|r| r.kind == "CronJob" && r.name == name))
                            .unwrap_or(false)
                    })
                    .max_by_key(|j| j.metadata.creation_timestamp.as_ref().map(|t| t.0));
                match latest {
                    | Some(j) => {
                        let job_name = j.metadata.name.as_deref().unwrap_or("");
                        let lp = ListParams::default().labels(&format!("job-name={}", job_name));
                        self.pods_from_list(&api, &lp).await
                    },
                    | None => Ok(vec![]),
                }
            },
            | ResourceType::Deployment
            | ResourceType::StatefulSet
            | ResourceType::DaemonSet
            | ResourceType::ReplicaSet => {
                // Use the resource's spec.selector.matchLabels
                let val = self.get_resource(rt, ns, name).await?;
                let labels = val
                    .get("spec")
                    .and_then(|s| s.get("selector"))
                    .and_then(|s| s.get("matchLabels"))
                    .and_then(|m| m.as_object());
                match labels {
                    | Some(map) => {
                        let selector = map
                            .iter()
                            .map(|(k, v)| format!("{}={}", k, v.as_str().unwrap_or("")))
                            .collect::<Vec<_>>()
                            .join(",");
                        let lp = ListParams::default().labels(&selector);
                        self.pods_from_list(&api, &lp).await
                    },
                    | None => Ok(vec![]),
                }
            },
            | _ => Ok(vec![]),
        }
    }

    async fn pods_from_list(&self, api: &Api<Pod>, lp: &ListParams) -> Result<Vec<PodInfo>> {
        let list = api.list(lp).await?;
        Ok(list
            .items
            .iter()
            .map(|p| {
                PodInfo {
                    name: meta_name(&p.metadata),
                    containers: p
                        .spec
                        .as_ref()
                        .map(|s| s.containers.iter().map(|c| c.name.clone()).collect())
                        .unwrap_or_default(),
                }
            })
            .collect())
    }

    /// Fetch logs for a single pod/container.
    pub async fn fetch_logs(
        &self,
        ns: &str,
        pod: &str,
        container: &str,
        tail: i64,
        since_seconds: Option<i64>,
    ) -> Result<String> {
        let api: Api<Pod> = Api::namespaced(self.client.default.clone(), ns);
        let lp = kube::api::LogParams {
            container: Some(container.to_string()),
            tail_lines: Some(tail),
            timestamps: true,
            since_seconds,
            ..Default::default()
        };
        Ok(api.logs(pod, &lp).await?)
    }

    /// Fetch logs for multiple pod/container pairs and merge them with
    /// prefixes.  Returns structured `LogLine`s sorted by timestamp.
    pub async fn fetch_logs_multi(
        &self,
        ns: &str,
        pairs: &[(String, String)],
        tail: i64,
        since_seconds: Option<i64>,
    ) -> Result<Vec<crate::tui::logs::LogLine>> {
        use crate::tui::logs::LogLine;
        let mut all_lines = Vec::new();
        for (pod, container) in pairs {
            let prefix = format!("[{}/{}] ", pod, container);
            match self.fetch_logs(ns, pod, container, tail, since_seconds).await {
                | Ok(logs) => {
                    for line in logs.lines() {
                        all_lines.push(LogLine::parse(&format!("{}{}", prefix, line)));
                    }
                },
                | Err(_) => {
                    all_lines.push(LogLine::parse(&format!("{}(failed to fetch logs)", prefix)));
                },
            }
        }
        // Sort by timestamp — RFC3339 timestamps are lexicographically sortable.
        all_lines.sort_by(|a, b| a.timestamp.cmp(&b.timestamp));
        Ok(all_lines)
    }

    // -- Generic list helper -------------------------------------------------

    async fn list_typed<K>(&self, mapper: fn(&K) -> ResourceEntry) -> Result<Vec<ResourceEntry>>
    where
        K: kube::Resource<DynamicType=()>+serde::de::DeserializeOwned+Clone+std::fmt::Debug,
        K: kube::Resource<Scope=k8s_openapi::NamespaceResourceScope>,
    {
        let lp = ListParams::default();
        let list = match &self.context.namespace {
            | Some(ns) => Api::<K>::namespaced(self.client.default.clone(), ns).list(&lp).await?,
            | None => Api::<K>::all(self.client.default.clone()).list(&lp).await?,
        };
        Ok(list.items.iter().map(mapper).collect())
    }

    // -- Generic value fetch for any namespaced resource --------------------

    async fn get_value<K>(&self, ns: &str, name: &str) -> Result<Value>
    where
        K: kube::Resource<DynamicType=()>+serde::de::DeserializeOwned+serde::Serialize+Clone+std::fmt::Debug,
        K: kube::Resource<Scope=k8s_openapi::NamespaceResourceScope>,
    {
        let api: Api<K> = Api::namespaced(self.client.default.clone(), ns);
        let obj = api.get(name).await?;
        strip_managed_fields(serde_json::to_value(&obj)?)
    }

    // -- Cluster-scoped list helper ------------------------------------------

    async fn list_cluster<K>(&self, mapper: fn(&K) -> ResourceEntry) -> Result<Vec<ResourceEntry>>
    where
        K: kube::Resource<DynamicType=()>+serde::de::DeserializeOwned+Clone+std::fmt::Debug,
        K: kube::Resource<Scope=k8s_openapi::ClusterResourceScope>,
    {
        let api: Api<K> = Api::all(self.client.default.clone());
        let list = api.list(&ListParams::default()).await?;
        Ok(list.items.iter().map(mapper).collect())
    }

    // -- Cluster-scoped value fetch ------------------------------------------

    async fn get_value_cluster<K>(&self, name: &str) -> Result<Value>
    where
        K: kube::Resource<DynamicType=()>+serde::de::DeserializeOwned+serde::Serialize+Clone+std::fmt::Debug,
        K: kube::Resource<Scope=k8s_openapi::ClusterResourceScope>,
    {
        let api: Api<K> = Api::all(self.client.default.clone());
        let obj = api.get(name).await?;
        strip_managed_fields(serde_json::to_value(&obj)?)
    }

    // -- Per-resource mappers ------------------------------------------------

    fn map_deployment(d: &Deployment) -> ResourceEntry {
        let status = d.status.as_ref();
        let replicas = d.spec.as_ref().and_then(|s| s.replicas).unwrap_or(0);
        let ready = status.and_then(|s| s.ready_replicas).unwrap_or(0);
        let updated = status.and_then(|s| s.updated_replicas).unwrap_or(0);
        let available = status.and_then(|s| s.available_replicas).unwrap_or(0);
        ResourceEntry {
            name: meta_name(&d.metadata),
            namespace: meta_ns(&d.metadata),
            columns: vec![
                format!("{}/{}", ready, replicas),
                updated.to_string(),
                available.to_string(),
                format_age(d.metadata.creation_timestamp.as_ref()),
            ],
            sort_key: None,
        }
    }

    fn map_statefulset(s: &StatefulSet) -> ResourceEntry {
        let status = s.status.as_ref();
        let replicas = s.spec.as_ref().and_then(|sp| sp.replicas).unwrap_or(0);
        let ready = status.and_then(|st| st.ready_replicas).unwrap_or(0);
        ResourceEntry {
            name: meta_name(&s.metadata),
            namespace: meta_ns(&s.metadata),
            columns: vec![
                format!("{}/{}", ready, replicas),
                format_age(s.metadata.creation_timestamp.as_ref()),
            ],
            sort_key: None,
        }
    }

    fn map_replicaset(r: &ReplicaSet) -> ResourceEntry {
        let status = r.status.as_ref();
        let desired = r.spec.as_ref().and_then(|s| s.replicas).unwrap_or(0);
        let current = status.map(|s| s.replicas).unwrap_or(0);
        let ready = status.and_then(|s| s.ready_replicas).unwrap_or(0);
        ResourceEntry {
            name: meta_name(&r.metadata),
            namespace: meta_ns(&r.metadata),
            columns: vec![
                desired.to_string(),
                current.to_string(),
                ready.to_string(),
                format_age(r.metadata.creation_timestamp.as_ref()),
            ],
            sort_key: None,
        }
    }

    fn map_pod(p: &Pod) -> ResourceEntry {
        let status = p.status.as_ref();
        let phase = status.and_then(|s| s.phase.clone()).unwrap_or_else(|| "Unknown".into());
        let containers = status
            .and_then(|s| s.container_statuses.as_ref())
            .cloned()
            .unwrap_or_default();
        let total = containers.len();
        let ready_count = containers.iter().filter(|c| c.ready).count();
        let restarts: i32 = containers.iter().map(|c| c.restart_count).sum();
        ResourceEntry {
            name: meta_name(&p.metadata),
            namespace: meta_ns(&p.metadata),
            columns: vec![
                format!("{}/{}", ready_count, total),
                phase,
                restarts.to_string(),
                format_age(p.metadata.creation_timestamp.as_ref()),
            ],
            sort_key: None,
        }
    }

    fn map_cronjob(c: &CronJob) -> ResourceEntry {
        let spec = c.spec.as_ref();
        let schedule = spec.map(|s| s.schedule.clone()).unwrap_or_default();
        let suspend = spec.and_then(|s| s.suspend).unwrap_or(false);
        let active = c
            .status
            .as_ref()
            .and_then(|s| s.active.as_ref())
            .map(|a| a.len())
            .unwrap_or(0);
        ResourceEntry {
            name: meta_name(&c.metadata),
            namespace: meta_ns(&c.metadata),
            columns: vec![
                schedule,
                suspend.to_string(),
                active.to_string(),
                format_age(c.metadata.creation_timestamp.as_ref()),
            ],
            sort_key: None,
        }
    }

    fn map_job(j: &Job) -> ResourceEntry {
        let status = j.status.as_ref();
        let succeeded = status.and_then(|s| s.succeeded).unwrap_or(0);
        let completions = j.spec.as_ref().and_then(|s| s.completions).unwrap_or(1);
        let duration = status
            .and_then(|s| {
                let start = s.start_time.as_ref()?;
                let end = s.completion_time.as_ref().map(|t| t.0).unwrap_or_else(Timestamp::now);
                let dur = end.duration_since(start.0);
                Some(format_duration(dur))
            })
            .unwrap_or_default();
        ResourceEntry {
            name: meta_name(&j.metadata),
            namespace: meta_ns(&j.metadata),
            columns: vec![
                format!("{}/{}", succeeded, completions),
                duration,
                format_age(j.metadata.creation_timestamp.as_ref()),
            ],
            sort_key: None,
        }
    }

    fn map_configmap(c: &ConfigMap) -> ResourceEntry {
        let data_count =
            c.data.as_ref().map(|d| d.len()).unwrap_or(0) + c.binary_data.as_ref().map(|d| d.len()).unwrap_or(0);
        ResourceEntry {
            name: meta_name(&c.metadata),
            namespace: meta_ns(&c.metadata),
            columns: vec![
                data_count.to_string(),
                format_age(c.metadata.creation_timestamp.as_ref()),
            ],
            sort_key: None,
        }
    }

    fn map_secret(s: &Secret) -> ResourceEntry {
        let secret_type = s.type_.clone().unwrap_or_default();
        let data_count = s.data.as_ref().map(|d| d.len()).unwrap_or(0);
        ResourceEntry {
            name: meta_name(&s.metadata),
            namespace: meta_ns(&s.metadata),
            columns: vec![
                secret_type,
                data_count.to_string(),
                format_age(s.metadata.creation_timestamp.as_ref()),
            ],
            sort_key: None,
        }
    }

    fn map_service(s: &Service) -> ResourceEntry {
        let spec = s.spec.as_ref();
        let svc_type = spec.and_then(|s| s.type_.clone()).unwrap_or_default();
        let cluster_ip = spec
            .and_then(|s| s.cluster_ip.clone())
            .unwrap_or_else(|| "<none>".into());
        let ports = spec
            .and_then(|s| s.ports.as_ref())
            .map(|ports| {
                ports
                    .iter()
                    .map(|p| {
                        let proto = p.protocol.as_deref().unwrap_or("TCP");
                        match p.node_port {
                            | Some(np) => format!("{}:{}/{}", p.port, np, proto),
                            | None => format!("{}/{}", p.port, proto),
                        }
                    })
                    .collect::<Vec<_>>()
                    .join(",")
            })
            .unwrap_or_default();
        ResourceEntry {
            name: meta_name(&s.metadata),
            namespace: meta_ns(&s.metadata),
            columns: vec![
                svc_type,
                cluster_ip,
                ports,
                format_age(s.metadata.creation_timestamp.as_ref()),
            ],
            sort_key: None,
        }
    }

    fn map_daemonset(d: &DaemonSet) -> ResourceEntry {
        let status = d.status.as_ref();
        let desired = status.map(|s| s.desired_number_scheduled).unwrap_or(0);
        let current = status.map(|s| s.current_number_scheduled).unwrap_or(0);
        let ready = status.map(|s| s.number_ready).unwrap_or(0);
        let updated = status.and_then(|s| s.updated_number_scheduled).unwrap_or(0);
        ResourceEntry {
            name: meta_name(&d.metadata),
            namespace: meta_ns(&d.metadata),
            columns: vec![
                desired.to_string(),
                current.to_string(),
                ready.to_string(),
                updated.to_string(),
                format_age(d.metadata.creation_timestamp.as_ref()),
            ],
            sort_key: None,
        }
    }

    fn map_hpa(h: &HorizontalPodAutoscaler) -> ResourceEntry {
        let spec = h.spec.as_ref();
        let reference = spec
            .map(|s| {
                let kind = s.scale_target_ref.kind.as_str();
                let name = s.scale_target_ref.name.as_str();
                format!("{}/{}", kind, name)
            })
            .unwrap_or_default();
        let min_replicas = spec.and_then(|s| s.min_replicas).unwrap_or(0);
        let max_replicas = spec.map(|s| s.max_replicas).unwrap_or(0);
        let current = h.status.as_ref().map(|s| s.current_replicas).unwrap_or(0);
        ResourceEntry {
            name: meta_name(&h.metadata),
            namespace: meta_ns(&h.metadata),
            columns: vec![
                reference,
                min_replicas.to_string(),
                max_replicas.to_string(),
                current.to_string(),
                format_age(h.metadata.creation_timestamp.as_ref()),
            ],
            sort_key: None,
        }
    }

    fn map_ingress(i: &Ingress) -> ResourceEntry {
        let spec = i.spec.as_ref();
        let class = spec
            .and_then(|s| s.ingress_class_name.clone())
            .unwrap_or_else(|| "<none>".into());
        let hosts = spec
            .and_then(|s| s.rules.as_ref())
            .map(|rules| {
                rules
                    .iter()
                    .filter_map(|r| r.host.clone())
                    .collect::<Vec<_>>()
                    .join(",")
            })
            .unwrap_or_default();
        let address = i
            .status
            .as_ref()
            .and_then(|s| s.load_balancer.as_ref())
            .and_then(|lb| lb.ingress.as_ref())
            .map(|ingress| {
                ingress
                    .iter()
                    .filter_map(|i| i.ip.as_ref().or(i.hostname.as_ref()).cloned())
                    .collect::<Vec<_>>()
                    .join(",")
            })
            .unwrap_or_default();
        ResourceEntry {
            name: meta_name(&i.metadata),
            namespace: meta_ns(&i.metadata),
            columns: vec![
                class,
                hosts,
                address,
                format_age(i.metadata.creation_timestamp.as_ref()),
            ],
            sort_key: None,
        }
    }

    fn map_endpoints(e: &Endpoints) -> ResourceEntry {
        let endpoints_str = e
            .subsets
            .as_ref()
            .map(|subsets| {
                let mut addrs = Vec::new();
                for subset in subsets {
                    let ports: Vec<i32> = subset
                        .ports
                        .as_ref()
                        .map(|p| p.iter().map(|p| p.port).collect())
                        .unwrap_or_default();
                    if let Some(addresses) = &subset.addresses {
                        for addr in addresses {
                            for port in &ports {
                                addrs.push(format!("{}:{}", addr.ip, port));
                            }
                        }
                    }
                }
                if addrs.len() > 3 {
                    let total = addrs.len();
                    addrs.truncate(3);
                    format!("{} + {} more", addrs.join(","), total - 3)
                } else {
                    addrs.join(",")
                }
            })
            .unwrap_or_else(|| "<none>".into());
        ResourceEntry {
            name: meta_name(&e.metadata),
            namespace: meta_ns(&e.metadata),
            columns: vec![endpoints_str, format_age(e.metadata.creation_timestamp.as_ref())],
            sort_key: None,
        }
    }

    fn map_network_policy(n: &NetworkPolicy) -> ResourceEntry {
        let selector = n
            .spec
            .as_ref()
            .and_then(|s| s.pod_selector.as_ref())
            .and_then(|ps| ps.match_labels.as_ref())
            .map(|m| {
                m.iter()
                    .map(|(k, v)| format!("{}={}", k, v))
                    .collect::<Vec<_>>()
                    .join(",")
            })
            .unwrap_or_else(|| "<none>".into());
        ResourceEntry {
            name: meta_name(&n.metadata),
            namespace: meta_ns(&n.metadata),
            columns: vec![selector, format_age(n.metadata.creation_timestamp.as_ref())],
            sort_key: None,
        }
    }

    fn map_pvc(p: &PersistentVolumeClaim) -> ResourceEntry {
        let status = p.status.as_ref();
        let phase = status.and_then(|s| s.phase.clone()).unwrap_or_else(|| "Pending".into());
        let volume = p.spec.as_ref().and_then(|s| s.volume_name.clone()).unwrap_or_default();
        let capacity = status
            .and_then(|s| s.capacity.as_ref())
            .and_then(|c| c.get("storage"))
            .map(|q| q.0.clone())
            .unwrap_or_default();
        ResourceEntry {
            name: meta_name(&p.metadata),
            namespace: meta_ns(&p.metadata),
            columns: vec![
                phase,
                volume,
                capacity,
                format_age(p.metadata.creation_timestamp.as_ref()),
            ],
            sort_key: None,
        }
    }

    fn map_pv(p: &PersistentVolume) -> ResourceEntry {
        let spec = p.spec.as_ref();
        let capacity = spec
            .and_then(|s| s.capacity.as_ref())
            .and_then(|c| c.get("storage"))
            .map(|q| q.0.clone())
            .unwrap_or_default();
        let reclaim = spec
            .and_then(|s| s.persistent_volume_reclaim_policy.clone())
            .unwrap_or_default();
        let phase = p.status.as_ref().and_then(|s| s.phase.clone()).unwrap_or_default();
        let claim = spec
            .and_then(|s| s.claim_ref.as_ref())
            .map(|c| {
                format!(
                    "{}/{}",
                    c.namespace.as_deref().unwrap_or(""),
                    c.name.as_deref().unwrap_or("")
                )
            })
            .unwrap_or_default();
        ResourceEntry {
            name: meta_name(&p.metadata),
            namespace: String::new(),
            columns: vec![
                capacity,
                reclaim,
                phase,
                claim,
                format_age(p.metadata.creation_timestamp.as_ref()),
            ],
            sort_key: None,
        }
    }

    fn map_storage_class(s: &StorageClass) -> ResourceEntry {
        let provisioner = s.provisioner.clone();
        let reclaim = s.reclaim_policy.clone().unwrap_or_default();
        ResourceEntry {
            name: meta_name(&s.metadata),
            namespace: String::new(),
            columns: vec![provisioner, reclaim, format_age(s.metadata.creation_timestamp.as_ref())],
            sort_key: None,
        }
    }

    fn map_service_account(s: &ServiceAccount) -> ResourceEntry {
        let secrets_count = s.secrets.as_ref().map(|s| s.len()).unwrap_or(0);
        ResourceEntry {
            name: meta_name(&s.metadata),
            namespace: meta_ns(&s.metadata),
            columns: vec![
                secrets_count.to_string(),
                format_age(s.metadata.creation_timestamp.as_ref()),
            ],
            sort_key: None,
        }
    }

    fn map_role(r: &Role) -> ResourceEntry {
        ResourceEntry {
            name: meta_name(&r.metadata),
            namespace: meta_ns(&r.metadata),
            columns: vec![format_age(r.metadata.creation_timestamp.as_ref())],
            sort_key: None,
        }
    }

    fn map_role_binding(r: &RoleBinding) -> ResourceEntry {
        let role = format!("{}/{}", r.role_ref.kind, r.role_ref.name);
        ResourceEntry {
            name: meta_name(&r.metadata),
            namespace: meta_ns(&r.metadata),
            columns: vec![role, format_age(r.metadata.creation_timestamp.as_ref())],
            sort_key: None,
        }
    }

    fn map_cluster_role(r: &ClusterRole) -> ResourceEntry {
        ResourceEntry {
            name: meta_name(&r.metadata),
            namespace: String::new(),
            columns: vec![format_age(r.metadata.creation_timestamp.as_ref())],
            sort_key: None,
        }
    }

    fn map_cluster_role_binding(r: &ClusterRoleBinding) -> ResourceEntry {
        let role = format!("{}/{}", r.role_ref.kind, r.role_ref.name);
        ResourceEntry {
            name: meta_name(&r.metadata),
            namespace: String::new(),
            columns: vec![role, format_age(r.metadata.creation_timestamp.as_ref())],
            sort_key: None,
        }
    }

    fn map_node(n: &Node) -> ResourceEntry {
        let status = n.status.as_ref();
        let conditions = status.and_then(|s| s.conditions.as_ref());
        let ready = conditions
            .and_then(|conds| conds.iter().find(|c| c.type_ == "Ready").map(|c| c.status == "True"))
            .unwrap_or(false);
        let unschedulable = n.spec.as_ref().and_then(|s| s.unschedulable).unwrap_or(false);

        let status_str = match (ready, unschedulable) {
            | (true, false) => "Ready".to_string(),
            | (true, true) => "Ready,SchedulingDisabled".to_string(),
            | (false, false) => "NotReady".to_string(),
            | (false, true) => "NotReady,SchedulingDisabled".to_string(),
        };

        let roles = n
            .metadata
            .labels
            .as_ref()
            .map(|labels| {
                labels
                    .keys()
                    .filter_map(|k| {
                        k.strip_prefix("node-role.kubernetes.io/")
                            .map(|r| if r.is_empty() { "worker" } else { r })
                    })
                    .collect::<Vec<_>>()
                    .join(",")
            })
            .unwrap_or_default();
        let version = status
            .and_then(|s| s.node_info.as_ref())
            .map(|i| i.kubelet_version.clone())
            .unwrap_or_default();
        ResourceEntry {
            name: meta_name(&n.metadata),
            namespace: String::new(),
            columns: vec![
                status_str,
                if roles.is_empty() { "<none>".into() } else { roles },
                version,
                format_age(n.metadata.creation_timestamp.as_ref()),
            ],
            sort_key: None,
        }
    }

    fn map_namespace(n: &Namespace) -> ResourceEntry {
        let phase = n
            .status
            .as_ref()
            .and_then(|s| s.phase.clone())
            .unwrap_or_else(|| "Active".into());
        ResourceEntry {
            name: meta_name(&n.metadata),
            namespace: String::new(),
            columns: vec![phase, format_age(n.metadata.creation_timestamp.as_ref())],
            sort_key: None,
        }
    }

    fn map_event(e: &Event) -> ResourceEntry {
        let event_type = e.type_.clone().unwrap_or_default();
        let reason = e.reason.clone().unwrap_or_default();
        let object = e
            .regarding
            .as_ref()
            .and_then(|r| {
                r.name
                    .as_ref()
                    .map(|n| format!("{}/{}", r.kind.as_deref().unwrap_or(""), n))
            })
            .unwrap_or_default();
        let message = e.note.clone().unwrap_or_default();
        let count = e.series.as_ref().map(|s| s.count).or(e.deprecated_count).unwrap_or(1);
        // Prefer eventTime, fall back to deprecated lastTimestamp, then
        // creationTimestamp
        let best_time: Option<Timestamp> = e
            .event_time
            .as_ref()
            .map(|t| t.0)
            .or(e.deprecated_last_timestamp.as_ref().map(|t| t.0))
            .or(e.metadata.creation_timestamp.as_ref().map(|t| t.0));
        let sort_ts = best_time.map(|t| t.to_string()).unwrap_or_default();
        let age = best_time
            .map(|t| {
                let dur = Timestamp::now().duration_since(t);
                format_duration(dur)
            })
            .unwrap_or_else(|| "N/A".into());
        ResourceEntry {
            name: meta_name(&e.metadata),
            namespace: meta_ns(&e.metadata),
            // [0]=TYPE  [1]=REASON  [2]=OBJECT  [3]=AGE  [4]=MESSAGE  [5]=COUNT
            columns: vec![event_type, reason, object, age, message, count.to_string()],
            sort_key: Some(sort_ts),
        }
    }

    fn map_crd(c: &CustomResourceDefinition) -> ResourceEntry {
        let group = c.spec.group.clone();
        let versions = c
            .spec
            .versions
            .iter()
            .filter(|v| v.served)
            .map(|v| v.name.as_str())
            .collect::<Vec<_>>()
            .join(",");
        let scope = c.spec.scope.clone();
        ResourceEntry {
            name: meta_name(&c.metadata),
            namespace: String::new(),
            columns: vec![
                group,
                versions,
                scope,
                format_age(c.metadata.creation_timestamp.as_ref()),
            ],
            sort_key: None,
        }
    }

    // -- CRD discovery and dynamic CR operations --------------------------------

    /// Discover all CRDs in the cluster and return metadata for each.
    pub async fn discover_crds(&self) -> Result<Vec<CrdInfo>> {
        let api: Api<CustomResourceDefinition> = Api::all(self.client.default.clone());
        let list = api.list(&ListParams::default()).await?;
        let mut crds = Vec::new();
        for crd in &list.items {
            let spec = &crd.spec;
            // Find the served+storage version, or fall back to first served
            let version = spec
                .versions
                .iter()
                .find(|v| v.served && v.storage)
                .or_else(|| spec.versions.iter().find(|v| v.served));
            let version = match version {
                | Some(v) => v,
                | None => continue,
            };
            let columns: Vec<CrdColumn> = version
                .additional_printer_columns
                .as_ref()
                .map(|cols| {
                    cols.iter()
                        .filter(|c| c.json_path != ".metadata.creationTimestamp")
                        .map(|c| {
                            CrdColumn {
                                name: c.name.clone(),
                                json_path: c.json_path.clone(),
                                type_: c.type_.clone(),
                            }
                        })
                        .collect()
                })
                .unwrap_or_default();

            let scope = if spec.scope == "Namespaced" {
                CrdScope::Namespaced
            } else {
                CrdScope::Cluster
            };

            crds.push(CrdInfo {
                group: spec.group.clone(),
                version: version.name.clone(),
                kind: spec.names.kind.clone(),
                plural: spec.names.plural.clone(),
                scope,
                display_name: spec.names.kind.clone(),
                columns,
            });
        }
        crds.sort_by(|a, b| {
            let fqdn_a = format!("{}.{}", a.plural, a.group);
            let fqdn_b = format!("{}.{}", b.plural, b.group);
            fqdn_a.cmp(&fqdn_b)
        });
        Ok(crds)
    }

    /// List custom resource instances for a given CRD.
    pub async fn list_custom_resources(&self, crd: &CrdInfo) -> Result<Vec<ResourceEntry>> {
        let ar = crd.api_resource();
        let lp = ListParams::default();
        let api: Api<kube::api::DynamicObject> = if crd.is_cluster_scoped() {
            Api::all_with(self.client.default.clone(), &ar)
        } else {
            match &self.context.namespace {
                | Some(ns) => Api::namespaced_with(self.client.default.clone(), ns, &ar),
                | None => Api::all_with(self.client.default.clone(), &ar),
            }
        };
        let list = api.list(&lp).await?;
        Ok(list
            .items
            .iter()
            .map(|obj| {
                let data = serde_json::to_value(obj).unwrap_or_default();
                let name = obj.metadata.name.clone().unwrap_or_default();
                let namespace = obj.metadata.namespace.clone().unwrap_or_default();
                let mut columns: Vec<String> = crd
                    .columns
                    .iter()
                    .map(|col| resolve_json_path(&data, &col.json_path))
                    .collect();
                let age = obj
                    .metadata
                    .creation_timestamp
                    .as_ref()
                    .map(|t| format_age(Some(&Time(t.0))))
                    .unwrap_or_default();
                columns.push(age);
                ResourceEntry {
                    name,
                    namespace,
                    columns,
                    sort_key: None,
                }
            })
            .collect())
    }

    /// Get a single custom resource as a JSON Value.
    pub async fn get_custom_resource(&self, crd: &CrdInfo, ns: &str, name: &str) -> Result<Value> {
        let ar = crd.api_resource();
        let api: Api<kube::api::DynamicObject> = if crd.is_cluster_scoped() {
            Api::all_with(self.client.default.clone(), &ar)
        } else {
            Api::namespaced_with(self.client.default.clone(), ns, &ar)
        };
        let obj = api.get(name).await?;
        let val = serde_json::to_value(&obj)?;
        strip_managed_fields(val)
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Resolve a simple JSON path (dot-separated, starting with ".") against a
/// serde_json::Value. Supports basic dot notation like ".spec.replicas" and
/// simple array indexing like ".status.conditions[0].type".
fn resolve_json_path(data: &Value, path: &str) -> String {
    let path = path.strip_prefix('.').unwrap_or(path);
    let mut current = data;
    for segment in path.split('.') {
        // Handle array index: e.g., "conditions[0]"
        if let Some(idx_start) = segment.find('[') {
            let key = &segment[..idx_start];
            let idx_str = &segment[idx_start + 1..segment.len().saturating_sub(1)];
            if !key.is_empty() {
                current = match current.get(key) {
                    | Some(v) => v,
                    | None => return String::new(),
                };
            }
            if let Ok(idx) = idx_str.parse::<usize>() {
                current = match current.get(idx) {
                    | Some(v) => v,
                    | None => return String::new(),
                };
            }
        } else {
            current = match current.get(segment) {
                | Some(v) => v,
                | None => return String::new(),
            };
        }
    }
    match current {
        | Value::String(s) => s.clone(),
        | Value::Number(n) => n.to_string(),
        | Value::Bool(b) => b.to_string(),
        | Value::Null => String::new(),
        | other => other.to_string(),
    }
}

fn meta_name(meta: &k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta) -> String {
    meta.name.clone().unwrap_or_default()
}

fn meta_ns(meta: &k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta) -> String {
    meta.namespace.clone().unwrap_or_default()
}

/// Resolve the top-level owner of a pod (e.g. Deployment, not ReplicaSet).
/// For ReplicaSet-owned pods, strips the hash suffix to infer the Deployment
/// name.
fn resolve_top_owner(pod: &Pod) -> (String, String) {
    let owners = pod.metadata.owner_references.as_ref();
    let owner = owners.and_then(|refs| refs.first());
    match owner {
        | Some(oref) => {
            let kind = oref.kind.as_str();
            let name = &oref.name;
            match kind {
                // ReplicaSets are owned by Deployments — infer Deployment name
                | "ReplicaSet" => {
                    // Deployment-created RS names follow: <deploy>-<hash>
                    let deploy_name = name.rsplit_once('-').map(|(prefix, _)| prefix).unwrap_or(name);
                    (deploy_name.to_string(), "Deployment".to_string())
                },
                | _ => (name.clone(), kind.to_string()),
            }
        },
        | None => (meta_name(&pod.metadata), "Pod".to_string()),
    }
}

fn format_age(timestamp: Option<&Time>) -> String {
    match timestamp {
        | Some(time) => {
            let dur = Timestamp::now().duration_since(time.0);
            format_duration(dur)
        },
        | None => "N/A".into(),
    }
}

fn format_duration(dur: jiff::SignedDuration) -> String {
    let secs = dur.as_secs().max(0);
    let days = secs / 86400;
    let hours = (secs % 86400) / 3600;
    let mins = (secs % 3600) / 60;
    if days > 0 {
        format!("{}d", days)
    } else if hours > 0 {
        format!("{}h", hours)
    } else if mins > 0 {
        format!("{}m", mins)
    } else {
        format!("{}s", secs)
    }
}

pub fn parse_memory_bytes(raw: &str) -> f64 {
    if raw == "-" || raw.is_empty() {
        return 0.0;
    }
    if let Some(val) = raw.strip_suffix("Ki") {
        val.parse::<f64>().unwrap_or(0.0) * 1024.0
    } else if let Some(val) = raw.strip_suffix("Mi") {
        val.parse::<f64>().unwrap_or(0.0) * 1024.0 * 1024.0
    } else if let Some(val) = raw.strip_suffix("Gi") {
        val.parse::<f64>().unwrap_or(0.0) * 1024.0 * 1024.0 * 1024.0
    } else if let Some(val) = raw.strip_suffix("Ti") {
        val.parse::<f64>().unwrap_or(0.0) * 1024.0 * 1024.0 * 1024.0 * 1024.0
    } else if let Some(val) = raw.strip_suffix('K') {
        val.parse::<f64>().unwrap_or(0.0) * 1000.0
    } else if let Some(val) = raw.strip_suffix('M') {
        val.parse::<f64>().unwrap_or(0.0) * 1_000_000.0
    } else if let Some(val) = raw.strip_suffix('G') {
        val.parse::<f64>().unwrap_or(0.0) * 1_000_000_000.0
    } else if let Some(val) = raw.strip_suffix('T') {
        val.parse::<f64>().unwrap_or(0.0) * 1_000_000_000_000.0
    } else {
        raw.parse::<f64>().unwrap_or(0.0)
    }
}

pub fn parse_cpu_cores(raw: &str) -> f64 {
    if raw == "-" || raw.is_empty() {
        return 0.0;
    }
    if let Some(val) = raw.strip_suffix('m') {
        val.parse::<f64>().unwrap_or(0.0) / 1000.0
    } else if let Some(val) = raw.strip_suffix('n') {
        val.parse::<f64>().unwrap_or(0.0) / 1_000_000_000.0
    } else {
        raw.parse::<f64>().unwrap_or(0.0)
    }
}

fn format_memory_gb(raw: &str) -> String {
    if raw == "-" || raw.is_empty() {
        return raw.to_string();
    }
    let gb = parse_memory_bytes(raw) / (1024.0 * 1024.0 * 1024.0);
    if gb >= 10.0 {
        format!("{:.0}Gi", gb)
    } else {
        format!("{:.1}Gi", gb)
    }
}

pub fn format_memory_gb_from_bytes(bytes: f64) -> String {
    let gb = bytes / (1024.0 * 1024.0 * 1024.0);
    if gb >= 10.0 {
        format!("{:.0}Gi", gb)
    } else {
        format!("{:.1}Gi", gb)
    }
}

pub fn format_cpu_cores(cores: f64) -> String {
    if cores >= 10.0 {
        format!("{:.0}", cores)
    } else {
        format!("{:.1}", cores)
    }
}

fn strip_managed_fields(mut val: Value) -> Result<Value> {
    if let Some(metadata) = val.get_mut("metadata") {
        if let Some(map) = metadata.as_object_mut() {
            map.remove("managedFields");
        }
    }
    Ok(val)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gvk_uses_api_plural_not_a_suffix_guess() {
        let ingress = ResourceType::Ingress.api_resource();
        assert_eq!(ingress.plural, "ingresses");
        assert_eq!(ingress.api_version, "networking.k8s.io/v1");
        assert_eq!(
            ResourceType::from_gvk("networking.k8s.io/v1", "Ingress"),
            Some(ResourceType::Ingress)
        );

        let sc = ResourceType::StorageClass.api_resource();
        assert_eq!(sc.plural, "storageclasses");
        assert!(sc.kind != "storageclasss");
        assert!(ResourceType::StorageClass.is_cluster_scoped());
        assert!(!ResourceType::Ingress.is_cluster_scoped());
    }

    #[test]
    fn favorite_type_round_trips_builtin_and_custom_keys() {
        let deploy = FavoriteType::builtin(ResourceType::Deployment);
        assert_eq!(deploy.as_str(), "deployment");
        assert_eq!(deploy.builtin_type(), Some(ResourceType::Deployment));
        assert_eq!(deploy.short_label().as_ref(), "Deploy");

        let cert = FavoriteType::custom("cert-manager.io", "certificates");
        assert_eq!(cert.as_str(), "cr:cert-manager.io/certificates");
        assert_eq!(cert.custom_ref(), Some(("cert-manager.io", "certificates")));
        assert_eq!(cert.short_label().as_ref(), "certificates");
        assert!(cert.builtin_type().is_none());

        let yaml = serde_yaml::to_string(&deploy).unwrap();
        assert!(yaml.contains("deployment"));
        let back: FavoriteType = serde_yaml::from_str(&yaml).unwrap();
        assert_eq!(back, deploy);
    }

    #[test]
    fn quantity_parsers_accept_k8s_suffixes() {
        assert_eq!(parse_cpu_cores("250m"), 0.25);
        assert_eq!(parse_cpu_cores("2"), 2.0);
        assert_eq!(parse_memory_bytes("1Ki"), 1024.0);
        assert_eq!(parse_memory_bytes("1Mi"), 1024.0 * 1024.0);
        assert_eq!(parse_memory_bytes("-"), 0.0);
    }

    #[test]
    fn auth_expiry_is_the_kube_auth_error_or_http_401() {
        let plugin = kube::Error::Auth(kube::client::AuthError::ExecPluginFailed);
        assert!(ClusterFailure::from_kube(&plugin).is_auth());

        let unauthorized = kube::Error::Api(
            kube::core::Status::failure("token rejected", kube::core::response::reason::UNAUTHORIZED)
                .with_code(401)
                .boxed(),
        );
        assert!(ClusterFailure::from_kube(&unauthorized).is_auth());

        let forbidden = kube::Error::Api(
            kube::core::Status::failure("pods is forbidden", kube::core::response::reason::FORBIDDEN)
                .with_code(403)
                .boxed(),
        );
        assert!(!ClusterFailure::from_kube(&forbidden).is_auth());

        let prose = anyhow::anyhow!("Error from server (Unauthorized): Unauthorized");
        assert!(!ClusterFailure::from_anyhow(prose).is_auth());
    }

    #[test]
    fn display_error_strips_ansi_and_does_not_split_a_char() {
        let raw = "fail \u{1b}[31mnow\u{1b}[0m\nline two — tail";
        let shown = KubeClient::display_error(raw);
        assert!(!shown.contains('\u{1b}'));
        assert!(!shown.contains('\n'));
        assert!(shown.contains("fail now line two"));
        let sliced = sanitize_terminal_text("éééé", 2);
        assert_eq!(sliced, "éé");
    }

    #[test]
    fn exec_plugin_stderr_is_redirected_and_not_saved_as_interactive() {
        let mut config: Kubeconfig = serde_yaml::from_str(
            r#"
users:
- name: gke
  user:
    exec:
      apiVersion: client.authentication.k8s.io/v1beta1
      command: gke-gcloud-auth-plugin
      args: ["--flag", "bar baz"]
      interactiveMode: IfAvailable
"#,
        )
        .unwrap();
        KubeClient::silence_exec_plugins(&mut config);
        let exec = config.auth_infos[0].auth_info.as_ref().unwrap().exec.as_ref().unwrap();
        assert_eq!(exec.command.as_deref(), Some("sh"));
        assert_eq!(exec.interactive_mode, Some(kube::config::ExecInteractiveMode::Never));
        let script = exec.args.as_ref().unwrap()[1].as_str();
        assert!(script.contains("gke-gcloud-auth-plugin"));
        assert!(script.contains("'bar baz'"));
        assert!(script.contains("2>/dev/null"));
        assert!(script.contains("CLOUDSDK_CORE_DISABLE_PROMPTS=1"));
    }
}
