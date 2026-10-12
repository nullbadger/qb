use {
    super::{
        jobs::{
            Slot,
            SlotPoll,
        },
        logs::LogViewState,
        smart::SecretDetailState,
    },
    crate::{
        config::QbConfig,
        k8s::{
            ClusterFailure,
            ClusterStatsData,
            CrdInfo,
            FavoriteType,
            KubeClient,
            MetricsData,
            RelatedEvent,
            ResourceEntry,
            ResourceKind,
            ResourceType,
        },
        portforward::{
            self,
            PfTarget,
            PortForwardManager,
            PortForwardRequest,
            PortInfo,
        },
    },
    crossterm::event::{
        KeyCode,
        KeyEvent,
        KeyModifiers,
    },
    ratatui::widgets::{
        ListState,
        TableState,
    },
    regex::Regex,
    serde_json::Value,
};

// ---------------------------------------------------------------------------
// Enums
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq)]
pub enum Focus {
    Nav,
    Resources,
}

#[derive(Clone, Copy, PartialEq)]
pub enum View {
    Main,
    Detail,
    Logs,
    EditDiff,
}

/// What the right panel displays in the Main view.
#[derive(Clone, PartialEq)]
pub enum Panel {
    Overview,
    ResourceMap,
    Favorites,
    PortForwards,
    Profiles,
    Resources(ResourceKind),
}

// ---------------------------------------------------------------------------
// Edit state
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq)]
pub enum DiffKind {
    Context,
    Added,
    Removed,
}

#[derive(Clone, Copy, PartialEq)]
pub enum DiffMode {
    Inline,
    SideBySide,
}

pub struct DiffMark {
    pub name: String,
    pub namespace: String,
    pub yaml: String,
}

pub struct EditContext {
    pub kind: ResourceKind,
    pub name: String,
    pub namespace: String,
    #[allow(dead_code)]
    pub original_yaml: String,
    pub edited_yaml: String,
    pub diff_lines: Vec<(DiffKind, String)>,
    pub diff_mode: DiffMode,
    pub scroll: u16,
    pub error: Option<String>,
    /// False for a side-by-side comparison. Enter must not write that YAML
    /// back.
    pub applyable: bool,
}

/// Set by key handler, consumed by the event loop to suspend TUI and run
/// $EDITOR.
pub struct PendingEdit {
    pub kind: ResourceKind,
    pub name: String,
    pub namespace: String,
    pub yaml: String,
    /// Set during re-edit so we can restore the diff view if user makes no
    /// changes.
    pub original_yaml: Option<String>,
}

/// Set by key handler, consumed by the event loop to edit metadata in $EDITOR.
pub struct PendingMetadataEdit {
    pub kind: MetadataEditKind,
    pub resource: ResourceKind,
    pub name: String,
    pub namespace: String,
    pub yaml: String,
}

pub struct PendingExec {
    pub pod_name: String,
    pub namespace: String,
    pub container: String,
    pub command: Vec<String>,
}

pub struct PendingCreate {
    pub yaml: String,
}

pub struct PaletteEntry {
    pub label: String,
    pub description: String,
    pub kind: PaletteEntryKind,
}

pub enum PaletteEntryKind {
    Resource {
        name: String,
        namespace: String,
        resource_type: Option<ResourceType>,
    },
    Command {
        action: super::command::Action,
    },
}

#[derive(Clone, Copy, PartialEq)]
pub enum DetailMode {
    Smart,
    Yaml,
}

#[derive(Clone, Copy, PartialEq)]
pub enum MetadataEditKind {
    Labels,
    Annotations,
}

#[allow(clippy::enum_variant_names)]
pub enum Popup {
    ContextSelect {
        items: Vec<String>,
        state: ListState,
    },
    NamespaceSelect {
        items: Vec<String>,
        state: ListState,
    },
    PodSelect {
        items: Vec<String>,
        state: ListState,
    },
    ContainerSelect {
        items: Vec<String>,
        state: ListState,
    },
    PortForwardCreate(PfCreateDialog),
    ConfirmDelete {
        name: String,
        namespace: String,
        kind: ResourceKind,
    },
    ConfirmDrain {
        node_name: String,
    },
    ConfirmQuit {
        pf_count: usize,
    },
    TriggerCronJob {
        cronjob_name: String,
        namespace: String,
        buf: String,
    },
    ScaleInput {
        name: String,
        namespace: String,
        resource_type: ResourceType,
        current: u32,
        buf: String,
    },
    ExecShell {
        pod_name: String,
        namespace: String,
        containers: Vec<String>,
        container_cursor: usize,
        command_buf: String,
        terminal_buf: String,
        editing_terminal: bool,
    },
    KubeconfigInput {
        buf: String,
    },
    TimeFilter {
        buf: String,
    },
    ProfileSave {
        buf: String,
    },
    ProfileLoad {
        items: Vec<String>,
        state: ListState,
    },
    PortForwardEditPort {
        pf_id: usize,
        old_port: u16,
        buf: String,
    },
    ProfileClone {
        source_name: String,
        buf: String,
    },
    ConfirmDeleteProfile {
        profile_name: String,
    },
    Confirm {
        title: String,
        body: String,
        kind: ConfirmKind,
    },
    /// Cluster credential plugin failed. Calls stay paused until retry or quit.
    AuthExpired {
        detail: String,
    },
}

/// A cluster or local mutation that must be confirmed before it runs.
/// Some variants are reserved for the shared confirm path; delete, quit, and
/// profile delete still use their existing popups.
#[allow(dead_code)]
pub enum ConfirmKind {
    Delete {
        name: String,
        namespace: String,
        kind: ResourceKind,
    },
    Drain {
        node_name: String,
    },
    Restart {
        rt: ResourceType,
        name: String,
        namespace: String,
    },
    Cordon {
        name: String,
        cordon: bool,
    },
    Scale {
        rt: ResourceType,
        name: String,
        namespace: String,
        replicas: u32,
    },
    Trigger {
        namespace: String,
        cronjob: String,
        job_name: String,
    },
    CancelPortForward {
        id: usize,
        resource_name: String,
        namespace: String,
        context: String,
        local_port: u16,
    },
    DeleteProfile {
        profile_name: String,
    },
    Quit,
    Create {
        yaml: String,
    },
    PatchMetadata {
        resource: ResourceKind,
        name: String,
        namespace: String,
        field: &'static str,
        patch: serde_json::Map<String, Value>,
    },
}

impl Popup {
    /// List popups share j/k navigation. Other popups return `None`.
    fn list_state_mut(&mut self) -> Option<(&mut ListState, usize)> {
        match self {
            | Self::ContextSelect { items, state }
            | Self::NamespaceSelect { items, state }
            | Self::PodSelect { items, state }
            | Self::ContainerSelect { items, state } => Some((state, items.len())),
            | _ => None,
        }
    }
}

pub struct PfCreateDialog {
    pub resource_type: ResourceType,
    pub resource_name: String,
    pub namespace: String,
    pub target: PfTarget,
    pub ports: Vec<PortInfo>,
    pub port_cursor: usize,
    pub local_port_buf: String,
}

#[derive(Clone, PartialEq)]
pub enum PendingLoad {
    Resources,
    Namespaces,
    SwitchContext(String),
    ResourceDetail { name: String, namespace: String },
    Logs { name: String, namespace: String },
    ReloadLogs,
    CheckLogPods,
    ClusterStats,
    DiscoverCrds,
}

type Apply = Box<dyn FnOnce(&mut App)+Send>;

struct ReadFlight {
    slot: Slot<Apply>,
    token: ReadToken,
}

#[derive(Clone, PartialEq)]
enum ReadToken {
    Nav(PendingLoad),
    Interactive,
}

enum Write {
    Delete {
        kind: ResourceKind,
        name: String,
        namespace: String,
    },
    Drain {
        name: String,
    },
    Restart {
        rt: ResourceType,
        name: String,
        namespace: String,
    },
    Cordon {
        name: String,
        cordon: bool,
    },
    Scale {
        rt: ResourceType,
        name: String,
        namespace: String,
        replicas: u32,
    },
    Trigger {
        namespace: String,
        cronjob: String,
        job_name: String,
    },
    Create {
        yaml: String,
        crds: Vec<CrdInfo>,
    },
    Replace {
        kind: ResourceKind,
        name: String,
        namespace: String,
        yaml: String,
    },
    PatchMetadata {
        resource: ResourceKind,
        name: String,
        namespace: String,
        field: &'static str,
        patch: serde_json::Map<String, Value>,
    },
    SwitchContext {
        ctx: String,
    },
    LoadKubeconfig {
        path: String,
    },
    SwitchProfile {
        name: String,
        kubeconfig: Option<String>,
        context: Option<String>,
        same_kubeconfig: bool,
    },
}

enum ValueUse {
    Edit,
    Diff { mark: Option<DiffMark> },
    PortForward { rt: ResourceType },
    Scale { rt: ResourceType },
}

struct DetailPayload {
    kind: ResourceKind,
    name: String,
    namespace: String,
    value: Value,
    events: Vec<crate::k8s::RelatedEvent>,
    related: Vec<crate::k8s::RelatedResource>,
}

struct LogPayload {
    rt: ResourceType,
    name: String,
    namespace: String,
    pods: Vec<crate::k8s::PodInfo>,
    lines: Vec<super::logs::LogLine>,
    since: Option<i64>,
}

// ---------------------------------------------------------------------------
// Navigation items
// ---------------------------------------------------------------------------

pub struct NavItem {
    pub label: String,
    pub kind: NavItemKind,
}

pub enum NavItemKind {
    Category,
    SubCategory,
    Resource(ResourceKind),
    ClusterStats,
    ResourceMap,
    PortForwards,
    Favorites,
    Profiles,
}

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Sentinel label used as the first entry in the namespace popup.
pub const ALL_NAMESPACES_LABEL: &str = "All Namespaces";

/// An item in a grouped display list — either a category header or a
/// reference (by original index) into the underlying Vec.
pub enum DisplayItem {
    Header(String),
    Entry(usize),
}

// ---------------------------------------------------------------------------
// Sub-state structs
// ---------------------------------------------------------------------------

pub struct NavState {
    pub items: Vec<NavItem>,
    pub state: ListState,
}

pub struct ResourceFilter {
    pub text: String,
    pub regex: Option<Regex>,
    pub editing: bool,
    pub buf: String,
}

pub struct ResourceTable {
    pub entries: Vec<ResourceEntry>,
    pub state: TableState,
    pub table_state: TableState,
    pub counts: std::collections::HashMap<ResourceKind, usize>,
    pub filter: ResourceFilter,
}

pub struct ClusterOverview {
    pub stats: Option<ClusterStatsData>,
    pub scroll: u16,
    /// Background fetch — polled each tick, never blocks the event loop.
    pub pending_rx: Option<tokio::sync::oneshot::Receiver<anyhow::Result<ClusterStatsData>>>,
    pub stats_task: Option<tokio::task::JoinHandle<()>>,
    /// Live metrics from metrics-server (fetched in background).
    pub metrics: Option<MetricsData>,
    pub metrics_rx: Option<tokio::sync::oneshot::Receiver<MetricsData>>,
    pub metrics_task: Option<tokio::task::JoinHandle<()>>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ResourceMapSort {
    Cpu,
    Memory,
}

pub enum ResourceMapFocus {
    /// Normal scrollable view, no element focused.
    Overview,
    /// Workload list is focused — j/k moves cursor within it.
    Workloads { cursor: usize },
}

pub struct ResourceMapState {
    pub scroll: u16,
    pub sort: ResourceMapSort,
    pub metrics: Option<MetricsData>,
    pub focus: ResourceMapFocus,
    /// Filter text for workloads (applied when non-empty).
    pub filter: String,
    /// True while the user is typing in the filter bar.
    pub filter_editing: bool,
    pub filter_buf: String,
}

pub struct RelatedState {
    pub events: Vec<RelatedEvent>,
    pub resources: Vec<crate::k8s::RelatedResource>,
    pub cursor: Option<usize>,
    pub tab: usize,
    pub line_start: usize,
}

pub struct DictSelection {
    pub entries: Vec<(String, String, String)>,
    pub cursor: Option<usize>,
    pub line_offsets: Vec<usize>,
    pub expanded_keys: std::collections::HashSet<String>,
}

pub struct DetailState {
    pub value: Value,
    pub yaml: String,
    pub scroll: u16,
    pub mode: DetailMode,
    pub secret: Option<SecretDetailState>,
    pub name: String,
    pub namespace: String,
    pub auto_refresh: bool,
    pub area_height: usize,
    pub related: RelatedState,
    pub dict: DictSelection,
}

pub struct EventsState {
    pub scroll: usize,
    pub cursor: usize,
    pub auto_scroll: bool,
}

pub struct PaletteState {
    #[allow(dead_code)]
    pub global: bool,
    pub buf: String,
    pub results: Vec<PaletteEntry>,
    pub cursor: usize,
    #[allow(dead_code)]
    pub all_resources: Vec<(ResourceType, Vec<crate::k8s::ResourceEntry>)>,
}

pub struct HelpState {
    pub buf: String,
    pub cursor: usize,
    pub scroll: usize,
    pub context_only: bool,
}

pub struct PortForwardView {
    pub manager: PortForwardManager,
    pub cursor: usize,
    pub table_state: TableState,
}

pub struct FavoritesView {
    pub cursor: usize,
    pub table_state: TableState,
}

pub struct ProfilesView {
    pub cursor: usize,
    pub table_state: TableState,
}

// ---------------------------------------------------------------------------
// App state
// ---------------------------------------------------------------------------

pub struct App {
    pub kube: KubeClient,
    pub nav: NavState,
    pub resources: ResourceTable,
    pub panel: Panel,
    pub return_panel: Option<Panel>,
    pub overview: ClusterOverview,
    pub resource_map: ResourceMapState,
    pub detail: DetailState,
    pub log: Option<LogViewState>,
    pub log_detail_line: Option<String>,
    pub events: EventsState,
    pub pending_edit: Option<PendingEdit>,
    pub pending_exec: Option<PendingExec>,
    pub pending_create: Option<PendingCreate>,
    pub pending_metadata_edit: Option<PendingMetadataEdit>,
    pub exec_terminal_override: Option<String>,
    pub edit_ctx: Option<EditContext>,
    pub focus: Focus,
    pub view: View,
    pub popup: Option<Popup>,
    pub should_quit: bool,
    pub experimental: bool,
    pub status: String,
    pub status_history: Vec<(std::time::Instant, String)>,
    pub error: Option<String>,
    pub pending_load: Option<PendingLoad>,
    read_job: Option<ReadFlight>,
    write_job: Option<Slot<Apply>>,
    write_queued: Option<Write>,
    pub paused: bool,
    /// Set when Google/exec credentials are rejected. Blocks further cluster
    /// calls.
    auth_hold: bool,
    auth_paused_pfs: Vec<usize>,
    pub last_refresh: std::time::Instant,
    pub diff_mark: Option<DiffMark>,
    pub palette: Option<PaletteState>,
    pub help: Option<HelpState>,
    pub pf: PortForwardView,
    pub config: QbConfig,
    pub favorites: FavoritesView,
    pub profiles: ProfilesView,
    pub discovered_crds: Vec<CrdInfo>,
    pub nav_width: u16,
}

impl App {
    pub fn new(kube: KubeClient, experimental: bool, config: QbConfig) -> Self {
        let nav_items = Self::build_nav_items(&[]);
        let mut nav_state = ListState::default();
        // Select first selectable item (skip Category headers)
        let first_selectable = nav_items
            .iter()
            .position(|item| Self::is_selectable_nav(&item.kind))
            .unwrap_or(0);
        nav_state.select(Some(first_selectable));

        let mut app = Self {
            kube,
            nav: NavState {
                items: nav_items,
                state: nav_state,
            },
            resources: ResourceTable {
                entries: Vec::new(),
                state: TableState::default(),
                table_state: TableState::default(),
                counts: std::collections::HashMap::new(),
                filter: ResourceFilter {
                    text: String::new(),
                    regex: None,
                    editing: false,
                    buf: String::new(),
                },
            },
            panel: Panel::Overview,
            return_panel: None,
            overview: ClusterOverview {
                stats: None,
                scroll: 0,
                pending_rx: None,
                stats_task: None,
                metrics: None,
                metrics_rx: None,
                metrics_task: None,
            },
            resource_map: ResourceMapState {
                scroll: 0,
                sort: ResourceMapSort::Cpu,
                metrics: None,
                focus: ResourceMapFocus::Overview,
                filter: String::new(),
                filter_editing: false,
                filter_buf: String::new(),
            },
            detail: DetailState {
                value: Value::Null,
                yaml: String::new(),
                scroll: 0,
                mode: DetailMode::Smart,
                secret: None,
                name: String::new(),
                namespace: String::new(),
                auto_refresh: true,
                area_height: 0,
                related: RelatedState {
                    events: Vec::new(),
                    resources: Vec::new(),
                    cursor: None,
                    tab: 0,
                    line_start: 0,
                },
                dict: DictSelection {
                    entries: Vec::new(),
                    cursor: None,
                    line_offsets: Vec::new(),
                    expanded_keys: std::collections::HashSet::new(),
                },
            },
            log: None,
            log_detail_line: None,
            events: EventsState {
                scroll: 0,
                cursor: 0,
                auto_scroll: true,
            },
            pending_edit: None,
            pending_exec: None,
            pending_create: None,
            pending_metadata_edit: None,
            exec_terminal_override: None,
            edit_ctx: None,
            paused: false,
            auth_hold: false,
            auth_paused_pfs: Vec::new(),
            focus: Focus::Nav,
            view: View::Main,
            popup: None,
            should_quit: false,
            experimental,
            status: String::new(),
            status_history: Vec::new(),
            error: None,
            pending_load: Some(PendingLoad::DiscoverCrds),
            read_job: None,
            write_job: None,
            write_queued: None,
            last_refresh: std::time::Instant::now(),
            diff_mark: None,
            palette: None,
            help: None,
            pf: PortForwardView {
                manager: PortForwardManager::new(),
                cursor: 0,
                table_state: TableState::default(),
            },
            config,
            favorites: FavoritesView {
                cursor: 0,
                table_state: TableState::default(),
            },
            profiles: ProfilesView {
                cursor: 0,
                table_state: TableState::default(),
            },
            discovered_crds: Vec::new(),
            nav_width: 26,
        };
        app.config.active_profile_mut().kubeconfig = app.kube.kubeconfig.path.clone();
        let _ = app.config.save();
        app.restore_saved_port_forwards();
        app.update_status();
        app
    }

    /// Restore all saved port forwards from the active profile on startup.
    fn restore_saved_port_forwards(&mut self) {
        let saved: Vec<_> = self.config.active_profile().port_forwards.clone();
        for spf in &saved {
            let req = PortForwardRequest::from_saved(spf);
            if spf.paused {
                self.pf.manager.create_paused(req);
            } else {
                let client = self.kube.client.streaming.clone();
                self.pf.manager.create(client, req);
            }
        }
    }

    /// Which command context the app is currently in.
    pub fn current_context(&self) -> super::command::Ctx {
        use super::command::Ctx;
        match self.view {
            | View::Detail => Ctx::Detail,
            | View::Logs => Ctx::Logs,
            | View::EditDiff => Ctx::EditDiff,
            | View::Main if self.focus == Focus::Nav => Ctx::Nav,
            | View::Main => {
                match &self.panel {
                    | Panel::Overview => Ctx::ClusterStats,
                    | Panel::Favorites => Ctx::Resources,
                    | Panel::PortForwards => Ctx::PortForwards,
                    | Panel::Profiles => Ctx::Profiles,
                    | Panel::ResourceMap => Ctx::ClusterStats,
                    | Panel::Resources(kind) if kind.is_event() => Ctx::Events,
                    | Panel::Resources(_) => Ctx::Resources,
                }
            },
        }
    }

    /// Snapshot of app state for command availability checks.
    pub fn cmd_flags(&self) -> super::command::CmdFlags {
        let (has_pods_gt1, has_containers_gt1, following, wrapping, has_since) = if let Some(s) = &self.log {
            (
                s.pods.len() > 1,
                s.active_containers().len() > 1,
                s.following,
                s.wrap,
                s.since_seconds.is_some(),
            )
        } else {
            (false, false, false, false, false)
        };
        let effective_resource_type = self.effective_resource_type();

        super::command::CmdFlags {
            resource_type: effective_resource_type,
            experimental: self.experimental,
            has_filter: !self.resources.filter.text.is_empty(),
            has_pods_gt1,
            has_containers_gt1,
            following,
            wrapping,
            has_since,
            has_labels: self.detail.dict.entries.iter().any(|(q, ..)| q.starts_with("Labels:")),
            has_annotations: self
                .detail
                .dict
                .entries
                .iter()
                .any(|(q, ..)| q.starts_with("Annotations:")),
            dict_cursor_active: self.detail.dict.cursor.is_some(),
            has_related: !self.detail.related.resources.is_empty(),
            paused: self.paused,
            detail_auto_refresh: self.detail.auto_refresh,
            pf_count: self.pf.manager.entries().len(),
            diff_mark_set: self.diff_mark.is_some(),
            node_cordoned: if effective_resource_type == Some(ResourceType::Node) {
                if self.view == View::Detail {
                    self.detail
                        .value
                        .get("spec")
                        .and_then(|s| s.get("unschedulable"))
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false)
                } else {
                    self.resources
                        .state
                        .selected()
                        .and_then(|idx| self.resources.entries.get(idx))
                        .and_then(|e| e.columns.first())
                        .map(|s| s.contains("SchedulingDisabled"))
                        .unwrap_or(false)
                }
            } else {
                false
            },
        }
    }

    fn build_nav_items(crds: &[CrdInfo]) -> Vec<NavItem> {
        let mut items = vec![
            NavItem {
                label: " Overview".to_string(),
                kind: NavItemKind::ClusterStats,
            },
            NavItem {
                label: " Resource Map".to_string(),
                kind: NavItemKind::ResourceMap,
            },
            NavItem {
                label: " Favorites".to_string(),
                kind: NavItemKind::Favorites,
            },
            NavItem {
                label: " Port Forwards".to_string(),
                kind: NavItemKind::PortForwards,
            },
            NavItem {
                label: " Profiles".to_string(),
                kind: NavItemKind::Profiles,
            },
        ];

        // Resource categories
        for (cat, types) in ResourceType::all_by_category() {
            items.push(NavItem {
                label: cat.display_name().to_uppercase(),
                kind: NavItemKind::Category,
            });
            for rt in types {
                items.push(NavItem {
                    label: format!(" {}", rt.display_name()),
                    kind: NavItemKind::Resource(ResourceKind::Builtin(rt)),
                });
            }
        }

        // Custom Resources (discovered CRDs), grouped by API group domain
        if !crds.is_empty() {
            items.push(NavItem {
                label: "CUSTOM RESOURCES".to_string(),
                kind: NavItemKind::Category,
            });
            let mut by_group: std::collections::BTreeMap<&str, Vec<&CrdInfo>> = std::collections::BTreeMap::new();
            for crd in crds {
                by_group.entry(crd.group.as_str()).or_default().push(crd);
            }
            for (group, group_crds) in &by_group {
                items.push(NavItem {
                    label: format!("  {}", group),
                    kind: NavItemKind::SubCategory,
                });
                for crd in group_crds {
                    items.push(NavItem {
                        label: format!("   {}", crd.display_name),
                        kind: NavItemKind::Resource(ResourceKind::Custom((*crd).clone())),
                    });
                }
            }
        }

        items
    }

    fn rebuild_nav(&mut self) {
        // Preserve current selection across rebuild
        let prev_label = self
            .nav
            .state
            .selected()
            .and_then(|i| self.nav.items.get(i))
            .map(|item| item.label.clone());
        self.nav.items = Self::build_nav_items(&self.discovered_crds);
        // Try to restore selection by label
        if let Some(label) = prev_label {
            if let Some(pos) = self.nav.items.iter().position(|item| item.label == label) {
                self.nav.state.select(Some(pos));
                return;
            }
        }
        // Fallback: select first selectable
        let first = self
            .nav
            .items
            .iter()
            .position(|item| Self::is_selectable_nav(&item.kind))
            .unwrap_or(0);
        self.nav.state.select(Some(first));
    }

