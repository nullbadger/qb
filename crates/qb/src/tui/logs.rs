use {
    crate::k8s::{
        PodInfo,
        ResourceType,
    },
    k8s_openapi::api::core::v1::Pod,
    kube::{
        api::Api,
        Client,
    },
    ratatui::style::Color,
    regex::Regex,
    std::{
        collections::HashMap,
        sync::mpsc,
        time::Instant,
    },
};

// ---------------------------------------------------------------------------
// Structured log line
// ---------------------------------------------------------------------------

pub struct LogLine {
    pub pod: String,
    pub container: String,
    pub timestamp: String,
    pub message: String,
}

impl LogLine {
    /// Parse a raw prefixed log line: `[pod/container] 2024-01-15T10:30:00Z
    /// message`
    pub fn parse(raw: &str) -> Self {
        // Extract [pod/container] prefix
        if let Some(bracket_end) = raw.find("] ") {
            let inner = &raw[1..bracket_end]; // "pod/container"
            let rest = &raw[bracket_end + 2..]; // "timestamp message"
            let (pod, container) = inner.split_once('/').unwrap_or((inner, ""));
            // Split timestamp from message
            let (timestamp, message) = if let Some(space) = rest.find(' ') {
                let ts = &rest[..space];
                // Only treat as timestamp if it looks like one (starts with digit)
                if ts.starts_with(|c: char| c.is_ascii_digit()) {
                    (ts.to_string(), rest[space + 1..].to_string())
                } else {
                    (String::new(), rest.to_string())
                }
            } else {
                // No space — entire rest might be a timestamp-only line
                if rest.starts_with(|c: char| c.is_ascii_digit()) {
                    (rest.to_string(), String::new())
                } else {
                    (String::new(), rest.to_string())
                }
            };
            Self {
                pod: pod.to_string(),
                container: container.to_string(),
                timestamp,
                message,
            }
        } else {
            // No prefix — treat as plain message
            Self {
                pod: String::new(),
                container: String::new(),
                timestamp: String::new(),
                message: raw.to_string(),
            }
        }
    }

    /// Prefix shown to user: `[pod/container] `
    pub fn prefix(&self) -> String {
        if self.pod.is_empty() {
            String::new()
        } else {
            format!("[{}/{}] ", self.pod, self.container)
        }
    }

    /// Display text without the K8s timestamp (prefix + message).
    pub fn display_text(&self) -> String {
        let prefix = self.prefix();
        format!("{}{}", prefix, self.message)
    }
}

// ---------------------------------------------------------------------------
// Pod color palette
// ---------------------------------------------------------------------------

/// Newest lines kept in the view. Older lines are dropped.
/// ponytail: fixed cap, raise it if a session needs a longer tail.
pub const MAX_LOG_LINES: usize = 2_000;

/// Bound per follow stream. A full channel drops the line instead of
/// growing without limit while the UI is busy or the machine is asleep.
const LOG_CHANNEL_BOUND: usize = 256;

const POD_COLORS: &[Color] = &[
    Color::Cyan,
    Color::Green,
    Color::Yellow,
    Color::Magenta,
    Color::Blue,
    Color::LightRed,
    Color::LightGreen,
    Color::LightCyan,
    Color::LightMagenta,
    Color::LightBlue,
    Color::LightYellow,
    Color::Red,
];

// ---------------------------------------------------------------------------
// Log source context (for auto-discovery of new pods)
// ---------------------------------------------------------------------------

pub struct LogSource {
    pub resource_type: ResourceType,
    pub resource_name: String,
}

// ---------------------------------------------------------------------------
// Log view state
// ---------------------------------------------------------------------------

/// Tracks which pod/container subset to show.
/// `None` means "all".
pub struct LogFilter {
    pub text: String,
    pub regex: Option<Regex>,
    pub editing: bool,
    pub buf: String,
}

pub struct LogSelection {
    pub pod: Option<usize>,       // None = all pods
    pub container: Option<usize>, // None = all containers (scoped to selected pod)
}

pub struct LogViewState {
    pub lines: Vec<LogLine>,
    pub filter: LogFilter,

    // Follow
    pub following: bool,
    pub receivers: Vec<mpsc::Receiver<String>>,
    pub stream_handles: Vec<tokio::task::JoinHandle<()>>,
    /// Set of (pod, container) pairs that have active stream tasks.
    pub streaming_pairs: std::collections::HashSet<(String, String)>,

