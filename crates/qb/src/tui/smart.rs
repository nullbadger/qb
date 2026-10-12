use {
    crate::k8s::{
        CrdInfo,
        ResourceType,
    },
    base64::{
        engine::general_purpose::STANDARD,
        Engine,
    },
    ratatui::prelude::*,
    serde_json::Value,
    std::collections::{
        HashMap,
        HashSet,
    },
};

// ---------------------------------------------------------------------------
// Secret detail state
// ---------------------------------------------------------------------------

pub struct SecretDetailState {
    pub keys: Vec<String>,
    pub values: HashMap<String, String>,
    pub selected: usize,
    pub decoded: HashSet<String>,
}

impl SecretDetailState {
    pub fn from_value(v: &Value) -> Self {
        let mut keys = Vec::new();
        let mut values = HashMap::new();
        if let Some(data) = v.get("data").and_then(|d| d.as_object()) {
            for (k, v) in data {
                keys.push(k.clone());
                values.insert(k.clone(), v.as_str().unwrap_or("").to_string());
            }
        }
        keys.sort();
        Self {
            keys,
            values,
            selected: 0,
            decoded: HashSet::new(),
        }
    }

    pub fn update_values(&mut self, v: &Value) {
        if let Some(data) = v.get("data").and_then(|d| d.as_object()) {
            let mut new_keys = Vec::new();
            let mut new_values = HashMap::new();
            for (k, v) in data {
                new_keys.push(k.clone());
                new_values.insert(k.clone(), v.as_str().unwrap_or("").to_string());
            }
            new_keys.sort();
            self.keys = new_keys;
            self.values = new_values;
            // Clamp selection
            if !self.keys.is_empty() {
                self.selected = self.selected.min(self.keys.len() - 1);
            }
            // Remove decoded entries for keys that no longer exist
            self.decoded.retain(|k| self.keys.contains(k));
        }
    }

    pub fn nav_up(&mut self) {
        if self.selected > 0 {
            self.selected -= 1;
        }
    }

    pub fn nav_down(&mut self) {
        if !self.keys.is_empty() && self.selected + 1 < self.keys.len() {
            self.selected += 1;
        }
    }

    pub fn toggle_decode(&mut self) {
        if let Some(key) = self.keys.get(self.selected) {
            let key = key.clone();
            if self.decoded.contains(&key) {
                self.decoded.remove(&key);
            } else {
                self.decoded.insert(key);
            }
        }
    }

    /// Returns the decoded plaintext value for the selected key (always decodes
    /// base64).
    pub fn selected_plaintext_value(&self) -> Option<String> {
        let key = self.keys.get(self.selected)?;
        let b64 = self.values.get(key)?;
        let bytes = STANDARD.decode(b64).ok()?;
        String::from_utf8(bytes).ok()
    }

    pub fn selected_key(&self) -> Option<&str> {
        self.keys.get(self.selected).map(|s| s.as_str())
    }
}

// ---------------------------------------------------------------------------
// Dict entry selection state (labels/annotations)
// ---------------------------------------------------------------------------

/// Mutable state passed into renderers so `dict_section` can register
/// selectable entries.
pub struct DictState {
    /// All dict entries across all sections: (qualified_key, display_key,
    /// value).
    pub entries: Vec<(String, String, String)>,
    /// Line offset where each entry starts in the rendered output.
    pub line_offsets: Vec<usize>,
    /// Currently selected entry index, if any.
    pub cursor: Option<usize>,
    /// Which entries are expanded.
    pub expanded: std::collections::HashSet<String>,
}

// ---------------------------------------------------------------------------
// Dispatch
// ---------------------------------------------------------------------------

pub fn render(
    rt: ResourceType,
    v: &Value,
    secret_state: Option<&mut SecretDetailState>,
    dict_state: &mut DictState,
) -> Vec<Line<'static>> {
    dict_state.entries.clear();
    dict_state.line_offsets.clear();
    let ds = &mut *dict_state;
    match rt {
        | ResourceType::Deployment => render_deployment(v, ds),
        | ResourceType::StatefulSet => render_statefulset(v, ds),
        | ResourceType::DaemonSet => render_daemonset(v, ds),
        | ResourceType::ReplicaSet => render_replicaset(v, ds),
        | ResourceType::Pod => render_pod(v, ds),
        | ResourceType::CronJob => render_cronjob(v, ds),
        | ResourceType::Job => render_job(v, ds),
        | ResourceType::HorizontalPodAutoscaler => render_hpa(v, ds),
        | ResourceType::ConfigMap => render_configmap(v, ds),
        | ResourceType::Secret => render_secret(v, secret_state, ds),
        | ResourceType::Service => render_service(v, ds),
        | ResourceType::Ingress => render_ingress(v, ds),
        | ResourceType::Endpoints => render_endpoints(v, ds),
        | ResourceType::NetworkPolicy => render_network_policy(v, ds),
        | ResourceType::PersistentVolumeClaim => render_pvc(v, ds),
        | ResourceType::PersistentVolume => render_pv(v, ds),
        | ResourceType::StorageClass => render_storage_class(v, ds),
        | ResourceType::ServiceAccount => render_service_account(v, ds),
        | ResourceType::Role => render_role(v, ds),
        | ResourceType::RoleBinding => render_role_binding(v, ds),
        | ResourceType::ClusterRole => render_role(v, ds),
        | ResourceType::ClusterRoleBinding => render_role_binding(v, ds),
        | ResourceType::Node => render_node(v, ds),
        | ResourceType::Namespace => render_namespace(v, ds),
        | ResourceType::Event => render_event(v, ds),
        | ResourceType::CustomResourceDefinition => render_crd(v, ds),
    }
}

// ---------------------------------------------------------------------------
// Per-type renderers
// ---------------------------------------------------------------------------

fn render_deployment(v: &Value, ds: &mut DictState) -> Vec<Line<'static>> {
    let mut l = metadata_lines(v, "Deployment", ds);

    let replicas = ji(v, "spec.replicas").unwrap_or(0);
    let ready = ji(v, "status.readyReplicas").unwrap_or(0);
    let updated = ji(v, "status.updatedReplicas").unwrap_or(0);
    let available = ji(v, "status.availableReplicas").unwrap_or(0);
    field(
        &mut l,
        "Replicas",
        &format!(
            "{}/{} ready, {} updated, {} available",
            ready, replicas, updated, available
        ),
    );

    let strategy = js(v, "spec.strategy.type");
    if !strategy.is_empty() {
        let mut s = strategy.clone();
        if strategy == "RollingUpdate" {
            let max_surge = js(v, "spec.strategy.rollingUpdate.maxSurge");
            let max_unavail = js(v, "spec.strategy.rollingUpdate.maxUnavailable");
            s = format!(
                "{} (maxSurge: {}, maxUnavailable: {})",
                strategy, max_surge, max_unavail
            );
        }
        field(&mut l, "Strategy", &s);
    }

    let selector = labels_str(v, "spec.selector.matchLabels");
    if !selector.is_empty() {
        field(&mut l, "Selector", &selector);
    }

    containers_section(&mut l, v, "spec.template.spec.containers");
    conditions_section(&mut l, v);
    l
}

fn render_statefulset(v: &Value, ds: &mut DictState) -> Vec<Line<'static>> {
    let mut l = metadata_lines(v, "StatefulSet", ds);

    let replicas = ji(v, "spec.replicas").unwrap_or(0);
    let ready = ji(v, "status.readyReplicas").unwrap_or(0);
    field(&mut l, "Replicas", &format!("{}/{} ready", ready, replicas));

    let svc = js(v, "spec.serviceName");
    if !svc.is_empty() {
        field(&mut l, "Service", &svc);
    }

    let policy = js(v, "spec.updateStrategy.type");
    if !policy.is_empty() {
        field(&mut l, "Update Strategy", &policy);
    }

    containers_section(&mut l, v, "spec.template.spec.containers");
    conditions_section(&mut l, v);
    l
}