    fn update_status(&mut self) {
        let ctx = self.kube.context.name.as_str();
        let ns = self.kube.context.namespace.as_deref().unwrap_or("All Namespaces");
        let rt_name = self
            .selected_resource_type()
            .map(|r| r.display_name().to_string())
            .or_else(|| self.selected_crd_info().map(|c| c.display_name.clone()))
            .unwrap_or_else(|| "None".to_string());
        let count = self.resources.entries.len();
        self.status = format!("ctx: {} | ns: {} | {}: {}", ctx, ns, rt_name, count);
    }

    /// Clear all cached resource data. Called on context switch, profile
    /// switch, and namespace change.
    fn clear_cached_state(&mut self) {
        self.resources.counts.clear();
        self.resources.entries.clear();
        self.overview.stats = None;
        self.overview.metrics = None;
        self.resource_map.metrics = None;
        self.detail.value = serde_json::Value::Null;
        self.detail.yaml.clear();
        self.detail.name.clear();
        self.detail.namespace.clear();
        self.detail.related.events.clear();
        self.detail.related.resources.clear();
        self.detail.related.cursor = None;
    }

    pub fn push_status(&mut self, msg: impl Into<String>) {
        let s = msg.into();
        self.status = s.clone();
        self.status_history.push((std::time::Instant::now(), s));
        // Keep only last 10 entries
        if self.status_history.len() > 10 {
            self.status_history.remove(0);
        }
    }

    pub fn selected_kind(&self) -> Option<&ResourceKind> {
        match &self.panel {
            | Panel::Resources(kind) => Some(kind),
            | _ => None,
        }
    }

    pub fn selected_resource_type(&self) -> Option<ResourceType> {
        self.selected_kind().and_then(ResourceKind::builtin)
    }

    pub fn selected_crd_info(&self) -> Option<&CrdInfo> {
        self.selected_kind().and_then(ResourceKind::crd)
    }

    pub fn effective_resource_type(&self) -> Option<ResourceType> {
        match &self.panel {
            | Panel::Resources(kind) => kind.builtin(),
            | Panel::Favorites => {
                self.favorites_entry_at_cursor()
                    .and_then(|idx| self.config.active_profile().favorites.get(idx))
                    .and_then(|fav| fav.resource_type.builtin_type())
            },
            | _ => None,
        }
    }

    fn favorite_kind(&self, fav: &crate::config::FavoriteEntry) -> Option<ResourceKind> {
        if let Some(rt) = fav.resource_type.builtin_type() {
            return Some(ResourceKind::Builtin(rt));
        }
        let (group, plural) = fav.resource_type.custom_ref()?;
        self.discovered_crds
            .iter()
            .find(|crd| crd.group == group && crd.plural == plural)
            .cloned()
            .map(ResourceKind::Custom)
    }

    fn return_to_main(&mut self) {
        self.view = View::Main;
        self.focus = Focus::Resources;
        if let Some(panel) = self.return_panel.take() {
            self.panel = panel;
        }
    }

    fn is_secret_smart_view(&self) -> bool {
        self.detail.mode == DetailMode::Smart
            && self.selected_resource_type() == Some(ResourceType::Secret)
            && self.detail.secret.is_some()
    }

    // -----------------------------------------------------------------------
    // Resource filter
    // -----------------------------------------------------------------------

    /// Returns indices into `self.resources.entries` that match the current
    /// filter.
    pub fn visible_resource_indices(&self) -> Vec<usize> {
        if self.resources.filter.text.is_empty() {
            (0..self.resources.entries.len()).collect()
        } else {
            self.resources
                .entries
                .iter()
                .enumerate()
                .filter(|(_, e)| self.resource_matches(e))
                .map(|(i, _)| i)
                .collect()
        }
    }

    fn resource_matches(&self, entry: &ResourceEntry) -> bool {
        if let Some(re) = &self.resources.filter.regex {
            re.is_match(&entry.name) || re.is_match(&entry.namespace) || entry.columns.iter().any(|c| re.is_match(c))
        } else {
            let needle = &self.resources.filter.text;
            entry.name.contains(needle)
                || entry.namespace.contains(needle)
                || entry.columns.iter().any(|c| c.contains(needle))
        }
    }

    fn begin_resource_filter(&mut self) {
        self.resources.filter.editing = true;
        self.resources.filter.buf = self.resources.filter.text.clone();
    }

    fn apply_resource_filter(&mut self) {
        self.resources.filter.text = self.resources.filter.buf.clone();
        self.resources.filter.regex = if self.resources.filter.text.is_empty() {
            None
        } else {
            Regex::new(&self.resources.filter.text).ok()
        };
        self.resources.filter.editing = false;
        // Reset selection to first visible entry
        let visible = self.visible_resource_indices();
        if let Some(&first) = visible.first() {
            self.resources.state.select(Some(first));
            self.events.cursor = 0;
        }
    }

    fn cancel_resource_filter(&mut self) {
        self.resources.filter.editing = false;
        self.resources.filter.buf = self.resources.filter.text.clone();
    }

    fn clear_resource_filter(&mut self) {
        self.resources.filter.text.clear();
        self.resources.filter.regex = None;
        self.resources.filter.buf.clear();
    }

    // -----------------------------------------------------------------------
    // Deferred loading
    // -----------------------------------------------------------------------

    pub fn is_busy(&self) -> bool {
        self.read_job.is_some()
            || self.write_job.is_some()
            || self.overview.pending_rx.is_some()
            || self.overview.metrics_rx.is_some()
    }

    /// Drop in-flight reads and log buffers after a sleep/wake. Writes are left
    /// alone: the request may already have reached the API server.
    pub fn on_wake(&mut self) {
        self.read_job = None;
        self.pending_load = None;
        if let Some(task) = self.overview.stats_task.take() {
            task.abort();
        }
        self.overview.pending_rx = None;
        if let Some(task) = self.overview.metrics_task.take() {
            task.abort();
        }
        self.overview.metrics_rx = None;
        if let Some(log) = &mut self.log {
            log.stop_following();
            log.trim();
        }
        self.paused = true;
        self.push_status("Resumed from sleep — streams reset, refresh paused");
    }

    pub fn auth_blocked(&self) -> bool {
        self.auth_hold
    }

    /// Stop cluster calls and show the login prompt on this frame's next draw.
    fn enter_auth_hold(&mut self, raw: &str) {
        if self.auth_hold {
            return;
        }
        self.auth_hold = true;
        self.paused = true;
        self.pending_load = None;
        self.read_job = None;
        if let Some(task) = self.overview.stats_task.take() {
            task.abort();
        }
        self.overview.pending_rx = None;
        if let Some(task) = self.overview.metrics_task.take() {
            task.abort();
        }
        self.overview.metrics_rx = None;
        if let Some(log) = &mut self.log {
            log.stop_following();
        }
        self.auth_paused_pfs = self.pf.manager.pause_running();
        self.palette = None;
        self.help = None;
        self.error = None;
        self.popup = Some(Popup::AuthExpired {
            detail: KubeClient::display_error(raw),
        });
    }

    fn report_cluster_error(&mut self, failure: ClusterFailure) {
        if self.auth_hold {
            return;
        }
        match failure {
            | ClusterFailure::AuthExpired { detail } => self.enter_auth_hold(&detail),
            | ClusterFailure::Other { detail } => self.error = Some(detail),
        }
    }

    fn retry_auth(&mut self) {
        self.auth_hold = false;
        self.paused = false;
        self.popup = None;
        self.error = None;
        let client = self.kube.client.streaming.clone();
        let mut ids = std::mem::take(&mut self.auth_paused_pfs);
        ids.extend(
            self.pf
                .manager
                .entries()
                .iter()
                .filter(|entry| matches!(entry.status, portforward::PortForwardStatus::AuthExpired))
                .map(|entry| entry.id),
        );
        for id in ids {
            let _ = self.pf.manager.resume_spawn(id, client.clone());
        }
        self.push_status("Retrying cluster connection…");
        self.pending_load = Some(match self.view {
            | View::Detail if !self.detail.name.is_empty() => {
                PendingLoad::ResourceDetail {
                    name: self.detail.name.clone(),
                    namespace: self.detail.namespace.clone(),
                }
            },
            | View::Logs => PendingLoad::ReloadLogs,
            | _ => {
                match &self.panel {
                    | Panel::Resources(_) => PendingLoad::Resources,
                    | Panel::Overview | Panel::ResourceMap => PendingLoad::ClusterStats,
                    | _ => PendingLoad::DiscoverCrds,
                }
            },
        });
    }

    fn handle_auth_hold_key(&mut self, key: KeyEvent) {
        match key.code {
            | KeyCode::Char('r') => self.retry_auth(),
            | KeyCode::Char('q') => {
                self.should_quit = true;
            },
            | _ => {},
        }
    }

    pub fn kick_pending(&mut self) {
        if self.auth_hold {
            self.pending_load = None;
            return;
        }
        let Some(load) = self.pending_load.take() else {
            return;
        };
        if let Some(job) = &self.read_job {
            if job.token == ReadToken::Interactive {
                self.pending_load = Some(load);
                return;
            }
            if job.token == ReadToken::Nav(load.clone()) {
                return;
            }
        }
        self.start_nav(load);
    }

    pub fn poll_jobs(&mut self) {
        let read = match self.read_job.as_mut().map(|job| job.slot.poll()) {
            | Some(SlotPoll::Ready(apply)) => {
                self.read_job = None;
                Some(apply)
            },
            | Some(SlotPoll::Closed) => {
                self.read_job = None;
                None
            },
            | _ => None,
        };
        if let Some(apply) = read {
            apply(self);
        }

        let write = match self.write_job.as_mut().map(|job| job.poll()) {
            | Some(SlotPoll::Ready(apply)) => {
                self.write_job = None;
                Some(apply)
            },
            | Some(SlotPoll::Closed) => {
                self.write_job = None;
                None
            },
            | _ => None,
        };
        if let Some(apply) = write {
            apply(self);
            if let Some(queued) = self.write_queued.take() {
                self.start_write(queued);
            }
        }
    }

    fn spawn_read(&mut self, token: ReadToken, fut: impl std::future::Future<Output=Apply>+Send+'static) {
        self.read_job = Some(ReadFlight {
            slot: Slot::spawn(fut),
            token,
        });
    }

    fn enqueue_write(&mut self, write: Write) {
        if self.auth_hold {
            self.push_status("Cluster login expired — press r to retry or q to quit");
            return;
        }
        if self.write_job.is_some() {
            self.write_queued = Some(write);
            self.push_status("Change queued");
            return;
        }
        self.start_write(write);
    }

    fn start_nav(&mut self, load: PendingLoad) {
        let kube = self.kube.clone();
        match load.clone() {
            | PendingLoad::ClusterStats => {
                self.load_cluster_stats();
            },
            | PendingLoad::Resources => {
                let Some(kind) = self.selected_kind().cloned() else {
                    return;
                };
                let prev = self
                    .resources
                    .state
                    .selected()
                    .and_then(|idx| self.resources.entries.get(idx))
                    .map(|e| (e.name.clone(), e.namespace.clone()));
                self.spawn_read(ReadToken::Nav(load), async move {
                    let result = kube.list_kind(&kind).await.map_err(ClusterFailure::from_anyhow);
                    let apply: Apply = Box::new(move |app: &mut App| app.finish_resources(kind, prev, result));
                    apply
                });
            },
            | PendingLoad::DiscoverCrds => {
                self.spawn_read(ReadToken::Nav(load), async move {
                    let result = kube.discover_crds().await.map_err(ClusterFailure::from_anyhow);
                    let apply: Apply = Box::new(move |app: &mut App| app.finish_crds(result));
                    apply
                });
            },
            | PendingLoad::Namespaces => {
                let current = self.kube.context.namespace.clone();
                self.spawn_read(ReadToken::Nav(load), async move {
                    let result = kube.list_namespaces().await.map_err(ClusterFailure::from_anyhow);
                    let apply: Apply = Box::new(move |app: &mut App| app.finish_namespaces(current, result));
                    apply
                });
            },
            | PendingLoad::SwitchContext(ctx) => {
                self.enqueue_write(Write::SwitchContext { ctx });
            },
            | PendingLoad::ResourceDetail { name, namespace } => {
                let Some(kind) = self.selected_kind().cloned() else {
                    return;
                };
                self.spawn_read(ReadToken::Nav(load), async move {
                    let result = App::fetch_detail(&kube, kind, namespace, name).await;
                    let apply: Apply = Box::new(move |app: &mut App| app.finish_detail(result));
                    apply
                });
            },
            | PendingLoad::Logs { name, namespace } => {
                let Some(rt) = self.selected_resource_type().filter(|rt| rt.supports_logs()) else {
                    return;
                };
                self.spawn_read(ReadToken::Nav(load), async move {
                    let result = App::fetch_log_view(&kube, rt, namespace, name).await;
                    let apply: Apply = Box::new(move |app: &mut App| app.finish_logs(result));
                    apply
                });
            },
            | PendingLoad::ReloadLogs => {
                let Some(state) = &mut self.log else {
                    return;
                };
                state.stop_following();
                let pairs = state.active_streams();
                let ns = state.namespace.clone();
                let since = state.since_seconds;
                self.spawn_read(ReadToken::Nav(load), async move {
                    let result = kube
                        .fetch_logs_multi(&ns, &pairs, 500, since)
                        .await
                        .map_err(ClusterFailure::from_anyhow);
                    let apply: Apply = Box::new(move |app: &mut App| app.finish_reload_logs(result));
                    apply
                });
            },
            | PendingLoad::CheckLogPods => {
                let Some(state) = &self.log else {
                    return;
                };
                let Some(source) = &state.source else {
                    if let Some(state) = &mut self.log {
                        state.last_pod_check = std::time::Instant::now();
                    }
                    return;
                };
                let rt = source.resource_type;
                let name = source.resource_name.clone();
                let ns = state.namespace.clone();
                self.spawn_read(ReadToken::Nav(load), async move {
                    let result = kube
                        .find_pods(rt, &ns, &name)
                        .await
                        .map_err(ClusterFailure::from_anyhow);
                    let apply: Apply = Box::new(move |app: &mut App| app.finish_log_pods(result));
                    apply
                });
            },
        }
    }

    async fn fetch_detail(
        kube: &KubeClient,
        kind: ResourceKind,
        namespace: String,
        name: String,
    ) -> Result<DetailPayload, ClusterFailure> {
        let value = kube
            .get_kind(&kind, &namespace, &name)
            .await
            .map_err(ClusterFailure::from_anyhow)?;
        let events = kube.fetch_related_events(&namespace, &name).await.unwrap_or_default();
        let related = if let Some(rt) = kind.builtin() {
            kube.fetch_related_resources(rt, &namespace, &name, &value).await
        } else {
            Vec::new()
        };
        Ok(DetailPayload {
            kind,
            name,
            namespace,
            value,
            events,
            related,
        })
    }

    async fn fetch_log_view(
        kube: &KubeClient,
        rt: ResourceType,
        namespace: String,
        name: String,
    ) -> Result<LogPayload, ClusterFailure> {
        let pods = kube
            .find_pods(rt, &namespace, &name)
            .await
            .map_err(|e| ClusterFailure::from_anyhow(e).context("Failed to find pods"))?;
        if pods.is_empty() {
            return Err(ClusterFailure::other("No pods found for this resource"));
        }
        let pairs: Vec<(String, String)> = pods
            .iter()
            .flat_map(|p| p.containers.iter().map(move |c| (p.name.clone(), c.clone())))
            .collect();
        let since = Some(3600);
        let lines = kube
            .fetch_logs_multi(&namespace, &pairs, 500, since)
            .await
            .map_err(|e| ClusterFailure::from_anyhow(e).context("Failed to fetch logs"))?;
        Ok(LogPayload {
            rt,
            name,
            namespace,
            pods,
            lines,
            since,
        })
    }

    fn start_write(&mut self, write: Write) {
        let kube = self.kube.clone();
        let fut = async move {
            match write {
                | Write::Delete { kind, name, namespace } => {
                    let display = kind.display_name().to_string();
                    let result = kube
                        .delete_kind(&kind, &namespace, &name)
                        .await
                        .map_err(ClusterFailure::from_anyhow);
                    let apply: Apply = Box::new(move |app: &mut App| {
                        match result {
                            | Ok(()) => {
                                app.push_status(format!("Deleted {}/{}", display, name));
                                app.error = None;
                                if app.view == View::Detail {
                                    app.return_to_main();
                                }
                                if !matches!(app.panel, Panel::Favorites) {
                                    app.pending_load = Some(PendingLoad::Resources);
                                }
                            },
                            | Err(e) => app.report_cluster_error(e.context("Delete failed")),
                        }
                    });
                    apply
                },
                | Write::Drain { name } => {
                    let result = kube.drain_node(&name).await.map_err(ClusterFailure::from_anyhow);
                    let apply: Apply = Box::new(move |app: &mut App| {
                        match result {
                            | Ok(evicted) => {
                                app.push_status(format!("Drained node {} ({} pods evicted)", name, evicted));
                                app.error = None;
                                app.pending_load = Some(PendingLoad::Resources);
                            },
                            | Err(e) => app.report_cluster_error(e.context("Drain failed")),
                        }
                    });
                    apply
                },
                | Write::Restart { rt, name, namespace } => {
                    let result = kube
                        .restart_workload(rt, &namespace, &name)
                        .await
                        .map_err(ClusterFailure::from_anyhow);
                    let apply: Apply = Box::new(move |app: &mut App| {
                        match result {
                            | Ok(()) => {
                                app.push_status(format!("Restarted {}/{}", rt.display_name(), name));
                                app.error = None;
                            },
                            | Err(e) => app.report_cluster_error(e.context("Restart failed")),
                        }
                    });
                    apply
                },
                | Write::Cordon { name, cordon } => {
                    let result = if cordon {
                        kube.cordon_node(&name).await
                    } else {
                        kube.uncordon_node(&name).await
                    }
                    .map_err(ClusterFailure::from_anyhow);
                    let apply: Apply = Box::new(move |app: &mut App| {
                        match result {
                            | Ok(()) => {
                                let action = if cordon { "Cordoned" } else { "Uncordoned" };
                                app.push_status(format!("{} node {}", action, name));
                                app.error = None;
                                if app.view == View::Detail {
                                    app.pending_load = Some(PendingLoad::ResourceDetail {
                                        name: name.clone(),
                                        namespace: String::new(),
                                    });
                                } else {
                                    app.pending_load = Some(PendingLoad::Resources);
                                }
                            },
                            | Err(e) => app.report_cluster_error(e.context("Cordon failed")),
                        }
                    });
                    apply
                },
                | Write::Scale {
                    rt,
                    name,
                    namespace,
                    replicas,
                } => {
                    let result = kube
                        .scale_resource(rt, &namespace, &name, replicas)
                        .await
                        .map_err(ClusterFailure::from_anyhow);
                    let apply: Apply = Box::new(move |app: &mut App| {
                        match result {
                            | Ok(()) => {
                                app.push_status(format!(
                                    "Scaled {}/{} to {} replicas",
                                    rt.display_name(),
                                    name,
                                    replicas
                                ));
                                app.error = None;
                                app.pending_load = Some(PendingLoad::Resources);
                            },
                            | Err(e) => app.report_cluster_error(e.context("Scale failed")),
                        }
                    });
                    apply
                },
                | Write::Trigger {
                    namespace,
                    cronjob,
                    job_name,
                } => {
                    let result = kube
                        .trigger_cronjob(&namespace, &cronjob, &job_name)
                        .await
                        .map_err(ClusterFailure::from_anyhow);
                    let apply: Apply = Box::new(move |app: &mut App| {
                        match result {
                            | Ok(created) => {
                                app.push_status(format!("Created job {}", created));
                                app.error = None;
                                app.panel = Panel::Resources(ResourceKind::Builtin(ResourceType::Job));
                                app.pending_load = Some(PendingLoad::ResourceDetail {
                                    name: created,
                                    namespace,
                                });
                            },
                            | Err(e) => app.report_cluster_error(e.context("Trigger failed")),
                        }
                    });
                    apply
                },
                | Write::Create { yaml, crds } => {
                    let result = kube
                        .create_resource_yaml(&yaml, &crds)
                        .await
                        .map_err(ClusterFailure::from_anyhow);
                    let apply: Apply = Box::new(move |app: &mut App| {
                        match result {
                            | Ok(_) => {
                                app.push_status("Resource created");
                                app.error = None;
                                app.pending_load = Some(PendingLoad::Resources);
                            },
                            | Err(e) => app.report_cluster_error(e.context("Create failed")),
                        }
                    });
                    apply
                },
                | Write::Replace {
                    kind,
                    name,
                    namespace,
                    yaml,
                } => {
                    let display = kind.display_name().to_string();
                    let result = kube
                        .replace_kind_yaml(&kind, &namespace, &name, &yaml)
                        .await
                        .map_err(ClusterFailure::from_anyhow);
                    let apply: Apply = Box::new(move |app: &mut App| {
                        match result {
                            | Ok(value) => {
                                app.push_status(format!("Applied changes to {}/{}", display, name));
                                app.error = None;
                                app.detail.yaml = serde_yaml::to_string(&value).unwrap_or_default();
                                app.detail.value = value;
                                app.detail.name = name;
                                app.detail.namespace = namespace;
                                app.edit_ctx = None;
                                app.view = View::Detail;
                            },
                            | Err(e) => {
                                if e.is_auth() {
                                    app.enter_auth_hold(e.detail());
                                } else if let Some(ctx) = &mut app.edit_ctx {
                                    ctx.error = Some(e.detail().to_string());
                                }
                            },
                        }
                    });
                    apply
                },
                | Write::PatchMetadata {
                    resource,
                    name,
                    namespace,
                    field,
                    patch,
                } => {
                    let (labels, annotations) = match field {
                        | "labels" => (Some(patch), None),
                        | _ => (None, Some(patch)),
                    };
                    let result = kube
                        .patch_kind_metadata(&resource, &namespace, &name, labels.as_ref(), annotations.as_ref())
                        .await
                        .map_err(ClusterFailure::from_anyhow);
                    let apply: Apply = Box::new(move |app: &mut App| {
                        match result {
                            | Ok(_) => {
                                app.push_status(format!("Updated {} on {}", field, name));
                                app.error = None;
                                app.pending_load = Some(PendingLoad::ResourceDetail { name, namespace });
                            },
                            | Err(e) => app.report_cluster_error(e.context("Patch failed")),
                        }
                    });
                    apply
                },
                | Write::SwitchContext { ctx } => {
                    let mut kube = kube;
                    let result = kube.switch_context(&ctx).await.map_err(ClusterFailure::from_anyhow);
                    let apply: Apply = Box::new(move |app: &mut App| app.finish_context_switch(ctx, kube, result));
                    apply
                },
                | Write::LoadKubeconfig { path } => {
                    let result = KubeClient::new(Some(path), None, None)
                        .await
                        .map_err(ClusterFailure::from_anyhow);
                    let apply: Apply = Box::new(move |app: &mut App| app.finish_kubeconfig(result));
                    apply
                },
                | Write::SwitchProfile {
                    name,
                    kubeconfig,
                    context,
                    same_kubeconfig,
                } => {
                    let result = if same_kubeconfig {
                        if let Some(ctx) = context.clone() {
                            let mut kube = kube;
                            let switched = kube.switch_context(&ctx).await.map_err(ClusterFailure::from_anyhow);
                            switched.map(|_| kube)
                        } else {
                            Ok(kube)
                        }
                    } else {
                        KubeClient::new(kubeconfig, context, None)
                            .await
                            .map_err(ClusterFailure::from_anyhow)
                    };
                    let apply: Apply = Box::new(move |app: &mut App| app.finish_profile_switch(name, result));
                    apply
                },
            }
        };
        self.write_job = Some(Slot::spawn(fut));
    }

