use {
    k8s_openapi::api::core::v1::Pod,
    kube::{
        api::{
            Api,
            ListParams,
        },
        Client,
    },
    tokio::{
        net::TcpListener,
        sync::{
            mpsc,
            watch,
        },
    },
};

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
pub enum PortForwardStatus {
    Starting,
    Active,
    Paused,
    Reconnecting {
        attempt: u32,
    },
    Error(String),
    /// Credential plugin or token rejection. Not retried.
    AuthExpired,
    Cancelled,
}

impl PortForwardStatus {
    pub fn display(&self) -> &str {
        match self {
            | Self::Starting => "Starting",
            | Self::Active => "Active",
            | Self::Paused => "Paused",
            | Self::Reconnecting { .. } => "Reconnecting",
            | Self::Error(_) => "Error",
            | Self::AuthExpired => "Login expired",
            | Self::Cancelled => "Cancelled",
        }
    }

    pub fn is_running(&self) -> bool {
        matches!(self, Self::Active | Self::Reconnecting { .. } | Self::Starting)
    }
}

/// How to resolve the target pod for a port forward.
#[derive(Clone, Debug)]
pub enum PfTarget {
    /// Forward to a specific pod by name.
    DirectPod { pod_name: String },
    /// Resolve a running pod via a label selector (for services, deployments,
    /// etc.).
    LabelSelector { selector: String },
}

impl PfTarget {
    /// Config stores the variant name and the selector/pod name as two fields.
    pub fn from_saved(target_type: &str, selector: &str) -> Self {
        match target_type {
            | "direct_pod" => {
                Self::DirectPod {
                    pod_name: selector.to_string(),
                }
            },
            | _ => {
                Self::LabelSelector {
                    selector: selector.to_string(),
                }
            },
        }
    }

    pub fn saved_parts(&self) -> (&'static str, &str) {
        match self {
            | Self::DirectPod { pod_name } => ("direct_pod", pod_name),
            | Self::LabelSelector { selector } => ("label_selector", selector),
        }
    }

    pub fn display_pod(&self) -> &str {
        match self {
            | Self::DirectPod { pod_name } => pod_name,
            | Self::LabelSelector { .. } => "(resolving)",
        }
    }
}

/// Information about one available port on a resource.
#[derive(Clone, Debug)]
pub struct PortInfo {
    /// The port on the remote pod to forward to.
    pub container_port: u16,
    /// Human-readable name (e.g., "http", "grpc").
    pub name: String,
    /// Protocol (e.g., "TCP").
    pub protocol: String,
}

pub struct PfPorts {
    pub local: u16,
    pub remote: u16,
}

/// Who the forward was created from. Name is stored separately from the
/// display label so port edits don't have to parse "Deployments/name".
pub struct PfIdentity {
    pub type_key: String,
    pub name: String,
}

impl PfIdentity {
    pub fn type_label(&self) -> &str {
        &self.type_key
    }

    pub fn resource_label(&self) -> String {
        let display = crate::k8s::ResourceType::from_singular_name(&self.type_key)
            .map(|rt| rt.display_name())
            .unwrap_or(self.type_key.as_str());
        format!("{display}/{}", self.name)
    }
}

#[derive(Clone)]
pub struct PortForwardRequest {
    pub namespace: String,
    pub context: String,
    pub type_key: String,
    pub resource_name: String,
    pub local_port: u16,
    pub remote_port: u16,
    pub target: PfTarget,
}

impl PortForwardRequest {
    pub fn from_saved(saved: &crate::config::SavedPortForward) -> Self {
        Self {
            namespace: saved.namespace.clone(),
            context: saved.context.clone(),
            type_key: saved.resource.r#type.clone(),
            resource_name: saved.resource.name.clone(),
            local_port: saved.port.local_port,
            remote_port: saved.port.remote_port,
            target: PfTarget::from_saved(&saved.target.target_type, &saved.target.selector),
        }
    }