fn render_replicaset(v: &Value, ds: &mut DictState) -> Vec<Line<'static>> {
    let mut l = metadata_lines(v, "ReplicaSet", ds);

    let desired = ji(v, "spec.replicas").unwrap_or(0);
    let current = ji(v, "status.replicas").unwrap_or(0);
    let ready = ji(v, "status.readyReplicas").unwrap_or(0);
    field(
        &mut l,
        "Replicas",
        &format!("{} desired, {} current, {} ready", desired, current, ready),
    );

    let selector = labels_str(v, "spec.selector.matchLabels");
    if !selector.is_empty() {
        field(&mut l, "Selector", &selector);
    }

    if let Some(owners) = jget(v, "metadata.ownerReferences").and_then(|v| v.as_array()) {
        for o in owners {
            let kind = o.get("kind").and_then(|v| v.as_str()).unwrap_or("");
            let name = o.get("name").and_then(|v| v.as_str()).unwrap_or("");
            field(&mut l, "Owner", &format!("{}/{}", kind, name));
        }
    }

    conditions_section(&mut l, v);
    l
}

fn render_pod(v: &Value, ds: &mut DictState) -> Vec<Line<'static>> {
    let mut l = metadata_lines(v, "Pod", ds);

    field(&mut l, "Phase", &js(v, "status.phase"));

    let reason = js(v, "status.reason");
    if !reason.is_empty() {
        field(&mut l, "Reason", &reason);
    }

    let node = js(v, "spec.nodeName");
    if !node.is_empty() {
        field(&mut l, "Node", &node);
    }

    let pod_ip = js(v, "status.podIP");
    let host_ip = js(v, "status.hostIP");
    if !pod_ip.is_empty() {
        field(&mut l, "Pod IP", &pod_ip);
    }
    if !host_ip.is_empty() {
        field(&mut l, "Host IP", &host_ip);
    }

    // Container statuses
    if let Some(statuses) = jget(v, "status.containerStatuses").and_then(|v| v.as_array()) {
        blank(&mut l);
        section(&mut l, "Containers");
        for cs in statuses {
            let name = cs.get("name").and_then(|v| v.as_str()).unwrap_or("");
            let image = cs.get("image").and_then(|v| v.as_str()).unwrap_or("");
            let ready = cs.get("ready").and_then(|v| v.as_bool()).unwrap_or(false);
            let state = container_state_str(cs.get("state"));

            subheading(&mut l, &format!("▸ {}", name));
            field(&mut l, "    Image", image);
            field(&mut l, "    State", &state);
            if let Some(last) = cs.get("lastState") {
                let last_str = container_state_str(Some(last));
                if last_str != "Unknown" && last_str != "Waiting" {
                    l.push(Line::from(vec![
                        Span::styled(
                            format!("{:<w$}", "    Last State", w = 18),
                            Style::default().fg(Color::DarkGray),
                        ),
                        Span::styled(last_str, Style::default().fg(Color::Yellow)),
                    ]));
                    if let Some(terminated) = last.get("terminated") {
                        if let Some(reason) = terminated.get("reason").and_then(|v| v.as_str()) {
                            let exit_code = terminated.get("exitCode").and_then(|v| v.as_i64()).unwrap_or(-1);
                            let finished = terminated.get("finishedAt").and_then(|v| v.as_str()).unwrap_or("");
                            l.push(Line::from(vec![
                                Span::styled(
                                    format!("{:<w$}", "    Reason", w = 18),
                                    Style::default().fg(Color::DarkGray),
                                ),
                                Span::styled(
                                    format!("{} (exit code: {})", reason, exit_code),
                                    Style::default().fg(Color::Red),
                                ),
                            ]));
                            if !finished.is_empty() {
                                l.push(Line::from(vec![
                                    Span::styled(
                                        format!("{:<w$}", "    Finished", w = 18),
                                        Style::default().fg(Color::DarkGray),
                                    ),
                                    Span::styled(finished.to_string(), Style::default().fg(Color::White)),
                                ]));
                            }
                        }
                    }
                }
            }
            field(&mut l, "    Ready", if ready { "true" } else { "false" });
            let restarts = cs.get("restartCount").and_then(|v| v.as_i64()).unwrap_or(0);
            if restarts > 0 {
                let restart_style = if restarts > 5 {
                    Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(Color::Yellow)
                };
                l.push(Line::from(vec![
                    Span::styled(
                        format!("{:<w$}", "    Restarts", w = 18),
                        Style::default().fg(Color::DarkGray),
                    ),
                    Span::styled(format!("{}", restarts), restart_style),
                ]));
            }
        }
    } else {
        // Fall back to spec containers if no status yet
        containers_section(&mut l, v, "spec.containers");
    }

    conditions_section(&mut l, v);
    l
}

fn render_cronjob(v: &Value, ds: &mut DictState) -> Vec<Line<'static>> {
    let mut l = metadata_lines(v, "CronJob", ds);

    field(&mut l, "Schedule", &js(v, "spec.schedule"));

    let suspend = jget(v, "spec.suspend").and_then(|v| v.as_bool()).unwrap_or(false);
    field(&mut l, "Suspend", if suspend { "true" } else { "false" });

    let policy = js(v, "spec.concurrencyPolicy");
    if !policy.is_empty() {
        field(&mut l, "Concurrency", &policy);
    }

    let last = js(v, "status.lastScheduleTime");
    if !last.is_empty() {
        field(&mut l, "Last Schedule", &last);
    }

    let active = jget(v, "status.active")
        .and_then(|v| v.as_array())
        .map(|a| a.len())
        .unwrap_or(0);
    field(&mut l, "Active Jobs", &active.to_string());

    l
}

fn render_job(v: &Value, ds: &mut DictState) -> Vec<Line<'static>> {
    let mut l = metadata_lines(v, "Job", ds);

    let completions = ji(v, "spec.completions").unwrap_or(1);
    let succeeded = ji(v, "status.succeeded").unwrap_or(0);
    field(&mut l, "Completions", &format!("{}/{}", succeeded, completions));

    let parallelism = ji(v, "spec.parallelism").unwrap_or(1);
    field(&mut l, "Parallelism", &parallelism.to_string());

    let start = js(v, "status.startTime");
    if !start.is_empty() {
        field(&mut l, "Start Time", &start);
    }

    let end = js(v, "status.completionTime");
    if !end.is_empty() {
        field(&mut l, "Completion", &end);
    }

    if let Some(owners) = jget(v, "metadata.ownerReferences").and_then(|v| v.as_array()) {
        for o in owners {
            let kind = o.get("kind").and_then(|v| v.as_str()).unwrap_or("");
            let name = o.get("name").and_then(|v| v.as_str()).unwrap_or("");
            field(&mut l, "Owner", &format!("{}/{}", kind, name));
        }
    }

    conditions_section(&mut l, v);
    l
}

fn render_configmap(v: &Value, ds: &mut DictState) -> Vec<Line<'static>> {
    let mut l = metadata_lines(v, "ConfigMap", ds);

    if let Some(data) = v.get("data").and_then(|d| d.as_object()) {
        blank(&mut l);
        section(&mut l, &format!("Data ({} keys)", data.len()));
        for (k, val) in data {
            let val_str = val.as_str().unwrap_or("");
            let line_count = val_str.lines().count();
            if line_count > 1 {
                subheading(&mut l, &format!("▸ {}  ({} lines)", k, line_count));
                // Show first 5 lines
                for (i, line) in val_str.lines().take(5).enumerate() {
                    let truncated = if line.len() > 80 { &line[..80] } else { line };
                    l.push(Line::from(Span::styled(
                        format!("      {}", truncated),
                        Style::default().fg(Color::DarkGray),
                    )));
                    if i == 4 && line_count > 5 {
                        l.push(Line::from(Span::styled(
                            format!("      ... ({} more lines)", line_count - 5),
                            Style::default().fg(Color::DarkGray),
                        )));
                    }
                }
            } else {
                let display = if val_str.len() > 60 {
                    format!("{}...", &val_str[..60])
                } else {
                    val_str.to_string()
                };
                field(&mut l, &format!("  {}", k), &display);
            }
        }
    }

    if let Some(bdata) = v.get("binaryData").and_then(|d| d.as_object()) {
        if !bdata.is_empty() {
            blank(&mut l);
            section(&mut l, &format!("Binary Data ({} keys)", bdata.len()));
            for k in bdata.keys() {
                field(&mut l, &format!("  {}", k), "(binary)");
            }
        }
    }

    l
}