    fn finish_kubeconfig(&mut self, result: Result<KubeClient, ClusterFailure>) {
        match result {
            | Ok(new_client) => {
                self.pf.manager.cancel_all();
                self.kube = new_client;
                self.config.active_profile_mut().kubeconfig = self.kube.kubeconfig.path.clone();
                if let Err(e) = self.config.save() {
                    self.error = Some(format!("Failed to save config: {}", e));
                }
                self.panel = Panel::Overview;
                self.resources.counts.clear();
                self.overview.scroll = 0;
                self.discovered_crds.clear();
                self.pending_load = Some(PendingLoad::DiscoverCrds);
                self.error = None;
                self.push_status("Kubeconfig loaded");
                let first = self
                    .nav
                    .items
                    .iter()
                    .position(|item| Self::is_selectable_nav(&item.kind))
                    .unwrap_or(0);
                self.nav.state.select(Some(first));
            },
            | Err(e) => self.report_cluster_error(e.context("Failed to load kubeconfig")),
        }
    }

    fn finish_profile_switch(&mut self, name: String, result: Result<KubeClient, ClusterFailure>) {
        match result {
            | Ok(client) => {
                self.kube = client;
                self.config.active_profile_mut().kubeconfig = self.kube.kubeconfig.path.clone();
                self.push_status(format!("Switched to profile '{}'", name));
            },
            | Err(e) => {
                self.report_cluster_error(e.context("Failed to switch profile client"));
            },
        }
        self.discovered_crds.clear();
        self.pending_load = Some(PendingLoad::DiscoverCrds);
        self.restore_saved_port_forwards();
        if let Err(e) = self.config.save() {
            self.error = Some(format!("Failed to save config: {}", e));
        }
    }

    /// Poll the log stream channel for new lines (called every event loop
    /// tick). Paused state suppresses polling. Also triggers pod re-discovery
    /// for rolling updates when in follow mode.
    pub fn poll_log_stream(&mut self) {
        if self.paused {
            return;
        }
        if let Some(state) = &mut self.log {
            state.poll_stream();
            // Queue pod re-discovery if due (only when following a workload)
            if state.needs_pod_check() && self.pending_load.is_none() {
                self.pending_load = Some(PendingLoad::CheckLogPods);
            }
        }
    }

    fn finish_resources(
        &mut self,
        kind: ResourceKind,
        prev_selected: Option<(String, String)>,
        result: Result<Vec<ResourceEntry>, ClusterFailure>,
    ) {
        if self.selected_kind() != Some(&kind) {
            return;
        }
        match result {
            | Ok(mut entries) => {
                // Sort events chronologically (oldest first, newest at bottom)
                if kind.is_event() {
                    entries.sort_by(|a, b| {
                        let ts_a = a.sort_key.as_deref().unwrap_or("");
                        let ts_b = b.sort_key.as_deref().unwrap_or("");
                        ts_a.cmp(ts_b)
                    });
                }
                self.resources.counts.insert(kind.clone(), entries.len());
                self.resources.entries = entries;
                let new_idx = prev_selected
                    .and_then(|(name, ns)| {
                        self.resources
                            .entries
                            .iter()
                            .position(|e| e.name == name && e.namespace == ns)
                    })
                    .or_else(|| {
                        if self.resources.entries.is_empty() {
                            None
                        } else {
                            let prev_idx = self.resources.state.selected().unwrap_or(0);
                            Some(prev_idx.min(self.resources.entries.len() - 1))
                        }
                    });
                // Preserve viewport offset — only update the selected index, not the
                // entire TableState, so auto-refresh doesn't jump the scroll position.
                self.resources.state.select(new_idx);
                if kind.is_event() && !self.resources.entries.is_empty() {
                    self.events.cursor = self.events.cursor.min(self.resources.entries.len() - 1);
                }
                self.error = None;
            },
            | Err(e) => {
                self.resources.entries.clear();
                self.resources.state.select(None);
                self.report_cluster_error(e.context(&format!("Failed to load {}", kind.display_name())));
            },
        }
        self.last_refresh = std::time::Instant::now();
        self.update_status();
    }

    fn finish_crds(&mut self, result: Result<Vec<CrdInfo>, ClusterFailure>) {
        if let Err(e) = &result {
            self.report_cluster_error(e.clone().context("Failed to discover CRDs"));
        }
        if let Ok(crds) = result {
            self.discovered_crds = crds;
            self.rebuild_nav();
        }
        if self.auth_hold {
            return;
        }
        match &self.panel {
            | Panel::Overview => {
                self.pending_load = Some(PendingLoad::ClusterStats);
            },
            | Panel::Resources(_) => {
                self.pending_load = Some(PendingLoad::Resources);
            },
            | _ => {},
        }
    }

    pub fn maybe_auto_refresh(&mut self) {
        if self.paused || self.auth_hold {
            return;
        }
        if self.view == View::Main
            && self.popup.is_none()
            && self.pending_load.is_none()
            && self.read_job.is_none()
            && self.last_refresh.elapsed() >= std::time::Duration::from_secs(2)
        {
            match &self.panel {
                | Panel::Overview => {
                    self.pending_load = Some(PendingLoad::ClusterStats);
                },
                | Panel::Resources(_) => {
                    self.pending_load = Some(PendingLoad::Resources);
                },
                | Panel::ResourceMap => {
                    self.load_metrics();
                    self.last_refresh = std::time::Instant::now();
                },
                | _ => {},
            }
        }
        // Detail view watch mode
        if self.detail.auto_refresh
            && self.view == View::Detail
            && self.popup.is_none()
            && self.pending_load.is_none()
            && self.read_job.is_none()
            && !self.detail.name.is_empty()
            && self.last_refresh.elapsed() >= std::time::Duration::from_secs(2)
        {
            self.pending_load = Some(PendingLoad::ResourceDetail {
                name: self.detail.name.clone(),
                namespace: self.detail.namespace.clone(),
            });
            self.last_refresh = std::time::Instant::now();
        }
    }

    fn finish_namespaces(&mut self, current: Option<String>, result: Result<Vec<String>, ClusterFailure>) {
        match result {
            | Ok(namespaces) => {
                let mut items = vec![ALL_NAMESPACES_LABEL.to_string()];
                items.extend(namespaces);
                let mut state = ListState::default();
                match current.as_deref() {
                    | None => state.select(Some(0)),
                    | Some(current) => {
                        if let Some(idx) = items.iter().position(|n| n == current) {
                            state.select(Some(idx));
                        } else {
                            state.select(Some(0));
                        }
                    },
                }
                self.popup = Some(Popup::NamespaceSelect { items, state });
                self.error = None;
            },
            | Err(e) => {
                self.report_cluster_error(e.context("Failed to load namespaces"));
            },
        }
    }

    fn finish_context_switch(&mut self, ctx: String, kube: KubeClient, result: Result<(), ClusterFailure>) {
        match result {
            | Ok(()) => {
                self.kube = kube;
                self.clear_cached_state();
                self.discovered_crds.clear();
                self.pending_load = Some(PendingLoad::DiscoverCrds);
                self.error = None;
                self.config.active_profile_mut().context = Some(ctx);
                if let Err(e) = self.config.save() {
                    self.error = Some(format!("Failed to save config: {}", e));
                }
            },
            | Err(e) => {
                self.report_cluster_error(e.context("Failed to switch context"));
            },
        }
    }

    fn finish_detail(&mut self, result: Result<DetailPayload, ClusterFailure>) {
        let payload = match result {
            | Ok(payload) => payload,
            | Err(e) => {
                self.report_cluster_error(e.context("Failed to load resource"));
                return;
            },
        };
        if self.selected_kind() != Some(&payload.kind) {
            return;
        }
        let DetailPayload {
            kind,
            name,
            namespace,
            value,
            events,
            related,
        } = payload;
        let rt = kind.builtin();
        self.detail.yaml = serde_yaml::to_string(&value).unwrap_or_default();
        let is_same_resource = self.detail.name == name && self.detail.namespace == namespace;
        if rt == Some(ResourceType::Secret) {
            if is_same_resource && self.detail.auto_refresh && self.detail.secret.is_some() {
                if let Some(state) = &mut self.detail.secret {
                    state.update_values(&value);
                }
            } else {
                self.detail.secret = Some(SecretDetailState::from_value(&value));
            }
        } else {
            self.detail.secret = None;
        }
        self.detail.value = value;
        self.detail.name = name;
        self.detail.namespace = namespace;
        if !self.detail.auto_refresh || !is_same_resource {
            self.detail.scroll = 0;
            self.detail.mode = DetailMode::Smart;
            self.detail.dict.expanded_keys.clear();
            self.detail.dict.entries.clear();
            self.detail.dict.cursor = None;
            self.detail.dict.line_offsets.clear();
        }
        self.view = View::Detail;
        self.error = None;
        self.detail.related.events = events;
        if is_same_resource && self.detail.auto_refresh {
            if let Some(cursor) = self.detail.related.cursor {
                if let Some(old) = self.detail.related.resources.get(cursor) {
                    let old_name = old.name.clone();
                    let old_ns = old.namespace.clone();
                    let old_rt = old.resource_type;
                    self.detail.related.cursor = related
                        .iter()
                        .position(|r| r.resource_type == old_rt && r.name == old_name && r.namespace == old_ns);
                }
            }
        } else {
            self.detail.related.cursor = None;
            self.detail.related.tab = 0;
        }
        self.detail.related.resources = related;
        self.last_refresh = std::time::Instant::now();
    }

    fn finish_logs(&mut self, result: Result<LogPayload, ClusterFailure>) {
        match result {
            | Ok(payload) => {
                let mut state = LogViewState::new(payload.pods, payload.namespace, payload.lines);
                state.since_seconds = payload.since;
                if payload.rt != ResourceType::Pod {
                    state.source = Some(super::logs::LogSource {
                        resource_type: payload.rt,
                        resource_name: payload.name,
                    });
                }
                self.log = Some(state);
                self.view = View::Logs;
                self.error = None;
            },
            | Err(e) => {
                self.report_cluster_error(e);
            },
        }
    }

    fn finish_reload_logs(&mut self, result: Result<Vec<super::logs::LogLine>, ClusterFailure>) {
        let Some(log_state) = &mut self.log else {
            return;
        };
        match result {
            | Ok(lines) => {
                for line in &lines {
                    log_state.ensure_pod_color(&line.pod);
                }
                log_state.lines = lines;
                log_state.trim();
                log_state.selected_line = None;
                log_state.selection_anchor = None;
                log_state.scroll = 0;
                log_state.auto_scroll = true;
                self.error = None;
            },
            | Err(e) => {
                self.report_cluster_error(e.context("Failed to reload logs"));
            },
        }
    }

    fn finish_log_pods(&mut self, result: Result<Vec<crate::k8s::PodInfo>, ClusterFailure>) {
        match result {
            | Ok(new_pods) => {
                let client = self.kube.client.streaming.clone();
                if let Some(state) = &mut self.log {
                    state.integrate_new_pods(new_pods, client);
                }
            },
            | Err(e) => {
                if let Some(state) = &mut self.log {
                    state.last_pod_check = std::time::Instant::now();
                }
                self.report_cluster_error(e);
            },
        }
    }

    fn load_cluster_stats(&mut self) {
        if self.auth_hold || self.overview.pending_rx.is_some() {
            return;
        }
        let client = self.kube.client.default.clone();
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.overview.pending_rx = Some(rx);
        self.overview.stats_task = Some(tokio::spawn(async move {
            let result = crate::k8s::KubeClient::fetch_cluster_stats_with_client(&client).await;
            let _ = tx.send(result);
        }));
        // Also refresh metrics
        self.load_metrics();
    }

    /// Poll background cluster stats fetch — called every event loop tick.
    pub fn poll_cluster_stats(&mut self) {
        let rx = match &mut self.overview.pending_rx {
            | Some(rx) => rx,
            | None => return,
        };
        match rx.try_recv() {
            | Ok(Ok(stats)) => {
                self.overview.stats = Some(stats);
                self.error = None;
                self.overview.pending_rx = None;
                self.last_refresh = std::time::Instant::now();
            },
            | Ok(Err(e)) => {
                self.overview.pending_rx = None;
                self.last_refresh = std::time::Instant::now();
                self.report_cluster_error(ClusterFailure::from_anyhow(e).context("Failed to load cluster stats"));
            },
            | Err(tokio::sync::oneshot::error::TryRecvError::Empty) => {
                // Still loading — don't block
            },
            | Err(tokio::sync::oneshot::error::TryRecvError::Closed) => {
                self.overview.pending_rx = None;
            },
        }
    }

    /// Spawn a background metrics fetch (for overview and resource map).
    fn load_metrics(&mut self) {
        if self.auth_hold || self.overview.metrics_rx.is_some() {
            return;
        }
        let client = self.kube.client.default.clone();
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.overview.metrics_rx = Some(rx);
        self.overview.metrics_task = Some(tokio::spawn(async move {
            let data = crate::k8s::KubeClient::fetch_metrics_with_client(&client).await;
            let _ = tx.send(data);
        }));
    }

    /// Poll background metrics fetch — called every event loop tick.
    pub fn poll_metrics(&mut self) {
        let rx = match &mut self.overview.metrics_rx {
            | Some(rx) => rx,
            | None => return,
        };
        match rx.try_recv() {
            | Ok(data) => {
                // Merge node usage into node_list for overview rendering
                if data.available {
                    if let Some(stats) = &mut self.overview.stats {
                        for node in &mut stats.node_list {
                            if let Some(nm) = data.node_metrics.iter().find(|m| m.name == node.name) {
                                node.usage = Some(crate::k8s::ResourceUsage {
                                    cpu_cores: nm.cpu_cores,
                                    memory_bytes: nm.memory_bytes,
                                });
                            }
                        }
                    }
                }
                self.resource_map.metrics = if data.available { Some(data.clone()) } else { None };
                self.overview.metrics = if data.available { Some(data) } else { None };
                self.overview.metrics_rx = None;
            },
            | Err(tokio::sync::oneshot::error::TryRecvError::Empty) => {},
            | Err(tokio::sync::oneshot::error::TryRecvError::Closed) => {
                self.overview.metrics_rx = None;
            },
        }
    }

    /// Count of workloads visible in the resource map after namespace + filter.
    fn filtered_workload_count(&self) -> usize {
        self.resource_map
            .metrics
            .as_ref()
            .map(|m| {
                let ns = self.kube.context.namespace.as_deref();
                let f = &self.resource_map.filter;
                m.workload_metrics
                    .iter()
                    .filter(|w| ns.is_none() || ns == Some(w.namespace.as_str()))
                    .filter(|w| {
                        f.is_empty()
                            || w.name.contains(f.as_str())
                            || w.namespace.contains(f.as_str())
                            || w.kind.contains(f.as_str())
                    })
                    .count()
            })
            .unwrap_or(0)
    }