    // Scroll & selection. `selected_line` and `scroll` are indexes into the
    // filtered view. The viewport always contains the cursor.
    pub scroll: usize,
    pub auto_scroll: bool,
    pub selected_line: Option<usize>,
    pub selection_anchor: Option<usize>,
    pub wrap: bool,
    pub viewport_rows: usize,
    pub viewport_cols: usize,

    // Pod / container
    pub pods: Vec<PodInfo>,
    pub selection: LogSelection,
    pub namespace: String,

    // Pod color assignments
    pub pod_colors: HashMap<String, Color>,
    pod_color_next: usize,

    // Time-based filter (only fetch logs from last N seconds)
    pub since_seconds: Option<i64>,

    // Auto-discovery of new pods during follow mode
    pub source: Option<LogSource>,
    pub last_pod_check: Instant,
}

impl LogViewState {
    pub fn new(pods: Vec<PodInfo>, namespace: String, initial_lines: Vec<LogLine>) -> Self {
        let mut state = Self {
            lines: Vec::new(),
            filter: LogFilter {
                text: String::new(),
                regex: None,
                editing: false,
                buf: String::new(),
            },
            following: false,
            receivers: Vec::new(),
            stream_handles: Vec::new(),
            streaming_pairs: std::collections::HashSet::new(),
            scroll: 0,
            auto_scroll: true,
            selected_line: None,
            selection_anchor: None,
            wrap: false,
            viewport_rows: 20,
            viewport_cols: 80,
            pods,
            selection: LogSelection {
                pod: None,
                container: None,
            },
            namespace,
            pod_colors: HashMap::new(),
            pod_color_next: 0,
            since_seconds: None,
            source: None,
            last_pod_check: Instant::now(),
        };
        // Register colors for initial lines
        for line in &initial_lines {
            state.ensure_pod_color(&line.pod);
        }
        state.lines = initial_lines;
        state.trim();
        state
    }

    /// Drop oldest lines past [`MAX_LOG_LINES`] and shift the cursor by the
    /// same number of visible lines, so a follow trim does not select a
    /// different row.
    pub fn trim(&mut self) {
        if self.lines.len() <= MAX_LOG_LINES {
            return;
        }
        let drop_n = self.lines.len() - MAX_LOG_LINES;
        let visible_dropped = self.lines[..drop_n]
            .iter()
            .filter(|line| self.line_matches(line))
            .count();
        trim_log_lines(&mut self.lines, MAX_LOG_LINES);
        self.note_dropped_visible(visible_dropped);
    }

    fn note_dropped_visible(&mut self, dropped: usize) {
        if dropped == 0 {
            return;
        }
        self.scroll = self.scroll.saturating_sub(dropped);
        if let Some(sel) = self.selected_line {
            self.selected_line = Some(sel.saturating_sub(dropped));
        }
        if let Some(anchor) = self.selection_anchor {
            self.selection_anchor = Some(anchor.saturating_sub(dropped));
        }
    }

    pub fn display_rows(&self, line: &LogLine) -> usize {
        row_height(
            self.wrap,
            line.display_text().chars().count(),
            self.viewport_cols.max(1),
        )
    }

    fn line_matches(&self, line: &LogLine) -> bool {
        if self.filter.text.is_empty() {
            return true;
        }
        let text = line.display_text();
        if let Some(re) = &self.filter.regex {
            re.is_match(&text)
        } else {
            text.contains(&self.filter.text)
        }
    }

    // -----------------------------------------------------------------------
    // Pod color management
    // -----------------------------------------------------------------------

    pub fn ensure_pod_color(&mut self, pod: &str) {
        if !pod.is_empty() && !self.pod_colors.contains_key(pod) {
            let color = POD_COLORS[self.pod_color_next % POD_COLORS.len()];
            self.pod_colors.insert(pod.to_string(), color);
            self.pod_color_next += 1;
        }
    }

    pub fn color_for_pod(&self, pod: &str) -> Color {
        self.pod_colors.get(pod).copied().unwrap_or(Color::DarkGray)
    }

    // -----------------------------------------------------------------------
    // Selection labels for the hotkey bar
    // -----------------------------------------------------------------------

    pub fn pod_label(&self) -> String {
        match self.selection.pod {
            | None => format!("All ({})", self.pods.len()),
            | Some(i) => self.pods.get(i).map(|p| p.name.clone()).unwrap_or_else(|| "?".into()),
        }
    }