fn render_secret(v: &Value, state: Option<&mut SecretDetailState>, ds: &mut DictState) -> Vec<Line<'static>> {
    let mut l = metadata_lines(v, "Secret", ds);

    let stype = js(v, "type");
    if !stype.is_empty() {
        field(&mut l, "Type", &stype);
    }

    let state = match state {
        | Some(s) => s,
        | None => return l,
    };

    if state.keys.is_empty() {
        field(&mut l, "Data", "(empty)");
        return l;
    }

    blank(&mut l);
    section(&mut l, &format!("Data ({} keys)", state.keys.len()));

    // Record the line offset where each key starts
    for (i, key) in state.keys.iter().enumerate() {
        let is_selected = i == state.selected;
        let is_decoded = state.decoded.contains(key);
        let b64 = state.values.get(key).map(|s| s.as_str()).unwrap_or("");
        let byte_len = STANDARD.decode(b64).map(|b| b.len()).unwrap_or(0);

        let marker = if is_selected { "▸" } else { " " };
        let size_str = format!("({} bytes)", byte_len);
        let status_icon = if is_decoded { "[decoded]" } else { "[hidden]" };

        // Selected row gets reversed highlight so it's obvious which value d/y apply to
        let row_style = if is_selected {
            Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::White)
        };
        let status_style = if is_decoded {
            Style::default().fg(Color::Green)
        } else {
            Style::default().fg(Color::DarkGray)
        };

        l.push(Line::from(vec![
            Span::styled(format!("  {} ", marker), row_style),
            Span::styled(key.clone(), row_style),
            Span::styled(format!("  {} ", size_str), Style::default().fg(Color::DarkGray)),
            Span::styled(status_icon.to_string(), status_style),
        ]));

        if is_decoded {
            let decoded = STANDARD
                .decode(b64)
                .ok()
                .and_then(|b| String::from_utf8(b).ok())
                .unwrap_or_else(|| "(binary data)".into());

            for dline in decoded.lines() {
                l.push(Line::from(Span::styled(
                    format!("      {}", dline),
                    Style::default().fg(Color::Green),
                )));
            }
        }
    }

    l
}

fn render_service(v: &Value, ds: &mut DictState) -> Vec<Line<'static>> {
    let mut l = metadata_lines(v, "Service", ds);

    field(&mut l, "Type", &js(v, "spec.type"));
    field(&mut l, "Cluster IP", &js(v, "spec.clusterIP"));

    let external_ips = jget(v, "spec.externalIPs")
        .and_then(|v| v.as_array())
        .map(|arr| arr.iter().filter_map(|v| v.as_str()).collect::<Vec<_>>().join(", "))
        .unwrap_or_default();
    if !external_ips.is_empty() {
        field(&mut l, "External IPs", &external_ips);
    }

    let lb_ip = js(v, "status.loadBalancer.ingress");
    if !lb_ip.is_empty() && lb_ip != "null" {
        if let Some(ingress) = jget(v, "status.loadBalancer.ingress").and_then(|v| v.as_array()) {
            let ips: Vec<String> = ingress
                .iter()
                .filter_map(|i| {
                    i.get("ip")
                        .or_else(|| i.get("hostname"))
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string())
                })
                .collect();
            if !ips.is_empty() {
                field(&mut l, "Load Balancer", &ips.join(", "));
            }
        }
    }

    // Ports
    if let Some(ports) = jget(v, "spec.ports").and_then(|v| v.as_array()) {
        if !ports.is_empty() {
            blank(&mut l);
            section(&mut l, "Ports");
            for p in ports {
                let name = p.get("name").and_then(|v| v.as_str()).unwrap_or("");
                let port = p.get("port").and_then(|v| v.as_i64()).unwrap_or(0);
                let target = p
                    .get("targetPort")
                    .map(|v| {
                        match v {
                            | Value::Number(n) => n.to_string(),
                            | Value::String(s) => s.clone(),
                            | _ => "".into(),
                        }
                    })
                    .unwrap_or_default();
                let proto = p.get("protocol").and_then(|v| v.as_str()).unwrap_or("TCP");
                let node_port = p.get("nodePort").and_then(|v| v.as_i64());

                let mut desc = format!("{}:{}/{}", port, target, proto);
                if let Some(np) = node_port {
                    desc.push_str(&format!(" NodePort:{}", np));
                }
                let label = if name.is_empty() {
                    "  -".to_string()
                } else {
                    format!("  {}", name)
                };
                field(&mut l, &label, &desc);
            }
        }
    }

    let selector = labels_str(v, "spec.selector");
    if !selector.is_empty() {
        blank(&mut l);
        field(&mut l, "Selector", &selector);
    }

    l
}

fn render_daemonset(v: &Value, ds: &mut DictState) -> Vec<Line<'static>> {
    let mut l = metadata_lines(v, "DaemonSet", ds);

    let desired = ji(v, "status.desiredNumberScheduled").unwrap_or(0);
    let current = ji(v, "status.currentNumberScheduled").unwrap_or(0);
    let ready = ji(v, "status.numberReady").unwrap_or(0);
    let updated = ji(v, "status.updatedNumberScheduled").unwrap_or(0);
    let available = ji(v, "status.numberAvailable").unwrap_or(0);
    field(
        &mut l,
        "Pods",
        &format!(
            "{} desired, {} current, {} ready, {} updated, {} available",
            desired, current, ready, updated, available
        ),
    );

    let strategy = js(v, "spec.updateStrategy.type");
    if !strategy.is_empty() {
        field(&mut l, "Update Strategy", &strategy);
    }

    let selector = labels_str(v, "spec.selector.matchLabels");
    if !selector.is_empty() {
        field(&mut l, "Selector", &selector);
    }

    containers_section(&mut l, v, "spec.template.spec.containers");
    conditions_section(&mut l, v);
    l
}

fn render_hpa(v: &Value, ds: &mut DictState) -> Vec<Line<'static>> {
    let mut l = metadata_lines(v, "HorizontalPodAutoscaler", ds);

    let kind = js(v, "spec.scaleTargetRef.kind");
    let name = js(v, "spec.scaleTargetRef.name");
    field(&mut l, "Reference", &format!("{}/{}", kind, name));

    let min = ji(v, "spec.minReplicas").unwrap_or(0);
    let max = ji(v, "spec.maxReplicas").unwrap_or(0);
    field(&mut l, "Min Replicas", &min.to_string());
    field(&mut l, "Max Replicas", &max.to_string());

    let current = ji(v, "status.currentReplicas").unwrap_or(0);
    let desired = ji(v, "status.desiredReplicas").unwrap_or(0);
    field(&mut l, "Current", &current.to_string());
    field(&mut l, "Desired", &desired.to_string());

    let cpu = ji(v, "spec.targetCPUUtilizationPercentage");
    if let Some(pct) = cpu {
        let current_cpu = ji(v, "status.currentCPUUtilizationPercentage");
        let current_str = current_cpu
            .map(|c| format!("{}%", c))
            .unwrap_or_else(|| "<unknown>".into());
        field(&mut l, "CPU Target", &format!("{}% (current: {})", pct, current_str));
    }

    conditions_section(&mut l, v);
    l
}