    fn handle_resource_map_key(&mut self, key: KeyEvent) {
        // ── Filter editing mode ──────────────────────────────
        if self.resource_map.filter_editing {
            match key.code {
                | KeyCode::Enter => {
                    self.resource_map.filter = self.resource_map.filter_buf.clone();
                    self.resource_map.filter_editing = false;
                    // Reset cursor to 0 after filter change
                    if let ResourceMapFocus::Workloads { ref mut cursor } = self.resource_map.focus {
                        *cursor = 0;
                    }
                },
                | KeyCode::Esc => {
                    self.resource_map.filter_buf = self.resource_map.filter.clone();
                    self.resource_map.filter_editing = false;
                },
                | KeyCode::Backspace => {
                    self.resource_map.filter_buf.pop();
                },
                | KeyCode::Char(c) => {
                    self.resource_map.filter_buf.push(c);
                },
                | _ => {},
            }
            return;
        }

        let wl_count = self.filtered_workload_count();

        match &mut self.resource_map.focus {
            | ResourceMapFocus::Workloads { cursor } => {
                match key.code {
                    | KeyCode::Up | KeyCode::Char('k') => {
                        *cursor = cursor.saturating_sub(1);
                    },
                    | KeyCode::Down | KeyCode::Char('j') => {
                        if wl_count > 0 {
                            *cursor = (*cursor + 1).min(wl_count.saturating_sub(1));
                        }
                    },
                    | KeyCode::PageUp | KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        *cursor = cursor.saturating_sub(10);
                    },
                    | KeyCode::PageDown | KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        if wl_count > 0 {
                            *cursor = (*cursor + 10).min(wl_count.saturating_sub(1));
                        }
                    },
                    | KeyCode::Home | KeyCode::Char('g') => {
                        *cursor = 0;
                    },
                    | KeyCode::End | KeyCode::Char('G') => {
                        if wl_count > 0 {
                            *cursor = wl_count.saturating_sub(1);
                        }
                    },
                    | KeyCode::Char('/') => {
                        self.resource_map.filter_editing = true;
                        self.resource_map.filter_buf = self.resource_map.filter.clone();
                    },
                    | KeyCode::Char('x') => {
                        self.resource_map.filter.clear();
                        self.resource_map.filter_buf.clear();
                        *cursor = 0;
                    },
                    | KeyCode::Char('w') | KeyCode::Esc => {
                        self.resource_map.focus = ResourceMapFocus::Overview;
                    },
                    | KeyCode::Char('m') => {
                        self.resource_map.sort = match self.resource_map.sort {
                            | ResourceMapSort::Cpu => ResourceMapSort::Memory,
                            | ResourceMapSort::Memory => ResourceMapSort::Cpu,
                        };
                    },
                    | _ => {},
                }
            },
            | ResourceMapFocus::Overview => {
                match key.code {
                    | KeyCode::Char('w') => {
                        if wl_count > 0 {
                            self.resource_map.focus = ResourceMapFocus::Workloads { cursor: 0 };
                        }
                    },
                    | KeyCode::Up | KeyCode::Char('k') => {
                        self.resource_map.scroll = self.resource_map.scroll.saturating_sub(1);
                    },
                    | KeyCode::Down | KeyCode::Char('j') => {
                        self.resource_map.scroll = self.resource_map.scroll.saturating_add(1);
                    },
                    | KeyCode::PageUp | KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        self.resource_map.scroll = self.resource_map.scroll.saturating_sub(20);
                    },
                    | KeyCode::PageDown | KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        self.resource_map.scroll = self.resource_map.scroll.saturating_add(20);
                    },
                    | KeyCode::Home => {
                        self.resource_map.scroll = 0;
                    },
                    | KeyCode::Char('m') => {
                        self.resource_map.sort = match self.resource_map.sort {
                            | ResourceMapSort::Cpu => ResourceMapSort::Memory,
                            | ResourceMapSort::Memory => ResourceMapSort::Cpu,
                        };
                    },
                    | _ => {},
                }
            },
        }

        // Clamp cursor after filter/sort changes
        let count = self.filtered_workload_count();
        if let ResourceMapFocus::Workloads { ref mut cursor } = self.resource_map.focus {
            if count == 0 {
                *cursor = 0;
            } else if *cursor >= count {
                *cursor = count.saturating_sub(1);
            }
        }
    }

    // -----------------------------------------------------------------------
    // Event handling
    // -----------------------------------------------------------------------

    pub fn handle_key(&mut self, key: KeyEvent) {
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            self.open_confirm_quit();
            return;
        }

        if self.auth_hold {
            self.handle_auth_hold_key(key);
            return;
        }

        if key.code == KeyCode::Char('p') && key.modifiers.contains(KeyModifiers::CONTROL) {
            self.toggle_palette();
            return;
        }

        if key.code == KeyCode::Char('s') && key.modifiers.contains(KeyModifiers::CONTROL) {
            self.open_profile_save();
            return;
        }

        if self.help.is_some() {
            self.handle_help_key(key);
            return;
        }

        if self.palette.is_some() {
            self.handle_palette_key(key);
            return;
        }

        if key.code == KeyCode::Char('?')
            && self.popup.is_none()
            && !self.resources.filter.editing
            && !self.resource_map.filter_editing
        {
            self.help = Some(HelpState {
                buf: String::new(),
                cursor: 0,
                scroll: 0,
                context_only: true,
            });
            return;
        }

        if self.popup.is_some() {
            self.handle_popup_key(key);
            return;
        }

        if self.view == View::Detail {
            self.handle_detail_key(key);
            return;
        }

        if self.view == View::Logs {
            self.handle_log_key(key);
            return;
        }

        if self.view == View::EditDiff {
            self.handle_edit_diff_key(key);
            return;
        }

        // Resource filter editing captures all input in main view
        if self.resources.filter.editing {
            self.handle_resource_filter_key(key);
            return;
        }

        // Sidebar width: < shrink, > grow
        match key.code {
            | KeyCode::Char('<') => {
                self.nav_width = self.nav_width.saturating_sub(2).max(16);
                return;
            },
            | KeyCode::Char('>') => {
                self.nav_width = (self.nav_width + 2).min(60);
                return;
            },
            | _ => {},
        }

        // When resource map has keyboard focus (workload browsing / filter editing),
        // skip the global shortcut map and dispatch directly to the panel handler.
        let rmap_captures = matches!(self.panel, Panel::ResourceMap)
            && self.focus == Focus::Resources
            && (self.resource_map.filter_editing
                || matches!(self.resource_map.focus, ResourceMapFocus::Workloads { .. }));
        if rmap_captures {
            self.handle_resource_map_key(key);
            return;
        }

        match key.code {
            | KeyCode::Esc => {
                if self.focus == Focus::Resources {
                    self.focus = Focus::Nav;
                } else {
                    self.open_confirm_quit();
                }
            },
            | KeyCode::Char('q') => {
                self.open_confirm_quit();
            },
            | KeyCode::Char('r') => {
                self.focus = Focus::Resources;
            },
            | KeyCode::Char('c') if !matches!(self.panel, Panel::Profiles) => {
                self.open_context_selector();
            },
            | KeyCode::Char('n') => {
                self.pending_load = Some(PendingLoad::Namespaces);
            },
            | KeyCode::Char('O') => {
                let default = self
                    .kube
                    .kubeconfig
                    .path
                    .clone()
                    .or_else(|| std::env::var("KUBECONFIG").ok())
                    .unwrap_or_else(|| "~/.kube/config".to_string());
                let default = if default.is_empty() {
                    std::env::var("KUBECONFIG").unwrap_or_else(|_| "~/.kube/config".to_string())
                } else {
                    default
                };
                self.popup = Some(Popup::KubeconfigInput { buf: default });
            },
            | KeyCode::Char('P') => {
                self.open_profile_load();
            },
            | KeyCode::Char('/') => {
                self.begin_resource_filter();
            },
            | KeyCode::Char('x') if !self.resources.filter.text.is_empty() => {
                self.clear_resource_filter();
            },
            | KeyCode::Tab | KeyCode::BackTab => {
                self.focus = match self.focus {
                    | Focus::Nav => Focus::Resources,
                    | Focus::Resources => Focus::Nav,
                };
            },
            | _ => {
                match self.focus {
                    | Focus::Nav => self.handle_nav_key(key),
                    | Focus::Resources => self.handle_resource_key(key),
                }
            },
        }
    }

    fn handle_resource_filter_key(&mut self, key: KeyEvent) {
        match key.code {
            | KeyCode::Enter => self.apply_resource_filter(),
            | KeyCode::Esc => self.cancel_resource_filter(),
            | KeyCode::Backspace => {
                self.resources.filter.buf.pop();
            },
            | KeyCode::Char(c) => {
                self.resources.filter.buf.push(c);
            },
            | _ => {},
        }
    }

    fn handle_nav_key(&mut self, key: KeyEvent) {
        match key.code {
            | KeyCode::Up | KeyCode::Char('k') => {
                self.nav_up();
                self.load_nav_selection();
            },
            | KeyCode::Down | KeyCode::Char('j') => {
                self.nav_down();
                self.load_nav_selection();
            },
            | KeyCode::PageUp | KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                for _ in 0..10 {
                    self.nav_up();
                }
                self.load_nav_selection();
            },
            | KeyCode::PageDown | KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                for _ in 0..10 {
                    self.nav_down();
                }
                self.load_nav_selection();
            },
            | KeyCode::Enter => {
                self.load_nav_selection();
                self.focus = Focus::Resources;
            },
            | _ => {},
        }
    }

    /// Load whichever resource type or cluster stats is currently highlighted
    /// in the nav.
    fn load_nav_selection(&mut self) {
        self.return_panel = None;
        let idx = match self.nav.state.selected() {
            | Some(i) => i,
            | None => return,
        };
        let kind = &self.nav.items[idx].kind;
        match kind {
            | NavItemKind::Favorites => {
                self.panel = Panel::Favorites;
                self.clear_resource_filter();
                self.view = View::Main;
            },
            | NavItemKind::PortForwards => {
                self.panel = Panel::PortForwards;
                self.clear_resource_filter();
                self.view = View::Main;
            },
            | NavItemKind::Profiles => {
                self.panel = Panel::Profiles;
                self.clear_resource_filter();
                self.view = View::Main;
            },
            | NavItemKind::ClusterStats => {
                if !matches!(self.panel, Panel::Overview) {
                    self.panel = Panel::Overview;
                    self.clear_resource_filter();
                    self.overview.scroll = 0;
                    self.pending_load = Some(PendingLoad::ClusterStats);
                }
            },
            | NavItemKind::ResourceMap => {
                if !matches!(self.panel, Panel::ResourceMap) {
                    self.panel = Panel::ResourceMap;
                    self.clear_resource_filter();
                    self.resource_map.scroll = 0;
                    self.resource_map.focus = ResourceMapFocus::Overview;
                    self.resource_map.filter.clear();
                    self.resource_map.filter_buf.clear();
                    self.resource_map.filter_editing = false;
                    self.load_metrics();
                }
            },
            | NavItemKind::Resource(kind) => {
                let kind = kind.clone();
                if self.selected_kind() != Some(&kind) {
                    let is_event = kind.is_event();
                    self.panel = Panel::Resources(kind);
                    self.resources.table_state = TableState::default();
                    if is_event {
                        self.events.scroll = 0;
                        self.events.cursor = 0;
                        self.events.auto_scroll = true;
                    }
                    self.clear_resource_filter();
                    self.pending_load = Some(PendingLoad::Resources);
                }
            },
            | NavItemKind::Category | NavItemKind::SubCategory => {},
        }
    }

    fn is_selectable_nav(kind: &NavItemKind) -> bool {
        matches!(
            kind,
            NavItemKind::Resource(_)
                | NavItemKind::ClusterStats
                | NavItemKind::ResourceMap
                | NavItemKind::PortForwards
                | NavItemKind::Favorites
                | NavItemKind::Profiles
        )
    }

    fn nav_up(&mut self) {
        let current = self.nav.state.selected().unwrap_or(0);
        if current == 0 {
            return;
        }
        let mut next = current - 1;
        while next > 0 {
            if Self::is_selectable_nav(&self.nav.items[next].kind) {
                break;
            }
            next -= 1;
        }
        if Self::is_selectable_nav(&self.nav.items[next].kind) {
            self.nav.state.select(Some(next));
        }
    }

    fn nav_down(&mut self) {
        let current = self.nav.state.selected().unwrap_or(0);
        let max = self.nav.items.len() - 1;
        if current >= max {
            return;
        }
        let mut next = current + 1;
        while next < max {
            if Self::is_selectable_nav(&self.nav.items[next].kind) {
                break;
            }
            next += 1;
        }
        if Self::is_selectable_nav(&self.nav.items[next].kind) {
            self.nav.state.select(Some(next));
        }
    }

    fn handle_resource_key(&mut self, key: KeyEvent) {
        match self.panel.clone() {
            | Panel::Favorites => {
                self.handle_favorites_key(key);
                return;
            },
            | Panel::Profiles => {
                self.handle_profiles_key(key);
                return;
            },
            | Panel::PortForwards => {
                self.handle_port_forwards_key(key);
                return;
            },
            | Panel::Overview => {
                match key.code {
                    | KeyCode::Up | KeyCode::Char('k') => {
                        self.overview.scroll = self.overview.scroll.saturating_sub(1);
                    },
                    | KeyCode::Down | KeyCode::Char('j') => {
                        self.overview.scroll = self.overview.scroll.saturating_add(1);
                    },
                    | KeyCode::PageUp | KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        self.overview.scroll = self.overview.scroll.saturating_sub(20);
                    },
                    | KeyCode::PageDown | KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        self.overview.scroll = self.overview.scroll.saturating_add(20);
                    },
                    | KeyCode::Home => {
                        self.overview.scroll = 0;
                    },
                    | _ => {},
                }
                return;
            },
            | Panel::ResourceMap => {
                self.handle_resource_map_key(key);
                return;
            },
            | Panel::Resources(kind) if kind.is_event() => {
                self.handle_events_key(key);
                return;
            },
            | Panel::Resources(_) => {
                // fall through to normal resource handling below
            },
        }
        let visible = self.visible_resource_indices();
        let vis_len = visible.len();
        // Map current TableState selection (real index) to filtered position
        let vis_pos = self
            .resources
            .state
            .selected()
            .and_then(|sel| visible.iter().position(|&i| i == sel))
            .unwrap_or(0);

        match key.code {
            | KeyCode::Up | KeyCode::Char('k') => {
                if vis_pos > 0 {
                    self.resources.state.select(Some(visible[vis_pos - 1]));
                }
            },
            | KeyCode::Down | KeyCode::Char('j') => {
                if vis_pos + 1 < vis_len {
                    self.resources.state.select(Some(visible[vis_pos + 1]));
                }
            },
            | KeyCode::Enter => {
                if let Some(&real_idx) = visible.get(vis_pos) {
                    if let Some(entry) = self.resources.entries.get(real_idx) {
                        self.resources.state.select(Some(real_idx));
                        self.pending_load = Some(PendingLoad::ResourceDetail {
                            name: entry.name.clone(),
                            namespace: entry.namespace.clone(),
                        });
                    }
                }
            },
            | KeyCode::PageUp | KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                let new_pos = vis_pos.saturating_sub(20);
                if let Some(&idx) = visible.get(new_pos) {
                    self.resources.state.select(Some(idx));
                }
            },
            | KeyCode::PageDown | KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                let new_pos = (vis_pos + 20).min(vis_len.saturating_sub(1));
                if let Some(&idx) = visible.get(new_pos) {
                    self.resources.state.select(Some(idx));
                }
            },
            | KeyCode::Home => {
                if let Some(&first) = visible.first() {
                    self.resources.state.select(Some(first));
                }
            },
            | KeyCode::End => {
                if let Some(&last) = visible.last() {
                    self.resources.state.select(Some(last));
                }
            },
            | KeyCode::Char('l') => {
                self.open_logs_for_selected();
            },
            | KeyCode::Char('e') => {
                self.begin_edit();
            },
            | KeyCode::Char('y') => {
                self.copy_resource_name();
            },
            | KeyCode::Char('F') => {
                self.begin_port_forward();
            },
            | KeyCode::Char('D') => {
                self.open_delete_confirm();
            },
            | KeyCode::Char('R') => {
                self.confirm_restart();
            },
            | KeyCode::Char('d') => {
                self.begin_diff();
            },
            | KeyCode::Char('C') => {
                self.start_create_resource();
            },
            | KeyCode::Char('S') => {
                self.begin_scale();
            },
            | KeyCode::Char('K') => {
                self.confirm_cordon();
            },
            | KeyCode::Char('T') => {
                if self.selected_resource_type() == Some(ResourceType::Node) {
                    let (name, _) = self.selected_resource_name_ns();
                    if !name.is_empty() {
                        self.popup = Some(Popup::ConfirmDrain { node_name: name });
                    }
                } else if self.selected_resource_type() == Some(ResourceType::CronJob) {
                    self.open_trigger_cronjob();
                }
            },
            | KeyCode::Char('X') if self.experimental => {
                self.begin_exec();
            },
            | KeyCode::Char('*') => {
                self.toggle_favorite_selected();
            },
            | _ => {},
        }
    }

    fn handle_events_key(&mut self, key: KeyEvent) {
        let visible = self.visible_resource_indices();
        let max = visible.len().saturating_sub(1);
        match key.code {
            | KeyCode::Up | KeyCode::Char('k') => {
                self.events.cursor = self.events.cursor.saturating_sub(1);
                self.events.auto_scroll = false;
            },
            | KeyCode::Down | KeyCode::Char('j') => {
                if self.events.cursor < max {
                    self.events.cursor += 1;
                }
                self.events.auto_scroll = self.events.cursor == max;
            },
            | KeyCode::PageUp | KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.events.cursor = self.events.cursor.saturating_sub(30);
                self.events.auto_scroll = false;
            },
            | KeyCode::PageDown | KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.events.cursor = (self.events.cursor + 30).min(max);
                self.events.auto_scroll = self.events.cursor == max;
            },
            | KeyCode::Home | KeyCode::Char('g') => {
                self.events.cursor = 0;
                self.events.auto_scroll = false;
            },
            | KeyCode::End | KeyCode::Char('G') => {
                self.events.cursor = max;
                self.events.auto_scroll = true;
            },
            | KeyCode::Enter => {
                // Translate cursor position in filtered view → real resource index
                if let Some(&real_idx) = visible.get(self.events.cursor) {
                    if let Some(entry) = self.resources.entries.get(real_idx) {
                        self.resources.state.select(Some(real_idx));
                        self.pending_load = Some(PendingLoad::ResourceDetail {
                            name: entry.name.clone(),
                            namespace: entry.namespace.clone(),
                        });
                    }
                }
            },
            | _ => {},
        }
    }

    fn handle_detail_key(&mut self, key: KeyEvent) {
        let secret_smart = self.is_secret_smart_view();

        match key.code {
            | KeyCode::Esc | KeyCode::Char('q') => {
                // Cascading dismiss: selection → related → view
                if self.detail.dict.cursor.is_some() {
                    self.detail.dict.cursor = None;
                } else if self.detail.related.cursor.is_some() {
                    self.detail.related.cursor = None;
                } else {
                    self.return_to_main();
                }
            },
            // [v] Cycle view: Smart → YAML → Smart
            | KeyCode::Char('v') => {
                self.detail.mode = match self.detail.mode {
                    | DetailMode::Smart => DetailMode::Yaml,
                    | DetailMode::Yaml => DetailMode::Smart,
                };
                self.detail.scroll = 0;
            },
            // [y] Copy to clipboard — dict selection takes priority over secret data
            | KeyCode::Char('y') => {
                if self.detail.dict.cursor.is_some() {
                    self.copy_dict_entry_to_clipboard();
                } else if secret_smart {
                    self.copy_secret_to_clipboard();
                } else if self.detail.mode == DetailMode::Yaml {
                    self.copy_yaml_to_clipboard();
                }
            },
            // [Space] Expand/decode — dict selection takes priority over secret data
            | KeyCode::Char(' ') => {
                if let Some(cursor) = self.detail.dict.cursor {
                    if let Some((qualified_key, ..)) = self.detail.dict.entries.get(cursor) {
                        let key = qualified_key.clone();
                        if self.detail.dict.expanded_keys.contains(&key) {
                            self.detail.dict.expanded_keys.remove(&key);
                        } else {
                            self.detail.dict.expanded_keys.insert(key);
                        }
                    }
                } else if secret_smart {
                    if let Some(state) = &mut self.detail.secret {
                        state.toggle_decode();
                    }
                }
            },
            // j/k: navigate dict entries (when selected) or secret keys, scroll otherwise
            | KeyCode::Left => {
                if self.detail.related.cursor.is_some() {
                    let cat_count = self.related_categories().len();
                    if cat_count > 0 && self.detail.related.tab > 0 {
                        self.detail.related.tab -= 1;
                        self.detail.related.cursor = Some(self.related_tab_indices().first().copied().unwrap_or(0));
                        self.scroll_to_related_cursor(0);
                    }
                }
            },
            | KeyCode::Right => {
                if self.detail.related.cursor.is_some() {
                    let cat_count = self.related_categories().len();
                    if cat_count > 0 && self.detail.related.tab + 1 < cat_count {
                        self.detail.related.tab += 1;
                        self.detail.related.cursor = Some(self.related_tab_indices().first().copied().unwrap_or(0));
                        self.scroll_to_related_cursor(0);
                    }
                }
            },
            | KeyCode::Up | KeyCode::Char('k') => {
                if let Some(c) = self.detail.related.cursor {
                    let indices = self.related_tab_indices();
                    if let Some(pos) = indices.iter().position(|&i| i == c) {
                        if pos > 0 {
                            self.detail.related.cursor = Some(indices[pos - 1]);
                            self.scroll_to_related_cursor(pos - 1);
                        }
                    }
                } else if secret_smart {
                    if let Some(state) = &mut self.detail.secret {
                        state.nav_up();
                    }
                } else if self.detail.dict.cursor.is_some() {
                    self.dict_nav_up();
                } else {
                    self.detail.scroll = self.detail.scroll.saturating_sub(1);
                }
            },
            | KeyCode::Down | KeyCode::Char('j') => {
                if let Some(c) = self.detail.related.cursor {
                    let indices = self.related_tab_indices();
                    if let Some(pos) = indices.iter().position(|&i| i == c) {
                        if pos + 1 < indices.len() {
                            self.detail.related.cursor = Some(indices[pos + 1]);
                            self.scroll_to_related_cursor(pos + 1);
                        }
                    }
                } else if secret_smart {
                    if let Some(state) = &mut self.detail.secret {
                        state.nav_down();
                    }
                } else if self.detail.dict.cursor.is_some() {
                    self.dict_nav_down();
                } else {
                    self.detail.scroll = self.detail.scroll.saturating_add(1);
                }
            },
            | KeyCode::PageUp | KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.detail.scroll = self.detail.scroll.saturating_sub(20);
            },
            | KeyCode::PageDown | KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.detail.scroll = self.detail.scroll.saturating_add(20);
            },
            | KeyCode::Home => {
                self.detail.scroll = 0;
            },
            // [l] Enter/leave label selection mode
            | KeyCode::Char('l') => {
                if secret_smart {
                    // no-op for secrets
                } else if self.detail.dict.cursor.is_some()
                    && self
                        .detail
                        .dict
                        .entries
                        .get(self.detail.dict.cursor.unwrap_or(0))
                        .is_some_and(|(q, ..)| q.starts_with("Labels:"))
                {
                    self.detail.dict.cursor = None;
                } else {
                    let first_label = self
                        .detail
                        .dict
                        .entries
                        .iter()
                        .position(|(q, ..)| q.starts_with("Labels:"));
                    if let Some(idx) = first_label {
                        self.detail.related.cursor = None;
                        self.detail.dict.cursor = Some(idx);
                        self.scroll_to_dict_cursor();
                    }
                }
            },
            // [a] Enter/leave annotation selection mode
            | KeyCode::Char('a') => {
                if self.detail.dict.cursor.is_some()
                    && self
                        .detail
                        .dict
                        .entries
                        .get(self.detail.dict.cursor.unwrap_or(0))
                        .is_some_and(|(q, ..)| q.starts_with("Annotations:"))
                {
                    self.detail.dict.cursor = None;
                } else {
                    let first_annot = self
                        .detail
                        .dict
                        .entries
                        .iter()
                        .position(|(q, ..)| q.starts_with("Annotations:"));
                    if let Some(idx) = first_annot {
                        self.detail.related.cursor = None;
                        self.detail.dict.cursor = Some(idx);
                        self.scroll_to_dict_cursor();
                    }
                }
            },
            // [e] Edit resource
            | KeyCode::Char('e') => {
                self.start_edit_from_detail();
            },
            // [L] Open logs (for workload resources)
            | KeyCode::Char('L') => {
                self.open_logs_for_selected();
            },
            // [F] Port forward
            | KeyCode::Char('F') => {
                self.begin_port_forward();
            },
            | KeyCode::Char('D') => {
                self.open_delete_confirm_detail();
            },
            | KeyCode::Char('R') => {
                self.confirm_restart();
            },
            | KeyCode::Char('w') => {
                self.detail.auto_refresh = !self.detail.auto_refresh;
                self.push_status(if self.detail.auto_refresh {
                    "Watch mode enabled"
                } else {
                    "Watch mode disabled"
                });
            },
            | KeyCode::Char('S') => {
                self.open_scale_input_detail();
            },
            | KeyCode::Char('X') if self.experimental => {
                self.begin_exec();
            },
            | KeyCode::Char('K') => {
                self.confirm_cordon();
            },
            | KeyCode::Char('T') => {
                if self.selected_resource_type() == Some(ResourceType::Node) {
                    let name = self.detail.name.clone();
                    if !name.is_empty() {
                        self.popup = Some(Popup::ConfirmDrain { node_name: name });
                    }
                } else if self.selected_resource_type() == Some(ResourceType::CronJob) {
                    self.open_trigger_cronjob();
                }
            },
            // [r] Toggle related resource selection
            | KeyCode::Char('r') => {
                if self.detail.related.resources.is_empty() {
                    // no-op
                } else if self.detail.related.cursor.is_some() {
                    self.detail.related.cursor = None;
                } else {
                    self.detail.dict.cursor = None; // exit dict selection
                    let cats = self.related_categories();
                    if !cats.is_empty() {
                        self.detail.related.tab = self.detail.related.tab.min(cats.len().saturating_sub(1));
                        let indices = self.related_tab_indices();
                        self.detail.related.cursor = indices.first().copied();
                        self.scroll_to_related_cursor(0);
                    }
                }
            },
            // [Enter] Edit selected label/annotation, or navigate to related resource
            | KeyCode::Enter => {
                if let Some(cursor) = self.detail.dict.cursor {
                    if let Some((qualified_key, ..)) = self.detail.dict.entries.get(cursor) {
                        let kind = if qualified_key.starts_with("Labels:") {
                            MetadataEditKind::Labels
                        } else {
                            MetadataEditKind::Annotations
                        };
                        self.open_metadata_edit(kind);
                    }
                } else if let Some(cursor) = self.detail.related.cursor {
                    if let Some(rel) = self.detail.related.resources.get(cursor) {
                        self.navigate_to_related(rel.resource_type, rel.name.clone(), rel.namespace.clone());
                    }
                }
            },
            | KeyCode::Char('*') => {
                self.toggle_favorite_detail();
            },
            | _ => {},
        }
    }

    /// Returns the unique category names from related resources, in order.
    pub fn related_categories(&self) -> Vec<&'static str> {
        let mut cats = Vec::new();
        for r in &self.detail.related.resources {
            if !cats.contains(&r.category) {
                cats.push(r.category);
            }
        }
        cats
    }

    /// Returns indices into related_resources for the currently selected tab.
    pub fn related_tab_indices(&self) -> Vec<usize> {
        let cats = self.related_categories();
        let current_cat = cats.get(self.detail.related.tab).copied().unwrap_or("");
        self.detail
            .related
            .resources
            .iter()
            .enumerate()
            .filter(|(_, r)| r.category == current_cat)
            .map(|(i, _)| i)
            .collect()
    }

    /// Scroll detail view to keep the related resource at `pos` visible.
    fn scroll_to_related_cursor(&mut self, pos: usize) {
        let target_line = self.detail.related.line_start + pos;
        let scroll = self.detail.scroll as usize;
        let visible = self.detail.area_height;
        if visible == 0 {
            return;
        }
        if target_line >= scroll + visible {
            self.detail.scroll = (target_line - visible + 1) as u16;
        } else if target_line < scroll {
            self.detail.scroll = target_line as u16;
        }
    }

    fn navigate_to_related(&mut self, rt: ResourceType, name: String, namespace: String) {
        self.panel = Panel::Resources(ResourceKind::Builtin(rt));
        self.detail.related.cursor = None;
        self.detail.related.tab = 0;
        self.resources.table_state = TableState::default();
        let kind = ResourceKind::Builtin(rt);
        if let Some(nav_idx) = self
            .nav
            .items
            .iter()
            .position(|item| matches!(&item.kind, NavItemKind::Resource(r) if r == &kind))
        {
            self.nav.state.select(Some(nav_idx));
        }
        self.pending_load = Some(PendingLoad::ResourceDetail { name, namespace });
    }

    fn dict_nav_up(&mut self) {
        match self.detail.dict.cursor {
            | Some(0) => {
                // Stay at first entry — don't exit selection mode
            },
            | Some(i) => {
                self.detail.dict.cursor = Some(i - 1);
                self.scroll_to_dict_cursor();
            },
            | None => {},
        }
    }

    fn dict_nav_down(&mut self) {
        let max = self.detail.dict.entries.len().saturating_sub(1);
        match self.detail.dict.cursor {
            | Some(i) if i >= max => {
                // Stay at last entry — don't exit selection mode
            },
            | Some(i) => {
                self.detail.dict.cursor = Some(i + 1);
                self.scroll_to_dict_cursor();
            },
            | None => {},
        }
    }

    /// Scroll the detail view to keep the selected dict entry visible.
    fn scroll_to_dict_cursor(&mut self) {
        if let Some(cursor) = self.detail.dict.cursor {
            if let Some(&line_offset) = self.detail.dict.line_offsets.get(cursor) {
                let scroll = self.detail.scroll as usize;
                // Rough visible height estimate (will be exact next frame)
                let visible_height = 30usize;
                if line_offset < scroll {
                    self.detail.scroll = line_offset as u16;
                } else if line_offset >= scroll + visible_height {
                    self.detail.scroll = line_offset.saturating_sub(visible_height / 2) as u16;
                }
            }
        }
    }

    fn copy_yaml_to_clipboard(&mut self) {
        match arboard::Clipboard::new().and_then(|mut cb| cb.set_text(&self.detail.yaml)) {
            | Ok(()) => {
                self.error = None;
                self.push_status(format!("Copied YAML ({} bytes)", self.detail.yaml.len()));
            },
            | Err(e) => {
                self.error = Some(format!("Clipboard error: {}", e));
            },
        }
    }

    fn copy_dict_entry_to_clipboard(&mut self) {
        if let Some(cursor) = self.detail.dict.cursor {
            if let Some((_, key, value)) = self.detail.dict.entries.get(cursor) {
                let key = key.clone();
                let value = value.clone();
                let text = format!("{}: {}", key, value);
                match arboard::Clipboard::new().and_then(|mut cb| cb.set_text(&text)) {
                    | Ok(()) => {
                        self.error = None;
                        self.push_status(format!("Copied '{}' ({} bytes)", key, value.len()));
                    },
                    | Err(e) => {
                        self.error = Some(format!("Clipboard error: {}", e));
                    },
                }
            }
        }
    }

    fn handle_log_key(&mut self, key: KeyEvent) {
        // Filter editing mode captures all input
        if let Some(state) = &self.log {
            if state.filter.editing {
                self.handle_log_filter_key(key);
                return;
            }
        }

        match key.code {
            | KeyCode::Esc | KeyCode::Char('q') => {
                // The follow cursor is always set, so it is not a mode to dismiss.
                if self.log_detail_line.is_some() {
                    self.log_detail_line = None;
                } else {
                    if let Some(state) = &mut self.log {
                        state.stop_following();
                    }
                    self.log = None;
                    self.log_detail_line = None;
                    self.return_to_main();
                }
            },
            // [/] Start filter edit
            | KeyCode::Char('/') => {
                if let Some(state) = &mut self.log {
                    state.begin_filter_edit();
                }
            },
            // [f] Toggle follow
            | KeyCode::Char('f') => {
                if let Some(state) = &mut self.log {
                    if state.following {
                        state.stop_following();
                    } else {
                        state.start_following(self.kube.client.streaming.clone());
                    }
                }
            },
            // [c] Select container
            | KeyCode::Char('c') => {
                self.open_container_selector();
            },
            // [p] Select pod
            | KeyCode::Char('p') => {
                self.open_pod_selector();
            },
            // [Y] Copy all logs to clipboard
            | KeyCode::Char('Y') => {
                self.copy_logs_to_clipboard();
            },
            // [x] Clear filter
            | KeyCode::Char('x') => {
                if let Some(state) = &mut self.log {
                    state.clear_filter();
                }
            },
            // [w] Toggle wrap
            | KeyCode::Char('w') => {
                if let Some(state) = &mut self.log {
                    state.wrap = !state.wrap;
                }
            },
            // [t] Time filter
            | KeyCode::Char('t') => {
                self.popup = Some(Popup::TimeFilter { buf: "30m".to_string() });
            },
            // [Enter] Open selected line in detail popup
            | KeyCode::Enter => {
                if let Some(state) = &self.log {
                    if let Some(sel) = state.selected_line {
                        let visible = state.visible_lines();
                        if let Some(line) = visible.get(sel) {
                            self.log_detail_line = Some(line.display_text());
                        }
                    }
                }
            },
            // One cursor. j/k, paging, and g/G all move it, and the viewport follows.
            | KeyCode::Up | KeyCode::Char('k') | KeyCode::Char('K') => {
                let extending = key.modifiers.contains(KeyModifiers::SHIFT) || key.code == KeyCode::Char('K');
                if let Some(state) = &mut self.log {
                    state.move_cursor(-1, extending);
                }
            },
            | KeyCode::Down | KeyCode::Char('j') | KeyCode::Char('J') => {
                let extending = key.modifiers.contains(KeyModifiers::SHIFT) || key.code == KeyCode::Char('J');
                if let Some(state) = &mut self.log {
                    state.move_cursor(1, extending);
                }
            },
            | KeyCode::PageUp | KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                if let Some(state) = &mut self.log {
                    state.page(-1);
                }
            },
            | KeyCode::PageDown | KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                if let Some(state) = &mut self.log {
                    state.page(1);
                }
            },
            | KeyCode::Home | KeyCode::Char('g') => {
                if let Some(state) = &mut self.log {
                    state.jump(false);
                }
            },
            | KeyCode::End | KeyCode::Char('G') => {
                if let Some(state) = &mut self.log {
                    state.jump(true);
                }
            },
            | _ => {},
        }
    }

    fn handle_log_filter_key(&mut self, key: KeyEvent) {
        let state = match &mut self.log {
            | Some(s) => s,
            | None => return,
        };

        match key.code {
            | KeyCode::Enter => state.apply_filter(),
            | KeyCode::Esc => state.cancel_filter_edit(),
            | KeyCode::Backspace => {
                state.filter.buf.pop();
            },
            | KeyCode::Char(c) => {
                state.filter.buf.push(c);
            },
            | _ => {},
        }
    }

    fn copy_secret_to_clipboard(&mut self) {
        if let Some(state) = &self.detail.secret {
            let key_name = state.selected_key().unwrap_or("").to_string();

            let decoded = match state.selected_plaintext_value() {
                | Some(v) => v,
                | None => {
                    self.error = Some(format!("Cannot decode '{}' (not valid UTF-8)", key_name));
                    return;
                },
            };

            // Use arboard for system clipboard access.
            // The decoded value is the actual plaintext secret (already base64-decoded).
            match arboard::Clipboard::new().and_then(|mut cb| cb.set_text(&decoded)) {
                | Ok(()) => {
                    self.error = None;
                    self.push_status(format!("Copied '{}' to clipboard ({} bytes)", key_name, decoded.len()));
                },
                | Err(e) => {
                    self.error = Some(format!("Clipboard error: {}", e));
                },
            }
        }
    }

    fn handle_popup_key(&mut self, key: KeyEvent) {
        // Port forward create popup has its own handler
        if matches!(self.popup, Some(Popup::PortForwardCreate(_))) {
            self.handle_pf_create_popup_key(key);
            return;
        }
        if matches!(self.popup, Some(Popup::Confirm { .. })) {
            self.handle_generic_confirm_key(key);
            return;
        }
        if matches!(self.popup, Some(Popup::ConfirmDelete { .. })) {
            self.handle_confirm_delete_key(key);
            return;
        }
        if matches!(self.popup, Some(Popup::ConfirmDrain { .. })) {
            self.handle_confirm_drain_key(key);
            return;
        }
        if matches!(self.popup, Some(Popup::ConfirmQuit { .. })) {
            self.handle_confirm_quit_key(key);
            return;
        }
        if matches!(self.popup, Some(Popup::TriggerCronJob { .. })) {
            self.handle_trigger_cronjob_key(key);
            return;
        }
        if matches!(self.popup, Some(Popup::ScaleInput { .. })) {
            self.handle_scale_input_key(key);
            return;
        }
        if matches!(self.popup, Some(Popup::ExecShell { .. })) {
            self.handle_exec_shell_key(key);
            return;
        }
        if matches!(self.popup, Some(Popup::KubeconfigInput { .. })) {
            self.handle_kubeconfig_input_key(key);
            return;
        }
        if matches!(self.popup, Some(Popup::TimeFilter { .. })) {
            self.handle_time_filter_key(key);
            return;
        }
        if matches!(self.popup, Some(Popup::ProfileSave { .. })) {
            self.handle_profile_save_key(key);
            return;
        }
        if matches!(self.popup, Some(Popup::ProfileLoad { .. })) {
            self.handle_profile_load_key(key);
            return;
        }
        if matches!(self.popup, Some(Popup::PortForwardEditPort { .. })) {
            self.handle_pf_edit_port_key(key);
            return;
        }
        if matches!(self.popup, Some(Popup::ProfileClone { .. })) {
            self.handle_profile_clone_key(key);
            return;
        }
        if matches!(self.popup, Some(Popup::ConfirmDeleteProfile { .. })) {
            self.handle_confirm_delete_profile_key(key);
            return;
        }

        match key.code {
            | KeyCode::Esc => {
                self.popup = None;
            },
            | KeyCode::Up | KeyCode::Char('k') => {
                if let Some((state, _)) = self.popup.as_mut().and_then(Popup::list_state_mut) {
                    let current = state.selected().unwrap_or(0);
                    if current > 0 {
                        state.select(Some(current - 1));
                    }
                }
            },
            | KeyCode::Down | KeyCode::Char('j') => {
                if let Some((state, len)) = self.popup.as_mut().and_then(Popup::list_state_mut) {
                    let current = state.selected().unwrap_or(0);
                    if current + 1 < len {
                        state.select(Some(current + 1));
                    }
                }
            },
            | KeyCode::PageUp | KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                if let Some((state, _)) = self.popup.as_mut().and_then(Popup::list_state_mut) {
                    let current = state.selected().unwrap_or(0);
                    state.select(Some(current.saturating_sub(10)));
                }
            },
            | KeyCode::PageDown | KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                if let Some((state, len)) = self.popup.as_mut().and_then(Popup::list_state_mut) {
                    let current = state.selected().unwrap_or(0);
                    state.select(Some((current + 10).min(len.saturating_sub(1))));
                }
            },
            | KeyCode::Enter => {
                let action = match &self.popup {
                    | Some(Popup::ContextSelect { items, state }) => {
                        state
                            .selected()
                            .and_then(|idx| items.get(idx).cloned())
                            .map(PendingLoad::SwitchContext)
                    },
                    | Some(Popup::NamespaceSelect { items, state }) => {
                        let ns = state.selected().and_then(|idx| items.get(idx).cloned());
                        if let Some(ns) = ns {
                            if ns == ALL_NAMESPACES_LABEL {
                                self.kube.context.namespace = None;
                            } else {
                                self.kube.context.namespace = Some(ns);
                            }
                            self.clear_cached_state();
                            Some(PendingLoad::Resources)
                        } else {
                            None
                        }
                    },
                    | Some(Popup::PodSelect { state, .. }) => {
                        if let Some(idx) = state.selected() {
                            if let Some(log_state) = &mut self.log {
                                log_state.selection.pod = if idx == 0 { None } else { Some(idx - 1) };
                                log_state.selection.container = None;
                            }
                            Some(PendingLoad::ReloadLogs)
                        } else {
                            None
                        }
                    },
                    | Some(Popup::ContainerSelect { state, .. }) => {
                        if let Some(idx) = state.selected() {
                            if let Some(log_state) = &mut self.log {
                                log_state.selection.container = if idx == 0 { None } else { Some(idx - 1) };
                            }
                            Some(PendingLoad::ReloadLogs)
                        } else {
                            None
                        }
                    },
                    | Some(Popup::PortForwardCreate(_))
                    | Some(Popup::ConfirmDelete { .. })
                    | Some(Popup::ConfirmDrain { .. })
                    | Some(Popup::ScaleInput { .. })
                    | Some(Popup::ExecShell { .. })
                    | Some(Popup::KubeconfigInput { .. })
                    | Some(Popup::TriggerCronJob { .. })
                    | Some(Popup::ConfirmQuit { .. })
                    | Some(Popup::TimeFilter { .. })
                    | Some(Popup::ProfileSave { .. })
                    | Some(Popup::ProfileLoad { .. })
                    | Some(Popup::PortForwardEditPort { .. })
                    | Some(Popup::ProfileClone { .. })
                    | Some(Popup::ConfirmDeleteProfile { .. })
                    | Some(Popup::Confirm { .. })
                    | Some(Popup::AuthExpired { .. })
                    | None => None,
                };
                self.popup = None;
                if let Some(load) = action {
                    self.pending_load = Some(load);
                }
            },
            | _ => {},
        }
    }

    fn open_logs_for_selected(&mut self) {
        if matches!(self.panel, Panel::Favorites) && self.view != View::Detail {
            self.open_favorite_logs();
            return;
        }
        let Some(rt) = self.selected_resource_type().filter(|rt| rt.supports_logs()) else {
            return;
        };
        let _ = rt;
        let (name, namespace) = if self.view == View::Detail && !self.detail.name.is_empty() {
            (self.detail.name.clone(), self.detail.namespace.clone())
        } else {
            self.selected_resource_name_ns()
        };
        if name.is_empty() {
            return;
        }
        self.pending_load = Some(PendingLoad::Logs { name, namespace });
    }

    fn open_context_selector(&mut self) {
        let contexts = self.kube.contexts();
        let current = self.kube.context.name.as_str().to_string();
        let mut state = ListState::default();
        if let Some(idx) = contexts.iter().position(|c| c == &current) {
            state.select(Some(idx));
        } else if !contexts.is_empty() {
            state.select(Some(0));
        }
        self.popup = Some(Popup::ContextSelect { items: contexts, state });
    }

    fn open_pod_selector(&mut self) {
        let log_state = match &self.log {
            | Some(s) => s,
            | None => return,
        };
        if log_state.pods.len() <= 1 {
            return;
        }
        let mut items = vec!["All".to_string()];
        items.extend(log_state.pods.iter().map(|p| p.name.clone()));
        let mut state = ListState::default();
        // Current selection: None → "All" (0), Some(i) → i+1
        let sel = log_state.selection.pod.map(|i| i + 1).unwrap_or(0);
        state.select(Some(sel));
        self.popup = Some(Popup::PodSelect { items, state });
    }

    fn open_container_selector(&mut self) {
        let log_state = match &self.log {
            | Some(s) => s,
            | None => return,
        };
        let containers = log_state.active_containers();
        if containers.len() <= 1 {
            return;
        }
        let mut items = vec!["All".to_string()];
        items.extend(containers);
        let mut state = ListState::default();
        let sel = log_state.selection.container.map(|i| i + 1).unwrap_or(0);
        state.select(Some(sel));
        self.popup = Some(Popup::ContainerSelect { items, state });
    }

    // -----------------------------------------------------------------------
    // Port forwards
    // -----------------------------------------------------------------------

    /// Build a sorted, grouped display list for the port forwards panel.
    pub fn pf_display_items(&self) -> Vec<DisplayItem> {
        let entries = self.pf.manager.entries();
        if entries.is_empty() {
            return Vec::new();
        }
        let categories = ResourceType::all_by_category();
        let mut type_order: std::collections::HashMap<&str, (usize, usize)> = std::collections::HashMap::new();
        for (ci, (_cat, types)) in categories.iter().enumerate() {
            for (ti, rt) in types.iter().enumerate() {
                type_order.insert(rt.singular_name(), (ci, ti));
            }
        }
        let mut indexed: Vec<(usize, &portforward::PortForwardEntry)> = entries.iter().enumerate().collect();
        indexed.sort_by(|a, b| {
            let oa = type_order.get(a.1.identity.type_label()).copied().unwrap_or((999, 999));
            let ob = type_order.get(b.1.identity.type_label()).copied().unwrap_or((999, 999));
            oa.cmp(&ob).then(a.1.identity.name.cmp(&b.1.identity.name))
        });
        let mut items = Vec::new();
        let mut current_cat: Option<usize> = None;
        for (idx, entry) in &indexed {
            let (ci, _) = type_order
                .get(entry.identity.type_label())
                .copied()
                .unwrap_or((999, 999));
            if current_cat != Some(ci) {
                current_cat = Some(ci);
                let label = categories
                    .get(ci)
                    .map(|(cat, _)| cat.display_name().to_string())
                    .unwrap_or_else(|| "Other".to_string());
                items.push(DisplayItem::Header(label));
            }
            items.push(DisplayItem::Entry(*idx));
        }
        items
    }

    fn pf_entry_at_cursor(&self) -> Option<usize> {
        match self.pf_display_items().get(self.pf.cursor) {
            | Some(DisplayItem::Entry(idx)) => Some(*idx),
            | _ => None,
        }
    }

    fn snap_pf_cursor_to_entry(&mut self) {
        let display = self.pf_display_items();
        if display.is_empty() {
            self.pf.cursor = 0;
            return;
        }
        if let Some(i) = entry_index_from(&display, self.pf.cursor) {
            self.pf.cursor = i;
        } else if let Some(i) = entry_index_before(&display, self.pf.cursor) {
            self.pf.cursor = i;
        }
    }

    fn pf_cursor_up(&mut self) {
        if let Some(i) = entry_index_before(&self.pf_display_items(), self.pf.cursor) {
            self.pf.cursor = i;
        }
    }

    fn pf_cursor_down(&mut self) {
        if let Some(i) = entry_index_from(&self.pf_display_items(), self.pf.cursor + 1) {
            self.pf.cursor = i;
        }
    }

    fn handle_port_forwards_key(&mut self, key: KeyEvent) {
        let display_len = self.pf_display_items().len();
        match key.code {
            | KeyCode::Up | KeyCode::Char('k') => {
                self.pf_cursor_up();
            },
            | KeyCode::Down | KeyCode::Char('j') => {
                self.pf_cursor_down();
            },
            | KeyCode::PageUp | KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.pf.cursor = self.pf.cursor.saturating_sub(10);
                self.snap_pf_cursor_to_entry();
            },
            | KeyCode::PageDown | KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                if display_len > 0 {
                    self.pf.cursor = (self.pf.cursor + 10).min(display_len.saturating_sub(1));
                    self.snap_pf_cursor_to_entry();
                }
            },
            // [p] Toggle: running→pause, paused→activate, error→pause
            | KeyCode::Char('p') => {
                if let Some(idx) = self.pf_entry_at_cursor() {
                    let entry = &self.pf.manager.entries()[idx];
                    let id = entry.id;
                    let local_port = entry.port.local;
                    let resource_name = entry.identity.name.clone();
                    let namespace = entry.namespace.clone();
                    let context = entry.context.clone();
                    if matches!(entry.status, portforward::PortForwardStatus::Paused) {
                        // Paused → activate: spawn a new task
                        let client = self.kube.client.streaming.clone();
                        if !self.pf.manager.resume_spawn(id, client) {
                            self.pf.manager.resume(id);
                        }
                        self.update_saved_pf_paused(&resource_name, &namespace, &context, local_port, false);
                    } else if matches!(entry.status, portforward::PortForwardStatus::Error(_)) {
                        // Error → pause (stop retrying, user can activate later)
                        self.pf.manager.pause(id);
                        self.update_saved_pf_paused(&resource_name, &namespace, &context, local_port, true);
                    } else if entry.status.is_running() {
                        // Running → pause
                        self.pf.manager.pause(id);
                        self.update_saved_pf_paused(&resource_name, &namespace, &context, local_port, true);
                    }
                }
            },
            // [d] Cancel (delete) selected forward
            | KeyCode::Char('d') => {
                self.confirm_cancel_port_forward();
            },
            // [e] Edit local port
            | KeyCode::Char('e') => {
                if let Some(idx) = self.pf_entry_at_cursor() {
                    let entry = &self.pf.manager.entries()[idx];
                    let id = entry.id;
                    let old_port = entry.port.local;
                    self.popup = Some(Popup::PortForwardEditPort {
                        pf_id: id,
                        old_port,
                        buf: old_port.to_string(),
                    });
                }
            },
            | _ => {},
        }
    }

    fn handle_pf_edit_port_key(&mut self, key: KeyEvent) {
        let (pf_id, old_port, buf) = match &mut self.popup {
            | Some(Popup::PortForwardEditPort { pf_id, old_port, buf }) => (*pf_id, *old_port, buf),
            | _ => return,
        };
        match key.code {
            | KeyCode::Esc => {
                self.popup = None;
            },
            | KeyCode::Backspace => {
                buf.pop();
            },
            | KeyCode::Char(c) if c.is_ascii_digit() => {
                buf.push(c);
            },
            | KeyCode::Enter => {
                let new_port: u16 = match buf.parse() {
                    | Ok(p) if p > 0 => p,
                    | _ => {
                        self.error = Some("Invalid port number".into());
                        return;
                    },
                };
                if new_port == old_port {
                    self.popup = None;
                    return;
                }
                self.apply_pf_port_change(pf_id, old_port, new_port);
                self.popup = None;
            },
            | _ => {},
        }
    }

    /// Change the local port of a port forward: cancel old, create new with
    /// updated port, update config.
    fn apply_pf_port_change(&mut self, pf_id: usize, old_port: u16, new_port: u16) {
        let entry = match self.pf.manager.entries().iter().find(|e| e.id == pf_id) {
            | Some(e) => e,
            | None => return,
        };

        let req = PortForwardRequest {
            namespace: entry.namespace.clone(),
            context: entry.context.clone(),
            type_key: entry.identity.type_key.clone(),
            resource_name: entry.identity.name.clone(),
            local_port: new_port,
            remote_port: entry.port.remote,
            target: entry.target.clone(),
        };
        let was_paused = matches!(entry.status, portforward::PortForwardStatus::Paused);

        self.pf.manager.cancel(pf_id);
        self.pf.manager.remove_cancelled();

        self.config.active_profile_mut().remove_port_forward(
            &req.resource_name,
            &req.namespace,
            &req.context,
            old_port,
        );
        self.config
            .active_profile_mut()
            .add_port_forward(req.to_saved(was_paused));
        if let Err(e) = self.config.save() {
            self.error = Some(format!("Failed to save config: {}", e));
        }

        if was_paused {
            self.pf.manager.create_paused(req);
        } else {
            let client = self.kube.client.streaming.clone();
            self.pf.manager.create(client, req);
        }

        self.push_status(format!("Port changed :{} → :{}", old_port, new_port));
    }

    /// Update the `paused` field of a saved port forward in config.
    fn update_saved_pf_paused(
        &mut self,
        resource_name: &str,
        namespace: &str,
        context: &str,
        local_port: u16,
        paused: bool,
    ) {
        if let Some(spf) = self.config.active_profile_mut().port_forwards.iter_mut().find(|pf| {
            pf.resource.name == resource_name
                && pf.namespace == namespace
                && pf.context == context
                && pf.port.local_port == local_port
        }) {
            spf.paused = paused;
        }
        if let Err(e) = self.config.save() {
            self.error = Some(format!("Failed to save config: {}", e));
        }
    }

    fn handle_pf_create_popup_key(&mut self, key: KeyEvent) {
        let dialog = match &mut self.popup {
            | Some(Popup::PortForwardCreate(d)) => d,
            | _ => return,
        };

        match key.code {
            | KeyCode::Esc => {
                self.popup = None;
            },
            | KeyCode::Up | KeyCode::Char('k') => {
                if dialog.port_cursor > 0 {
                    dialog.port_cursor -= 1;
                    dialog.local_port_buf = dialog.ports[dialog.port_cursor].container_port.to_string();
                }
            },
            | KeyCode::Down | KeyCode::Char('j') => {
                if dialog.port_cursor + 1 < dialog.ports.len() {
                    dialog.port_cursor += 1;
                    dialog.local_port_buf = dialog.ports[dialog.port_cursor].container_port.to_string();
                }
            },
            | KeyCode::Backspace => {
                dialog.local_port_buf.pop();
            },
            | KeyCode::Char(c) if c.is_ascii_digit() => {
                dialog.local_port_buf.push(c);
            },
            | KeyCode::Enter => {
                self.confirm_port_forward_create();
            },
            | _ => {},
        }
    }

    fn confirm_port_forward_create(&mut self) {
        let dialog = match &self.popup {
            | Some(Popup::PortForwardCreate(d)) => d,
            | _ => return,
        };

        let local_port: u16 = match dialog.local_port_buf.parse() {
            | Ok(p) if p > 0 => p,
            | _ => {
                self.error = Some("Invalid local port".into());
                return;
            },
        };

        let remote_port = dialog.ports[dialog.port_cursor].container_port;
        let req = PortForwardRequest {
            namespace: dialog.namespace.clone(),
            context: self.kube.context.name.clone(),
            type_key: dialog.resource_type.singular_name().to_string(),
            resource_name: dialog.resource_name.clone(),
            local_port,
            remote_port,
            target: dialog.target.clone(),
        };
        let client = self.kube.client.streaming.clone();
        self.pf.manager.create(client, req.clone());
        self.config.active_profile_mut().add_port_forward(req.to_saved(false));
        if let Err(e) = self.config.save() {
            self.error = Some(format!("Failed to save config: {}", e));
        }

        self.popup = None;
        self.push_status(format!("Port forward created: :{} -> :{}", local_port, remote_port));
        self.error = None;
    }

    /// Poll port forward status updates (called every event loop tick).
    pub fn poll_port_forwards(&mut self) {
        self.pf.manager.poll_updates();
        if self.auth_hold {
            return;
        }
        let auth_expired = self
            .pf
            .manager
            .entries()
            .iter()
            .any(|entry| matches!(entry.status, portforward::PortForwardStatus::AuthExpired));
        if auth_expired {
            self.enter_auth_hold("credential plugin rejected the token");
        }
    }

    // -----------------------------------------------------------------------
    // Exec — spawn new terminal window
    // -----------------------------------------------------------------------

    /// Spawn a new terminal window running kubectl exec.
    pub fn spawn_exec_terminal(&mut self) {
        let exec = match self.pending_exec.take() {
            | Some(e) => e,
            | None => return,
        };

        let terminal_app = match self.exec_terminal_override.take().or_else(resolve_terminal_app) {
            | Some(t) => t,
            | None => {
                self.error = Some("Set $TERMINAL or $TERM_PROGRAM to specify your terminal emulator".into());
                return;
            },
        };

        let context = self.kube.context.name.as_str().to_string();
        let kubeconfig_flag = self
            .kube
            .kubeconfig
            .path
            .as_deref()
            .map(|p| format!(" --kubeconfig {}", p))
            .unwrap_or_default();
        let cmd_str = exec.command.join(" ");

        let kubectl_cmd = format!(
            "kubectl exec -it {} -n {} -c {} --context {}{} -- {} ; exit",
            exec.pod_name, exec.namespace, exec.container, context, kubeconfig_flag, cmd_str,
        );

        let result = spawn_terminal_window(&terminal_app, &kubectl_cmd);

        match result {
            | Ok(_) => {
                self.push_status(format!(
                    "Opened shell in {}: {}/{}",
                    terminal_app, exec.pod_name, exec.container
                ));
                self.error = None;
            },
            | Err(e) => {
                self.error = Some(format!("Failed to open '{}': {}", terminal_app, e));
            },
        }
    }
}