    pub fn container_label(&self) -> String {
        match self.selection.container {
            | None => {
                let count = self.active_containers().len();
                format!("All ({})", count)
            },
            | Some(i) => self.active_containers().get(i).cloned().unwrap_or_else(|| "?".into()),
        }
    }

    /// Containers available given the current pod selection.
    pub fn active_containers(&self) -> Vec<String> {
        match self.selection.pod {
            | Some(i) => self.pods.get(i).map(|p| p.containers.clone()).unwrap_or_default(),
            | None => {
                let mut all: Vec<String> = self.pods.iter().flat_map(|p| p.containers.clone()).collect();
                all.sort();
                all.dedup();
                all
            },
        }
    }

    /// Returns the list of (pod_name, container_name) pairs to fetch logs from.
    pub fn active_streams(&self) -> Vec<(String, String)> {
        let mut pairs = Vec::new();
        let pod_iter: Vec<&PodInfo> = match self.selection.pod {
            | Some(i) => self.pods.get(i).into_iter().collect(),
            | None => self.pods.iter().collect(),
        };
        for pod in pod_iter {
            let containers: Vec<&String> = match self.selection.container {
                | Some(ci) => {
                    let active = self.active_containers();
                    let name = active.get(ci);
                    pod.containers
                        .iter()
                        .filter(|c| name.map(|n| n == *c).unwrap_or(false))
                        .collect()
                },
                | None => pod.containers.iter().collect(),
            };
            for c in containers {
                pairs.push((pod.name.clone(), c.clone()));
            }
        }
        pairs
    }

    // -----------------------------------------------------------------------
    // Visible (filtered) lines
    // -----------------------------------------------------------------------

    pub fn visible_lines(&self) -> Vec<&LogLine> {
        if self.filter.text.is_empty() {
            self.lines.iter().collect()
        } else if let Some(re) = &self.filter.regex {
            self.lines.iter().filter(|l| re.is_match(&l.display_text())).collect()
        } else {
            self.lines
                .iter()
                .filter(|l| l.display_text().contains(&self.filter.text))
                .collect()
        }
    }

    // -----------------------------------------------------------------------
    // Follow mode — streams ALL active pod/container pairs
    // -----------------------------------------------------------------------

    pub fn start_following(&mut self, client: Client) {
        self.stop_following();

        let pairs = self.active_streams();
        let ns = self.namespace.clone();

        for (pod, container) in pairs {
            self.start_stream_for(client.clone(), &ns, pod, container);
        }
        self.following = true;
        self.auto_scroll = true;
    }

    /// Start a single stream task for a pod/container pair (if not already
    /// streaming).
    fn start_stream_for(&mut self, client: Client, ns: &str, pod: String, container: String) {
        let key = (pod.clone(), container.clone());
        if self.streaming_pairs.contains(&key) {
            return;
        }
        self.streaming_pairs.insert(key);
        self.ensure_pod_color(&pod);

        let (tx, rx) = mpsc::sync_channel(LOG_CHANNEL_BOUND);
        let ns = ns.to_string();
        let pod_tag = pod.clone();
        let container_tag = container.clone();
        let handle = tokio::spawn(async move {
            log_stream_task(client, ns, pod, container, pod_tag, container_tag, tx).await;
        });
        self.receivers.push(rx);
        self.stream_handles.push(handle);
    }

    pub fn stop_following(&mut self) {
        for handle in self.stream_handles.drain(..) {
            handle.abort();
        }
        self.receivers.clear();
        self.streaming_pairs.clear();
        self.following = false;
    }