fn render_ingress(v: &Value, ds: &mut DictState) -> Vec<Line<'static>> {
    let mut l = metadata_lines(v, "Ingress", ds);

    let class = js(v, "spec.ingressClassName");
    if !class.is_empty() {
        field(&mut l, "Class", &class);
    }

    // Default backend
    let default_svc = js(v, "spec.defaultBackend.service.name");
    if !default_svc.is_empty() {
        let port = js(v, "spec.defaultBackend.service.port.number");
        field(&mut l, "Default Backend", &format!("{}:{}", default_svc, port));
    }

    // Rules
    if let Some(rules) = jget(v, "spec.rules").and_then(|v| v.as_array()) {
        blank(&mut l);
        section(&mut l, "Rules");
        for rule in rules {
            let host = rule.get("host").and_then(|h| h.as_str()).unwrap_or("*");
            subheading(&mut l, &format!("▸ {}", host));
            if let Some(paths) = rule.get("http").and_then(|h| h.get("paths")).and_then(|p| p.as_array()) {
                for path in paths {
                    let p = path.get("path").and_then(|p| p.as_str()).unwrap_or("/");
                    let path_type = path.get("pathType").and_then(|t| t.as_str()).unwrap_or("");
                    let svc = path
                        .get("backend")
                        .and_then(|b| b.get("service"))
                        .map(|s| {
                            let name = s.get("name").and_then(|n| n.as_str()).unwrap_or("");
                            let port = s
                                .get("port")
                                .and_then(|p| p.get("number").and_then(|n| n.as_i64()))
                                .map(|n| n.to_string())
                                .unwrap_or_default();
                            format!("{}:{}", name, port)
                        })
                        .unwrap_or_default();
                    field(&mut l, &format!("    {} ({})", p, path_type), &svc);
                }
            }
        }
    }

    // TLS
    if let Some(tls) = jget(v, "spec.tls").and_then(|v| v.as_array()) {
        if !tls.is_empty() {
            blank(&mut l);
            section(&mut l, "TLS");
            for t in tls {
                let secret = t.get("secretName").and_then(|s| s.as_str()).unwrap_or("");
                let hosts = t
                    .get("hosts")
                    .and_then(|h| h.as_array())
                    .map(|arr| arr.iter().filter_map(|h| h.as_str()).collect::<Vec<_>>().join(", "))
                    .unwrap_or_default();
                field(&mut l, "  Secret", secret);
                field(&mut l, "  Hosts", &hosts);
            }
        }
    }

    l
}

fn render_endpoints(v: &Value, ds: &mut DictState) -> Vec<Line<'static>> {
    let mut l = metadata_lines(v, "Endpoints", ds);

    if let Some(subsets) = jget(v, "subsets").and_then(|v| v.as_array()) {
        for (i, subset) in subsets.iter().enumerate() {
            blank(&mut l);
            section(&mut l, &format!("Subset {}", i));

            let ports: Vec<String> = subset
                .get("ports")
                .and_then(|p| p.as_array())
                .map(|arr| {
                    arr.iter()
                        .map(|p| {
                            let name = p.get("name").and_then(|n| n.as_str()).unwrap_or("");
                            let port = p.get("port").and_then(|n| n.as_i64()).unwrap_or(0);
                            let proto = p.get("protocol").and_then(|p| p.as_str()).unwrap_or("TCP");
                            if name.is_empty() {
                                format!("{}/{}", port, proto)
                            } else {
                                format!("{} {}/{}", name, port, proto)
                            }
                        })
                        .collect()
                })
                .unwrap_or_default();
            if !ports.is_empty() {
                field(&mut l, "Ports", &ports.join(", "));
            }

            if let Some(addrs) = subset.get("addresses").and_then(|a| a.as_array()) {
                field(&mut l, "Addresses", &format!("{} ready", addrs.len()));
                for addr in addrs.iter().take(10) {
                    let ip = addr.get("ip").and_then(|i| i.as_str()).unwrap_or("");
                    let target = addr
                        .get("targetRef")
                        .map(|t| {
                            let kind = t.get("kind").and_then(|k| k.as_str()).unwrap_or("");
                            let name = t.get("name").and_then(|n| n.as_str()).unwrap_or("");
                            format!(
                                " ({}{})",
                                kind,
                                if name.is_empty() {
                                    "".into()
                                } else {
                                    format!("/{}", name)
                                }
                            )
                        })
                        .unwrap_or_default();
                    l.push(Line::from(Span::styled(
                        format!("    {}{}", ip, target),
                        Style::default().fg(Color::White),
                    )));
                }
                if addrs.len() > 10 {
                    l.push(Line::from(Span::styled(
                        format!("    ... and {} more", addrs.len() - 10),
                        Style::default().fg(Color::DarkGray),
                    )));
                }
            }

            if let Some(not_ready) = subset.get("notReadyAddresses").and_then(|a| a.as_array()) {
                if !not_ready.is_empty() {
                    field(&mut l, "Not Ready", &format!("{} addresses", not_ready.len()));
                }
            }
        }
    }

    l
}

fn render_network_policy(v: &Value, ds: &mut DictState) -> Vec<Line<'static>> {
    let mut l = metadata_lines(v, "NetworkPolicy", ds);

    let selector = labels_str(v, "spec.podSelector.matchLabels");
    field(
        &mut l,
        "Pod Selector",
        if selector.is_empty() { "<all pods>" } else { &selector },
    );

    if let Some(types) = jget(v, "spec.policyTypes").and_then(|v| v.as_array()) {
        let types_str: Vec<&str> = types.iter().filter_map(|t| t.as_str()).collect();
        field(&mut l, "Policy Types", &types_str.join(", "));
    }

    if let Some(ingress) = jget(v, "spec.ingress").and_then(|v| v.as_array()) {
        blank(&mut l);
        section(&mut l, &format!("Ingress Rules ({})", ingress.len()));
        for (i, rule) in ingress.iter().enumerate() {
            subheading(&mut l, &format!("▸ Rule {}", i));
            if let Some(from) = rule.get("from").and_then(|f| f.as_array()) {
                field(&mut l, "    From", &format!("{} sources", from.len()));
            }
            if let Some(ports) = rule.get("ports").and_then(|p| p.as_array()) {
                let port_str: Vec<String> = ports.iter().map(format_netpol_port).collect();
                field(&mut l, "    Ports", &port_str.join(", "));
            }
        }
    }

    if let Some(egress) = jget(v, "spec.egress").and_then(|v| v.as_array()) {
        blank(&mut l);
        section(&mut l, &format!("Egress Rules ({})", egress.len()));
        for (i, rule) in egress.iter().enumerate() {
            subheading(&mut l, &format!("▸ Rule {}", i));
            if let Some(to) = rule.get("to").and_then(|t| t.as_array()) {
                field(&mut l, "    To", &format!("{} destinations", to.len()));
            }
            if let Some(ports) = rule.get("ports").and_then(|p| p.as_array()) {
                let port_str: Vec<String> = ports.iter().map(format_netpol_port).collect();
                field(&mut l, "    Ports", &port_str.join(", "));
            }
        }
    }

    l
}

fn render_pvc(v: &Value, ds: &mut DictState) -> Vec<Line<'static>> {
    let mut l = metadata_lines(v, "PersistentVolumeClaim", ds);

    let phase = js(v, "status.phase");
    if !phase.is_empty() {
        field(&mut l, "Status", &phase);
    }

    let volume = js(v, "spec.volumeName");
    if !volume.is_empty() {
        field(&mut l, "Volume", &volume);
    }

    let storage_class = js(v, "spec.storageClassName");
    if !storage_class.is_empty() {
        field(&mut l, "Storage Class", &storage_class);
    }

    if let Some(access) = jget(v, "spec.accessModes").and_then(|v| v.as_array()) {
        let modes: Vec<&str> = access.iter().filter_map(|m| m.as_str()).collect();
        field(&mut l, "Access Modes", &modes.join(", "));
    }

    let capacity = jget(v, "status.capacity.storage")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if !capacity.is_empty() {
        field(&mut l, "Capacity", capacity);
    }

    let requested = jget(v, "spec.resources.requests.storage")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if !requested.is_empty() {
        field(&mut l, "Requested", requested);
    }

    conditions_section(&mut l, v);
    l
}