// ---------------------------------------------------------------------------
// Terminal resolution helpers
// ---------------------------------------------------------------------------

/// Resolve the terminal application to use. Checks $TERMINAL, then
/// platform-specific defaults.
fn resolve_terminal_app() -> Option<String> {
    for var in ["TERMINAL", "TERM_PROGRAM"] {
        if let Ok(val) = std::env::var(var) {
            if !val.is_empty() {
                return Some(val);
            }
        }
    }
    None
}

/// Spawn a new terminal window running a command.
fn spawn_terminal_window(terminal_app: &str, command: &str) -> Result<std::process::Child, std::io::Error> {
    let tmp = tempfile::Builder::new()
        .prefix("qb-exec-")
        .suffix(".sh")
        .tempfile()
        .map_err(std::io::Error::other)?;
    std::fs::write(tmp.path(), format!("#!/bin/sh\n{}\n", command))?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(tmp.path(), std::fs::Permissions::from_mode(0o755))?;
    }

    let path = tmp.into_temp_path();
    let path_str = path.to_string_lossy().to_string();
    std::mem::forget(path); // keep alive for the spawned terminal

    // Apple Terminal.app has no CLI binary — must use osascript
    if matches!(terminal_app, "Apple_Terminal" | "Terminal" | "Terminal.app") {
        return std::process::Command::new("osascript")
            .arg("-e")
            .arg(format!(
                "tell application \"Terminal\"\nactivate\ndo script \"{}\"\nend tell",
                path_str
            ))
            .spawn();
    }

    // All other terminals: run binary directly with -e to get a NEW window
    std::process::Command::new(terminal_app)
        .arg("-e")
        .arg(&path_str)
        .spawn()
}