    pub fn poll_stream(&mut self) {
        let mut new_lines: Vec<LogLine> = Vec::new();
        for rx in &self.receivers {
            while let Ok(raw) = rx.try_recv() {
                new_lines.push(LogLine::parse(&raw));
            }
        }
        // Register colors for any new pods
        for line in &new_lines {
            if !line.pod.is_empty() && !self.pod_colors.contains_key(&line.pod) {
                let color = POD_COLORS[self.pod_color_next % POD_COLORS.len()];
                self.pod_colors.insert(line.pod.clone(), color);
                self.pod_color_next += 1;
            }
        }
        if new_lines.is_empty() {
            return;
        }
        // Sort new batch by timestamp
        new_lines.sort_by(|a, b| a.timestamp.cmp(&b.timestamp));
        // Merge into existing sorted lines: find insertion point via binary search
        let first_ts = &new_lines[0].timestamp;
        let insert_at = if self.lines.is_empty()
            || self
                .lines
                .last()
                .map(|l| l.timestamp.as_str() <= first_ts.as_str())
                .unwrap_or(true)
        {
            // Common case: all new lines are newer — just append
            self.lines.len()
        } else {
            // Find where the first new timestamp belongs
            self.lines
                .partition_point(|l| l.timestamp.as_str() <= first_ts.as_str())
        };
        if insert_at == self.lines.len() {
            self.lines.extend(new_lines);
        } else {
            // Merge: split existing at insertion point, merge new batch with tail,
            // reassemble
            let tail: Vec<LogLine> = self.lines.drain(insert_at..).collect();
            let mut merged = Vec::with_capacity(new_lines.len() + tail.len());
            let mut ni = 0;
            let mut ti = 0;
            while ni < new_lines.len() && ti < tail.len() {
                if new_lines[ni].timestamp <= tail[ti].timestamp {
                    // Need to remove from new_lines; use swap_remove-like approach
                    // Actually, let's just use indices and push references — but we own both vecs
                    // We'll drain new_lines later; for now build order indices
                    merged.push(true); // true = from new_lines[ni]
                    ni += 1;
                } else {
                    merged.push(false); // false = from tail[ti]
                    ti += 1;
                }
            }
            // Rebuild: collect remaining
            let mut result = Vec::with_capacity(merged.len() + (new_lines.len() - ni) + (tail.len() - ti));
            let mut new_iter = new_lines.into_iter();
            let mut tail_iter = tail.into_iter();
            for from_new in merged {
                if from_new {
                    if let Some(l) = new_iter.next() {
                        result.push(l);
                    }
                } else if let Some(l) = tail_iter.next() {
                    result.push(l);
                }
            }
            result.extend(new_iter);
            result.extend(tail_iter);
            self.lines.extend(result);
        }
        self.trim();
    }

    // -----------------------------------------------------------------------
    // Auto-discovery of new pods (for rolling updates)
    // -----------------------------------------------------------------------

    /// Check if new pods have appeared and start streams for them.
    /// Returns true if new pods were found (caller should re-discover).
    pub fn needs_pod_check(&self) -> bool {
        self.following && self.source.is_some() && self.last_pod_check.elapsed() >= std::time::Duration::from_secs(5)
    }

    /// Integrate newly discovered pods: update pod list, start streams for new
    /// ones.
    pub fn integrate_new_pods(&mut self, new_pods: Vec<PodInfo>, client: Client) {
        self.last_pod_check = Instant::now();

        let existing_names: std::collections::HashSet<String> = self.pods.iter().map(|p| p.name.clone()).collect();
        let mut added = false;

        // Collect new (pod, container) pairs to stream
        let mut to_start: Vec<(String, String)> = Vec::new();
        let active_container = self
            .selection
            .container
            .and_then(|ci| self.active_containers().get(ci).cloned());

        for pod in &new_pods {
            if !existing_names.contains(&pod.name) {
                added = true;
                self.ensure_pod_color(&pod.name);
                // Only start streams if in "all pods" mode
                if self.selection.pod.is_none() {
                    let containers: Vec<&String> = match &active_container {
                        | Some(name) => pod.containers.iter().filter(|c| *c == name).collect(),
                        | None => pod.containers.iter().collect(),
                    };
                    for c in containers {
                        to_start.push((pod.name.clone(), c.clone()));
                    }
                }
            }
        }

        // Start streams for new pods
        let ns = self.namespace.clone();
        for (pod, container) in to_start {
            self.start_stream_for(client.clone(), &ns, pod, container);
        }

        if added {
            self.pods = new_pods;
        }
    }

    // -----------------------------------------------------------------------
    // Filter
    // -----------------------------------------------------------------------

    pub fn begin_filter_edit(&mut self) {
        self.filter.editing = true;
        self.filter.buf = self.filter.text.clone();
    }

    pub fn apply_filter(&mut self) {
        self.filter.text = self.filter.buf.clone();
        self.filter.regex = if self.filter.text.is_empty() {
            None
        } else {
            Regex::new(&self.filter.text).ok()
        };
        self.filter.editing = false;
        self.reset_navigation();
    }

    pub fn cancel_filter_edit(&mut self) {
        self.filter.editing = false;
        self.filter.buf = self.filter.text.clone();
    }