    pub fn to_saved(&self, paused: bool) -> crate::config::SavedPortForward {
        let (target_type, selector) = self.target.saved_parts();
        crate::config::SavedPortForward {
            resource: crate::config::SavedPfResource {
                r#type: self.type_key.clone(),
                name: self.resource_name.clone(),
            },
            namespace: self.namespace.clone(),
            context: self.context.clone(),
            port: crate::config::SavedPfPorts {
                local_port: self.local_port,
                remote_port: self.remote_port,
            },
            target: crate::config::SavedPfTarget {
                target_type: target_type.to_string(),
                selector: selector.to_string(),
            },
            paused,
        }
    }
}

pub struct PortForwardEntry {
    pub id: usize,
    pub port: PfPorts,
    pub pod_name: String,
    pub namespace: String,
    pub context: String,
    pub identity: PfIdentity,
    pub status: PortForwardStatus,
    pub connections: usize,
    pub target: PfTarget,
    cancel_tx: watch::Sender<bool>,
    pause_tx: watch::Sender<bool>,
}

enum PfUpdate {
    Status { id: usize, status: PortForwardStatus },
    Connection { id: usize, delta: i32 },
    PodResolved { id: usize, pod_name: String },
}

// ---------------------------------------------------------------------------
// Manager
// ---------------------------------------------------------------------------

pub struct PortForwardManager {
    entries: Vec<PortForwardEntry>,
    next_id: usize,
    update_rx: mpsc::UnboundedReceiver<PfUpdate>,
    update_tx: mpsc::UnboundedSender<PfUpdate>,
}

impl PortForwardManager {
    pub fn new() -> Self {
        let (update_tx, update_rx) = mpsc::unbounded_channel();
        Self {
            entries: Vec::new(),
            next_id: 0,
            update_rx,
            update_tx,
        }
    }

    pub fn entries(&self) -> &[PortForwardEntry] {
        &self.entries
    }

    #[allow(dead_code)]
    pub fn get(&self, id: usize) -> Option<&PortForwardEntry> {
        self.entries.iter().find(|e| e.id == id)
    }

    pub fn create(&mut self, client: Client, req: PortForwardRequest) -> usize {
        let namespace = req.namespace.clone();
        let target = req.target.clone();
        let local_port = req.local_port;
        let remote_port = req.remote_port;
        let (id, cancel_rx, pause_rx) = self.insert(req, PortForwardStatus::Starting, false);
        let update_tx = self.update_tx.clone();
        tokio::spawn(port_forward_task(
            id,
            client,
            namespace,
            target,
            local_port,
            remote_port,
            cancel_rx,
            pause_rx,
            update_tx,
        ));
        id
    }

    /// Create a port forward entry in Paused state without spawning a
    /// background task. Used when restoring saved port forwards that were
    /// paused.
    pub fn create_paused(&mut self, req: PortForwardRequest) -> usize {
        self.insert(req, PortForwardStatus::Paused, true).0
    }

    fn insert(
        &mut self,
        req: PortForwardRequest,
        status: PortForwardStatus,
        paused: bool,
    ) -> (usize, watch::Receiver<bool>, watch::Receiver<bool>) {
        let id = self.next_id;
        self.next_id += 1;
        let (cancel_tx, cancel_rx) = watch::channel(false);
        let (pause_tx, pause_rx) = watch::channel(paused);
        self.entries.push(PortForwardEntry {
            id,
            port: PfPorts {
                local: req.local_port,
                remote: req.remote_port,
            },
            pod_name: req.target.display_pod().to_string(),
            namespace: req.namespace,
            context: req.context,
            identity: PfIdentity {
                type_key: req.type_key,
                name: req.resource_name,
            },
            status,
            connections: 0,
            target: req.target,
            cancel_tx,
            pause_tx,
        });
        (id, cancel_rx, pause_rx)
    }