impl App {
    // -----------------------------------------------------------------------
    // Restart workload
    // -----------------------------------------------------------------------
    fn open_trigger_cronjob(&mut self) {
        if self.selected_resource_type() != Some(ResourceType::CronJob) {
            return;
        }
        let (name, namespace) = if self.view == View::Detail {
            (self.detail.name.clone(), self.detail.namespace.clone())
        } else {
            self.selected_resource_name_ns()
        };
        if name.is_empty() {
            return;
        }
        let default_name = KubeClient::default_trigger_job_name(&name);
        self.popup = Some(Popup::TriggerCronJob {
            cronjob_name: name,
            namespace,
            buf: default_name,
        });
    }

    fn handle_trigger_cronjob_key(&mut self, key: KeyEvent) {
        match key.code {
            | KeyCode::Enter => {
                let (cronjob_name, namespace, job_name) = match &self.popup {
                    | Some(Popup::TriggerCronJob {
                        cronjob_name,
                        namespace,
                        buf,
                    }) => {
                        let job_name = buf.trim().to_string();
                        if job_name.is_empty() {
                            self.error = Some("Job name cannot be empty".into());
                            self.popup = None;
                            return;
                        }
                        (cronjob_name.clone(), namespace.clone(), job_name)
                    },
                    | _ => return,
                };
                self.popup = None;
                self.ask_confirm(
                    "Confirm Trigger",
                    format!("Create job {job_name} from CronJob/{cronjob_name}?"),
                    ConfirmKind::Trigger {
                        namespace,
                        cronjob: cronjob_name,
                        job_name,
                    },
                );
            },
            | KeyCode::Esc => {
                self.popup = None;
            },
            | KeyCode::Backspace => {
                if let Some(Popup::TriggerCronJob { buf, .. }) = &mut self.popup {
                    buf.pop();
                }
            },
            | KeyCode::Char(c) => {
                if let Some(Popup::TriggerCronJob { buf, .. }) = &mut self.popup {
                    buf.push(c);
                }
            },
            | _ => {},
        }
    }

    // -----------------------------------------------------------------------
    // Node cordon / drain
    // -----------------------------------------------------------------------
    fn selected_resource_name_ns(&self) -> (String, String) {
        if self.view == View::Detail {
            return (self.detail.name.clone(), self.detail.namespace.clone());
        }
        let visible = self.visible_resource_indices();
        let vis_pos = self
            .resources
            .state
            .selected()
            .and_then(|sel| visible.iter().position(|&i| i == sel))
            .unwrap_or(0);
        match visible.get(vis_pos).and_then(|&i| self.resources.entries.get(i)) {
            | Some(e) => (e.name.clone(), e.namespace.clone()),
            | None => (String::new(), String::new()),
        }
    }

    // -----------------------------------------------------------------------
    // Copy resource name
    // -----------------------------------------------------------------------

    fn copy_resource_name(&mut self) {
        let visible = self.visible_resource_indices();
        let vis_pos = self
            .resources
            .state
            .selected()
            .and_then(|sel| visible.iter().position(|&i| i == sel))
            .unwrap_or(0);
        let entry = match visible.get(vis_pos).and_then(|&i| self.resources.entries.get(i)) {
            | Some(e) => e,
            | None => return,
        };
        let name = entry.name.clone();
        match arboard::Clipboard::new().and_then(|mut cb| cb.set_text(&name)) {
            | Ok(()) => {
                self.push_status(format!("Copied '{}'", name));
                self.error = None;
            },
            | Err(e) => {
                self.error = Some(format!("Clipboard error: {}", e));
            },
        }
    }

    // -----------------------------------------------------------------------
    // Favorites
    // -----------------------------------------------------------------------

    /// Check if a resource is favorited in the active profile.
    pub fn is_favorite(&self, resource_type: ResourceType, name: &str, namespace: &str) -> bool {
        let context = self.kube.context.name.as_str();
        let key = FavoriteType::builtin(resource_type);
        self.config.active_profile().is_favorite(&key, name, namespace, context)
    }

    fn selected_favorite_key(&self) -> Option<FavoriteType> {
        self.selected_kind().map(ResourceKind::favorite_key)
    }

    /// Toggle favorite for the currently selected resource.
    fn toggle_favorite_selected(&mut self) {
        let Some(type_key) = self.selected_favorite_key() else {
            return;
        };
        let (name, namespace) = self.selected_resource_name_ns();
        if name.is_empty() {
            return;
        }
        let context = self.kube.context.name.as_str().to_string();
        let added = self
            .config
            .active_profile_mut()
            .toggle_favorite(type_key, name.clone(), namespace, context);

        if added {
            self.push_status(format!("★ Favorited '{}'", name));
        } else {
            self.push_status(format!("☆ Unfavorited '{}'", name));
        }
        if let Err(e) = self.config.save() {
            self.error = Some(format!("Failed to save config: {}", e));
        }
    }

    /// Toggle favorite for the resource in detail view.
    fn toggle_favorite_detail(&mut self) {
        let Some(type_key) = self.selected_favorite_key() else {
            return;
        };
        let name = self.detail.name.clone();
        let namespace = self.detail.namespace.clone();
        if name.is_empty() {
            return;
        }
        let context = self.kube.context.name.as_str().to_string();
        let added = self
            .config
            .active_profile_mut()
            .toggle_favorite(type_key, name.clone(), namespace, context);

        if added {
            self.push_status(format!("★ Favorited '{}'", name));
        } else {
            self.push_status(format!("☆ Unfavorited '{}'", name));
        }
        if let Err(e) = self.config.save() {
            self.error = Some(format!("Failed to save config: {}", e));
        }
    }

    /// Build a sorted, grouped display list for the favorites panel.
    /// Groups by category (matching sidebar order), then sorts by name within
    /// each group.
    pub fn favorites_display_items(&self) -> Vec<DisplayItem> {
        let favorites = &self.config.active_profile().favorites;
        if favorites.is_empty() {
            return Vec::new();
        }

        // Build ordering: (category_idx, type_idx) for each ResourceType
        let categories = ResourceType::all_by_category();
        let mut type_order: std::collections::HashMap<&str, (usize, usize)> = std::collections::HashMap::new();
        for (ci, (_cat, types)) in categories.iter().enumerate() {
            for (ti, rt) in types.iter().enumerate() {
                type_order.insert(rt.singular_name(), (ci, ti));
            }
        }

        let mut indexed: Vec<(usize, &crate::config::FavoriteEntry)> = favorites.iter().enumerate().collect();
        indexed.sort_by(|a, b| {
            let oa = type_order
                .get(a.1.resource_type.as_str())
                .copied()
                .unwrap_or((999, 999));
            let ob = type_order
                .get(b.1.resource_type.as_str())
                .copied()
                .unwrap_or((999, 999));
            oa.cmp(&ob).then(a.1.name.cmp(&b.1.name))
        });

        let mut items = Vec::new();
        let mut current_cat: Option<usize> = None;
        for (idx, fav) in &indexed {
            let (ci, _) = type_order
                .get(fav.resource_type.as_str())
                .copied()
                .unwrap_or((999, 999));
            if current_cat != Some(ci) {
                current_cat = Some(ci);
                let label = categories
                    .get(ci)
                    .map(|(cat, _)| cat.display_name().to_string())
                    .unwrap_or_else(|| "Other".to_string());
                items.push(DisplayItem::Header(label));
            }
            items.push(DisplayItem::Entry(*idx));
        }
        items
    }

    /// Resolve the original favorite index for the current display cursor.
    fn favorites_entry_at_cursor(&self) -> Option<usize> {
        match self.favorites_display_items().get(self.favorites.cursor) {
            | Some(DisplayItem::Entry(idx)) => Some(*idx),
            | _ => None,
        }
    }

    /// If the favorites cursor is on a header, snap it to the nearest entry.
    fn snap_favorites_cursor_to_entry(&mut self) {
        let display = self.favorites_display_items();
        if display.is_empty() {
            self.favorites.cursor = 0;
            return;
        }
        if let Some(i) = entry_index_from(&display, self.favorites.cursor) {
            self.favorites.cursor = i;
        } else if let Some(i) = entry_index_before(&display, self.favorites.cursor) {
            self.favorites.cursor = i;
        }
    }

    /// Move the favorites cursor up, skipping headers.
    fn favorites_cursor_up(&mut self) {
        if let Some(i) = entry_index_before(&self.favorites_display_items(), self.favorites.cursor) {
            self.favorites.cursor = i;
        }
    }

    /// Move the favorites cursor down, skipping headers.
    fn favorites_cursor_down(&mut self) {
        if let Some(i) = entry_index_from(&self.favorites_display_items(), self.favorites.cursor + 1) {
            self.favorites.cursor = i;
        }
    }

    /// Navigate to a favorite from the favorites view.
    fn open_favorite_at_cursor(&mut self) {
        if self.favorites_entry_at_cursor().is_none() {
            return;
        }
        let Some((kind, name, namespace)) = self.resolve_favorite_kind() else {
            self.error = Some("Favorite resource type is not available in this cluster".into());
            return;
        };
        self.return_panel = Some(self.panel.clone());
        self.panel = Panel::Resources(kind);
        self.pending_load = Some(PendingLoad::ResourceDetail { name, namespace });
    }

    /// Remove a favorite from the favorites view.
    fn remove_favorite_at_cursor(&mut self) {
        if let Some(idx) = self.favorites_entry_at_cursor() {
            let fav = self.config.active_profile().favorites[idx].clone();
            self.config.active_profile_mut().favorites.retain(|f| f != &fav);
            let display_len = self.favorites_display_items().len();
            if self.favorites.cursor >= display_len {
                self.favorites.cursor = display_len.saturating_sub(1);
            }
            self.snap_favorites_cursor_to_entry();
            self.push_status(format!("Removed '{}' from favorites", fav.name));
            if let Err(e) = self.config.save() {
                self.error = Some(format!("Failed to save config: {}", e));
            }
        }
    }

    fn resolve_favorite_kind(&self) -> Option<(ResourceKind, String, String)> {
        let idx = self.favorites_entry_at_cursor()?;
        let fav = self.config.active_profile().favorites.get(idx)?;
        let kind = self.favorite_kind(fav)?;
        Some((kind, fav.name.clone(), fav.namespace.clone()))
    }

    /// Builtin favorite at the cursor. Custom resources return `None`.
    fn resolve_favorite_at_cursor(&self) -> Option<(ResourceType, String, String)> {
        let (kind, name, namespace) = self.resolve_favorite_kind()?;
        Some((kind.builtin()?, name, namespace))
    }