fn render_pv(v: &Value, ds: &mut DictState) -> Vec<Line<'static>> {
    let mut l = metadata_lines(v, "PersistentVolume", ds);

    let phase = js(v, "status.phase");
    if !phase.is_empty() {
        field(&mut l, "Status", &phase);
    }

    let capacity = jget(v, "spec.capacity.storage").and_then(|v| v.as_str()).unwrap_or("");
    if !capacity.is_empty() {
        field(&mut l, "Capacity", capacity);
    }

    if let Some(access) = jget(v, "spec.accessModes").and_then(|v| v.as_array()) {
        let modes: Vec<&str> = access.iter().filter_map(|m| m.as_str()).collect();
        field(&mut l, "Access Modes", &modes.join(", "));
    }

    let reclaim = js(v, "spec.persistentVolumeReclaimPolicy");
    if !reclaim.is_empty() {
        field(&mut l, "Reclaim Policy", &reclaim);
    }

    let storage_class = js(v, "spec.storageClassName");
    if !storage_class.is_empty() {
        field(&mut l, "Storage Class", &storage_class);
    }

    let claim_ns = js(v, "spec.claimRef.namespace");
    let claim_name = js(v, "spec.claimRef.name");
    if !claim_name.is_empty() {
        field(&mut l, "Claim", &format!("{}/{}", claim_ns, claim_name));
    }

    l
}

fn render_storage_class(v: &Value, ds: &mut DictState) -> Vec<Line<'static>> {
    let mut l = metadata_lines(v, "StorageClass", ds);

    field(&mut l, "Provisioner", &js(v, "provisioner"));

    let reclaim = js(v, "reclaimPolicy");
    if !reclaim.is_empty() {
        field(&mut l, "Reclaim Policy", &reclaim);
    }

    let binding = js(v, "volumeBindingMode");
    if !binding.is_empty() {
        field(&mut l, "Volume Binding", &binding);
    }

    let expand = jget(v, "allowVolumeExpansion")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    field(&mut l, "Allow Expansion", if expand { "true" } else { "false" });

    if let Some(params) = jget(v, "parameters").and_then(|v| v.as_object()) {
        if !params.is_empty() {
            blank(&mut l);
            section(&mut l, &format!("Parameters ({})", params.len()));
            for (k, val) in params {
                let val_str = val.as_str().unwrap_or("");
                field(&mut l, &format!("  {}", k), val_str);
            }
        }
    }

    l
}

fn render_service_account(v: &Value, ds: &mut DictState) -> Vec<Line<'static>> {
    let mut l = metadata_lines(v, "ServiceAccount", ds);

    if let Some(secrets) = jget(v, "secrets").and_then(|v| v.as_array()) {
        if !secrets.is_empty() {
            blank(&mut l);
            section(&mut l, &format!("Secrets ({})", secrets.len()));
            for s in secrets {
                let name = s.get("name").and_then(|n| n.as_str()).unwrap_or("");
                field(&mut l, "  -", name);
            }
        }
    }

    if let Some(pull) = jget(v, "imagePullSecrets").and_then(|v| v.as_array()) {
        if !pull.is_empty() {
            blank(&mut l);
            section(&mut l, "Image Pull Secrets");
            for s in pull {
                let name = s.get("name").and_then(|n| n.as_str()).unwrap_or("");
                field(&mut l, "  -", name);
            }
        }
    }

    l
}

fn render_role(v: &Value, ds: &mut DictState) -> Vec<Line<'static>> {
    let kind = jget(v, "kind").and_then(|v| v.as_str()).unwrap_or("Role");
    let mut l = metadata_lines(v, kind, ds);

    if let Some(rules) = jget(v, "rules").and_then(|v| v.as_array()) {
        blank(&mut l);
        section(&mut l, &format!("Rules ({})", rules.len()));
        for (i, rule) in rules.iter().enumerate() {
            subheading(&mut l, &format!("▸ Rule {}", i));
            let api_groups = rule
                .get("apiGroups")
                .and_then(|a| a.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str())
                        .map(|s| if s.is_empty() { "core" } else { s })
                        .collect::<Vec<_>>()
                        .join(", ")
                })
                .unwrap_or_default();
            field(&mut l, "    API Groups", &api_groups);

            let resources = rule
                .get("resources")
                .and_then(|r| r.as_array())
                .map(|arr| arr.iter().filter_map(|v| v.as_str()).collect::<Vec<_>>().join(", "))
                .unwrap_or_default();
            field(&mut l, "    Resources", &resources);

            let verbs = rule
                .get("verbs")
                .and_then(|v| v.as_array())
                .map(|arr| arr.iter().filter_map(|v| v.as_str()).collect::<Vec<_>>().join(", "))
                .unwrap_or_default();
            field(&mut l, "    Verbs", &verbs);
        }
    }

    l
}

fn render_role_binding(v: &Value, ds: &mut DictState) -> Vec<Line<'static>> {
    let kind = jget(v, "kind").and_then(|v| v.as_str()).unwrap_or("RoleBinding");
    let mut l = metadata_lines(v, kind, ds);

    let role_kind = js(v, "roleRef.kind");
    let role_name = js(v, "roleRef.name");
    field(&mut l, "Role", &format!("{}/{}", role_kind, role_name));

    if let Some(subjects) = jget(v, "subjects").and_then(|v| v.as_array()) {
        blank(&mut l);
        section(&mut l, &format!("Subjects ({})", subjects.len()));
        for s in subjects {
            let kind = s.get("kind").and_then(|k| k.as_str()).unwrap_or("");
            let name = s.get("name").and_then(|n| n.as_str()).unwrap_or("");
            let ns = s.get("namespace").and_then(|n| n.as_str()).unwrap_or("");
            let display = if ns.is_empty() {
                format!("{}/{}", kind, name)
            } else {
                format!("{}/{} ({})", kind, name, ns)
            };
            field(&mut l, "  -", &display);
        }
    }

    l
}

fn render_node(v: &Value, ds: &mut DictState) -> Vec<Line<'static>> {
    let mut l = metadata_lines(v, "Node", ds);

    // Status from conditions
    if let Some(conds) = jget(v, "status.conditions").and_then(|v| v.as_array()) {
        let ready = conds
            .iter()
            .find(|c| c.get("type").and_then(|t| t.as_str()) == Some("Ready"))
            .and_then(|c| c.get("status").and_then(|s| s.as_str()))
            .unwrap_or("Unknown");
        field(&mut l, "Status", if ready == "True" { "Ready" } else { "NotReady" });
    }

    // Resource pressure warnings
    if let Some(conds) = jget(v, "status.conditions").and_then(|v| v.as_array()) {
        for ptype in &["DiskPressure", "MemoryPressure", "PIDPressure"] {
            if let Some(cond) = conds
                .iter()
                .find(|c| c.get("type").and_then(|t| t.as_str()) == Some(ptype))
            {
                if cond.get("status").and_then(|s| s.as_str()) == Some("True") {
                    l.push(Line::from(Span::styled(
                        format!("  ⚠ {} ACTIVE", ptype),
                        Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
                    )));
                }
            }
        }
    }

    // Roles
    if let Some(labels) = jget(v, "metadata.labels").and_then(|v| v.as_object()) {
        let roles: Vec<&str> = labels
            .keys()
            .filter_map(|k| {
                k.strip_prefix("node-role.kubernetes.io/")
                    .map(|r| if r.is_empty() { "worker" } else { r })
            })
            .collect();
        if !roles.is_empty() {
            field(&mut l, "Roles", &roles.join(", "));
        }
    }

    // Node info
    let os = js(v, "status.nodeInfo.operatingSystem");
    let arch = js(v, "status.nodeInfo.architecture");
    let kubelet = js(v, "status.nodeInfo.kubeletVersion");
    let runtime = js(v, "status.nodeInfo.containerRuntimeVersion");
    let os_image = js(v, "status.nodeInfo.osImage");
    let kernel = js(v, "status.nodeInfo.kernelVersion");

    blank(&mut l);
    section(&mut l, "System Info");
    if !kubelet.is_empty() {
        field(&mut l, "Kubelet", &kubelet);
    }
    if !runtime.is_empty() {
        field(&mut l, "Runtime", &runtime);
    }
    if !os.is_empty() || !arch.is_empty() {
        field(&mut l, "OS/Arch", &format!("{}/{}", os, arch));
    }
    if !os_image.is_empty() {
        field(&mut l, "OS Image", &os_image);
    }
    if !kernel.is_empty() {
        field(&mut l, "Kernel", &kernel);
    }

    // Addresses
    if let Some(addrs) = jget(v, "status.addresses").and_then(|v| v.as_array()) {
        blank(&mut l);
        section(&mut l, "Addresses");
        for addr in addrs {
            let atype = addr.get("type").and_then(|t| t.as_str()).unwrap_or("");
            let address = addr.get("address").and_then(|a| a.as_str()).unwrap_or("");
            field(&mut l, &format!("  {}", atype), address);
        }
    }

    // Capacity & Allocatable
    if let Some(capacity) = jget(v, "status.capacity").and_then(|v| v.as_object()) {
        blank(&mut l);
        section(&mut l, "Capacity");
        for (k, val) in capacity {
            let val_str = val.as_str().unwrap_or("");
            let alloc = jget(v, "status.allocatable")
                .and_then(|a| a.get(k.as_str()))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            field(
                &mut l,
                &format!("  {}", k),
                &format!("{} (allocatable: {})", val_str, alloc),
            );
        }
    }

    conditions_section(&mut l, v);
    l
}