    pub fn clear_filter(&mut self) {
        self.filter.text.clear();
        self.filter.regex = None;
        self.filter.buf.clear();
        self.reset_navigation();
    }

    // -----------------------------------------------------------------------
    // Navigation
    // -----------------------------------------------------------------------

    fn reset_navigation(&mut self) {
        self.scroll = 0;
        self.selected_line = None;
        self.selection_anchor = None;
        self.auto_scroll = false;
    }

    pub fn visible_len(&self) -> usize {
        self.visible_lines().len()
    }

    /// Move the cursor by `delta` visible lines. The viewport follows.
    /// Moving down while already on the tail does not unpin follow.
    pub fn move_cursor(&mut self, delta: isize, extend: bool) {
        let n = self.visible_len();
        if n == 0 {
            self.reset_navigation();
            return;
        }
        let current = self.selected_line.unwrap_or_else(|| self.scroll.min(n - 1));
        if extend {
            if self.selection_anchor.is_none() {
                self.selection_anchor = Some(current);
            }
        } else {
            self.selection_anchor = None;
        }
        let next = if delta < 0 {
            current.saturating_sub(delta.unsigned_abs())
        } else {
            (current + delta as usize).min(n - 1)
        };
        if next != current {
            self.auto_scroll = false;
        }
        self.selected_line = Some(next);
        if !self.auto_scroll {
            self.scroll = self.reveal_scroll(next);
        }
    }

    pub fn page(&mut self, direction: isize) {
        let step = self.viewport_rows.max(1) as isize;
        self.move_cursor(direction * step, false);
    }

    pub fn jump(&mut self, to_end: bool) {
        let n = self.visible_len();
        if n == 0 {
            self.reset_navigation();
            return;
        }
        self.selection_anchor = None;
        if to_end {
            self.selected_line = Some(n - 1);
            self.auto_scroll = self.following;
            self.scroll = self.reveal_scroll(n - 1);
        } else {
            self.auto_scroll = false;
            self.selected_line = Some(0);
            self.scroll = 0;
        }
    }

    /// Pin the cursor to the last visible line and scroll so that line is in
    /// view.
    pub fn stick_to_tail(&mut self) {
        let n = self.visible_len();
        if n == 0 {
            self.scroll = 0;
            self.selected_line = None;
            return;
        }
        self.selection_anchor = None;
        self.selected_line = Some(n - 1);
        self.scroll = self.reveal_scroll(n - 1);
    }

    pub fn clamp_navigation(&mut self) {
        let n = self.visible_len();
        if n == 0 {
            self.reset_navigation();
            return;
        }
        if let Some(sel) = self.selected_line {
            self.selected_line = Some(sel.min(n - 1));
        }
        if let Some(anchor) = self.selection_anchor {
            self.selection_anchor = Some(anchor.min(n - 1));
        }
        self.scroll = self.scroll.min(n - 1);
        if let Some(sel) = self.selected_line {
            self.scroll = self.reveal_scroll(sel);
        }
    }

    fn reveal_scroll(&self, cursor: usize) -> usize {
        let lines = self.visible_lines();
        let n = lines.len();
        if n == 0 || cursor >= n {
            return 0;
        }
        let height = self.viewport_rows.max(1);
        if !self.wrap {
            if cursor < self.scroll {
                return cursor;
            }
            if cursor >= self.scroll.saturating_add(height) {
                return cursor + 1 - height;
            }
            return self.scroll.min(n - 1);
        }
        if cursor < self.scroll {
            return cursor;
        }
        let cursor_h = self.display_rows(lines[cursor]);
        if cursor_h >= height {
            return cursor;
        }
        let mut used = cursor_h;
        let mut scroll = cursor;
        while scroll > 0 {
            let h = self.display_rows(lines[scroll - 1]);
            if used + h > height {
                break;
            }
            used += h;
            scroll -= 1;
        }
        if self.scroll <= cursor && self.scroll >= scroll {
            let span: usize = (self.scroll..=cursor).map(|i| self.display_rows(lines[i])).sum();
            if span <= height {
                return self.scroll;
            }
        }
        scroll
    }

    /// Returns (start, end) inclusive range if a multi-line selection exists.
    pub fn selection_range(&self) -> Option<(usize, usize)> {
        let anchor = self.selection_anchor?;
        let cursor = self.selected_line?;
        Some((anchor.min(cursor), anchor.max(cursor)))
    }
}