    /// Handle keys in the favorites view.
    /// Supports all standard resource list commands plus `*` to de-favorite.
    fn handle_favorites_key(&mut self, key: KeyEvent) {
        let display_len = self.favorites_display_items().len();
        match key.code {
            | KeyCode::Up | KeyCode::Char('k') => {
                self.favorites_cursor_up();
            },
            | KeyCode::Down | KeyCode::Char('j') => {
                self.favorites_cursor_down();
            },
            | KeyCode::PageUp | KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.favorites.cursor = self.favorites.cursor.saturating_sub(20);
                self.snap_favorites_cursor_to_entry();
            },
            | KeyCode::PageDown | KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.favorites.cursor = (self.favorites.cursor + 20).min(display_len.saturating_sub(1));
                self.snap_favorites_cursor_to_entry();
            },
            | KeyCode::Home | KeyCode::Char('g') => {
                self.favorites.cursor = 0;
                self.snap_favorites_cursor_to_entry();
            },
            | KeyCode::End | KeyCode::Char('G') => {
                if display_len > 0 {
                    self.favorites.cursor = display_len - 1;
                    self.snap_favorites_cursor_to_entry();
                }
            },
            // [Enter] Open detail view for this favorite
            | KeyCode::Enter => {
                self.open_favorite_at_cursor();
            },
            // [*] De-favorite (remove from favorites)
            | KeyCode::Char('*') => {
                self.remove_favorite_at_cursor();
            },
            // [l] Open logs (workload resources)
            | KeyCode::Char('l') => {
                self.open_favorite_logs();
            },
            // [e] Edit resource ($EDITOR)
            | KeyCode::Char('e') => {
                self.begin_edit();
            },
            // [y] Copy resource name
            | KeyCode::Char('y') => {
                if let Some(idx) = self.favorites_entry_at_cursor() {
                    let name = self.config.active_profile().favorites[idx].name.clone();
                    match arboard::Clipboard::new().and_then(|mut cb| cb.set_text(&name)) {
                        | Ok(()) => {
                            self.push_status(format!("Copied '{}'", name));
                            self.error = None;
                        },
                        | Err(e) => {
                            self.error = Some(format!("Clipboard error: {}", e));
                        },
                    }
                }
            },
            // [F] Port forward — open dialog directly with favorite data
            | KeyCode::Char('F') => {
                self.begin_port_forward();
            },
            // [D] Delete resource
            | KeyCode::Char('D') => {
                if let Some((kind, name, namespace)) = self.resolve_favorite_kind() {
                    self.popup = Some(Popup::ConfirmDelete { name, namespace, kind });
                }
            },
            // [R] Restart workload
            | KeyCode::Char('R') => {
                self.confirm_restart();
            },
            | KeyCode::Char('S') => {
                self.begin_scale();
            },
            | KeyCode::Char('d') => {
                self.begin_diff();
            },
            | _ => {},
        }
    }

    /// Open logs for the favorite at cursor.
    fn open_favorite_logs(&mut self) {
        if let Some(idx) = self.favorites_entry_at_cursor() {
            let favorites = self.config.active_profile().favorites.clone();
            if let Some(fav) = favorites.get(idx) {
                if let Some(rt) = fav.resource_type.builtin_type() {
                    if rt.supports_logs() {
                        self.return_panel = Some(self.panel.clone());
                        self.panel = Panel::Resources(ResourceKind::Builtin(rt));
                        self.pending_load = Some(PendingLoad::Logs {
                            name: fav.name.clone(),
                            namespace: fav.namespace.clone(),
                        });
                    }
                }
            }
        }
    }

    /// Edit the resource at the favorites cursor.
    fn sorted_profile_names(&self) -> Vec<String> {
        let mut names: Vec<String> = self.config.profiles.keys().cloned().collect();
        names.sort();
        names
    }

    fn handle_profiles_key(&mut self, key: KeyEvent) {
        let names = self.sorted_profile_names();
        let count = names.len();
        match key.code {
            | KeyCode::Up | KeyCode::Char('k') => {
                if self.profiles.cursor > 0 {
                    self.profiles.cursor -= 1;
                }
            },
            | KeyCode::Down | KeyCode::Char('j') => {
                if count > 0 && self.profiles.cursor + 1 < count {
                    self.profiles.cursor += 1;
                }
            },
            | KeyCode::PageUp | KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.profiles.cursor = self.profiles.cursor.saturating_sub(20);
            },
            | KeyCode::PageDown | KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.profiles.cursor = (self.profiles.cursor + 20).min(count.saturating_sub(1));
            },
            | KeyCode::Home | KeyCode::Char('g') => {
                self.profiles.cursor = 0;
            },
            | KeyCode::End | KeyCode::Char('G') => {
                if count > 0 {
                    self.profiles.cursor = count - 1;
                }
            },
            // [Enter] Switch to selected profile
            | KeyCode::Enter => {
                if let Some(name) = names.get(self.profiles.cursor) {
                    let name = name.clone();
                    self.request_switch_profile(name);
                }
            },
            // [c] Clone profile
            | KeyCode::Char('c') => {
                if let Some(source) = names.get(self.profiles.cursor) {
                    let source = source.clone();
                    self.popup = Some(Popup::ProfileClone {
                        source_name: source.clone(),
                        buf: format!("{}-copy", source),
                    });
                }
            },
            // [D] Delete profile
            | KeyCode::Char('D') => {
                if let Some(name) = names.get(self.profiles.cursor) {
                    let name = name.clone();
                    if name == "default" {
                        self.error = Some("Cannot delete the default profile".into());
                    } else if name == self.config.active_profile {
                        self.error = Some("Cannot delete the active profile".into());
                    } else {
                        self.popup = Some(Popup::ConfirmDeleteProfile { profile_name: name });
                    }
                }
            },
            // [C] Create new empty profile
            | KeyCode::Char('C') => {
                self.popup = Some(Popup::ProfileSave { buf: String::new() });
            },
            | _ => {},
        }
    }

    fn handle_profile_clone_key(&mut self, key: KeyEvent) {
        let (source_name, buf) = match &mut self.popup {
            | Some(Popup::ProfileClone { source_name, buf }) => (source_name.clone(), buf),
            | _ => return,
        };
        match key.code {
            | KeyCode::Esc => {
                self.popup = None;
            },
            | KeyCode::Backspace => {
                buf.pop();
            },
            | KeyCode::Char(c) => {
                buf.push(c);
            },
            | KeyCode::Enter => {
                let new_name = buf.clone();
                if new_name.is_empty() {
                    return;
                }
                if self.config.profiles.contains_key(&new_name) {
                    self.error = Some(format!("Profile '{}' already exists", new_name));
                    return;
                }
                let profile = self.config.profiles.get(&source_name).cloned().unwrap_or_default();
                self.config.profiles.insert(new_name.clone(), profile);
                self.popup = None;
                self.push_status(format!("Cloned '{}' → '{}'", source_name, new_name));
                if let Err(e) = self.config.save() {
                    self.error = Some(format!("Failed to save config: {}", e));
                }
            },
            | _ => {},
        }
    }

    fn handle_confirm_delete_profile_key(&mut self, key: KeyEvent) {
        let profile_name = match &self.popup {
            | Some(Popup::ConfirmDeleteProfile { profile_name }) => profile_name.clone(),
            | _ => return,
        };
        match key.code {
            | KeyCode::Enter | KeyCode::Char('y') => {
                self.config.profiles.remove(&profile_name);
                self.popup = None;
                self.push_status(format!("Deleted profile '{}'", profile_name));
                let count = self.config.profiles.len();
                if self.profiles.cursor >= count && count > 0 {
                    self.profiles.cursor = count - 1;
                }
                if let Err(e) = self.config.save() {
                    self.error = Some(format!("Failed to save config: {}", e));
                }
            },
            | KeyCode::Esc | KeyCode::Char('n') => {
                self.popup = None;
            },
            | _ => {},
        }
    }

    // -----------------------------------------------------------------------
    // Profile save/load
    // -----------------------------------------------------------------------

    /// Switch to a different profile: cancel all current port forwards,
    /// update the active profile, restore the new profile's port forwards
    /// and context.
    fn open_profile_save(&mut self) {
        let current = self.config.active_profile.clone();
        self.popup = Some(Popup::ProfileSave { buf: current });
    }

    fn open_profile_load(&mut self) {
        let mut names: Vec<String> = self.config.profile_names().into_iter().map(|s| s.to_string()).collect();
        names.sort();
        let mut state = ListState::default();
        if !names.is_empty() {
            let current_pos = names.iter().position(|n| n == &self.config.active_profile).unwrap_or(0);
            state.select(Some(current_pos));
        }
        self.popup = Some(Popup::ProfileLoad { items: names, state });
    }

    fn handle_profile_save_key(&mut self, key: KeyEvent) {
        let buf = match &mut self.popup {
            | Some(Popup::ProfileSave { buf }) => buf,
            | _ => return,
        };
        match key.code {
            | KeyCode::Esc => {
                self.popup = None;
            },
            | KeyCode::Backspace => {
                buf.pop();
            },
            | KeyCode::Char(c) => {
                buf.push(c);
            },
            | KeyCode::Enter => {
                let name = buf.clone();
                if name.is_empty() {
                    return;
                }
                self.config.save_as(name.clone());
                self.popup = None;
                self.push_status(format!("Profile saved as '{}'", name));
                if let Err(e) = self.config.save() {
                    self.error = Some(format!("Failed to save config: {}", e));
                }
            },
            | _ => {},
        }
    }

    fn handle_profile_load_key(&mut self, key: KeyEvent) {
        let (items, state) = match &mut self.popup {
            | Some(Popup::ProfileLoad { items, state }) => (items, state),
            | _ => return,
        };
        match key.code {
            | KeyCode::Esc => {
                self.popup = None;
            },
            | KeyCode::Up | KeyCode::Char('k') => {
                let i = state.selected().unwrap_or(0);
                if i > 0 {
                    state.select(Some(i - 1));
                }
            },
            | KeyCode::Down | KeyCode::Char('j') => {
                let i = state.selected().unwrap_or(0);
                if i + 1 < items.len() {
                    state.select(Some(i + 1));
                }
            },
            | KeyCode::Enter => {
                if let Some(idx) = state.selected() {
                    if let Some(name) = items.get(idx) {
                        let name = name.clone();
                        self.popup = None;
                        self.request_switch_profile(name);
                    }
                }
            },
            | _ => {},
        }
    }

    fn copy_logs_to_clipboard(&mut self) {
        if let Some(state) = &self.log {
            let visible = state.visible_lines();
            let (text, count) = if let Some((start, end)) = state.selection_range() {
                let selected: Vec<String> = visible
                    .iter()
                    .enumerate()
                    .filter(|(i, _)| *i >= start && *i <= end)
                    .map(|(_, l)| l.display_text())
                    .collect();
                let n = selected.len();
                (selected.join("\n"), n)
            } else {
                let n = visible.len();
                let texts: Vec<String> = visible.iter().map(|l| l.display_text()).collect();
                (texts.join("\n"), n)
            };
            match arboard::Clipboard::new().and_then(|mut cb| cb.set_text(&text)) {
                | Ok(()) => {
                    self.push_status(format!("Copied {} log lines to clipboard", count));
                },
                | Err(e) => {
                    self.error = Some(format!("Clipboard error: {}", e));
                },
            }
        }
    }

    // -----------------------------------------------------------------------
    // Diff between resources
    // -----------------------------------------------------------------------
    fn start_create_resource(&mut self) {
        let ns = self.kube.context.namespace.as_deref().unwrap_or("default").to_string();
        let template = format!(
            "apiVersion: v1\nkind: ConfigMap\nmetadata:\n  name: new-resource\n  namespace: {}\ndata: {{}}\n",
            ns
        );
        self.pending_create = Some(PendingCreate { yaml: template });
    }

    pub fn handle_create_result(&mut self, yaml: String) {
        if yaml.trim().is_empty() {
            self.push_status("Empty YAML, nothing created");
            return;
        }
        self.ask_confirm(
            "Confirm Create",
            "Create this resource in the cluster?".into(),
            ConfirmKind::Create { yaml },
        );
    }

    // -----------------------------------------------------------------------
    // Command palette
    // -----------------------------------------------------------------------

    fn toggle_palette(&mut self) {
        if self.palette.is_some() {
            self.palette = None;
            return;
        }
        self.palette = Some(PaletteState {
            global: false,
            buf: String::new(),
            results: Vec::new(),
            cursor: 0,
            all_resources: Vec::new(),
        });
        self.update_palette_results();
    }

    fn update_palette_results(&mut self) {
        let query = match &self.palette {
            | Some(p) => p.buf.to_lowercase(),
            | None => return,
        };
        let query = query.trim_start_matches('>').trim().to_string();
        let flags = self.cmd_flags();
        let ctx = self.current_context();
        let singular = self.selected_resource_type().map(|rt| rt.singular_name()).unwrap_or("");
        let mut results = Vec::new();

        for cmd in super::command::palette_commands(ctx, &flags) {
            let Some(action) = super::command::command_action(cmd) else {
                continue;
            };
            let haystack = format!("{} {} {}", cmd.label, cmd.description, cmd.key).to_lowercase();
            if query.is_empty() || query.split_whitespace().all(|word| haystack.contains(word)) {
                let key = if cmd.key.is_empty() || cmd.key == "palette" {
                    String::new()
                } else {
                    format!("  {}", cmd.key)
                };
                results.push(PaletteEntry {
                    label: cmd.label.to_string(),
                    description: format!("{}{}", cmd.description, key),
                    kind: PaletteEntryKind::Command { action },
                });
            }
        }

        if !query.is_empty() {
            for entry in &self.resources.entries {
                let haystack = format!(
                    "{} {}/{} {} {}",
                    singular,
                    singular,
                    entry.name,
                    entry.namespace,
                    entry.columns.join(" ")
                )
                .to_lowercase();
                if query.split_whitespace().all(|word| haystack.contains(word)) {
                    let label = if entry.namespace.is_empty() {
                        format!("{}/{}", singular, entry.name)
                    } else {
                        format!("{}/{}  ({})", singular, entry.name, entry.namespace)
                    };
                    results.push(PaletteEntry {
                        label,
                        description: String::new(),
                        kind: PaletteEntryKind::Resource {
                            name: entry.name.clone(),
                            namespace: entry.namespace.clone(),
                            resource_type: None,
                        },
                    });
                }
                if results.len() >= 80 {
                    break;
                }
            }
        }

        let Some(p) = &mut self.palette else {
            return;
        };
        p.results = results;
        if p.results.is_empty() {
            p.cursor = 0;
        } else {
            p.cursor = p.cursor.min(p.results.len() - 1);
        }
    }

    fn handle_palette_key(&mut self, key: KeyEvent) {
        match key.code {
            | KeyCode::Esc => {
                self.palette = None;
            },
            | KeyCode::Enter => {
                let selected = self.palette.as_ref().and_then(|p| {
                    p.results.get(p.cursor).map(|entry| {
                        match &entry.kind {
                            | PaletteEntryKind::Resource {
                                name,
                                namespace,
                                resource_type,
                            } => (Some((name.clone(), namespace.clone(), *resource_type)), None),
                            | PaletteEntryKind::Command { action } => (None, Some(*action)),
                        }
                    })
                });
                if let Some((resource, command)) = selected {
                    self.palette = None;
                    if let Some((name, namespace, rt)) = resource {
                        if let Some(rt) = rt {
                            let kind = ResourceKind::Builtin(rt);
                            self.panel = Panel::Resources(kind.clone());
                            self.view = View::Main;
                            if let Some(nav_idx) = self
                                .nav
                                .items
                                .iter()
                                .position(|item| matches!(&item.kind, NavItemKind::Resource(r) if r == &kind))
                            {
                                self.nav.state.select(Some(nav_idx));
                            }
                        }
                        self.pending_load = Some(PendingLoad::ResourceDetail { name, namespace });
                    } else if let Some(action) = command {
                        self.run_action(action);
                    }
                }
            },
            | KeyCode::Tab => {},
            | KeyCode::Up => {
                if let Some(p) = &mut self.palette {
                    if p.cursor > 0 {
                        p.cursor -= 1;
                    }
                }
            },
            | KeyCode::Down => {
                if let Some(p) = &mut self.palette {
                    if !p.results.is_empty() && p.cursor < p.results.len() - 1 {
                        p.cursor += 1;
                    }
                }
            },
            | KeyCode::PageUp => {
                if let Some(p) = &mut self.palette {
                    p.cursor = p.cursor.saturating_sub(10);
                }
            },
            | KeyCode::PageDown => {
                if let Some(p) = &mut self.palette {
                    if !p.results.is_empty() {
                        p.cursor = (p.cursor + 10).min(p.results.len().saturating_sub(1));
                    }
                }
            },
            | KeyCode::Backspace => {
                if let Some(p) = &mut self.palette {
                    p.buf.pop();
                    p.cursor = 0;
                }
                self.update_palette_results();
            },
            | KeyCode::Char(c) => {
                if let Some(p) = &mut self.palette {
                    p.buf.push(c);
                    p.cursor = 0;
                }
                self.update_palette_results();
            },
            | _ => {},
        }
    }

    fn toggle_pause(&mut self) {
        self.paused = !self.paused;
        self.push_status(if self.paused {
            "Paused — auto-refresh disabled"
        } else {
            "Resumed — auto-refresh enabled"
        });
    }

    fn open_kubeconfig_input(&mut self) {
        let default = self
            .kube
            .kubeconfig
            .path
            .clone()
            .or_else(|| std::env::var("KUBECONFIG").ok())
            .filter(|p| !p.is_empty())
            .unwrap_or_else(|| "~/.kube/config".to_string());
        self.popup = Some(Popup::KubeconfigInput { buf: default });
    }

    fn selected_target(&self) -> Option<(ResourceKind, String, String)> {
        if self.view == View::Detail && !self.detail.name.is_empty() {
            return self
                .selected_kind()
                .cloned()
                .map(|kind| (kind, self.detail.name.clone(), self.detail.namespace.clone()));
        }
        if matches!(self.panel, Panel::Favorites) {
            return self.resolve_favorite_kind();
        }
        let kind = self.selected_kind().cloned()?;
        let (name, namespace) = self.selected_resource_name_ns();
        if name.is_empty() {
            None
        } else {
            Some((kind, name, namespace))
        }
    }

    fn begin_edit(&mut self) {
        if self.view == View::Detail && !self.detail.yaml.is_empty() {
            self.start_edit_from_detail();
            return;
        }
        let Some((kind, name, namespace)) = self.selected_target() else {
            return;
        };
        self.request_resource_value(kind, name, namespace, ValueUse::Edit);
    }

    fn begin_diff(&mut self) {
        let Some((kind, name, namespace)) = self.selected_target() else {
            return;
        };
        let mark = self.diff_mark.take();
        self.request_resource_value(kind, name, namespace, ValueUse::Diff { mark });
    }

    fn begin_port_forward(&mut self) {
        let Some((kind, name, namespace)) = self.selected_target() else {
            return;
        };
        let Some(rt) = kind.builtin().filter(|rt| rt.supports_port_forward()) else {
            return;
        };
        self.request_resource_value(kind, name, namespace, ValueUse::PortForward { rt });
    }

    fn begin_scale(&mut self) {
        let Some((kind, name, namespace)) = self.selected_target() else {
            return;
        };
        let Some(rt) = kind.builtin().filter(|rt| rt.supports_scale()) else {
            return;
        };
        if self.view == View::Detail {
            self.open_scale_input_detail();
            return;
        }
        self.request_resource_value(kind, name, namespace, ValueUse::Scale { rt });
    }

    fn begin_exec(&mut self) {
        let Some(rt) = self
            .selected_resource_type()
            .filter(|rt| self.experimental && rt.supports_exec())
        else {
            return;
        };
        let (name, namespace) = if self.view == View::Detail {
            (self.detail.name.clone(), self.detail.namespace.clone())
        } else if matches!(self.panel, Panel::Favorites) {
            let Some((rt2, name, namespace)) = self.resolve_favorite_at_cursor() else {
                return;
            };
            if rt2 != rt {
                return;
            }
            (name, namespace)
        } else {
            self.selected_resource_name_ns()
        };
        if name.is_empty() {
            return;
        }
        let kube = self.kube.clone();
        self.spawn_read(ReadToken::Interactive, async move {
            let result = kube
                .find_pods(rt, &namespace, &name)
                .await
                .map_err(ClusterFailure::from_anyhow);
            let apply: Apply = Box::new(move |app: &mut App| app.finish_exec(namespace, result));
            apply
        });
    }

    fn request_resource_value(&mut self, kind: ResourceKind, name: String, namespace: String, use_: ValueUse) {
        let kube = self.kube.clone();
        self.spawn_read(ReadToken::Interactive, async move {
            let result = kube
                .get_kind(&kind, &namespace, &name)
                .await
                .map_err(ClusterFailure::from_anyhow);
            let apply: Apply =
                Box::new(move |app: &mut App| app.finish_resource_value(kind, name, namespace, use_, result));
            apply
        });
    }

    fn finish_resource_value(
        &mut self,
        kind: ResourceKind,
        name: String,
        namespace: String,
        use_: ValueUse,
        result: Result<Value, ClusterFailure>,
    ) {
        let value = match result {
            | Ok(value) => value,
            | Err(e) => {
                if let ValueUse::Diff { mark } = use_ {
                    self.diff_mark = mark;
                }
                self.report_cluster_error(e.context("Failed to fetch resource"));
                return;
            },
        };
        match use_ {
            | ValueUse::Edit => {
                self.pending_edit = Some(PendingEdit {
                    kind,
                    name,
                    namespace,
                    yaml: serde_yaml::to_string(&value).unwrap_or_default(),
                    original_yaml: None,
                });
            },
            | ValueUse::Diff { mark } => {
                let yaml = serde_yaml::to_string(&value).unwrap_or_default();
                if let Some(mark) = mark {
                    let diff_lines = compute_diff(&mark.yaml, &yaml);
                    self.edit_ctx = Some(EditContext {
                        kind,
                        name: format!("{} vs {}", mark.name, name),
                        namespace,
                        original_yaml: mark.yaml,
                        edited_yaml: yaml,
                        diff_lines,
                        diff_mode: DiffMode::Inline,
                        scroll: 0,
                        error: None,
                        applyable: false,
                    });
                    self.view = View::EditDiff;
                    self.push_status("Diff ready — Esc closes, this comparison is not applied");
                } else {
                    self.diff_mark = Some(DiffMark { name, namespace, yaml });
                    self.push_status("Marked for diff — select another and run Diff again");
                }
            },
            | ValueUse::PortForward { rt } => self.open_port_forward_from_value(rt, name, namespace, &value),
            | ValueUse::Scale { rt } => {
                let current = value
                    .get("spec")
                    .and_then(|s| s.get("replicas"))
                    .and_then(|r| r.as_u64())
                    .unwrap_or(1) as u32;
                self.popup = Some(Popup::ScaleInput {
                    name,
                    namespace,
                    resource_type: rt,
                    current,
                    buf: current.to_string(),
                });
            },
        }
    }

    fn open_port_forward_from_value(&mut self, rt: ResourceType, name: String, namespace: String, value: &Value) {
        let ports = portforward::extract_ports(rt, value);
        if ports.is_empty() {
            self.error = Some("No ports found on this resource".into());
            return;
        }
        let target = if rt == ResourceType::Pod {
            PfTarget::DirectPod { pod_name: name.clone() }
        } else {
            match portforward::extract_selector(rt, value) {
                | Some(selector) => PfTarget::LabelSelector { selector },
                | None => {
                    self.error = Some("Cannot resolve pod selector for this resource".into());
                    return;
                },
            }
        };
        let default_port = ports[0].container_port.to_string();
        self.popup = Some(Popup::PortForwardCreate(PfCreateDialog {
            resource_type: rt,
            resource_name: name,
            namespace,
            target,
            ports,
            port_cursor: 0,
            local_port_buf: default_port,
        }));
    }

    fn finish_exec(&mut self, namespace: String, result: Result<Vec<crate::k8s::PodInfo>, ClusterFailure>) {
        match result {
            | Ok(pods) if !pods.is_empty() => {
                let pod = &pods[0];
                self.popup = Some(Popup::ExecShell {
                    pod_name: pod.name.clone(),
                    namespace,
                    containers: pod.containers.clone(),
                    container_cursor: 0,
                    command_buf: "/bin/sh".to_string(),
                    terminal_buf: resolve_terminal_app().unwrap_or_default(),
                    editing_terminal: false,
                });
            },
            | Ok(_) => self.error = Some("No pods found for exec".into()),
            | Err(e) => self.report_cluster_error(e.context("Failed to find pods")),
        }
    }

    fn confirm_restart(&mut self) {
        let Some((kind, name, namespace)) = self.selected_target() else {
            return;
        };
        let Some(rt) = kind.builtin().filter(|rt| rt.supports_restart()) else {
            return;
        };
        self.ask_confirm(
            "Confirm Restart",
            format!("Restart {}/{}? This rolls the workload.", rt.display_name(), name),
            ConfirmKind::Restart { rt, name, namespace },
        );
    }

    fn confirm_cordon(&mut self) {
        if self.selected_resource_type() != Some(ResourceType::Node) {
            return;
        }
        let (name, _) = self.selected_resource_name_ns();
        if name.is_empty() {
            return;
        }
        let schedulable = if self.view == View::Detail {
            !self
                .detail
                .value
                .get("spec")
                .and_then(|s| s.get("unschedulable"))
                .and_then(|v| v.as_bool())
                .unwrap_or(false)
        } else {
            self.resources
                .state
                .selected()
                .and_then(|idx| self.resources.entries.get(idx))
                .and_then(|e| e.columns.first())
                .map(|status| !status.contains("SchedulingDisabled"))
                .unwrap_or(true)
        };
        let verb = if schedulable { "Cordon" } else { "Uncordon" };
        self.ask_confirm("Confirm Cordon", format!("{verb} node {name}?"), ConfirmKind::Cordon {
            name,
            cordon: schedulable,
        });
    }

    fn confirm_drain(&mut self) {
        if self.selected_resource_type() != Some(ResourceType::Node) {
            return;
        }
        let (name, _) = self.selected_resource_name_ns();
        if name.is_empty() {
            return;
        }
        self.ask_confirm(
            "Confirm Drain",
            format!("Drain node {name}? This cordons the node and evicts pods."),
            ConfirmKind::Drain { node_name: name },
        );
    }

    fn confirm_apply_edit(&mut self) {
        let Some(ctx) = &self.edit_ctx else {
            return;
        };
        if !ctx.applyable {
            self.push_status("Comparison only — Esc to close");
            return;
        }
        let kind = ctx.kind.clone();
        let name = ctx.name.clone();
        let namespace = ctx.namespace.clone();
        let yaml = ctx.edited_yaml.clone();
        self.enqueue_write(Write::Replace {
            kind,
            name,
            namespace,
            yaml,
        });
        self.push_status("Applying changes…");
    }

    fn confirm_cancel_port_forward(&mut self) {
        let Some(idx) = self.pf_entry_at_cursor() else {
            return;
        };
        let entry = &self.pf.manager.entries()[idx];
        let id = entry.id;
        let resource_name = entry.identity.name.clone();
        let namespace = entry.namespace.clone();
        let context = entry.context.clone();
        let local_port = entry.port.local;
        self.ask_confirm(
            "Confirm Cancel",
            format!("Cancel port forward :{local_port} for {resource_name}?"),
            ConfirmKind::CancelPortForward {
                id,
                resource_name,
                namespace,
                context,
                local_port,
            },
        );
    }

    fn cancel_port_forward(&mut self, id: usize, resource_name: &str, namespace: &str, context: &str, local_port: u16) {
        self.pf.manager.cancel(id);
        self.pf.manager.remove_cancelled();
        self.config
            .active_profile_mut()
            .remove_port_forward(resource_name, namespace, context, local_port);
        if let Err(e) = self.config.save() {
            self.error = Some(format!("Failed to save config: {}", e));
        }
        let new_display_len = self.pf_display_items().len();
        if new_display_len == 0 {
            self.pf.cursor = 0;
        } else {
            self.pf.cursor = self.pf.cursor.min(new_display_len.saturating_sub(1));
            self.snap_pf_cursor_to_entry();
        }
        self.push_status(format!("Cancelled port forward :{local_port}"));
    }

    fn begin_pf_port_edit(&mut self) {
        let Some(idx) = self.pf_entry_at_cursor() else {
            return;
        };
        let entry = &self.pf.manager.entries()[idx];
        self.popup = Some(Popup::PortForwardEditPort {
            pf_id: entry.id,
            old_port: entry.port.local,
            buf: entry.port.local.to_string(),
        });
    }

    fn toggle_selected_port_forward(&mut self) {
        let Some(idx) = self.pf_entry_at_cursor() else {
            return;
        };
        let entry = &self.pf.manager.entries()[idx];
        let id = entry.id;
        let local_port = entry.port.local;
        let resource_name = entry.identity.name.clone();
        let namespace = entry.namespace.clone();
        let context = entry.context.clone();
        if matches!(entry.status, portforward::PortForwardStatus::Paused) {
            let client = self.kube.client.streaming.clone();
            if !self.pf.manager.resume_spawn(id, client) {
                self.pf.manager.resume(id);
            }
            self.update_saved_pf_paused(&resource_name, &namespace, &context, local_port, false);
        } else if matches!(entry.status, portforward::PortForwardStatus::Error(_)) || entry.status.is_running() {
            self.pf.manager.pause(id);
            self.update_saved_pf_paused(&resource_name, &namespace, &context, local_port, true);
        }
    }

    fn request_switch_profile(&mut self, name: String) {
        if name == self.config.active_profile {
            return;
        }
        if !self.config.activate(&name) {
            self.error = Some(format!("Profile '{}' does not exist", name));
            return;
        }
        self.pf.manager.cancel_all();
        self.clear_cached_state();
        let profile_kubeconfig = self.config.active_profile().kubeconfig.clone();
        let context = self.config.active_profile().context.clone();
        let same_kubeconfig = profile_kubeconfig == self.kube.kubeconfig.path;
        self.enqueue_write(Write::SwitchProfile {
            name,
            kubeconfig: profile_kubeconfig,
            context,
            same_kubeconfig,
        });
    }

    fn ask_confirm(&mut self, title: &str, body: String, kind: ConfirmKind) {
        self.popup = Some(Popup::Confirm {
            title: title.to_string(),
            body,
            kind,
        });
    }

    fn accept_confirm(&mut self) {
        let Some(Popup::Confirm { kind, .. }) = self.popup.take() else {
            return;
        };
        match kind {
            | ConfirmKind::Delete { name, namespace, kind } => {
                self.enqueue_write(Write::Delete { kind, name, namespace });
            },
            | ConfirmKind::Drain { node_name } => {
                self.enqueue_write(Write::Drain { name: node_name });
            },
            | ConfirmKind::Restart { rt, name, namespace } => {
                self.enqueue_write(Write::Restart { rt, name, namespace });
            },
            | ConfirmKind::Cordon { name, cordon } => {
                self.enqueue_write(Write::Cordon { name, cordon });
            },
            | ConfirmKind::Scale {
                rt,
                name,
                namespace,
                replicas,
            } => {
                self.enqueue_write(Write::Scale {
                    rt,
                    name,
                    namespace,
                    replicas,
                });
            },
            | ConfirmKind::Trigger {
                namespace,
                cronjob,
                job_name,
            } => {
                self.enqueue_write(Write::Trigger {
                    namespace,
                    cronjob,
                    job_name,
                });
            },
            | ConfirmKind::CancelPortForward {
                id,
                resource_name,
                namespace,
                context,
                local_port,
            } => {
                self.cancel_port_forward(id, &resource_name, &namespace, &context, local_port);
            },
            | ConfirmKind::DeleteProfile { profile_name } => {
                self.config.profiles.remove(&profile_name);
                self.push_status(format!("Deleted profile '{}'", profile_name));
                let count = self.config.profiles.len();
                if self.profiles.cursor >= count && count > 0 {
                    self.profiles.cursor = count - 1;
                }
                if let Err(e) = self.config.save() {
                    self.error = Some(format!("Failed to save config: {}", e));
                }
            },
            | ConfirmKind::Quit => {
                self.should_quit = true;
            },
            | ConfirmKind::Create { yaml } => {
                let crds = self.discovered_crds.clone();
                self.enqueue_write(Write::Create { yaml, crds });
            },
            | ConfirmKind::PatchMetadata {
                resource,
                name,
                namespace,
                field,
                patch,
            } => {
                self.enqueue_write(Write::PatchMetadata {
                    resource,
                    name,
                    namespace,
                    field,
                    patch,
                });
            },
        }
    }

    fn run_action(&mut self, action: super::command::Action) {
        use super::command::Action;
        match action {
            | Action::Quit => self.open_confirm_quit(),
            | Action::Help => {
                self.help = Some(HelpState {
                    buf: String::new(),
                    cursor: 0,
                    scroll: 0,
                    context_only: true,
                });
            },
            | Action::Pause => self.toggle_pause(),
            | Action::SwitchCluster => self.open_context_selector(),
            | Action::SwitchNamespace => {
                self.pending_load = Some(PendingLoad::Namespaces);
            },
            | Action::OpenKubeconfig => self.open_kubeconfig_input(),
            | Action::Filter => {
                if self.view == View::Logs {
                    if let Some(state) = &mut self.log {
                        state.begin_filter_edit();
                    }
                } else {
                    self.begin_resource_filter();
                }
            },
            | Action::ClearFilter => {
                if self.view == View::Logs {
                    if let Some(state) = &mut self.log {
                        state.clear_filter();
                    }
                } else {
                    self.clear_resource_filter();
                }
            },
            | Action::Edit => self.begin_edit(),
            | Action::Logs => self.open_logs_for_selected(),
            | Action::PortForward => self.begin_port_forward(),
            | Action::Delete => self.open_delete_confirm(),
            | Action::Restart => self.confirm_restart(),
            | Action::Scale => self.begin_scale(),
            | Action::Copy => self.copy_resource_name(),
            | Action::CopyDetail => {
                if self.detail.dict.cursor.is_some() {
                    self.copy_dict_entry_to_clipboard();
                } else if self.is_secret_smart_view() {
                    self.copy_secret_to_clipboard();
                } else {
                    self.copy_yaml_to_clipboard();
                }
            },
            | Action::Diff => self.begin_diff(),
            | Action::Create => self.start_create_resource(),
            | Action::Exec => self.begin_exec(),
            | Action::Cordon => self.confirm_cordon(),
            | Action::Drain => self.confirm_drain(),
            | Action::Trigger => self.open_trigger_cronjob(),
            | Action::Star => {
                if self.view == View::Detail {
                    self.toggle_favorite_detail();
                } else {
                    self.toggle_favorite_selected();
                }
            },
            | Action::SaveProfile => self.open_profile_save(),
            | Action::LoadProfile => self.open_profile_load(),
            | Action::CycleView => {
                self.detail.mode = match self.detail.mode {
                    | DetailMode::Smart => DetailMode::Yaml,
                    | DetailMode::Yaml => DetailMode::Smart,
                };
                self.detail.scroll = 0;
            },
            | Action::Watch => {
                self.detail.auto_refresh = !self.detail.auto_refresh;
            },
            | Action::Labels | Action::Annotations | Action::Expand | Action::Related => {
                self.push_status("Use the key shown in Help for in-view selection");
            },
            | Action::Follow => {
                if let Some(state) = &mut self.log {
                    if state.following {
                        state.stop_following();
                    } else {
                        let client = self.kube.client.streaming.clone();
                        state.start_following(client);
                    }
                }
            },
            | Action::Wrap => {
                if let Some(state) = &mut self.log {
                    state.wrap = !state.wrap;
                }
            },
            | Action::TimeRange => {
                self.popup = Some(Popup::TimeFilter { buf: "30m".to_string() });
            },
            | Action::SelectPod => self.open_pod_selector(),
            | Action::SelectContainer => self.open_container_selector(),
            | Action::CopyLogs => self.copy_logs_to_clipboard(),
            | Action::ApplyEdit => self.confirm_apply_edit(),
            | Action::Reedit => {
                if let Some(ctx) = self.edit_ctx.take() {
                    self.pending_edit = Some(PendingEdit {
                        kind: ctx.kind,
                        name: ctx.name,
                        namespace: ctx.namespace,
                        yaml: ctx.edited_yaml,
                        original_yaml: Some(ctx.original_yaml),
                    });
                }
            },
            | Action::CancelEdit => {
                self.edit_ctx = None;
                if self.detail.name.is_empty() {
                    self.return_to_main();
                } else {
                    self.view = View::Detail;
                }
            },
            | Action::CycleDiff => {
                if let Some(ctx) = &mut self.edit_ctx {
                    ctx.diff_mode = match ctx.diff_mode {
                        | DiffMode::Inline => DiffMode::SideBySide,
                        | DiffMode::SideBySide => DiffMode::Inline,
                    };
                }
            },
            | Action::PfPause => self.toggle_selected_port_forward(),
            | Action::PfCancel => self.confirm_cancel_port_forward(),
            | Action::PfEditPort => self.begin_pf_port_edit(),
            | Action::SwitchProfile => {
                let names = self.sorted_profile_names();
                if let Some(name) = names.get(self.profiles.cursor) {
                    let name = name.clone();
                    self.request_switch_profile(name);
                }
            },
            | Action::CloneProfile | Action::CreateProfile | Action::DeleteProfile => {
                self.push_status("Use the Profiles view for profile create, clone, and delete");
            },
        }
    }

    // -----------------------------------------------------------------------
    // Help palette
    // -----------------------------------------------------------------------

    pub fn filtered_help_entries(&self) -> Vec<&'static super::command::Cmd> {
        let flags = self.cmd_flags();
        let h = match &self.help {
            | Some(h) => h,
            | None => return Vec::new(),
        };
        let entries = if h.context_only {
            super::command::help_entries_for_context(self.current_context(), &flags)
        } else {
            super::command::help_entries(&flags)
        };
        let query = h.buf.to_lowercase();
        if query.is_empty() {
            return entries;
        }
        entries
            .into_iter()
            .filter(|cmd| {
                let haystack = format!(
                    "{} {} {} {}",
                    cmd.key,
                    cmd.label,
                    cmd.description,
                    cmd.contexts.iter().map(|c| c.label()).collect::<Vec<_>>().join(" ")
                )
                .to_lowercase();
                query.split_whitespace().all(|word| haystack.contains(word))
            })
            .collect()
    }

    fn handle_help_key(&mut self, key: KeyEvent) {
        match key.code {
            | KeyCode::Esc => {
                self.help = None;
            },
            | KeyCode::Up | KeyCode::Char('k') => {
                if let Some(h) = &mut self.help {
                    if h.cursor > 0 {
                        h.cursor -= 1;
                    }
                }
            },
            | KeyCode::Down | KeyCode::Char('j') => {
                let count = self.filtered_help_entries().len();
                if let Some(h) = &mut self.help {
                    if count > 0 && h.cursor < count.saturating_sub(1) {
                        h.cursor += 1;
                    }
                }
            },
            | KeyCode::PageUp | KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                if let Some(h) = &mut self.help {
                    h.cursor = h.cursor.saturating_sub(10);
                }
            },
            | KeyCode::PageDown | KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                let count = self.filtered_help_entries().len();
                if let Some(h) = &mut self.help {
                    if count > 0 {
                        h.cursor = (h.cursor + 10).min(count.saturating_sub(1));
                    }
                }
            },
            | KeyCode::Tab => {
                if let Some(h) = &mut self.help {
                    h.context_only = !h.context_only;
                    h.cursor = 0;
                    h.scroll = 0;
                }
            },
            | KeyCode::Backspace => {
                if let Some(h) = &mut self.help {
                    h.buf.pop();
                    h.cursor = 0;
                    h.scroll = 0;
                }
            },
            | KeyCode::Char(c) => {
                if let Some(h) = &mut self.help {
                    h.buf.push(c);
                    h.cursor = 0;
                    h.scroll = 0;
                }
            },
            | _ => {},
        }
    }

    // -----------------------------------------------------------------------
    // Quit confirmation
    // -----------------------------------------------------------------------

    fn open_confirm_quit(&mut self) {
        let pf_count = self
            .pf
            .manager
            .entries()
            .iter()
            .filter(|e| e.status.is_running())
            .count();
        self.popup = Some(Popup::ConfirmQuit { pf_count });
    }

    fn handle_confirm_quit_key(&mut self, key: KeyEvent) {
        match key.code {
            | KeyCode::Enter | KeyCode::Char('y') => {
                self.popup = None;
                self.should_quit = true;
            },
            | KeyCode::Esc | KeyCode::Char('n') => {
                self.popup = None;
            },
            | _ => {},
        }
    }

    // -----------------------------------------------------------------------
    // Delete
    // -----------------------------------------------------------------------

    fn open_delete_confirm(&mut self) {
        let Some(kind) = self.selected_kind().cloned() else {
            return;
        };
        let visible = self.visible_resource_indices();
        let vis_pos = self
            .resources
            .state
            .selected()
            .and_then(|sel| visible.iter().position(|&i| i == sel))
            .unwrap_or(0);
        let entry = match visible.get(vis_pos).and_then(|&i| self.resources.entries.get(i)) {
            | Some(e) => e,
            | None => return,
        };
        self.popup = Some(Popup::ConfirmDelete {
            name: entry.name.clone(),
            namespace: entry.namespace.clone(),
            kind,
        });
    }

    fn open_delete_confirm_detail(&mut self) {
        let Some(kind) = self.selected_kind().cloned() else {
            return;
        };
        if self.detail.name.is_empty() {
            return;
        }
        self.popup = Some(Popup::ConfirmDelete {
            name: self.detail.name.clone(),
            namespace: self.detail.namespace.clone(),
            kind,
        });
    }

    fn handle_generic_confirm_key(&mut self, key: KeyEvent) {
        match key.code {
            | KeyCode::Enter | KeyCode::Char('y') => self.accept_confirm(),
            | KeyCode::Esc | KeyCode::Char('n') => {
                self.popup = None;
            },
            | _ => {},
        }
    }

    fn handle_confirm_delete_key(&mut self, key: KeyEvent) {
        match key.code {
            | KeyCode::Enter | KeyCode::Char('y') => {
                let (name, namespace, kind) = match &self.popup {
                    | Some(Popup::ConfirmDelete { name, namespace, kind }) => {
                        (name.clone(), namespace.clone(), kind.clone())
                    },
                    | _ => return,
                };
                self.popup = None;
                self.enqueue_write(Write::Delete { kind, name, namespace });
                self.push_status("Deleting…");
            },
            | KeyCode::Esc | KeyCode::Char('n') => {
                self.popup = None;
            },
            | _ => {},
        }
    }

    fn handle_confirm_drain_key(&mut self, key: KeyEvent) {
        match key.code {
            | KeyCode::Enter | KeyCode::Char('y') => {
                let node_name = match &self.popup {
                    | Some(Popup::ConfirmDrain { node_name }) => node_name.clone(),
                    | _ => return,
                };
                self.popup = None;
                self.enqueue_write(Write::Drain { name: node_name });
                self.push_status("Draining…");
            },
            | KeyCode::Esc | KeyCode::Char('n') => {
                self.popup = None;
            },
            | _ => {},
        }
    }

    fn open_scale_input_detail(&mut self) {
        let rt = match self.selected_resource_type() {
            | Some(rt) if rt.supports_scale() => rt,
            | _ => return,
        };
        if self.detail.name.is_empty() {
            return;
        }
        let current = self
            .detail
            .value
            .get("spec")
            .and_then(|s| s.get("replicas"))
            .and_then(|r| r.as_u64())
            .unwrap_or(1) as u32;
        self.popup = Some(Popup::ScaleInput {
            name: self.detail.name.clone(),
            namespace: self.detail.namespace.clone(),
            resource_type: rt,
            current,
            buf: current.to_string(),
        });
    }

    fn handle_scale_input_key(&mut self, key: KeyEvent) {
        match key.code {
            | KeyCode::Enter => {
                let (name, namespace, rt, replicas) = match &self.popup {
                    | Some(Popup::ScaleInput {
                        name,
                        namespace,
                        resource_type,
                        buf,
                        ..
                    }) => {
                        match buf.parse::<u32>() {
                            | Ok(r) => (name.clone(), namespace.clone(), *resource_type, r),
                            | Err(_) => {
                                self.error = Some("Invalid replica count".into());
                                self.popup = None;
                                return;
                            },
                        }
                    },
                    | _ => return,
                };
                self.popup = None;
                self.ask_confirm(
                    "Confirm Scale",
                    format!("Scale {}/{} to {} replicas?", rt.display_name(), name, replicas),
                    ConfirmKind::Scale {
                        rt,
                        name,
                        namespace,
                        replicas,
                    },
                );
            },
            | KeyCode::Esc => {
                self.popup = None;
            },
            | KeyCode::Backspace => {
                if let Some(Popup::ScaleInput { buf, .. }) = &mut self.popup {
                    buf.pop();
                }
            },
            | KeyCode::Char(c) if c.is_ascii_digit() => {
                if let Some(Popup::ScaleInput { buf, .. }) = &mut self.popup {
                    buf.push(c);
                }
            },
            | _ => {},
        }
    }

    // -----------------------------------------------------------------------
    // Exec
    // -----------------------------------------------------------------------

    // -----------------------------------------------------------------------
    // Kubeconfig
    // -----------------------------------------------------------------------

    fn handle_kubeconfig_input_key(&mut self, key: KeyEvent) {
        match key.code {
            | KeyCode::Enter => {
                let path = match &self.popup {
                    | Some(Popup::KubeconfigInput { buf }) => buf.clone(),
                    | _ => return,
                };
                self.popup = None;
                let expanded = if path.starts_with('~') {
                    if let Ok(home) = std::env::var("HOME") {
                        path.replacen('~', &home, 1)
                    } else {
                        path
                    }
                } else {
                    path
                };
                self.enqueue_write(Write::LoadKubeconfig { path: expanded });
                self.push_status("Loading kubeconfig…");
            },
            | KeyCode::Esc => {
                self.popup = None;
            },
            | KeyCode::Backspace => {
                if let Some(Popup::KubeconfigInput { buf }) = &mut self.popup {
                    buf.pop();
                }
            },
            | KeyCode::Char(c) => {
                if let Some(Popup::KubeconfigInput { buf }) = &mut self.popup {
                    buf.push(c);
                }
            },
            | _ => {},
        }
    }

    // -----------------------------------------------------------------------
    // Exec
    // -----------------------------------------------------------------------
    fn handle_exec_shell_key(&mut self, key: KeyEvent) {
        match key.code {
            | KeyCode::Enter => {
                let (pod_name, namespace, container, command, terminal) = match &self.popup {
                    | Some(Popup::ExecShell {
                        pod_name,
                        namespace,
                        containers,
                        container_cursor,
                        command_buf,
                        terminal_buf,
                        ..
                    }) => {
                        let container = containers.get(*container_cursor).cloned().unwrap_or_default();
                        let cmd = if command_buf.is_empty() {
                            "/bin/sh".to_string()
                        } else {
                            command_buf.clone()
                        };
                        (
                            pod_name.clone(),
                            namespace.clone(),
                            container,
                            cmd,
                            terminal_buf.clone(),
                        )
                    },
                    | _ => return,
                };
                self.popup = None;
                self.exec_terminal_override = Some(terminal);
                self.pending_exec = Some(PendingExec {
                    pod_name,
                    namespace,
                    container,
                    command: command.split_whitespace().map(String::from).collect(),
                });
            },
            | KeyCode::Esc => {
                self.popup = None;
            },
            | KeyCode::Up => {
                if let Some(Popup::ExecShell {
                    container_cursor,
                    editing_terminal,
                    ..
                }) = &mut self.popup
                {
                    if !*editing_terminal && *container_cursor > 0 {
                        *container_cursor -= 1;
                    }
                }
            },
            | KeyCode::Down => {
                if let Some(Popup::ExecShell {
                    containers,
                    container_cursor,
                    editing_terminal,
                    ..
                }) = &mut self.popup
                {
                    if !*editing_terminal && *container_cursor + 1 < containers.len() {
                        *container_cursor += 1;
                    }
                }
            },
            | KeyCode::Tab => {
                // Tab toggles between command/terminal editing and container selection
                if let Some(Popup::ExecShell { editing_terminal, .. }) = &mut self.popup {
                    *editing_terminal = !*editing_terminal;
                }
            },
            | KeyCode::Backspace => {
                if let Some(Popup::ExecShell {
                    command_buf,
                    terminal_buf,
                    editing_terminal,
                    ..
                }) = &mut self.popup
                {
                    if *editing_terminal {
                        terminal_buf.pop();
                    } else {
                        command_buf.pop();
                    }
                }
            },
            | KeyCode::Char(c) => {
                if let Some(Popup::ExecShell {
                    command_buf,
                    terminal_buf,
                    editing_terminal,
                    ..
                }) = &mut self.popup
                {
                    if *editing_terminal {
                        terminal_buf.push(c);
                    } else {
                        command_buf.push(c);
                    }
                }
            },
            | _ => {},
        }
    }

    // -----------------------------------------------------------------------
    // Edit flow
    // -----------------------------------------------------------------------

    /// Start edit from the resource list view: fetch YAML for the selected
    /// resource.
    fn start_edit_from_detail(&mut self) {
        let Some(kind) = self.selected_kind().cloned() else {
            return;
        };
        self.pending_edit = Some(PendingEdit {
            kind,
            name: self.detail.name.clone(),
            namespace: self.detail.namespace.clone(),
            yaml: self.detail.yaml.clone(),
            original_yaml: None,
        });
    }

    /// Called by the event loop after the editor exits. Computes the diff.
    pub fn handle_edit_result(&mut self, edit: PendingEdit, edited_yaml: String) {
        if edited_yaml.trim() == edit.yaml.trim() {
            // If this was a re-edit, restore the previous diff view
            if let Some(original_yaml) = edit.original_yaml {
                let diff_lines = compute_diff(&original_yaml, &edited_yaml);
                self.edit_ctx = Some(EditContext {
                    kind: edit.kind,
                    name: edit.name,
                    namespace: edit.namespace,
                    original_yaml,
                    edited_yaml,
                    diff_lines,
                    diff_mode: DiffMode::Inline,
                    scroll: 0,
                    error: None,
                    applyable: true,
                });
                self.push_status("No changes from re-edit");
            } else {
                self.push_status("No changes");
            }
            return;
        }

        let original_yaml = edit.original_yaml.unwrap_or(edit.yaml);
        let diff_lines = compute_diff(&original_yaml, &edited_yaml);

        self.edit_ctx = Some(EditContext {
            kind: edit.kind,
            name: edit.name,
            namespace: edit.namespace,
            original_yaml,
            edited_yaml,
            diff_lines,
            diff_mode: DiffMode::Inline,
            scroll: 0,
            error: None,
            applyable: true,
        });
        self.view = View::EditDiff;
    }

    /// Key handler for the diff preview view.
    pub fn handle_edit_diff_key(&mut self, key: KeyEvent) {
        match key.code {
            | KeyCode::Esc | KeyCode::Char('q') => {
                self.edit_ctx = None;
                // Return to previous view
                if self.detail.name.is_empty() {
                    self.return_to_main();
                } else {
                    self.view = View::Detail;
                }
            },
            | KeyCode::Char('v') => {
                if let Some(ctx) = &mut self.edit_ctx {
                    ctx.diff_mode = match ctx.diff_mode {
                        | DiffMode::Inline => DiffMode::SideBySide,
                        | DiffMode::SideBySide => DiffMode::Inline,
                    };
                    ctx.scroll = 0;
                }
            },
            | KeyCode::Enter => {
                self.confirm_apply_edit();
            },
            | KeyCode::Char('e') => {
                // Re-edit: reopen editor with the edited YAML
                if let Some(ctx) = self.edit_ctx.take() {
                    self.pending_edit = Some(PendingEdit {
                        kind: ctx.kind,
                        name: ctx.name,
                        namespace: ctx.namespace,
                        yaml: ctx.edited_yaml,
                        original_yaml: Some(ctx.original_yaml),
                    });
                }
            },
            | KeyCode::Up | KeyCode::Char('k') => {
                if let Some(ctx) = &mut self.edit_ctx {
                    ctx.scroll = ctx.scroll.saturating_sub(1);
                }
            },
            | KeyCode::Down | KeyCode::Char('j') => {
                if let Some(ctx) = &mut self.edit_ctx {
                    ctx.scroll = ctx.scroll.saturating_add(1);
                }
            },
            | KeyCode::PageUp | KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                if let Some(ctx) = &mut self.edit_ctx {
                    ctx.scroll = ctx.scroll.saturating_sub(20);
                }
            },
            | KeyCode::PageDown | KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                if let Some(ctx) = &mut self.edit_ctx {
                    ctx.scroll = ctx.scroll.saturating_add(20);
                }
            },
            | _ => {},
        }
    }

    fn handle_time_filter_key(&mut self, key: KeyEvent) {
        match key.code {
            | KeyCode::Enter => {
                let buf = match &self.popup {
                    | Some(Popup::TimeFilter { buf }) => buf.clone(),
                    | _ => return,
                };
                self.popup = None;
                let seconds = parse_duration_to_seconds(&buf);
                if seconds == 0 {
                    self.error = Some("Invalid duration. Use e.g. 30m, 2h, 1h30m".into());
                    return;
                }
                if let Some(state) = &mut self.log {
                    state.since_seconds = Some(seconds);
                }
                self.pending_load = Some(PendingLoad::ReloadLogs);
                self.push_status(format!("Log time filter: last {}", buf));
            },
            | KeyCode::Esc => {
                self.popup = None;
            },
            | KeyCode::Backspace => {
                if let Some(Popup::TimeFilter { buf, .. }) = &mut self.popup {
                    buf.pop();
                }
            },
            | KeyCode::Char(c) => {
                if let Some(Popup::TimeFilter { buf, .. }) = &mut self.popup {
                    buf.push(c);
                }
            },
            | _ => {},
        }
    }

    // -----------------------------------------------------------------------
    // Metadata edit popup (annotations / labels)
    // -----------------------------------------------------------------------

    fn open_metadata_edit(&mut self, kind: MetadataEditKind) {
        let Some(resource) = self.selected_kind().cloned() else {
            return;
        };
        let field = match kind {
            | MetadataEditKind::Labels => "labels",
            | MetadataEditKind::Annotations => "annotations",
        };
        let map = self
            .detail
            .value
            .get("metadata")
            .and_then(|m| m.get(field))
            .cloned()
            .unwrap_or(Value::Object(serde_json::Map::new()));
        let yaml = serde_yaml::to_string(&map).unwrap_or_else(|_| "{}\n".to_string());
        let header = format!(
            "# Edit {} for {}/{}\n# Save and close to apply. Empty keys are removed.\n#\n",
            field,
            resource.display_name(),
            self.detail.name
        );
        self.pending_metadata_edit = Some(PendingMetadataEdit {
            kind,
            resource,
            name: self.detail.name.clone(),
            namespace: self.detail.namespace.clone(),
            yaml: format!("{}{}", header, yaml),
        });
    }

    pub fn handle_metadata_edit_result(&mut self, edit: PendingMetadataEdit, edited_yaml: String) {
        // Strip comment lines
        let cleaned: String = edited_yaml
            .lines()
            .filter(|l| !l.starts_with('#'))
            .collect::<Vec<_>>()
            .join("\n");
        let new_map: serde_json::Map<String, Value> = match serde_yaml::from_str(&cleaned) {
            | Ok(m) => m,
            | Err(e) => {
                self.error = Some(format!("Invalid YAML: {}", e));
                return;
            },
        };

        // Build the original map for diffing
        let field = match edit.kind {
            | MetadataEditKind::Labels => "labels",
            | MetadataEditKind::Annotations => "annotations",
        };
        let original: serde_json::Map<String, Value> = self
            .detail
            .value
            .get("metadata")
            .and_then(|m| m.get(field))
            .and_then(|v| v.as_object())
            .cloned()
            .unwrap_or_default();

        // Compute patch: new keys, changed values, and removed keys (null)
        let mut patch = serde_json::Map::new();
        for (k, v) in &new_map {
            if original.get(k) != Some(v) {
                patch.insert(k.clone(), v.clone());
            }
        }
        for k in original.keys() {
            if !new_map.contains_key(k) {
                patch.insert(k.clone(), Value::Null);
            }
        }

        if patch.is_empty() {
            self.push_status("No changes");
            return;
        }

        self.ask_confirm(
            "Confirm Update",
            format!("Update {} on {}?", field, edit.name),
            ConfirmKind::PatchMetadata {
                resource: edit.resource,
                name: edit.name,
                namespace: edit.namespace,
                field,
                patch,
            },
        );
    }
}