fn render_namespace(v: &Value, ds: &mut DictState) -> Vec<Line<'static>> {
    let mut l = metadata_lines(v, "Namespace", ds);

    let phase = js(v, "status.phase");
    if !phase.is_empty() {
        field(&mut l, "Status", &phase);
    }

    conditions_section(&mut l, v);
    l
}

fn render_event(v: &Value, ds: &mut DictState) -> Vec<Line<'static>> {
    let mut l = metadata_lines(v, "Event", ds);

    let event_type = js(v, "type");
    if !event_type.is_empty() {
        field(&mut l, "Type", &event_type);
    }

    let reason = js(v, "reason");
    if !reason.is_empty() {
        field(&mut l, "Reason", &reason);
    }

    let note = js(v, "note");
    if !note.is_empty() {
        blank(&mut l);
        section(&mut l, "Message");
        for line in note.lines() {
            l.push(Line::from(Span::styled(
                format!("  {}", line),
                Style::default().fg(Color::White),
            )));
        }
    }

    let obj_kind = js(v, "regarding.kind");
    let obj_name = js(v, "regarding.name");
    let obj_ns = js(v, "regarding.namespace");
    if !obj_name.is_empty() {
        blank(&mut l);
        section(&mut l, "Regarding");
        field(&mut l, "Kind", &obj_kind);
        field(&mut l, "Name", &obj_name);
        if !obj_ns.is_empty() {
            field(&mut l, "Namespace", &obj_ns);
        }
    }

    // Series count (modern) or deprecated count
    let series_count = ji(v, "series.count");
    let count = series_count.or_else(|| ji(v, "deprecatedCount"));
    if let Some(c) = count {
        field(&mut l, "Count", &c.to_string());
    }

    let event_time = js(v, "eventTime");
    if !event_time.is_empty() {
        field(&mut l, "Event Time", &event_time);
    }

    let reporting = js(v, "reportingController");
    if !reporting.is_empty() {
        field(&mut l, "Reporting", &reporting);
    }

    l
}

// ---------------------------------------------------------------------------
// CRD renderer
// ---------------------------------------------------------------------------

fn render_crd(v: &Value, ds: &mut DictState) -> Vec<Line<'static>> {
    let mut l = metadata_lines(v, "CustomResourceDefinition", ds);

    field(&mut l, "Group", &js(v, "spec.group"));
    field(&mut l, "Scope", &js(v, "spec.scope"));
    field(&mut l, "Kind", &js(v, "spec.names.kind"));
    field(&mut l, "Plural", &js(v, "spec.names.plural"));

    let singular = js(v, "spec.names.singular");
    if !singular.is_empty() {
        field(&mut l, "Singular", &singular);
    }
    let short_names = jget(v, "spec.names.shortNames")
        .and_then(|v| v.as_array())
        .map(|a| a.iter().filter_map(|v| v.as_str()).collect::<Vec<_>>().join(", "))
        .unwrap_or_default();
    if !short_names.is_empty() {
        field(&mut l, "Short Names", &short_names);
    }

    // Versions
    if let Some(versions) = jget(v, "spec.versions").and_then(|v| v.as_array()) {
        section(&mut l, "Versions");
        for ver in versions {
            let name = ver.get("name").and_then(|v| v.as_str()).unwrap_or("");
            let served = ver.get("served").and_then(|v| v.as_bool()).unwrap_or(false);
            let storage = ver.get("storage").and_then(|v| v.as_bool()).unwrap_or(false);
            let flags = format!("served={}, storage={}", served, storage);
            field(&mut l, name, &flags);
        }
    }

    conditions_section(&mut l, v);
    l
}

// ---------------------------------------------------------------------------
// Generic custom resource renderer
// ---------------------------------------------------------------------------

pub fn render_custom_resource(v: &Value, crd: &CrdInfo, ds: &mut DictState) -> Vec<Line<'static>> {
    ds.entries.clear();
    ds.line_offsets.clear();
    let mut l = metadata_lines(v, &crd.kind, ds);

    field(&mut l, "API Version", &format!("{}/{}", crd.group, crd.version));

    // Render spec as key-value pairs
    if let Some(spec) = v.get("spec") {
        if let Some(obj) = spec.as_object() {
            if !obj.is_empty() {
                section(&mut l, "Spec");
                render_value_tree(&mut l, spec, 1);
            }
        }
    }

    // Render status as key-value pairs
    if let Some(status) = v.get("status") {
        if let Some(obj) = status.as_object() {
            if !obj.is_empty() {
                section(&mut l, "Status");
                render_value_tree(&mut l, status, 1);
            }
        }
    }

    conditions_section(&mut l, v);
    l
}

/// Render an arbitrary JSON value as indented key-value lines.
fn render_value_tree(l: &mut Vec<Line<'static>>, v: &Value, depth: usize) {
    let indent = "  ".repeat(depth);
    let key_style = Style::default().fg(Color::Cyan);
    let val_style = Style::default().fg(Color::White);

    match v {
        | Value::Object(map) => {
            for (k, val) in map {
                if k == "conditions" {
                    // Conditions are rendered separately
                    continue;
                }
                match val {
                    | Value::Object(inner) if !inner.is_empty() => {
                        l.push(Line::from(vec![
                            Span::raw(indent.clone()),
                            Span::styled(format!("{}:", k), key_style),
                        ]));
                        render_value_tree(l, val, depth + 1);
                    },
                    | Value::Array(arr) if !arr.is_empty() => {
                        l.push(Line::from(vec![
                            Span::raw(indent.clone()),
                            Span::styled(format!("{}:", k), key_style),
                        ]));
                        for item in arr {
                            match item {
                                | Value::Object(_) => {
                                    l.push(Line::from(vec![Span::raw(format!("{}  - ", indent))]));
                                    render_value_tree(l, item, depth + 2);
                                },
                                | _ => {
                                    let s = value_to_string(item);
                                    l.push(Line::from(vec![
                                        Span::raw(format!("{}  - ", indent)),
                                        Span::styled(s, val_style),
                                    ]));
                                },
                            }
                        }
                    },
                    | _ => {
                        let s = value_to_string(val);
                        if !s.is_empty() {
                            l.push(Line::from(vec![
                                Span::raw(indent.clone()),
                                Span::styled(format!("{}: ", k), key_style),
                                Span::styled(s, val_style),
                            ]));
                        }
                    },
                }
            }
        },
        | _ => {
            let s = value_to_string(v);
            l.push(Line::from(vec![Span::raw(indent), Span::styled(s, val_style)]));
        },
    }
}