    /// Spawn a new background task for a paused or errored entry.
    /// Returns true if a task was spawned.
    pub fn resume_spawn(&mut self, id: usize, client: Client) -> bool {
        let entry = match self.entries.iter_mut().find(|e| e.id == id) {
            | Some(e)
                if matches!(
                    e.status,
                    PortForwardStatus::Paused | PortForwardStatus::Error(_) | PortForwardStatus::AuthExpired
                ) =>
            {
                e
            },
            | _ => return false,
        };

        let target = entry.target.clone();

        let local_port = entry.port.local;
        let remote_port = entry.port.remote;
        let namespace = entry.namespace.clone();

        // Replace channels so the new task gets fresh ones
        let (cancel_tx, cancel_rx) = watch::channel(false);
        let (pause_tx, pause_rx) = watch::channel(false);
        entry.cancel_tx = cancel_tx;
        entry.pause_tx = pause_tx;
        entry.status = PortForwardStatus::Starting;

        let update_tx = self.update_tx.clone();

        tokio::spawn(port_forward_task(
            id,
            client,
            namespace,
            target,
            local_port,
            remote_port,
            cancel_rx,
            pause_rx,
            update_tx,
        ));

        true
    }

    pub fn cancel(&mut self, id: usize) {
        if let Some(entry) = self.entries.iter_mut().find(|e| e.id == id) {
            let _ = entry.cancel_tx.send(true);
            entry.status = PortForwardStatus::Cancelled;
        }
    }

    /// Pause every running forward. Returns the ids so a later retry can resume
    /// them.
    pub fn pause_running(&mut self) -> Vec<usize> {
        let ids: Vec<usize> = self
            .entries
            .iter()
            .filter(|entry| entry.status.is_running())
            .map(|entry| entry.id)
            .collect();
        for id in &ids {
            self.pause(*id);
        }
        ids
    }

    pub fn pause(&mut self, id: usize) {
        if let Some(entry) = self.entries.iter_mut().find(|e| e.id == id) {
            if entry.status.is_running() {
                let _ = entry.pause_tx.send(true);
            }
            // Allow pausing from running or error states
            if entry.status.is_running() || matches!(entry.status, PortForwardStatus::Error(_)) {
                entry.status = PortForwardStatus::Paused;
            }
        }
    }

    pub fn resume(&mut self, id: usize) {
        if let Some(entry) = self.entries.iter_mut().find(|e| e.id == id) {
            if matches!(entry.status, PortForwardStatus::Paused) {
                let _ = entry.pause_tx.send(false);
                entry.status = PortForwardStatus::Active;
            }
        }
    }

    /// Cancel all active port forwards.
    pub fn cancel_all(&mut self) {
        for entry in &mut self.entries {
            if !matches!(entry.status, PortForwardStatus::Cancelled) {
                let _ = entry.cancel_tx.send(true);
                entry.status = PortForwardStatus::Cancelled;
            }
        }
        self.entries.clear();
    }

    /// Remove entries that have been cancelled.
    pub fn remove_cancelled(&mut self) {
        self.entries
            .retain(|e| !matches!(e.status, PortForwardStatus::Cancelled));
    }

    /// Drain pending status updates from background tasks.
    pub fn poll_updates(&mut self) {
        while let Ok(update) = self.update_rx.try_recv() {
            match update {
                | PfUpdate::Status { id, status } => {
                    if let Some(entry) = self.entries.iter_mut().find(|e| e.id == id) {
                        // Don't override user-initiated pause/cancel
                        if !matches!(entry.status, PortForwardStatus::Cancelled | PortForwardStatus::Paused) {
                            entry.status = status;
                        }
                    }
                },
                | PfUpdate::Connection { id, delta } => {
                    if let Some(entry) = self.entries.iter_mut().find(|e| e.id == id) {
                        if delta > 0 {
                            entry.connections = entry.connections.saturating_add(delta as usize);
                        } else {
                            entry.connections = entry.connections.saturating_sub(delta.unsigned_abs() as usize);
                        }
                    }
                },
                | PfUpdate::PodResolved { id, pod_name } => {
                    if let Some(entry) = self.entries.iter_mut().find(|e| e.id == id) {
                        entry.pod_name = pod_name;
                    }
                },
            }
        }
    }
}