impl Drop for LogViewState {
    fn drop(&mut self) {
        for handle in self.stream_handles.drain(..) {
            handle.abort();
        }
    }
}

// ---------------------------------------------------------------------------
// Async log streaming task — one per pod/container pair
// ---------------------------------------------------------------------------

fn row_height(wrap: bool, text_len: usize, width: usize) -> usize {
    if !wrap || width == 0 || text_len == 0 {
        1
    } else {
        text_len.div_ceil(width).max(1)
    }
}

fn trim_log_lines(lines: &mut Vec<LogLine>, max: usize) {
    if lines.len() > max {
        let drop_n = lines.len() - max;
        lines.drain(0..drop_n);
    }
}

async fn log_stream_task(
    client: Client,
    ns: String,
    pod: String,
    container: String,
    pod_tag: String,
    container_tag: String,
    tx: mpsc::SyncSender<String>,
) {
    use futures::{
        AsyncBufReadExt,
        StreamExt,
    };

    let api: Api<Pod> = Api::namespaced(client, &ns);
    let lp = kube::api::LogParams {
        follow: true,
        container: Some(container),
        tail_lines: Some(0),
        timestamps: true,
        ..Default::default()
    };

    let stream = match api.log_stream(&pod, &lp).await {
        | Ok(s) => s,
        | Err(_) => return,
    };

    let prefix = format!("[{}/{}] ", pod_tag, container_tag);
    let mut lines = stream.lines();
    while let Some(result) = lines.next().await {
        match result {
            | Ok(line) => {
                // ponytail: drop when the UI is behind. A blocking send would
                // pin a runtime worker and grow memory across sleep.
                match tx.try_send(format!("{}{}", prefix, line)) {
                    | Ok(()) => {},
                    | Err(mpsc::TrySendError::Disconnected(_)) => return,
                    | Err(mpsc::TrySendError::Full(_)) => {},
                }
            },
            | Err(_) => return,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        trim_log_lines,
        LogLine,
        LogViewState,
    };

    fn view_with(n: usize) -> LogViewState {
        let lines = (0..n)
            .map(|i| {
                LogLine {
                    pod: String::new(),
                    container: String::new(),
                    timestamp: String::new(),
                    message: i.to_string(),
                }
            })
            .collect();
        let mut state = LogViewState::new(Vec::new(), "ns".into(), lines);
        state.viewport_rows = 4;
        state.viewport_cols = 80;
        state.auto_scroll = false;
        state
    }

    #[test]
    fn moving_up_from_the_tail_stays_on_that_line() {
        let mut state = view_with(10);
        state.auto_scroll = true;
        state.stick_to_tail();
        assert_eq!(state.selected_line, Some(9));
        state.move_cursor(-1, false);
        assert_eq!(state.selected_line, Some(8));
        assert!(!state.auto_scroll);
        assert!(state.scroll <= 8);
        assert!(state.scroll + state.viewport_rows > 8);
    }

    #[test]
    fn page_moves_the_cursor_with_the_viewport() {
        let mut state = view_with(20);
        state.viewport_rows = 5;
        state.selected_line = Some(0);
        state.scroll = 0;
        state.page(1);
        assert_eq!(state.selected_line, Some(5));
        assert!(state.scroll <= 5);
        assert!(state.scroll + state.viewport_rows > 5);
    }

    #[test]
    fn trim_keeps_the_same_logical_line_selected() {
        let mut state = view_with(6);
        state.selected_line = Some(4);
        state.scroll = 2;
        state.note_dropped_visible(2);
        assert_eq!(state.selected_line, Some(2));
        assert_eq!(state.scroll, 0);
    }

    #[test]
    fn filter_does_not_keep_a_stale_cursor() {
        let mut state = view_with(5);
        state.selected_line = Some(3);
        state.filter.buf = "1".into();
        state.apply_filter();
        assert_eq!(state.selected_line, None);
        assert_eq!(state.scroll, 0);
    }

    #[test]
    fn log_buffer_keeps_the_newest_lines() {
        let mut lines: Vec<LogLine> = (0..5)
            .map(|i| {
                LogLine {
                    pod: String::new(),
                    container: String::new(),
                    timestamp: String::new(),
                    message: i.to_string(),
                }
            })
            .collect();
        trim_log_lines(&mut lines, 2);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].message, "3");
        assert_eq!(lines[1].message, "4");
    }
}