fn value_to_string(v: &Value) -> String {
    match v {
        | Value::String(s) => s.clone(),
        | Value::Number(n) => n.to_string(),
        | Value::Bool(b) => b.to_string(),
        | Value::Null => String::new(),
        | other => serde_json::to_string(other).unwrap_or_default(),
    }
}

// ---------------------------------------------------------------------------
// Shared rendering helpers
// ---------------------------------------------------------------------------

fn metadata_lines(v: &Value, kind: &str, ds: &mut DictState) -> Vec<Line<'static>> {
    let name = js(v, "metadata.name");
    let ns = js(v, "metadata.namespace");
    let created = js(v, "metadata.creationTimestamp");

    let mut l = Vec::new();
    l.push(Line::from(Span::styled(
        format!(" {}: {}", kind, name),
        Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD),
    )));
    l.push(Line::from(Span::styled(
        " ──────────────────────────────────────────",
        Style::default().fg(Color::DarkGray),
    )));
    if !ns.is_empty() {
        field(&mut l, "Namespace", &ns);
    }
    field(&mut l, "Created", &created);

    dict_section(&mut l, v, "metadata.labels", "Labels", ds);
    dict_section(&mut l, v, "metadata.annotations", "Annotations", ds);

    l
}

fn containers_section(l: &mut Vec<Line<'static>>, v: &Value, path: &str) {
    if let Some(containers) = jget(v, path).and_then(|v| v.as_array()) {
        if containers.is_empty() {
            return;
        }
        blank(l);
        section(l, "Containers");
        for c in containers {
            let name = c.get("name").and_then(|v| v.as_str()).unwrap_or("");
            let image = c.get("image").and_then(|v| v.as_str()).unwrap_or("");

            subheading(l, &format!("▸ {}", name));
            field(l, "    Image", image);

            if let Some(ports) = c.get("ports").and_then(|p| p.as_array()) {
                let port_str: String = ports
                    .iter()
                    .map(|p| {
                        let port = p.get("containerPort").and_then(|v| v.as_i64()).unwrap_or(0);
                        let proto = p.get("protocol").and_then(|v| v.as_str()).unwrap_or("TCP");
                        format!("{}/{}", port, proto)
                    })
                    .collect::<Vec<_>>()
                    .join(", ");
                if !port_str.is_empty() {
                    field(l, "    Ports", &port_str);
                }
            }

            if let Some(res) = c.get("resources") {
                resources_section(l, res);
            }
        }
    }
}

fn resources_section(l: &mut Vec<Line<'static>>, res: &Value) {
    let requests = res.get("requests").and_then(|r| r.as_object());
    let limits = res.get("limits").and_then(|l| l.as_object());
    if requests.is_none() && limits.is_none() {
        return;
    }

    // Collect all resource dimensions (cpu, memory, etc.)
    let mut dimensions: Vec<String> = Vec::new();
    if let Some(req) = requests {
        for k in req.keys() {
            if !dimensions.contains(k) {
                dimensions.push(k.clone());
            }
        }
    }
    if let Some(lim) = limits {
        for k in lim.keys() {
            if !dimensions.contains(k) {
                dimensions.push(k.clone());
            }
        }
    }

    // Compute column widths from data
    let mut dim_w = 8usize; // "RESOURCE"
    let mut req_w = 7usize; // "REQUEST"
    for dim in &dimensions {
        dim_w = dim_w.max(dim.len());
        let req_val = requests
            .and_then(|r| r.get(dim.as_str()))
            .and_then(|v| v.as_str())
            .unwrap_or("-");
        req_w = req_w.max(req_val.len());
    }
    dim_w += 2;
    req_w += 2;

    // Header
    l.push(Line::from(vec![
        Span::styled(
            format!("    {:<dim_w$}", "RESOURCE"),
            Style::default().fg(Color::DarkGray),
        ),
        Span::styled(format!("{:<req_w$}", "REQUEST"), Style::default().fg(Color::DarkGray)),
        Span::styled("LIMIT", Style::default().fg(Color::DarkGray)),
    ]));

    for dim in &dimensions {
        let req_val = requests
            .and_then(|r| r.get(dim.as_str()))
            .and_then(|v| v.as_str())
            .unwrap_or("-");
        let lim_val = limits
            .and_then(|l| l.get(dim.as_str()))
            .and_then(|v| v.as_str())
            .unwrap_or("-");
        l.push(Line::from(vec![
            Span::styled(format!("    {:<dim_w$}", dim), Style::default().fg(Color::Cyan)),
            Span::styled(format!("{:<req_w$}", req_val), Style::default().fg(Color::Green)),
            Span::styled(lim_val.to_string(), Style::default().fg(Color::Yellow)),
        ]));
    }
}

fn conditions_section(l: &mut Vec<Line<'static>>, v: &Value) {
    let conditions = jget(v, "status.conditions").and_then(|v| v.as_array());
    let conditions = match conditions {
        | Some(c) if !c.is_empty() => c,
        | _ => return,
    };

    const POSITIVE_CONDITIONS: &[&str] = &[
        "Ready",
        "Available",
        "Progressing",
        "Initialized",
        "ContainersReady",
        "PodScheduled",
    ];
    const NEGATIVE_CONDITIONS: &[&str] = &["DiskPressure", "MemoryPressure", "PIDPressure", "NetworkUnavailable"];

    blank(l);
    section(l, "Conditions");

    // Compute column widths from data
    let mut type_w = 4usize; // "TYPE"
    let mut status_w = 6usize; // "STATUS"
    let mut reason_w = 6usize; // "REASON"
    let mut transition_w = 15usize; // "LAST TRANSITION"
    for c in conditions {
        let ctype = c.get("type").and_then(|v| v.as_str()).unwrap_or("");
        let status = c.get("status").and_then(|v| v.as_str()).unwrap_or("");
        let reason = c.get("reason").and_then(|v| v.as_str()).unwrap_or("");
        let transition = c
            .get("lastTransitionTime")
            .and_then(|v| v.as_str())
            .map(format_age_from_str)
            .unwrap_or_default();
        type_w = type_w.max(ctype.len());
        status_w = status_w.max(status.len());
        reason_w = reason_w.max(reason.len());
        transition_w = transition_w.max(transition.len());
    }
    type_w += 2; // padding
    status_w += 2;
    reason_w += 2;
    transition_w += 2;

    // Header
    l.push(Line::from(vec![
        Span::styled(format!("  {:<type_w$}", "TYPE"), Style::default().fg(Color::DarkGray)),
        Span::styled(format!("{:<status_w$}", "STATUS"), Style::default().fg(Color::DarkGray)),
        Span::styled(format!("{:<reason_w$}", "REASON"), Style::default().fg(Color::DarkGray)),
        Span::styled(
            format!("{:<transition_w$}", "LAST TRANSITION"),
            Style::default().fg(Color::DarkGray),
        ),
    ]));

    for c in conditions {
        let ctype = c.get("type").and_then(|v| v.as_str()).unwrap_or("");
        let status = c.get("status").and_then(|v| v.as_str()).unwrap_or("");
        let reason = c.get("reason").and_then(|v| v.as_str()).unwrap_or("");
        let message = c.get("message").and_then(|v| v.as_str()).unwrap_or("");
        let transition = c
            .get("lastTransitionTime")
            .and_then(|v| v.as_str())
            .map(format_age_from_str)
            .unwrap_or_default();

        let is_positive = POSITIVE_CONDITIONS.contains(&ctype);
        let is_negative = NEGATIVE_CONDITIONS.contains(&ctype);

        // Positive conditions win if a type is listed in both sets.
        let status_style = if status == "True" && (is_positive || !is_negative) {
            Style::default().fg(Color::Green)
        } else {
            Style::default().fg(Color::Red)
        };

        l.push(Line::from(vec![
            Span::styled(format!("  {:<type_w$}", ctype), Style::default().fg(Color::White)),
            Span::styled(format!("{:<status_w$}", status), status_style),
            Span::styled(format!("{:<reason_w$}", reason), Style::default().fg(Color::DarkGray)),
            Span::styled(transition, Style::default().fg(Color::DarkGray)),
        ]));

        if !message.is_empty() {
            l.push(Line::from(Span::styled(
                format!("    {}", message),
                Style::default().fg(Color::DarkGray),
            )));
        }
    }
}