impl Drop for PortForwardManager {
    fn drop(&mut self) {
        self.cancel_all();
    }
}

// ---------------------------------------------------------------------------
// Pod resolution
// ---------------------------------------------------------------------------

async fn resolve_pod(client: &Client, namespace: &str, target: &PfTarget) -> Option<String> {
    match target {
        | PfTarget::DirectPod { pod_name } => Some(pod_name.clone()),
        | PfTarget::LabelSelector { selector } => {
            let api: Api<Pod> = Api::namespaced(client.clone(), namespace);
            let lp = ListParams::default().labels(selector);
            let pods = api.list(&lp).await.ok()?;
            // Prefer a Running pod
            pods.items
                .iter()
                .find(|p| p.status.as_ref().and_then(|s| s.phase.as_deref()) == Some("Running"))
                .or_else(|| pods.items.first())
                .and_then(|p| p.metadata.name.clone())
        },
    }
}

// ---------------------------------------------------------------------------
// Background task
// ---------------------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
async fn port_forward_task(
    id: usize,
    client: Client,
    namespace: String,
    target: PfTarget,
    local_port: u16,
    remote_port: u16,
    mut cancel_rx: watch::Receiver<bool>,
    mut pause_rx: watch::Receiver<bool>,
    update_tx: mpsc::UnboundedSender<PfUpdate>,
) {
    let listener = match TcpListener::bind(("127.0.0.1", local_port)).await {
        | Ok(l) => l,
        | Err(_) => {
            let _ = update_tx.send(PfUpdate::Status {
                id,
                status: PortForwardStatus::Error(format!("Port :{} in use", local_port)),
            });
            return;
        },
    };

    let _ = update_tx.send(PfUpdate::Status {
        id,
        status: PortForwardStatus::Active,
    });

    let mut listener = Some(listener);

    loop {
        // If paused, drop the listener to free the port and wait for
        // resume or cancel.
        if *pause_rx.borrow() {
            drop(listener.take());
            loop {
                tokio::select! {
                    biased;
                    _ = cancel_rx.changed() => {
                        if *cancel_rx.borrow() {
                            return;
                        }
                    }
                    _ = pause_rx.changed() => {
                        if !*pause_rx.borrow() {
                            // Resumed — re-bind the port
                            match TcpListener::bind(("127.0.0.1", local_port)).await {
                                | Ok(l) => {
                                    listener = Some(l);
                                    let _ = update_tx.send(PfUpdate::Status {
                                        id,
                                        status: PortForwardStatus::Active,
                                    });
                                    break;
                                },
                                | Err(e) => {
                                    let _ = update_tx.send(PfUpdate::Status {
                                        id,
                                        status: PortForwardStatus::Error(format!("Re-bind :{} {}", local_port, e)),
                                    });
                                    return;
                                },
                            }
                        }
                    }
                }
            }
        }

        // Bound on start, and rebound before leaving the pause branch.
        let Some(l) = listener.as_ref() else {
            let _ = update_tx.send(PfUpdate::Status {
                id,
                status: PortForwardStatus::Error(format!("Port :{local_port} is not bound")),
            });
            return;
        };

        tokio::select! {
            biased;
            _ = cancel_rx.changed() => {
                if *cancel_rx.borrow() {
                    return;
                }
            }
            _ = pause_rx.changed() => {
                // Will drop listener at top of loop
                continue;
            }
            accept = l.accept() => {
                match accept {
                    | Ok((tcp_stream, _)) => {
                        // Resolve current pod for this connection
                        let pod_name = match resolve_pod(&client, &namespace, &target).await {
                            | Some(name) => {
                                let _ = update_tx.send(PfUpdate::PodResolved {
                                    id,
                                    pod_name: name.clone(),
                                });
                                name
                            },
                            | None => {
                                let _ = update_tx.send(PfUpdate::Status {
                                    id,
                                    status: PortForwardStatus::Reconnecting { attempt: 1 },
                                });
                                drop(tcp_stream);
                                continue;
                            },
                        };

                        let client = client.clone();
                        let ns = namespace.clone();
                        let tx = update_tx.clone();
                        let cancel_rx2 = cancel_rx.clone();

                        tokio::spawn(async move {
                            let _ = tx.send(PfUpdate::Connection { id, delta: 1 });
                            handle_pf_connection(
                                id,
                                client,
                                &ns,
                                &pod_name,
                                remote_port,
                                tcp_stream,
                                cancel_rx2,
                                &tx,
                            )
                            .await;
                            let _ = tx.send(PfUpdate::Connection { id, delta: -1 });
                        });
                    },
                    | Err(e) => {
                        let _ = update_tx.send(PfUpdate::Status {
                            id,
                            status: PortForwardStatus::Error(format!("Accept: {}", e)),
                        });
                        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                    },
                }
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn handle_pf_connection(
    id: usize,
    client: Client,
    namespace: &str,
    pod_name: &str,
    remote_port: u16,
    mut tcp_stream: tokio::net::TcpStream,
    mut cancel_rx: watch::Receiver<bool>,
    update_tx: &mpsc::UnboundedSender<PfUpdate>,
) {
    let pods: Api<Pod> = Api::namespaced(client, namespace);
    let mut attempt = 0u32;

    loop {
        if *cancel_rx.borrow() {
            return;
        }

        match pods.portforward(pod_name, &[remote_port]).await {
            | Ok(mut pf) => {
                let _ = update_tx.send(PfUpdate::Status {
                    id,
                    status: PortForwardStatus::Active,
                });

                if let Some(mut pf_stream) = pf.take_stream(remote_port) {
                    // Copy data bidirectionally until one side closes or errors
                    tokio::select! {
                        result = tokio::io::copy_bidirectional(&mut tcp_stream, &mut pf_stream) => {
                            drop(pf_stream);
                            let _ = pf.join().await;
                            match result {
                                | Ok(_) | Err(_) => return,
                            }
                        }
                        _ = cancel_rx.changed() => {
                            return;
                        }
                    }
                } else {
                    let _ = pf.join().await;
                    return;
                }
            },
            | Err(e) => {
                let failure = crate::k8s::ClusterFailure::from_kube(&e);
                if failure.is_auth() {
                    let _ = update_tx.send(PfUpdate::Status {
                        id,
                        status: PortForwardStatus::AuthExpired,
                    });
                    return;
                }
                attempt += 1;
                if attempt > 10 {
                    let _ = update_tx.send(PfUpdate::Status {
                        id,
                        status: PortForwardStatus::Error(format!("Portforward: {}", failure.detail())),
                    });
                    return;
                }
                let _ = update_tx.send(PfUpdate::Status {
                    id,
                    status: PortForwardStatus::Reconnecting { attempt },
                });
                let backoff = std::time::Duration::from_millis(500 * 2u64.pow(attempt.min(6)));
                tokio::select! {
                    _ = tokio::time::sleep(backoff) => {},
                    _ = cancel_rx.changed() => {
                        if *cancel_rx.borrow() {
                            return;
                        }
                    }
                }
            },
        }
    }
}

// ---------------------------------------------------------------------------
// Port extraction helpers
// ---------------------------------------------------------------------------

/// Extract available ports from a resource's JSON value.
pub fn extract_ports(rt: crate::k8s::ResourceType, value: &serde_json::Value) -> Vec<PortInfo> {
    use crate::k8s::ResourceType;

    match rt {
        | ResourceType::Service => extract_service_ports(value),
        | ResourceType::Pod => extract_pod_ports(value),
        | ResourceType::Deployment
        | ResourceType::StatefulSet
        | ResourceType::DaemonSet
        | ResourceType::ReplicaSet
        | ResourceType::Job
        | ResourceType::CronJob => extract_template_ports(value),
        | _ => Vec::new(),
    }
}

fn extract_service_ports(value: &serde_json::Value) -> Vec<PortInfo> {
    let ports = value
        .get("spec")
        .and_then(|s| s.get("ports"))
        .and_then(|p| p.as_array());

    match ports {
        | Some(arr) => {
            arr.iter()
                .filter_map(|p| {
                    let target = p
                        .get("targetPort")
                        .and_then(|v| v.as_u64().or_else(|| v.as_str().and_then(|s| s.parse().ok())))
                        .unwrap_or_else(|| p.get("port").and_then(|v| v.as_u64()).unwrap_or(0));

                    if target == 0 || target > 65535 {
                        return None;
                    }

                    Some(PortInfo {
                        container_port: target as u16,
                        name: p.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                        protocol: p.get("protocol").and_then(|v| v.as_str()).unwrap_or("TCP").to_string(),
                    })
                })
                .collect()
        },
        | None => Vec::new(),
    }
}

fn extract_container_ports(containers: &serde_json::Value) -> Vec<PortInfo> {
    let arr = match containers.as_array() {
        | Some(a) => a,
        | None => return Vec::new(),
    };

    arr.iter()
        .flat_map(|c| {
            c.get("ports")
                .and_then(|p| p.as_array())
                .into_iter()
                .flatten()
                .filter_map(|p| {
                    let port = p.get("containerPort").and_then(|v| v.as_u64())?;
                    if port == 0 || port > 65535 {
                        return None;
                    }
                    Some(PortInfo {
                        container_port: port as u16,
                        name: p.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                        protocol: p.get("protocol").and_then(|v| v.as_str()).unwrap_or("TCP").to_string(),
                    })
                })
        })
        .collect()
}

fn extract_pod_ports(value: &serde_json::Value) -> Vec<PortInfo> {
    value
        .get("spec")
        .and_then(|s| s.get("containers"))
        .map(extract_container_ports)
        .unwrap_or_default()
}

fn extract_template_ports(value: &serde_json::Value) -> Vec<PortInfo> {
    value
        .get("spec")
        .and_then(|s| s.get("template"))
        .and_then(|t| t.get("spec"))
        .and_then(|s| s.get("containers"))
        .map(extract_container_ports)
        .unwrap_or_default()
}

/// Extract a label selector string from a resource's JSON value for pod
/// resolution.
pub fn extract_selector(rt: crate::k8s::ResourceType, value: &serde_json::Value) -> Option<String> {
    use crate::k8s::ResourceType;

    match rt {
        | ResourceType::Job => {
            // Jobs label their pods with job-name=<name>
            let name = value
                .get("metadata")
                .and_then(|m| m.get("name"))
                .and_then(|n| n.as_str());
            return name.map(|n| format!("job-name={}", n));
        },
        | ResourceType::CronJob => {
            // CronJob pods are labeled via the latest Job; use template labels
            let labels = value
                .get("spec")
                .and_then(|s| s.get("jobTemplate"))
                .and_then(|j| j.get("spec"))
                .and_then(|s| s.get("template"))
                .and_then(|t| t.get("metadata"))
                .and_then(|m| m.get("labels"))
                .and_then(|l| l.as_object());
            return labels.map(|map| {
                map.iter()
                    .map(|(k, v)| format!("{}={}", k, v.as_str().unwrap_or("")))
                    .collect::<Vec<_>>()
                    .join(",")
            });
        },
        | _ => {},
    }

    let labels = match rt {
        | ResourceType::Service => {
            value
                .get("spec")
                .and_then(|s| s.get("selector"))
                .and_then(|s| s.as_object())
        },
        | ResourceType::Deployment | ResourceType::StatefulSet | ResourceType::DaemonSet | ResourceType::ReplicaSet => {
            value
                .get("spec")
                .and_then(|s| s.get("selector"))
                .and_then(|s| s.get("matchLabels"))
                .and_then(|m| m.as_object())
        },
        | _ => None,
    };

    labels.map(|map| {
        map.iter()
            .map(|(k, v)| format!("{}={}", k, v.as_str().unwrap_or("")))
            .collect::<Vec<_>>()
            .join(",")
    })
}