// ---------------------------------------------------------------------------
// Diff computation
// ---------------------------------------------------------------------------

fn entry_index_from(display: &[DisplayItem], start: usize) -> Option<usize> {
    display
        .iter()
        .enumerate()
        .skip(start)
        .find_map(|(i, item)| matches!(item, DisplayItem::Entry(_)).then_some(i))
}

fn entry_index_before(display: &[DisplayItem], end: usize) -> Option<usize> {
    display
        .iter()
        .enumerate()
        .take(end)
        .rev()
        .find_map(|(i, item)| matches!(item, DisplayItem::Entry(_)).then_some(i))
}

fn compute_diff(original: &str, edited: &str) -> Vec<(DiffKind, String)> {
    use similar::{
        ChangeTag,
        TextDiff,
    };
    let diff = TextDiff::from_lines(original, edited);
    let mut lines = Vec::new();
    for change in diff.iter_all_changes() {
        let kind = match change.tag() {
            | ChangeTag::Equal => DiffKind::Context,
            | ChangeTag::Insert => DiffKind::Added,
            | ChangeTag::Delete => DiffKind::Removed,
        };
        let prefix = match kind {
            | DiffKind::Context => " ",
            | DiffKind::Added => "+",
            | DiffKind::Removed => "-",
        };
        lines.push((kind, format!("{} {}", prefix, change.value().trim_end())));
    }
    lines
}

// ---------------------------------------------------------------------------
// Duration parsing (e.g. "30m", "2h", "1h30m", "1d")
// ---------------------------------------------------------------------------

fn parse_duration_to_seconds(s: &str) -> i64 {
    let mut total: i64 = 0;
    let mut num_buf = String::new();
    for c in s.chars() {
        if c.is_ascii_digit() {
            num_buf.push(c);
        } else {
            let n: i64 = num_buf.parse().unwrap_or(0);
            num_buf.clear();
            match c {
                | 'h' | 'H' => total += n * 3600,
                | 'm' | 'M' => total += n * 60,
                | 's' | 'S' => total += n,
                | 'd' | 'D' => total += n * 86400,
                | _ => {},
            }
        }
    }
    // If only digits remain, treat as minutes when no unit was parsed yet
    if !num_buf.is_empty() {
        let n: i64 = num_buf.parse().unwrap_or(0);
        if total == 0 {
            total = n * 60;
        } else {
            total += n;
        }
    }
    total
}