fn container_state_str(state: Option<&Value>) -> String {
    let state = match state {
        | Some(v) => v,
        | None => return "Unknown".into(),
    };
    if let Some(running) = state.get("running") {
        let since = running.get("startedAt").and_then(|v| v.as_str()).unwrap_or("");
        return format!("Running (since {})", since);
    }
    if let Some(waiting) = state.get("waiting") {
        let reason = waiting.get("reason").and_then(|v| v.as_str()).unwrap_or("Waiting");
        return reason.to_string();
    }
    if let Some(terminated) = state.get("terminated") {
        let reason = terminated
            .get("reason")
            .and_then(|v| v.as_str())
            .unwrap_or("Terminated");
        let code = terminated.get("exitCode").and_then(|v| v.as_i64()).unwrap_or(0);
        return format!("{} (exit {})", reason, code);
    }
    "Unknown".into()
}

// ---------------------------------------------------------------------------
// Line-building primitives
// ---------------------------------------------------------------------------

fn field(l: &mut Vec<Line<'static>>, label: &str, value: &str) {
    // Pad to at least 18 chars, but grow if label is longer (indented sub-fields)
    let pad = label.len().max(18);
    l.push(Line::from(vec![
        Span::styled(format!("  {:<pad$}", label), Style::default().fg(Color::Cyan)),
        Span::styled(value.to_string(), Style::default().fg(Color::White)),
    ]));
}

fn section(l: &mut Vec<Line<'static>>, title: &str) {
    l.push(Line::from(Span::styled(
        format!(" {}", title),
        Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD),
    )));
    l.push(Line::from(Span::styled(
        " ──────────────────",
        Style::default().fg(Color::DarkGray),
    )));
}

fn subheading(l: &mut Vec<Line<'static>>, text: &str) {
    l.push(Line::from(Span::styled(
        format!("  {}", text),
        Style::default().fg(Color::White).add_modifier(Modifier::BOLD),
    )));
}

/// Renders a JSON object at `path` as a labeled key=value dictionary.
/// Registers each entry in `ds` for selection/expansion. Shows nothing if
/// empty.
fn dict_section(l: &mut Vec<Line<'static>>, v: &Value, path: &str, title: &str, ds: &mut DictState) {
    let map = match jget(v, path).and_then(|v| v.as_object()) {
        | Some(m) if !m.is_empty() => m,
        | _ => return,
    };
    blank(l);
    section(l, &format!("{} ({})", title, map.len()));
    for (k, val) in map {
        let val_str = val.as_str().unwrap_or("");
        let qualified_key = format!("{}:{}", title, k);
        let entry_idx = ds.entries.len();
        ds.entries.push((qualified_key.clone(), k.clone(), val_str.to_string()));
        ds.line_offsets.push(l.len());

        let is_selected = ds.cursor == Some(entry_idx);
        let expanded = ds.expanded.contains(&qualified_key);
        let is_long = val_str.len() > 70;

        let marker = if is_selected { "▸ " } else { "  " };
        let key_style = if is_selected {
            Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::Cyan)
        };

        if is_long && !expanded {
            let mut line = Line::from(vec![
                Span::styled(format!("  {}{}", marker, k), key_style),
                Span::styled(": ", Style::default().fg(Color::DarkGray)),
                Span::styled(format!("{}...", &val_str[..70]), Style::default().fg(Color::White)),
            ]);
            if is_selected {
                line = line.style(Style::default().add_modifier(Modifier::REVERSED));
            }
            l.push(line);
        } else if is_long {
            let mut header_line = Line::from(vec![
                Span::styled(format!("  {}{}", marker, k), key_style),
                Span::styled(":", Style::default().fg(Color::DarkGray)),
            ]);
            if is_selected {
                header_line = header_line.style(Style::default().add_modifier(Modifier::REVERSED));
            }
            l.push(header_line);
            for chunk in wrap_str(val_str, 100) {
                l.push(Line::from(Span::styled(
                    format!("      {}", chunk),
                    Style::default().fg(Color::White),
                )));
            }
        } else {
            let mut line = Line::from(vec![
                Span::styled(format!("  {}{}", marker, k), key_style),
                Span::styled(": ", Style::default().fg(Color::DarkGray)),
                Span::styled(val_str.to_string(), Style::default().fg(Color::White)),
            ]);
            if is_selected {
                line = line.style(Style::default().add_modifier(Modifier::REVERSED));
            }
            l.push(line);
        }
    }
}

fn format_netpol_port(p: &Value) -> String {
    let port = p
        .get("port")
        .map(|v| {
            match v {
                | Value::Number(n) => n.to_string(),
                | Value::String(s) => s.clone(),
                | _ => String::new(),
            }
        })
        .unwrap_or_default();
    let proto = p.get("protocol").and_then(|p| p.as_str()).unwrap_or("TCP");
    format!("{}/{}", port, proto)
}

fn wrap_str(s: &str, width: usize) -> Vec<&str> {
    let mut lines = Vec::new();
    let mut start = 0;
    while start < s.len() {
        let end = (start + width).min(s.len());
        lines.push(&s[start..end]);
        start = end;
    }
    if lines.is_empty() {
        lines.push(s);
    }
    lines
}

fn blank(l: &mut Vec<Line<'static>>) {
    l.push(Line::from(""));
}

fn format_age_from_str(ts: &str) -> String {
    match jiff::Timestamp::strptime("%Y-%m-%dT%H:%M:%SZ", ts).or_else(|_| ts.parse::<jiff::Timestamp>()) {
        | Ok(t) => {
            let dur = jiff::Timestamp::now().duration_since(t);
            let secs = dur.as_secs().max(0);
            let days = secs / 86400;
            let hours = (secs % 86400) / 3600;
            let mins = (secs % 3600) / 60;
            if days > 0 {
                format!("{}d ago", days)
            } else if hours > 0 {
                format!("{}h ago", hours)
            } else if mins > 0 {
                format!("{}m ago", mins)
            } else {
                format!("{}s ago", secs)
            }
        },
        | Err(_) => ts.to_string(),
    }
}

// ---------------------------------------------------------------------------
// JSON value helpers
// ---------------------------------------------------------------------------

fn jget<'a>(v: &'a Value, path: &str) -> Option<&'a Value> {
    let mut current = v;
    for part in path.split('.') {
        current = current.get(part)?;
    }
    Some(current)
}

fn js(v: &Value, path: &str) -> String {
    jget(v, path)
        .map(|v| {
            match v {
                | Value::String(s) => s.clone(),
                | Value::Number(n) => n.to_string(),
                | Value::Bool(b) => b.to_string(),
                | Value::Null => String::new(),
                | _ => String::new(),
            }
        })
        .unwrap_or_default()
}

fn ji(v: &Value, path: &str) -> Option<i64> {
    jget(v, path).and_then(|v| v.as_i64())
}

fn labels_str(v: &Value, path: &str) -> String {
    jget(v, path)
        .and_then(|v| v.as_object())
        .map(|m| {
            m.iter()
                .map(|(k, v)| format!("{}={}", k, v.as_str().unwrap_or("")))
                .collect::<Vec<_>>()
                .join(", ")
        })
        .unwrap_or_default()
}
