use {
    super::{
        app::{
            App,
            DetailMode,
            Focus,
            NavItemKind,
            Panel,
            Popup,
            View,
            ALL_NAMESPACES_LABEL,
        },
        smart,
    },
    crate::{
        k8s::{
            Category,
            FavoriteType,
            ResourceType,
        },
        portforward::PortForwardStatus,
    },
    ratatui::{
        prelude::*,
        widgets::*,
    },
};

// ---------------------------------------------------------------------------
// Top-level render dispatch
// ---------------------------------------------------------------------------

pub fn render(f: &mut Frame, app: &mut App) {
    match app.view {
        | View::Main => render_main(f, app),
        | View::Detail => render_detail(f, app),
        | View::Logs => render_logs(f, app),
        | View::EditDiff => render_edit_diff(f, app),
    }

    if app.popup.is_some() {
        render_popup(f, app);
    }

    if app.palette.is_some() {
        render_palette(f, app);
    }

    if app.help.is_some() {
        render_help(f, app);
    }
}

// ---------------------------------------------------------------------------
// Breadcrumb bar
// ---------------------------------------------------------------------------

fn render_breadcrumb(f: &mut Frame, app: &App, area: Rect) {
    let dim = Style::default().fg(Color::DarkGray).bg(Color::DarkGray);
    let seg = Style::default().fg(Color::White).bg(Color::DarkGray);
    let active = Style::default()
        .fg(Color::Cyan)
        .bg(Color::DarkGray)
        .add_modifier(Modifier::BOLD);
    let sep = Span::styled(" › ", dim);

    let mut spans: Vec<Span> = vec![Span::styled(" ", dim)];

    let ctx = app.kube.context.name.as_str().to_string();
    let is_top = app.view == View::Main && app.selected_resource_type().is_none();
    spans.push(Span::styled(ctx, if is_top { active } else { seg }));

    // Namespace
    spans.push(sep.clone());
    let ns = app
        .kube
        .context
        .namespace
        .as_deref()
        .unwrap_or("All Namespaces")
        .to_string();
    spans.push(Span::styled(ns, seg));

    // Resource type
    let type_label = app
        .selected_resource_type()
        .map(|rt| rt.display_name().to_string())
        .or_else(|| app.selected_crd_info().map(|c| c.display_name.clone()));
    if let Some(type_name) = type_label {
        spans.push(sep.clone());
        let is_last = app.view == View::Main;
        spans.push(Span::styled(type_name, if is_last { active } else { seg }));

        // Resource name (detail/logs)
        if app.view == View::Detail || app.view == View::Logs {
            let name = app
                .resources
                .state
                .selected()
                .and_then(|idx| app.resources.entries.get(idx))
                .map(|e| e.name.clone())
                .unwrap_or_else(|| "?".into());
            spans.push(sep.clone());
            let is_detail = app.view == View::Detail;
            // Star indicator for favorites
            if let Some(rt) = app.selected_resource_type() {
                if app.is_favorite(rt, &app.detail.name, &app.detail.namespace) {
                    spans.push(Span::styled(
                        "★ ",
                        Style::default().fg(Color::Yellow).bg(Color::DarkGray),
                    ));
                }
            }
            spans.push(Span::styled(name, if is_detail { active } else { seg }));
        }

        if app.view == View::Logs {
            spans.push(sep.clone());
            spans.push(Span::styled("logs", active));
        }
    }

    if app.view == View::EditDiff {
        spans.push(sep.clone());
        spans.push(Span::styled("edit", active));
    }

    // Badges (filter, error, status)
    if !app.resources.filter.text.is_empty() && app.view == View::Main {
        spans.push(Span::styled("  ", dim));
        spans.push(Span::styled(
            format!(" /{} ", app.resources.filter.text),
            Style::default().fg(Color::Black).bg(Color::Yellow),
        ));
    }

    if let Some(err) = &app.error {
        spans.push(Span::styled("  ", dim));
        let clean = crate::k8s::KubeClient::display_error(err);
        let truncated: String = clean.chars().take(60).collect();
        spans.push(Span::styled(
            format!(" {} ", truncated),
            Style::default().fg(Color::White).bg(Color::Red),
        ));
    }

    // Right side: last update time — always visible
    let elapsed = app.last_refresh.elapsed().as_secs();
    let right_text = if app.paused {
        " ⏸ paused ".to_string()
    } else if app.is_busy() {
        " ⟳ loading ".to_string()
    } else if elapsed < 2 {
        " ⟳ just now ".to_string()
    } else if elapsed < 60 {
        format!(" ⟳ {}s ago ", elapsed)
    } else {
        format!(" ⟳ {}m{}s ago ", elapsed / 60, elapsed % 60)
    };

    let left_w: usize = spans.iter().map(|s| s.content.chars().count()).sum();
    let area_w = area.width as usize;
    let pad = area_w.saturating_sub(left_w + right_text.len());
    spans.push(Span::styled(" ".repeat(pad), dim));

    let right_style = if app.paused {
        Style::default().fg(Color::Yellow).bg(Color::DarkGray)
    } else {
        Style::default().fg(Color::White).bg(Color::DarkGray)
    };
    spans.push(Span::styled(right_text, right_style));

    f.render_widget(
        Paragraph::new(Line::from(spans)).style(Style::default().bg(Color::DarkGray)),
        area,
    );
}

// ---------------------------------------------------------------------------
// Main view
// ---------------------------------------------------------------------------

fn render_main(f: &mut Frame, app: &mut App) {
    let has_filter_bar = app.resources.filter.editing;

    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),                                  // breadcrumb
            Constraint::Min(3),                                     // main area
            Constraint::Length(if has_filter_bar { 1 } else { 0 }), // filter bar (editing only)
            Constraint::Length(1),                                  // hotkey bar
        ])
        .split(f.area());

    render_breadcrumb(f, app, outer[0]);

    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(app.nav_width), Constraint::Min(40)])
        .split(outer[1]);

    render_nav(f, app, cols[0]);
    render_resources(f, app, cols[1]);

    if has_filter_bar {
        render_resource_filter_bar(f, app, outer[2]);
    }

    let bar = build_hotkey_bar(app);
    f.render_widget(Paragraph::new(bar), outer[3]);
}

// ---------------------------------------------------------------------------
// Navigation sidebar
// ---------------------------------------------------------------------------

fn render_nav(f: &mut Frame, app: &mut App, area: Rect) {
    let items: Vec<ListItem> = app
        .nav
        .items
        .iter()
        .map(|item| {
            let is_active = match (&item.kind, &app.panel) {
                | (NavItemKind::ClusterStats, Panel::Overview) => true,
                | (NavItemKind::ResourceMap, Panel::ResourceMap) => true,
                | (NavItemKind::Favorites, Panel::Favorites) => true,
                | (NavItemKind::PortForwards, Panel::PortForwards) => true,
                | (NavItemKind::Profiles, Panel::Profiles) => true,
                | (NavItemKind::Resource(kind), Panel::Resources(panel_kind)) => kind == panel_kind,
                | _ => false,
            };
            let style = match &item.kind {
                | NavItemKind::Category => Style::default().fg(Color::DarkGray).add_modifier(Modifier::BOLD),
                | NavItemKind::SubCategory => Style::default().fg(Color::DarkGray),
                | NavItemKind::Favorites if is_active => Style::default().fg(Color::Yellow),
                | _ if is_active => Style::default().fg(Color::Green),
                | _ => Style::default().fg(Color::White),
            };
            // Append resource count badge if available
            let label = if let NavItemKind::Resource(kind) = &item.kind {
                if let Some(&count) = app.resources.counts.get(kind) {
                    format!("{} ({})", item.label, count)
                } else {
                    item.label.clone()
                }
            } else if matches!(item.kind, NavItemKind::Favorites) {
                let fav_count = app.config.active_profile().favorites.len();
                if fav_count > 0 {
                    format!("{} ({})", item.label, fav_count)
                } else {
                    item.label.clone()
                }
            } else {
                item.label.clone()
            };
            ListItem::new(label).style(style)
        })
        .collect();

    let focused = app.focus == Focus::Nav;
    let border_color = if focused { Color::Cyan } else { Color::DarkGray };

    // Show active port forward count in nav title
    let pf_count = app
        .pf
        .manager
        .entries()
        .iter()
        .filter(|e| e.status.is_running())
        .count();
    let title = if pf_count > 0 {
        format!(" Resources (PF:{}) ", pf_count)
    } else {
        " Resources ".to_string()
    };

    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(border_color))
                .title(title),
        )
        .highlight_style(Style::default().add_modifier(Modifier::REVERSED).fg(Color::Cyan))
        .highlight_symbol("▶ ")
        .scroll_padding(1);

    f.render_stateful_widget(list, area, &mut app.nav.state);
}

// ---------------------------------------------------------------------------
// Resource table
// ---------------------------------------------------------------------------

fn render_resources(f: &mut Frame, app: &mut App, area: Rect) {
    match &app.panel {
        | Panel::Favorites => {
            render_favorites(f, app, area);
            return;
        },
        | Panel::Profiles => {
            render_profiles(f, app, area);
            return;
        },
        | Panel::PortForwards => {
            render_port_forwards(f, app, area);
            return;
        },
        | Panel::Overview => {
            render_cluster_stats(f, app, area);
            return;
        },
        | Panel::ResourceMap => {
            render_resource_map(f, app, area);
            return;
        },
        | Panel::Resources(_) => {},
    }

    let Some(kind) = app.selected_kind() else {
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::DarkGray))
            .title(" Select a resource type ");
        f.render_widget(block, area);
        return;
    };
    let is_event = kind.is_event();
    let base_headers_owned = kind.column_headers();

    if is_event {
        render_events_log(f, app, area);
        return;
    }

    let visible_indices = app.visible_resource_indices();

    let all_ns = app.kube.context.namespace.is_none();

    // Build logical columns: [NAME, (NAMESPACE)?, col1, col2, ...]
    let mut col_headers: Vec<String> = vec![base_headers_owned[0].clone()];
    if all_ns {
        col_headers.push("NAMESPACE".to_string());
    }
    col_headers.extend(base_headers_owned[1..].iter().cloned());

    let header_style = Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD);
    let header = Row::new(
        col_headers
            .iter()
            .map(|h| Cell::from(h.as_str()).style(header_style))
            .collect::<Vec<_>>(),
    )
    .height(1);

    // Pre-compute favorite set for this resource type to avoid borrow conflicts
    let fav_key = app.selected_kind().map(|kind| kind.favorite_key());
    let fav_set: std::collections::HashSet<(String, String)> = if let Some(key) = fav_key {
        let context = app.kube.context.name.as_str();
        app.config
            .active_profile()
            .favorites
            .iter()
            .filter(|f| f.resource_type == key && f.context == context)
            .map(|f| (f.name.clone(), f.namespace.clone()))
            .collect()
    } else {
        std::collections::HashSet::new()
    };

    let opt_rt = app.selected_resource_type();
    let rows: Vec<Row> = visible_indices
        .iter()
        .map(|&idx| {
            let entry = &app.resources.entries[idx];
            let is_diff_marked = app
                .diff_mark
                .as_ref()
                .is_some_and(|mark| mark.name == entry.name && mark.namespace == entry.namespace);

            let is_favorited = fav_set.contains(&(entry.name.clone(), entry.namespace.clone()));

            let name_cell = if is_diff_marked {
                Cell::from(Span::styled(
                    format!("* {}", entry.name),
                    Style::default().fg(Color::Magenta).add_modifier(Modifier::BOLD),
                ))
            } else if is_favorited {
                Cell::from(Span::styled(
                    format!("★ {}", entry.name),
                    Style::default().fg(Color::Yellow),
                ))
            } else {
                Cell::from(entry.name.as_str())
            };

            let mut cells = vec![name_cell];
            if all_ns {
                cells.push(Cell::from(entry.namespace.as_str()));
            }
            for col in &entry.columns {
                cells.push(Cell::from(col.as_str()));
            }
            let row = if opt_rt == Some(ResourceType::Pod) {
                let status = entry.columns.get(1).map(|s| s.as_str()).unwrap_or("");
                let style = match status {
                    | "Running" => Style::default().fg(Color::Green),
                    | "Succeeded" | "Completed" => Style::default().fg(Color::DarkGray),
                    | "Pending" | "ContainerCreating" | "PodInitializing" => Style::default().fg(Color::Yellow),
                    | s if s.starts_with("Init:") => Style::default().fg(Color::Yellow),
                    | "CrashLoopBackOff"
                    | "Error"
                    | "OOMKilled"
                    | "ImagePullBackOff"
                    | "ErrImagePull"
                    | "CreateContainerConfigError" => Style::default().fg(Color::Red),
                    | "Terminating" => Style::default().fg(Color::DarkGray),
                    | _ => Style::default(),
                };
                Row::new(cells).style(style)
            } else if opt_rt == Some(ResourceType::Node) {
                let status = entry.columns.first().map(|s| s.as_str()).unwrap_or("");
                let style = if status.contains("SchedulingDisabled") {
                    Style::default().fg(Color::Yellow)
                } else if status.contains("NotReady") {
                    Style::default().fg(Color::Red)
                } else {
                    Style::default().fg(Color::Green)
                };
                Row::new(cells).style(style)
            } else {
                Row::new(cells)
            };
            row
        })
        .collect();

    // Compute column widths from header + data content
    let num_cols = col_headers.len();
    let mut max_widths: Vec<usize> = col_headers.iter().map(|h| h.len()).collect();
    for &idx in &visible_indices {
        let entry = &app.resources.entries[idx];
        // Column 0 = NAME
        max_widths[0] = max_widths[0].max(entry.name.len());
        let data_start = if all_ns {
            max_widths[1] = max_widths[1].max(entry.namespace.len());
            2
        } else {
            1
        };
        for (i, col) in entry.columns.iter().enumerate() {
            let ci = data_start + i;
            if ci < num_cols {
                max_widths[ci] = max_widths[ci].max(col.len());
            }
        }
    }
    // Add padding (2 chars) and cap individual columns at 50 to prevent blowout
    for w in &mut max_widths {
        *w = (*w + 2).min(50);
    }
    // NAME column (first) is flexible; all others are fixed width
    let mut constraints: Vec<Constraint> = Vec::with_capacity(num_cols);
    constraints.push(Constraint::Min(max_widths[0] as u16));
    for &w in &max_widths[1..] {
        constraints.push(Constraint::Length(w as u16));
    }

    let focused = app.focus == Focus::Resources;
    let border_color = if focused { Color::Cyan } else { Color::DarkGray };
    let title = app
        .selected_kind()
        .map(|kind| format!(" {} ", kind.display_name()))
        .unwrap_or_else(|| " Resources ".to_string());

    // Map real selection index to filtered row position for highlight.
    // Preserve the table offset across renders for smooth edge-scrolling.
    if let Some(sel) = app.resources.state.selected() {
        if let Some(vis_pos) = visible_indices.iter().position(|&i| i == sel) {
            app.resources.table_state.select(Some(vis_pos));
        } else if !visible_indices.is_empty() {
            app.resources.table_state.select(Some(0));
            app.resources.state.select(Some(visible_indices[0]));
        }
    } else {
        app.resources.table_state.select(None);
    }

    let table = Table::new(rows, constraints)
        .header(header)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(border_color))
                .title(title),
        )
        .row_highlight_style(Style::default().add_modifier(Modifier::REVERSED).fg(Color::Cyan))
        .highlight_symbol("▶ ");

    f.render_stateful_widget(table, area, &mut app.resources.table_state);
}

/// Builds a text gauge bar: `[████████░░░░░░░░░░░░] 75%`
fn gauge_bar(filled: usize, total: usize, width: usize) -> Vec<Span<'static>> {
    let pct = if total == 0 { 0.0 } else { filled as f64 / total as f64 };
    let pct_int = (pct * 100.0) as u64;
    let filled_w = ((pct * width as f64) as usize).min(width);
    let empty_w = width - filled_w;

    let bar_color = if pct >= 0.95 {
        Color::Green
    } else if pct >= 0.80 {
        Color::Yellow
    } else {
        Color::Red
    };

    vec![
        Span::styled("[", Style::default().fg(Color::DarkGray)),
        Span::styled("█".repeat(filled_w), Style::default().fg(bar_color)),
        Span::styled("░".repeat(empty_w), Style::default().fg(Color::DarkGray)),
        Span::styled("] ", Style::default().fg(Color::DarkGray)),
        Span::styled(format!("{}%", pct_int), Style::default().fg(bar_color)),
    ]
}

/// Like gauge_bar but takes a pre-computed percentage (0-100).
fn gauge_bar_inv(pct: usize, width: usize) -> Vec<Span<'static>> {
    let pct_f = (pct as f64 / 100.0).clamp(0.0, 1.0);
    let filled_w = ((pct_f * width as f64) as usize).min(width);
    let empty_w = width - filled_w;

    let bar_color = if pct >= 95 {
        Color::Green
    } else if pct >= 80 {
        Color::Yellow
    } else {
        Color::Red
    };

    vec![
        Span::styled("[", Style::default().fg(Color::DarkGray)),
        Span::styled("█".repeat(filled_w), Style::default().fg(bar_color)),
        Span::styled("░".repeat(empty_w), Style::default().fg(Color::DarkGray)),
        Span::styled("] ", Style::default().fg(Color::DarkGray)),
        Span::styled(format!("{}%", pct), Style::default().fg(bar_color)),
    ]
}

/// Builds a mini stat card: ` ╭ LABEL ─────╮\n │  VALUE      │\n
/// ╰─────────────╯`
fn stat_card(label: &str, value: &str, value_style: Style, width: usize) -> Vec<Line<'static>> {
    let dim = Style::default().fg(Color::DarkGray);
    let inner = width.saturating_sub(4); // 2 border + 2 padding
    let bar_w = width.saturating_sub(2);
    let label_pad = bar_w.saturating_sub(label.len() + 2);

    vec![
        Line::from(vec![
            Span::styled(" ╭ ", dim),
            Span::styled(label.to_string(), Style::default().fg(Color::Cyan)),
            Span::styled(format!(" {}", "─".repeat(label_pad)), dim),
            Span::styled("╮", dim),
        ]),
        Line::from(vec![
            Span::styled(" │ ", dim),
            Span::styled(format!("{:<width$}", value, width = inner), value_style),
            Span::styled(" │", dim),
        ]),
        Line::from(vec![Span::styled(format!(" ╰{}╯", "─".repeat(bar_w)), dim)]),
    ]
}

/// Builds a box-drawn card for one node, returned as rows of Spans.
/// Each row is exactly `w` display-columns wide (including the box border).
fn build_node_card(node: &crate::k8s::NodeStats, w: usize) -> Vec<Vec<Span<'static>>> {
    let dim = Style::default().fg(Color::DarkGray);
    let lbl = Style::default().fg(Color::Cyan);
    let val = Style::default().fg(Color::White);
    let bold = Style::default().fg(Color::White).add_modifier(Modifier::BOLD);
    let status_style = if node.unschedulable {
        Style::default().fg(Color::Yellow)
    } else if node.status.contains("NotReady") {
        Style::default().fg(Color::Red)
    } else {
        Style::default().fg(Color::Green)
    };
    let status_icon = if node.unschedulable {
        "⊘"
    } else if node.status.contains("NotReady") {
        "○"
    } else {
        "●"
    };
    let inner = w.saturating_sub(4); // │ + space ... space + │

    // Helper: pad a set of spans to exactly `inner` visible chars, then wrap in │ …
    // │
    let row = |content: Vec<Span<'static>>| -> Vec<Span<'static>> {
        // Calculate visible width of content spans
        let content_w: usize = content.iter().map(|s| s.content.chars().count()).sum();
        let pad = inner.saturating_sub(content_w);
        let mut r = vec![Span::styled("│ ", dim)];
        r.extend(content);
        r.push(Span::styled(format!("{} │", " ".repeat(pad)), dim));
        r
    };

    let bar_w = w.saturating_sub(2); // ╭ ... ╯ border chars

    // Truncate name to fit, leaving room for status icon
    let name_max = inner.saturating_sub(2); // "● " prefix
    let name_display = if node.name.len() > name_max {
        format!("{}…", &node.name[..name_max.saturating_sub(1)])
    } else {
        node.name.clone()
    };

    // Truncate role to fit the remaining space on the role line
    let role_display = if node.roles.len() > inner {
        format!("{}…", &node.roles[..inner.saturating_sub(1)])
    } else {
        node.roles.clone()
    };

    let mut card = Vec::new();

    // Top border with status
    let top_label = format!(" {} {} ", status_icon, name_display);
    let top_pad = bar_w.saturating_sub(top_label.chars().count() + 1);
    card.push(vec![
        Span::styled("╭", dim),
        Span::styled(format!(" {} ", status_icon), status_style),
        Span::styled(name_display, bold),
        Span::styled(format!(" {}", "─".repeat(top_pad)), dim),
        Span::styled("╮", dim),
    ]);

    // Role + status
    if node.unschedulable {
        card.push(row(vec![Span::styled(role_display, dim)]));
        card.push(row(vec![Span::styled(
            "⊘ CORDONED",
            Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD),
        )]));
    } else {
        card.push(row(vec![Span::styled(role_display, dim)]));
    }

    // Separator
    card.push(vec![Span::styled(format!("├{}┤", "─".repeat(bar_w)), dim)]);

    let res_lbl_w = 8;
    let res_val_w = inner.saturating_sub(res_lbl_w);

    // Version row
    card.push(row(vec![
        Span::styled(format!("{:<w$}", "version", w = res_lbl_w), lbl),
        Span::styled(format!("{:<w$}", node.version, w = res_val_w), val),
    ]));

    // CPU row — show usage if available
    if let Some(usage) = &node.usage {
        let cap = crate::k8s::parse_cpu_cores(&node.cpu.capacity);
        let pct = if cap > 0.0 {
            (usage.cpu_cores / cap * 100.0) as usize
        } else {
            0
        };
        let color = usage_color(pct);
        card.push(row(vec![
            Span::styled(format!("{:<w$}", "cpu", w = res_lbl_w), lbl),
            Span::styled(
                format!(
                    "{:<w$}",
                    format!(
                        "{} / {} ({}%)",
                        crate::k8s::format_cpu_cores(usage.cpu_cores),
                        node.cpu.capacity,
                        pct
                    ),
                    w = res_val_w
                ),
                Style::default().fg(color),
            ),
        ]));
    } else {
        card.push(row(vec![
            Span::styled(format!("{:<w$}", "cpu", w = res_lbl_w), lbl),
            Span::styled(
                format!(
                    "{:<w$}",
                    format!("{} / {}", node.cpu.allocatable, node.cpu.capacity),
                    w = res_val_w
                ),
                val,
            ),
        ]));
    }

    // Memory row — show usage if available
    if let Some(usage) = &node.usage {
        let cap = crate::k8s::parse_memory_bytes(&node.mem.capacity);
        let pct = if cap > 0.0 {
            (usage.memory_bytes / cap * 100.0) as usize
        } else {
            0
        };
        let color = usage_color(pct);
        card.push(row(vec![
            Span::styled(format!("{:<w$}", "memory", w = res_lbl_w), lbl),
            Span::styled(
                format!(
                    "{:<w$}",
                    format!(
                        "{} / {} ({}%)",
                        crate::k8s::format_memory_gb_from_bytes(usage.memory_bytes),
                        node.mem.capacity,
                        pct
                    ),
                    w = res_val_w
                ),
                Style::default().fg(color),
            ),
        ]));
    } else {
        card.push(row(vec![
            Span::styled(format!("{:<w$}", "memory", w = res_lbl_w), lbl),
            Span::styled(
                format!(
                    "{:<w$}",
                    format!("{} / {}", node.mem.allocatable, node.mem.capacity),
                    w = res_val_w
                ),
                val,
            ),
        ]));
    }

    // Pods row
    card.push(row(vec![
        Span::styled(format!("{:<w$}", "pods", w = res_lbl_w), lbl),
        Span::styled(
            format!(
                "{:<w$}",
                format!("{} / {}", node.pods.allocatable, node.pods.capacity),
                w = res_val_w
            ),
            val,
        ),
    ]));

    // Separator
    card.push(vec![Span::styled(format!("├{}┤", "─".repeat(bar_w)), dim)]);

    // OS/arch & age
    card.push(row(vec![
        Span::styled(format!("{:<width$}", node.os_arch, width = inner / 2), dim),
        Span::styled(format!("{:>width$}", node.age, width = inner - inner / 2), dim),
    ]));

    // Bottom border
    card.push(vec![Span::styled(format!("╰{}╯", "─".repeat(bar_w)), dim)]);

    card
}

fn section_heading<'a>(title: &str, heading: Style, dim: Style) -> Vec<Line<'a>> {
    vec![
        Line::from(Span::styled(format!(" {title}"), heading)),
        Line::from(Span::styled(
            " ──────────────────────────────────────────────────────────",
            dim,
        )),
    ]
}

fn render_card_row<'a>(cards: &[Vec<Line<'a>>], lines: &mut Vec<Line<'a>>) {
    for row in 0..3 {
        let mut spans = Vec::new();
        for card in cards {
            if let Some(line) = card.get(row) {
                spans.extend(line.spans.iter().cloned());
            }
            spans.push(Span::styled(" ", Style::default()));
        }
        lines.push(Line::from(spans));
    }
}

fn render_cluster_stats(f: &mut Frame, app: &mut App, area: Rect) {
    let focused = app.focus == Focus::Resources;
    let border_color = if focused { Color::Cyan } else { Color::DarkGray };

    let stats = match &app.overview.stats {
        | Some(s) => s,
        | None => {
            let block = Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(border_color))
                .title(" Cluster Overview — Loading... ");
            f.render_widget(block, area);
            return;
        },
    };

    let heading = Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD);
    let label = Style::default().fg(Color::Cyan);
    let value = Style::default().fg(Color::White);
    let good = Style::default().fg(Color::Green);
    let bad = Style::default().fg(Color::Red);
    let warn = Style::default().fg(Color::Yellow);
    let dim = Style::default().fg(Color::DarkGray);

    let mut lines: Vec<Line> = Vec::new();

    // ── Stat cards row 1: Cluster identity ───────────────────
    let card_w = 18;
    let node_style = if stats.nodes.not_ready > 0 {
        bad
    } else if stats.nodes.cordoned > 0 {
        warn
    } else {
        good
    };
    let node_value = format!("{}/{}", stats.nodes.ready, stats.nodes.total);
    let pod_style = if stats.pods.crash_loop > 0 || stats.pods.failed > 0 {
        bad
    } else if stats.pods.pending > 0 {
        warn
    } else {
        good
    };

    render_card_row(
        &[
            stat_card("K8s", &stats.server_version, value, card_w),
            stat_card("Nodes", &node_value, node_style, card_w),
            stat_card("Namespaces", &stats.namespace_count.to_string(), value, card_w),
            stat_card(
                "Pods",
                &format!("{}/{}", stats.pods.running, stats.pods.total),
                pod_style,
                card_w,
            ),
        ],
        &mut lines,
    );

    // ── Stat cards row 2: Workloads ──────────────────────────
    let dep_style = if stats.workload_health.deployments_ready < stats.workload_health.deployments_total {
        warn
    } else {
        value
    };
    let sts_style = if stats.workload_health.statefulsets_ready < stats.workload_health.statefulsets_total {
        warn
    } else {
        value
    };
    let ds_style = if stats.workload_health.daemonsets_ready < stats.workload_health.daemonsets_desired {
        warn
    } else {
        value
    };

    render_card_row(
        &[
            stat_card("Deploys", &stats.deployment_count.to_string(), dep_style, card_w),
            stat_card("StatefulSets", &stats.statefulset_count.to_string(), sts_style, card_w),
            stat_card("DaemonSets", &stats.daemonset_count.to_string(), ds_style, card_w),
            stat_card("Jobs", &stats.job_count.to_string(), value, card_w),
            stat_card("CronJobs", &stats.cronjob_count.to_string(), value, card_w),
        ],
        &mut lines,
    );

    // ── Stat cards row 3: Resources ──────────────────────────
    render_card_row(
        &[
            stat_card("ConfigMaps", &stats.configmap_count.to_string(), value, card_w),
            stat_card("Secrets", &stats.secret_count.to_string(), value, card_w),
            stat_card("Services", &stats.service_count.to_string(), value, card_w),
            stat_card("Ingresses", &stats.ingress_count.to_string(), value, card_w),
            stat_card("PVCs", &stats.pvc_count.to_string(), value, card_w),
            stat_card("HPAs", &stats.hpa_count.to_string(), value, card_w),
        ],
        &mut lines,
    );
    lines.push(Line::from(""));

    // ── Health warnings ─────────────────────────────────────
    {
        let mut warnings: Vec<Line> = Vec::new();
        if stats.pods.crash_loop > 0 {
            warnings.push(Line::from(Span::styled(
                format!("  ⚠ {} pod(s) in CrashLoopBackOff", stats.pods.crash_loop),
                bad,
            )));
        }
        if stats.pods.error > 0 {
            warnings.push(Line::from(Span::styled(
                format!("  ⚠ {} pod(s) in error state (ImagePull/Config)", stats.pods.error),
                bad,
            )));
        }
        if stats.nodes.not_ready > 0 {
            warnings.push(Line::from(Span::styled(
                format!("  ⚠ {} node(s) NotReady", stats.nodes.not_ready),
                bad,
            )));
        }
        if stats.nodes.cordoned > 0 {
            warnings.push(Line::from(Span::styled(
                format!("  ⊘ {} node(s) cordoned (scheduling disabled)", stats.nodes.cordoned),
                warn,
            )));
        }
        if stats.pressure.memory_pressure > 0 {
            warnings.push(Line::from(Span::styled(
                format!("  ⚠ {} node(s) with MemoryPressure", stats.pressure.memory_pressure),
                bad,
            )));
        }
        if stats.pressure.disk_pressure > 0 {
            warnings.push(Line::from(Span::styled(
                format!("  ⚠ {} node(s) with DiskPressure", stats.pressure.disk_pressure),
                bad,
            )));
        }
        if stats.pressure.pid_pressure > 0 {
            warnings.push(Line::from(Span::styled(
                format!("  ⚠ {} node(s) with PIDPressure", stats.pressure.pid_pressure),
                bad,
            )));
        }
        if stats.workload_health.deployments_ready < stats.workload_health.deployments_total {
            let unhealthy = stats.workload_health.deployments_total - stats.workload_health.deployments_ready;
            warnings.push(Line::from(Span::styled(
                format!("  ⚠ {} deployment(s) not fully available", unhealthy),
                warn,
            )));
        }
        if stats.workload_health.statefulsets_ready < stats.workload_health.statefulsets_total {
            let unhealthy = stats.workload_health.statefulsets_total - stats.workload_health.statefulsets_ready;
            warnings.push(Line::from(Span::styled(
                format!("  ⚠ {} statefulset(s) not fully ready", unhealthy),
                warn,
            )));
        }
        if stats.workload_health.daemonsets_ready < stats.workload_health.daemonsets_desired {
            let unhealthy = stats.workload_health.daemonsets_desired - stats.workload_health.daemonsets_ready;
            warnings.push(Line::from(Span::styled(
                format!("  ⚠ {} daemonset(s) not fully ready", unhealthy),
                warn,
            )));
        }
        if stats.recent_warnings > 0 {
            warnings.push(Line::from(Span::styled(
                format!("  ⚠ {} warning event(s) in last hour", stats.recent_warnings),
                warn,
            )));
        }
        if warnings.is_empty() {
            warnings.push(Line::from(Span::styled("  ✓ Cluster healthy — no warnings", good)));
        }
        lines.extend(section_heading("Health", heading, dim));
        lines.extend(warnings);
        lines.push(Line::from(""));
    }

    // ── Workload health bars ─────────────────────────────────
    {
        let wh = &stats.workload_health;
        let has_workloads = wh.deployments_total > 0 || wh.statefulsets_total > 0 || wh.daemonsets_desired > 0;
        if has_workloads {
            lines.extend(section_heading("Workload Health", heading, dim));
            let bar_w = 30;
            let lbl_w = 16;

            if wh.deployments_total > 0 {
                let mut spans = vec![Span::styled(format!("  {:<w$}", "Deployments", w = lbl_w), label)];
                spans.extend(gauge_bar(wh.deployments_ready, wh.deployments_total, bar_w));
                spans.push(Span::styled(
                    format!("  {}/{} ready", wh.deployments_ready, wh.deployments_total),
                    dim,
                ));
                lines.push(Line::from(spans));
            }
            if wh.statefulsets_total > 0 {
                let mut spans = vec![Span::styled(format!("  {:<w$}", "StatefulSets", w = lbl_w), label)];
                spans.extend(gauge_bar(wh.statefulsets_ready, wh.statefulsets_total, bar_w));
                spans.push(Span::styled(
                    format!("  {}/{} ready", wh.statefulsets_ready, wh.statefulsets_total),
                    dim,
                ));
                lines.push(Line::from(spans));
            }
            if wh.daemonsets_desired > 0 {
                let mut spans = vec![Span::styled(format!("  {:<w$}", "DaemonSets", w = lbl_w), label)];
                spans.extend(gauge_bar(wh.daemonsets_ready, wh.daemonsets_desired, bar_w));
                spans.push(Span::styled(
                    format!("  {}/{} ready", wh.daemonsets_ready, wh.daemonsets_desired),
                    dim,
                ));
                lines.push(Line::from(spans));
            }
            lines.push(Line::from(""));
        }
    }

    // ── Cluster Resources ────────────────────────────────────
    {
        let cr = &stats.cluster_resources;
        let m = &app.overview.metrics;
        if cr.cpu_capacity > 0.0 || cr.mem_capacity_bytes > 0.0 {
            lines.extend(section_heading("Cluster Resources", heading, dim));
            let bar_w = 30;
            let lbl_w = 16;

            // CPU: show live usage if metrics available, otherwise allocatable/capacity
            if let Some(metrics) = m.as_ref().filter(|m| m.available) {
                let pct = if cr.cpu_capacity > 0.0 {
                    (metrics.total_cpu_usage / cr.cpu_capacity * 100.0) as usize
                } else {
                    0
                };
                let mut spans = vec![Span::styled(format!("  {:<w$}", "CPU Usage", w = lbl_w), label)];
                spans.extend(gauge_bar_usage(pct, bar_w));
                spans.push(Span::styled(
                    format!(
                        "  {} / {} cores",
                        crate::k8s::format_cpu_cores(metrics.total_cpu_usage),
                        crate::k8s::format_cpu_cores(cr.cpu_capacity)
                    ),
                    dim,
                ));
                lines.push(Line::from(spans));
            } else {
                let cpu_alloc_pct = if cr.cpu_capacity > 0.0 {
                    (cr.cpu_allocatable / cr.cpu_capacity * 100.0) as usize
                } else {
                    0
                };
                let mut spans = vec![Span::styled(format!("  {:<w$}", "CPU", w = lbl_w), label)];
                spans.extend(gauge_bar_inv(cpu_alloc_pct, bar_w));
                spans.push(Span::styled(
                    format!(
                        "  {} / {} cores",
                        crate::k8s::format_cpu_cores(cr.cpu_allocatable),
                        crate::k8s::format_cpu_cores(cr.cpu_capacity)
                    ),
                    dim,
                ));
                lines.push(Line::from(spans));
            }

            // Memory: show live usage if metrics available
            if let Some(metrics) = m.as_ref().filter(|m| m.available) {
                let pct = if cr.mem_capacity_bytes > 0.0 {
                    (metrics.total_memory_usage / cr.mem_capacity_bytes * 100.0) as usize
                } else {
                    0
                };
                let mut spans = vec![Span::styled(format!("  {:<w$}", "Mem Usage", w = lbl_w), label)];
                spans.extend(gauge_bar_usage(pct, bar_w));
                spans.push(Span::styled(
                    format!(
                        "  {} / {}",
                        crate::k8s::format_memory_gb_from_bytes(metrics.total_memory_usage),
                        crate::k8s::format_memory_gb_from_bytes(cr.mem_capacity_bytes)
                    ),
                    dim,
                ));
                lines.push(Line::from(spans));
            } else {
                let mem_alloc_pct = if cr.mem_capacity_bytes > 0.0 {
                    (cr.mem_allocatable_bytes / cr.mem_capacity_bytes * 100.0) as usize
                } else {
                    0
                };
                let mut spans = vec![Span::styled(format!("  {:<w$}", "Memory", w = lbl_w), label)];
                spans.extend(gauge_bar_inv(mem_alloc_pct, bar_w));
                spans.push(Span::styled(
                    format!(
                        "  {} / {}",
                        crate::k8s::format_memory_gb_from_bytes(cr.mem_allocatable_bytes),
                        crate::k8s::format_memory_gb_from_bytes(cr.mem_capacity_bytes)
                    ),
                    dim,
                ));
                lines.push(Line::from(spans));
            }

            // Pods: used / capacity
            if cr.pod_capacity > 0 {
                let pods_used = stats.pods.running + stats.pods.pending;
                let mut spans = vec![Span::styled(format!("  {:<w$}", "Pod Usage", w = lbl_w), label)];
                spans.extend(gauge_bar_usage(
                    (pods_used as f64 / cr.pod_capacity as f64 * 100.0) as usize,
                    bar_w,
                ));
                spans.push(Span::styled(
                    format!("  {} / {} slots", pods_used, cr.pod_capacity),
                    dim,
                ));
                lines.push(Line::from(spans));
            }
            lines.push(Line::from(""));
        }
    }

    // ── Pod breakdown ────────────────────────────────────────
    lines.extend(section_heading(&format!("Pods ({})", stats.pods.total), heading, dim));

    if stats.pods.total > 0 {
        let bar_width = 30;
        let lbl_w = 16;

        // Running
        let mut running_spans = vec![Span::styled(format!("  {:<w$}", "Running", w = lbl_w), label)];
        running_spans.extend(gauge_bar(stats.pods.running, stats.pods.total, bar_width));
        running_spans.push(Span::styled(
            format!("  {}/{}", stats.pods.running, stats.pods.total),
            dim,
        ));
        lines.push(Line::from(running_spans));

        if stats.pods.succeeded > 0 {
            lines.push(Line::from(vec![
                Span::styled(format!("  {:<w$}", "Succeeded", w = lbl_w), label),
                Span::styled(stats.pods.succeeded.to_string(), good),
            ]));
        }
        if stats.pods.pending > 0 {
            lines.push(Line::from(vec![
                Span::styled(format!("  {:<w$}", "Pending", w = lbl_w), label),
                Span::styled(stats.pods.pending.to_string(), warn),
            ]));
        }
        if stats.pods.failed > 0 {
            lines.push(Line::from(vec![
                Span::styled(format!("  {:<w$}", "Failed", w = lbl_w), label),
                Span::styled(stats.pods.failed.to_string(), bad),
            ]));
        }

        // Containers
        lines.push(Line::from(vec![
            Span::styled(format!("  {:<w$}", "Containers", w = lbl_w), label),
            Span::styled(
                format!("{}/{} ready", stats.containers.ready, stats.containers.total),
                if stats.containers.ready < stats.containers.total {
                    warn
                } else {
                    value
                },
            ),
        ]));
        if stats.containers.total_restarts > 0 {
            let restart_style = if stats.containers.total_restarts > 100 {
                bad
            } else if stats.containers.total_restarts > 10 {
                warn
            } else {
                value
            };
            lines.push(Line::from(vec![
                Span::styled(format!("  {:<w$}", "Restarts", w = lbl_w), label),
                Span::styled(format!("{}", stats.containers.total_restarts), restart_style),
            ]));
        }
    } else {
        lines.push(Line::from(Span::styled("  No pods", dim)));
    }
    lines.push(Line::from(""));

    // ── Top restarting pods ──────────────────────────────────
    if !stats.top_restarting.is_empty() {
        lines.extend(section_heading("Top Restarting Pods", heading, dim));
        for pod in &stats.top_restarting {
            let restart_style = if pod.restart_count > 100 { bad } else { warn };
            lines.push(Line::from(vec![
                Span::styled("  ", dim),
                Span::styled(format!("{:<40}", pod.name), value),
                Span::styled(format!("{:<20}", pod.namespace), dim),
                Span::styled(format!("{}", pod.restart_count), restart_style),
                Span::styled(" restarts", dim),
            ]));
        }
        lines.push(Line::from(""));
    }

    // ── Recent warning events ────────────────────────────────
    if !stats.warning_details.is_empty() {
        lines.extend(section_heading("Recent Warnings (last hour)", heading, dim));
        for evt in &stats.warning_details {
            let msg = if evt.message.len() > 60 {
                format!("{}…", &evt.message[..59])
            } else {
                evt.message.clone()
            };
            lines.push(Line::from(vec![
                Span::styled("  ⚠ ", warn),
                Span::styled(format!("{:<18}", evt.reason), label),
                Span::styled(format!("{:<16}", evt.namespace), dim),
                Span::styled(format!("×{:<4}", evt.count), warn),
                Span::styled(msg, value),
            ]));
        }
        lines.push(Line::from(""));
    }

    // ── Node grid ─────────────────────────────────────────
    if !stats.node_list.is_empty() {
        lines.extend(section_heading(
            &format!("Nodes ({})", stats.node_list.len()),
            heading,
            dim,
        ));

        // Build node cards, then tile them in a grid
        let node_card_w: usize = 36;
        let gap = 1;
        let avail_w = area.width.saturating_sub(3) as usize;
        let cols = ((avail_w + gap) / (node_card_w + gap)).max(1);
        let node_cards: Vec<Vec<Vec<Span>>> = stats
            .node_list
            .iter()
            .map(|node| build_node_card(node, node_card_w))
            .collect();

        for chunk in node_cards.chunks(cols) {
            let card_height = chunk.iter().map(|c| c.len()).max().unwrap_or(0);
            for row in 0..card_height {
                let mut spans: Vec<Span> = vec![Span::raw(" ")];
                for (ci, card) in chunk.iter().enumerate() {
                    if ci > 0 {
                        spans.push(Span::styled(" ", Style::default()));
                    }
                    if let Some(card_row) = card.get(row) {
                        spans.extend(card_row.iter().cloned());
                    } else {
                        spans.push(Span::raw(" ".repeat(node_card_w)));
                    }
                }
                lines.push(Line::from(spans));
            }
            lines.push(Line::from(""));
        }
    }

    let paragraph = Paragraph::new(lines)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(border_color))
                .title(" Cluster Overview "),
        )
        .scroll((app.overview.scroll, 0));
    f.render_widget(paragraph, area);
}

fn usage_color(pct: usize) -> Color {
    if pct >= 90 {
        Color::Red
    } else if pct >= 70 {
        Color::Yellow
    } else {
        Color::Green
    }
}

/// Render a usage gauge bar where higher = worse (red at top).
fn gauge_bar_usage(pct: usize, width: usize) -> Vec<Span<'static>> {
    let pct_f = (pct as f64 / 100.0).clamp(0.0, 1.0);
    let filled_w = ((pct_f * width as f64) as usize).min(width);
    let empty_w = width - filled_w;
    let color = usage_color(pct);

    vec![
        Span::styled("[", Style::default().fg(Color::DarkGray)),
        Span::styled("█".repeat(filled_w), Style::default().fg(color)),
        Span::styled("░".repeat(empty_w), Style::default().fg(Color::DarkGray)),
        Span::styled("] ", Style::default().fg(Color::DarkGray)),
        Span::styled(format!("{}%", pct), Style::default().fg(color)),
    ]
}

// ---------------------------------------------------------------------------
// Resource Map (pod flamegraph / treemap)
// ---------------------------------------------------------------------------

fn truncate(s: &str, max: usize) -> String {
    if s.len() > max {
        format!("{}…", &s[..max.saturating_sub(1)])
    } else {
        s.to_string()
    }
}

/// Render a stacked bar: usage (green/red) | wasted request (blue) | headroom
/// to limit (dark) The bar shows three layers within `width` chars, scaled to
/// `max_val`.
fn stacked_bar(usage: f64, request: f64, limit: f64, max_val: f64, width: usize) -> Vec<Span<'static>> {
    if max_val <= 0.0 || width == 0 {
        return vec![Span::raw("")];
    }
    let scale = |v: f64| -> usize { ((v / max_val) * width as f64).round().min(width as f64) as usize };

    let use_w = scale(usage);
    let req_w = scale(request).saturating_sub(use_w);
    let lim_w = if limit > 0.0 {
        scale(limit).saturating_sub(use_w + req_w)
    } else {
        0
    };
    let empty_w = width.saturating_sub(use_w + req_w + lim_w);

    let use_color = if request > 0.0 && usage > request * 1.2 {
        Color::Red
    } else if request > 0.0 && usage < request * 0.2 {
        Color::Blue
    } else {
        Color::Green
    };

    vec![
        Span::styled("█".repeat(use_w), Style::default().fg(use_color)),
        Span::styled("▒".repeat(req_w), Style::default().fg(Color::DarkGray)),
        Span::styled("░".repeat(lim_w), Style::default().fg(Color::Rgb(50, 50, 50))),
        Span::styled("·".repeat(empty_w), Style::default().fg(Color::Rgb(30, 30, 30))),
    ]
}

/// Pick a unique-ish color for a namespace/workload name by hashing it.
fn name_color(name: &str) -> Color {
    let hash: u32 = name
        .bytes()
        .fold(5381u32, |h, b| h.wrapping_mul(33).wrapping_add(b as u32));
    let colors = [
        Color::Green,
        Color::Cyan,
        Color::Blue,
        Color::Magenta,
        Color::Yellow,
        Color::Red,
        Color::LightGreen,
        Color::LightCyan,
        Color::LightBlue,
        Color::LightMagenta,
        Color::LightYellow,
        Color::LightRed,
    ];
    colors[(hash as usize) % colors.len()]
}

fn render_resource_map(f: &mut Frame, app: &mut App, area: Rect) {
    use super::app::ResourceMapSort;

    let focused = app.focus == Focus::Resources;
    let border_color = if focused { Color::Cyan } else { Color::DarkGray };

    let metrics = match &app.resource_map.metrics {
        | Some(m) if m.available => m,
        | _ => {
            let msg = if app.overview.metrics_rx.is_some() {
                " Resource Map — Loading metrics... "
            } else {
                " Resource Map — metrics-server not available "
            };
            let block = Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(border_color))
                .title(msg);
            f.render_widget(block, area);
            return;
        },
    };

    let heading = Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD);
    let label = Style::default().fg(Color::Cyan);
    let value = Style::default().fg(Color::White);
    let dim = Style::default().fg(Color::DarkGray);
    let good = Style::default().fg(Color::Green);
    let warn = Style::default().fg(Color::Yellow);
    let bad = Style::default().fg(Color::Red);
    let over_prov = Style::default().fg(Color::Blue);

    // Filter by selected namespace
    let ns_filter = app.kube.context.namespace.as_deref();
    let filtered_pods: Vec<&crate::k8s::PodMetricsEntry> = metrics
        .pod_metrics
        .iter()
        .filter(|p| ns_filter.is_none() || ns_filter == Some(p.namespace.as_str()))
        .collect();
    let filtered_workloads: Vec<&crate::k8s::WorkloadMetrics> = metrics
        .workload_metrics
        .iter()
        .filter(|w| ns_filter.is_none() || ns_filter == Some(w.namespace.as_str()))
        .collect();

    // Totals for the filtered set
    let total_cpu: f64 = filtered_pods.iter().map(|p| p.cpu_cores).sum::<f64>().max(0.001);
    let total_mem: f64 = filtered_pods.iter().map(|p| p.memory_bytes).sum::<f64>().max(1.0);
    let total_cpu_req: f64 = filtered_pods.iter().map(|p| p.spec.cpu_request).sum();
    let total_cpu_lim: f64 = filtered_pods.iter().map(|p| p.spec.cpu_limit).sum();
    let total_mem_req: f64 = filtered_pods.iter().map(|p| p.spec.mem_request).sum();
    let total_mem_lim: f64 = filtered_pods.iter().map(|p| p.spec.mem_limit).sum();
    let inner_w = area.width.saturating_sub(4) as usize;
    let bar_w = inner_w.saturating_sub(32).clamp(15, 50);

    let mut lines: Vec<Line> = Vec::new();

    let sort = app.resource_map.sort;
    let sort_label = match sort {
        | ResourceMapSort::Cpu => "CPU",
        | ResourceMapSort::Memory => "Memory",
    };
    let ns_label = ns_filter.unwrap_or("all namespaces");

    // ── Header ───────────────────────────────────────────────
    lines.push(Line::from(vec![
        Span::styled(format!(" {ns_label}"), heading),
        Span::styled("  sorted by: ", dim),
        Span::styled(sort_label, label),
        Span::styled("  (", dim),
        Span::styled("m", label),
        Span::styled(" toggle  ", dim),
        Span::styled("n", label),
        Span::styled(" namespace)", dim),
    ]));
    // Legend
    lines.push(Line::from(vec![
        Span::styled("  ", dim),
        Span::styled("█", good),
        Span::styled(" usage  ", dim),
        Span::styled("▒", dim),
        Span::styled(" unused request  ", dim),
        Span::styled("░", Style::default().fg(Color::Rgb(50, 50, 50))),
        Span::styled(" headroom to limit  ", dim),
        Span::styled("█", bad),
        Span::styled(" over request  ", dim),
        Span::styled("█", over_prov),
        Span::styled(" <20% used", dim),
    ]));
    lines.push(Line::from(""));

    // ── Cluster/Namespace summary bars ───────────────────────
    {
        let eff_title = if ns_filter.is_some() {
            "Namespace Summary"
        } else {
            "Cluster Summary"
        };
        lines.extend(section_heading(eff_title, heading, dim));

        let max_cpu = total_cpu.max(total_cpu_req).max(total_cpu_lim).max(0.001);
        let max_mem = total_mem.max(total_mem_req).max(total_mem_lim).max(1.0);

        // CPU bar
        let mut s = vec![Span::styled("  CPU    ", label)];
        s.extend(stacked_bar(total_cpu, total_cpu_req, total_cpu_lim, max_cpu, bar_w));
        s.push(Span::styled(
            format!(" {} used", crate::k8s::format_cpu_cores(total_cpu),),
            value,
        ));
        if total_cpu_req > 0.0 {
            let pct = (total_cpu / total_cpu_req * 100.0) as usize;
            s.push(Span::styled(
                format!(" / {} req ({}%)", crate::k8s::format_cpu_cores(total_cpu_req), pct),
                dim,
            ));
        }
        if total_cpu_lim > 0.0 {
            s.push(Span::styled(
                format!(" / {} lim", crate::k8s::format_cpu_cores(total_cpu_lim)),
                dim,
            ));
        }
        lines.push(Line::from(s));

        // Memory bar
        let mut s = vec![Span::styled("  Memory ", label)];
        s.extend(stacked_bar(total_mem, total_mem_req, total_mem_lim, max_mem, bar_w));
        s.push(Span::styled(
            format!(" {} used", crate::k8s::format_memory_gb_from_bytes(total_mem)),
            value,
        ));
        if total_mem_req > 0.0 {
            let pct = (total_mem / total_mem_req * 100.0) as usize;
            s.push(Span::styled(
                format!(
                    " / {} req ({}%)",
                    crate::k8s::format_memory_gb_from_bytes(total_mem_req),
                    pct
                ),
                dim,
            ));
        }
        if total_mem_lim > 0.0 {
            s.push(Span::styled(
                format!(" / {} lim", crate::k8s::format_memory_gb_from_bytes(total_mem_lim)),
                dim,
            ));
        }
        lines.push(Line::from(s));
        lines.push(Line::from(""));
    }

    // ── Workload stacked bar chart ───────────────────────────
    if !filtered_workloads.is_empty() {
        let wl_filter = &app.resource_map.filter;
        let mut wls: Vec<&crate::k8s::WorkloadMetrics> = filtered_workloads
            .iter()
            .filter(|w| {
                wl_filter.is_empty()
                    || w.name.contains(wl_filter.as_str())
                    || w.namespace.contains(wl_filter.as_str())
                    || w.kind.contains(wl_filter.as_str())
            })
            .copied()
            .collect();
        match sort {
            | ResourceMapSort::Cpu => {
                wls.sort_by(|a, b| {
                    b.cpu_usage
                        .partial_cmp(&a.cpu_usage)
                        .unwrap_or(std::cmp::Ordering::Equal)
                })
            },
            | ResourceMapSort::Memory => {
                wls.sort_by(|a, b| {
                    b.mem_usage
                        .partial_cmp(&a.mem_usage)
                        .unwrap_or(std::cmp::Ordering::Equal)
                })
            },
        }

        // Find max for scaling all bars to the same axis
        let (max_bar_val, fmt_use): (f64, Box<dyn Fn(f64) -> String>) = match sort {
            | ResourceMapSort::Cpu => {
                let m = wls
                    .iter()
                    .map(|w| w.cpu_usage.max(w.cpu_request).max(w.cpu_limit))
                    .fold(0.0f64, f64::max);
                (m, Box::new(crate::k8s::format_cpu_cores) as Box<dyn Fn(f64) -> String>)
            },
            | ResourceMapSort::Memory => {
                let m = wls
                    .iter()
                    .map(|w| w.mem_usage.max(w.mem_request).max(w.mem_limit))
                    .fold(0.0f64, f64::max);
                (
                    m,
                    Box::new(crate::k8s::format_memory_gb_from_bytes) as Box<dyn Fn(f64) -> String>,
                )
            },
        };

        let wl_focused = matches!(app.resource_map.focus, super::app::ResourceMapFocus::Workloads { .. });
        let wl_cursor = match app.resource_map.focus {
            | super::app::ResourceMapFocus::Workloads { cursor } => Some(cursor),
            | _ => None,
        };
        let show_n = if wl_focused { wls.len() } else { 20.min(wls.len()) };

        let focus_hint = if wl_focused {
            " (w/Esc exit  / filter  x clear)"
        } else {
            " (w to focus)"
        };
        let wl_total = filtered_workloads.len();
        lines.extend(section_heading(
            &format!(
                "Workload {sort_label} — usage vs request vs limit ({}/{}){focus_hint}",
                wls.len(),
                wl_total
            ),
            heading,
            dim,
        ));

        // Filter bar
        if wl_focused {
            if app.resource_map.filter_editing {
                lines.push(Line::from(vec![
                    Span::styled("  /", label),
                    Span::styled(
                        format!("{}_", app.resource_map.filter_buf),
                        Style::default().fg(Color::White).add_modifier(Modifier::UNDERLINED),
                    ),
                ]));
            } else if !app.resource_map.filter.is_empty() {
                lines.push(Line::from(vec![
                    Span::styled("  filter: ", dim),
                    Span::styled(app.resource_map.filter.clone(), label),
                    Span::styled(
                        format!("  ({} match{})", wls.len(), if wls.len() == 1 { "" } else { "es" }),
                        dim,
                    ),
                ]));
            }
        }

        // Track line index where the workload list starts so we can auto-scroll
        let wl_section_start = lines.len();

        let name_w = 24;
        for (i, wl) in wls.iter().enumerate().take(show_n) {
            let (usage, request, limit) = match sort {
                | ResourceMapSort::Cpu => (wl.cpu_usage, wl.cpu_request, wl.cpu_limit),
                | ResourceMapSort::Memory => (wl.mem_usage, wl.mem_request, wl.mem_limit),
            };

            let kind_short = match wl.kind.as_str() {
                | "Deployment" => "dply",
                | "StatefulSet" => "sset",
                | "DaemonSet" => "ds",
                | "Job" => "job",
                | "Pod" => "pod",
                | k => &k[..4.min(k.len())],
            };

            let disp_name = format!("{}/{}", kind_short, truncate(&wl.name, name_w - kind_short.len() - 1));
            let pct_str = if request > 0.0 {
                format!("{}%", (usage / request * 100.0) as usize)
            } else {
                "–".into()
            };

            let is_selected = wl_cursor == Some(i);
            let row_style = if is_selected {
                Style::default().bg(Color::DarkGray)
            } else {
                Style::default()
            };
            let name_style = if is_selected {
                Style::default()
                    .fg(Color::Yellow)
                    .bg(Color::DarkGray)
                    .add_modifier(Modifier::BOLD)
            } else {
                value
            };

            let prefix = if is_selected { "▸ " } else { "  " };

            let mut s: Vec<Span> = vec![
                Span::styled(
                    prefix,
                    if is_selected {
                        Style::default().fg(Color::Yellow).bg(Color::DarkGray)
                    } else {
                        dim
                    },
                ),
                Span::styled(format!("{:<w$}", truncate(&disp_name, name_w), w = name_w), name_style),
            ];
            // Stacked bar — apply bg highlight if selected
            for mut span in stacked_bar(usage, request, limit, max_bar_val, bar_w) {
                if is_selected {
                    span.style = span.style.bg(Color::DarkGray);
                }
                s.push(span);
            }
            s.push(Span::styled(
                format!(" {} ", fmt_use(usage)),
                if is_selected { name_style } else { value },
            ));
            s.push(Span::styled(
                format!("({pct_str})"),
                if is_selected {
                    row_style.fg(Color::DarkGray)
                } else {
                    dim
                },
            ));
            lines.push(Line::from(s));

            // If selected, show expanded detail below
            if is_selected {
                let cpu_use = crate::k8s::format_cpu_cores(wl.cpu_usage);
                let cpu_req = crate::k8s::format_cpu_cores(wl.cpu_request);
                let cpu_lim = crate::k8s::format_cpu_cores(wl.cpu_limit);
                let mem_use = crate::k8s::format_memory_gb_from_bytes(wl.mem_usage);
                let mem_req = crate::k8s::format_memory_gb_from_bytes(wl.mem_request);
                let mem_lim = crate::k8s::format_memory_gb_from_bytes(wl.mem_limit);

                let detail_dim = Style::default().fg(Color::DarkGray);
                let detail_lbl = Style::default().fg(Color::Cyan);
                let detail_val = Style::default().fg(Color::White);

                lines.push(Line::from(vec![
                    Span::styled("    ", detail_dim),
                    Span::styled(format!("{} ", wl.kind), detail_dim),
                    Span::styled(wl.name.clone(), detail_val),
                    Span::styled(format!("  ns:{}", wl.namespace), detail_dim),
                    Span::styled(format!("  pods:{}", wl.pod_count), detail_dim),
                ]));
                lines.push(Line::from(vec![
                    Span::styled("    ", detail_dim),
                    Span::styled("cpu  ", detail_lbl),
                    Span::styled(format!("use:{cpu_use}  "), detail_val),
                    Span::styled(format!("req:{cpu_req}  "), detail_dim),
                    Span::styled(format!("lim:{cpu_lim}  "), detail_dim),
                    if wl.cpu_request > 0.0 {
                        let pct = (wl.cpu_usage / wl.cpu_request * 100.0) as usize;
                        Span::styled(
                            format!("({pct}% of req)"),
                            if pct > 100 {
                                bad
                            } else if pct < 20 {
                                over_prov
                            } else {
                                good
                            },
                        )
                    } else {
                        Span::styled("(no request)", warn)
                    },
                ]));
                lines.push(Line::from(vec![
                    Span::styled("    ", detail_dim),
                    Span::styled("mem  ", detail_lbl),
                    Span::styled(format!("use:{mem_use}  "), detail_val),
                    Span::styled(format!("req:{mem_req}  "), detail_dim),
                    Span::styled(format!("lim:{mem_lim}  "), detail_dim),
                    if wl.mem_request > 0.0 {
                        let pct = (wl.mem_usage / wl.mem_request * 100.0) as usize;
                        Span::styled(
                            format!("({pct}% of req)"),
                            if pct > 100 {
                                bad
                            } else if pct < 20 {
                                over_prov
                            } else {
                                good
                            },
                        )
                    } else {
                        Span::styled("(no request)", warn)
                    },
                ]));
                // Right-sizing suggestion
                if wl.cpu_request > 0.0 && wl.cpu_usage < wl.cpu_request * 0.3 {
                    let suggested = (wl.cpu_usage * 2.0).max(0.01);
                    lines.push(Line::from(vec![
                        Span::styled("    → ", label),
                        Span::styled(
                            format!(
                                "CPU request could be lowered to {}",
                                crate::k8s::format_cpu_cores(suggested)
                            ),
                            over_prov,
                        ),
                    ]));
                }
                if wl.cpu_limit > 0.0 && wl.cpu_usage > wl.cpu_limit * 0.9 {
                    let suggested = wl.cpu_usage * 1.5;
                    lines.push(Line::from(vec![
                        Span::styled("    → ", label),
                        Span::styled(
                            format!(
                                "CPU limit should be raised to {}",
                                crate::k8s::format_cpu_cores(suggested)
                            ),
                            bad,
                        ),
                    ]));
                }
                if wl.mem_request > 0.0 && wl.mem_usage < wl.mem_request * 0.3 {
                    let suggested = (wl.mem_usage * 2.0).max(64.0 * 1024.0 * 1024.0);
                    lines.push(Line::from(vec![
                        Span::styled("    → ", label),
                        Span::styled(
                            format!(
                                "Memory request could be lowered to {}",
                                crate::k8s::format_memory_gb_from_bytes(suggested)
                            ),
                            over_prov,
                        ),
                    ]));
                }
                if wl.mem_limit > 0.0 && wl.mem_usage > wl.mem_limit * 0.9 {
                    let suggested = wl.mem_usage * 1.5;
                    lines.push(Line::from(vec![
                        Span::styled("    → ", label),
                        Span::styled(
                            format!(
                                "Memory limit should be raised to {}",
                                crate::k8s::format_memory_gb_from_bytes(suggested)
                            ),
                            bad,
                        ),
                    ]));
                }
            }
        }
        if !wl_focused && wls.len() > show_n {
            lines.push(Line::from(Span::styled(
                format!("  ... and {} more (press w to see all)", wls.len() - show_n),
                dim,
            )));
        }
        lines.push(Line::from(""));

        // Auto-scroll to keep selected workload visible
        if let Some(cursor_idx) = wl_cursor {
            // Each selected row expands to ~5 extra lines, non-selected is 1 line
            let cursor_line = wl_section_start + cursor_idx.min(show_n);
            // Add the scroll position of the panel (area height minus border)
            let visible_h = area.height.saturating_sub(2) as usize;
            if cursor_line >= app.resource_map.scroll as usize + visible_h {
                app.resource_map.scroll = (cursor_line.saturating_sub(visible_h / 2)) as u16;
            } else if cursor_line < app.resource_map.scroll as usize {
                app.resource_map.scroll = cursor_line.saturating_sub(2) as u16;
            }
        }
    }

    // ── Namespace treemap ────────────────────────────────────
    // Shows proportional blocks per namespace, stacked horizontally like a
    // real treemap row. Each namespace gets a colored block sized by its
    // resource usage share.
    {
        let mut ns_map: std::collections::BTreeMap<&str, (f64, f64, usize)> = std::collections::BTreeMap::new();
        for pod in &filtered_pods {
            let e = ns_map.entry(pod.namespace.as_str()).or_insert((0.0, 0.0, 0));
            e.0 += pod.cpu_cores;
            e.1 += pod.memory_bytes;
            e.2 += 1;
        }

        let mut ns_list: Vec<(&str, f64, f64, usize)> = ns_map
            .into_iter()
            .map(|(ns, (cpu, mem, count))| (ns, cpu, mem, count))
            .collect();

        match sort {
            | ResourceMapSort::Cpu => {
                ns_list.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal))
            },
            | ResourceMapSort::Memory => {
                ns_list.sort_by(|a, b| b.2.partial_cmp(&a.2).unwrap_or(std::cmp::Ordering::Equal))
            },
        }

        if ns_list.len() > 1 && inner_w > 20 {
            lines.extend(section_heading(
                &format!("Namespace {sort_label} Treemap"),
                heading,
                dim,
            ));

            let total_val = match sort {
                | ResourceMapSort::Cpu => total_cpu,
                | ResourceMapSort::Memory => total_mem,
            };

            // Top row: colored blocks proportional to usage
            let map_w = inner_w.saturating_sub(2);
            let mut block_spans: Vec<Span> = vec![Span::styled(" ", dim)];
            let mut label_entries: Vec<(&str, usize, Color)> = Vec::new();

            for (ns, cpu, mem, _) in &ns_list {
                let ns_val = match sort {
                    | ResourceMapSort::Cpu => *cpu,
                    | ResourceMapSort::Memory => *mem,
                };
                let w = ((ns_val / total_val) * map_w as f64).round().max(1.0) as usize;
                let w = w.min(
                    map_w.saturating_sub(block_spans.iter().map(|s| s.content.chars().count()).sum::<usize>() - 1),
                );
                if w == 0 {
                    continue;
                }
                let color = name_color(ns);
                block_spans.push(Span::styled("█".repeat(w), Style::default().fg(color)));
                label_entries.push((ns, w, color));
            }
            lines.push(Line::from(block_spans));

            // Label row below the bar
            let mut lbl_spans: Vec<Span> = vec![Span::styled(" ", dim)];
            for (ns, w, color) in &label_entries {
                let ns_short = truncate(ns, w.saturating_sub(1));
                lbl_spans.push(Span::styled(
                    format!("{:<w$}", ns_short, w = w),
                    Style::default().fg(*color),
                ));
            }
            lines.push(Line::from(lbl_spans));
            lines.push(Line::from(""));

            // Detail rows per namespace
            for (ns, cpu, mem, count) in &ns_list {
                let ns_val = match sort {
                    | ResourceMapSort::Cpu => *cpu,
                    | ResourceMapSort::Memory => *mem,
                };
                let pct = (ns_val / total_val * 100.0) as usize;
                if pct == 0 {
                    continue;
                }
                let color = name_color(ns);
                let val_str = match sort {
                    | ResourceMapSort::Cpu => crate::k8s::format_cpu_cores(*cpu),
                    | ResourceMapSort::Memory => crate::k8s::format_memory_gb_from_bytes(*mem),
                };
                lines.push(Line::from(vec![
                    Span::styled("  █ ", Style::default().fg(color)),
                    Span::styled(
                        format!("{:<20}", ns),
                        Style::default().fg(color).add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(format!("{:>5}%  ", pct), value),
                    Span::styled(val_str, dim),
                    Span::styled(format!("  ({count} pods)"), dim),
                ]));
            }
            lines.push(Line::from(""));
        }
    }

    // ── Optimization recommendations ─────────────────────────
    {
        let over_limit_cpu: Vec<_> = filtered_pods
            .iter()
            .filter(|p| p.spec.cpu_limit > 0.0 && p.cpu_cores > p.spec.cpu_limit)
            .collect();
        let over_limit_mem: Vec<_> = filtered_pods
            .iter()
            .filter(|p| p.spec.mem_limit > 0.0 && p.memory_bytes > p.spec.mem_limit)
            .collect();
        let over_req_cpu: Vec<_> = filtered_pods
            .iter()
            .filter(|p| p.spec.cpu_request > 0.0 && p.cpu_cores > p.spec.cpu_request * 1.2)
            .collect();
        let under_cpu: Vec<_> = filtered_pods
            .iter()
            .filter(|p| p.spec.cpu_request > 0.0 && p.cpu_cores < p.spec.cpu_request * 0.2)
            .collect();
        let under_mem: Vec<_> = filtered_pods
            .iter()
            .filter(|p| p.spec.mem_request > 0.0 && p.memory_bytes < p.spec.mem_request * 0.2)
            .collect();
        let no_req: Vec<_> = filtered_pods
            .iter()
            .filter(|p| p.spec.cpu_request == 0.0 && p.spec.mem_request == 0.0)
            .collect();
        let no_lim: Vec<_> = filtered_pods
            .iter()
            .filter(|p| p.spec.cpu_limit == 0.0 && p.spec.mem_limit == 0.0)
            .collect();

        let has_any = !over_limit_cpu.is_empty()
            || !over_limit_mem.is_empty()
            || !over_req_cpu.is_empty()
            || !under_cpu.is_empty()
            || !under_mem.is_empty()
            || !no_req.is_empty()
            || !no_lim.is_empty();

        if has_any {
            lines.extend(section_heading("Optimization Recommendations", heading, dim));

            // Critical: over limits
            if !over_limit_cpu.is_empty() {
                lines.push(Line::from(vec![
                    Span::styled("  ", dim),
                    Span::styled(
                        "CRITICAL  ",
                        Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(
                        format!("{} pod(s) exceeding CPU limit — being throttled", over_limit_cpu.len()),
                        bad,
                    ),
                ]));
                for p in over_limit_cpu.iter().take(5) {
                    lines.push(Line::from(vec![
                        Span::styled("    ", dim),
                        Span::styled(format!("{:<40}", truncate(&p.owner, 38)), value),
                        Span::styled(
                            format!(
                                "using {} / {} limit",
                                crate::k8s::format_cpu_cores(p.cpu_cores),
                                crate::k8s::format_cpu_cores(p.spec.cpu_limit)
                            ),
                            bad,
                        ),
                        Span::styled(
                            format!("  → raise limit to {}", crate::k8s::format_cpu_cores(p.cpu_cores * 1.3)),
                            warn,
                        ),
                    ]));
                }
            }
            if !over_limit_mem.is_empty() {
                lines.push(Line::from(vec![
                    Span::styled("  ", dim),
                    Span::styled(
                        "CRITICAL  ",
                        Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(
                        format!("{} pod(s) exceeding memory limit — OOMKill risk", over_limit_mem.len()),
                        bad,
                    ),
                ]));
                for p in over_limit_mem.iter().take(5) {
                    lines.push(Line::from(vec![
                        Span::styled("    ", dim),
                        Span::styled(format!("{:<40}", truncate(&p.owner, 38)), value),
                        Span::styled(
                            format!(
                                "using {} / {} limit",
                                crate::k8s::format_memory_gb_from_bytes(p.memory_bytes),
                                crate::k8s::format_memory_gb_from_bytes(p.spec.mem_limit)
                            ),
                            bad,
                        ),
                        Span::styled(
                            format!(
                                "  → raise limit to {}",
                                crate::k8s::format_memory_gb_from_bytes(p.memory_bytes * 1.3)
                            ),
                            warn,
                        ),
                    ]));
                }
            }

            // Warning: over request
            if !over_req_cpu.is_empty() {
                lines.push(Line::from(vec![
                    Span::styled("  ", dim),
                    Span::styled(
                        "WARNING   ",
                        Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(
                        format!(
                            "{} pod(s) exceeding CPU request by >20% — may be evicted",
                            over_req_cpu.len()
                        ),
                        warn,
                    ),
                ]));
            }

            // Cost: over-provisioned
            if !under_cpu.is_empty() {
                let wasted: f64 = under_cpu
                    .iter()
                    .map(|p| p.spec.cpu_request * 0.8 - p.cpu_cores)
                    .sum::<f64>()
                    .max(0.0);
                lines.push(Line::from(vec![
                    Span::styled("  ", dim),
                    Span::styled(
                        "SAVINGS   ",
                        Style::default().fg(Color::Blue).add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(
                        format!(
                            "{} pod(s) using <20% of CPU request — {} cores reclaimable",
                            under_cpu.len(),
                            crate::k8s::format_cpu_cores(wasted)
                        ),
                        over_prov,
                    ),
                ]));
                // Show top wasters
                let mut wasters = under_cpu.clone();
                wasters.sort_by(|a, b| {
                    let aw = a.spec.cpu_request - a.cpu_cores;
                    let bw = b.spec.cpu_request - b.cpu_cores;
                    bw.partial_cmp(&aw).unwrap_or(std::cmp::Ordering::Equal)
                });
                for p in wasters.iter().take(5) {
                    let suggested = (p.cpu_cores * 2.0).max(0.01); // 2x headroom
                    lines.push(Line::from(vec![
                        Span::styled("    ", dim),
                        Span::styled(format!("{:<40}", truncate(&p.owner, 38)), value),
                        Span::styled(
                            format!(
                                "using {} / {} req",
                                crate::k8s::format_cpu_cores(p.cpu_cores),
                                crate::k8s::format_cpu_cores(p.spec.cpu_request)
                            ),
                            over_prov,
                        ),
                        Span::styled(
                            format!("  → lower to {}", crate::k8s::format_cpu_cores(suggested)),
                            label,
                        ),
                    ]));
                }
            }
            if !under_mem.is_empty() {
                let wasted: f64 = under_mem
                    .iter()
                    .map(|p| p.spec.mem_request * 0.8 - p.memory_bytes)
                    .sum::<f64>()
                    .max(0.0);
                lines.push(Line::from(vec![
                    Span::styled("  ", dim),
                    Span::styled(
                        "SAVINGS   ",
                        Style::default().fg(Color::Blue).add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(
                        format!(
                            "{} pod(s) using <20% of memory request — {} reclaimable",
                            under_mem.len(),
                            crate::k8s::format_memory_gb_from_bytes(wasted)
                        ),
                        over_prov,
                    ),
                ]));
                let mut wasters = under_mem.clone();
                wasters.sort_by(|a, b| {
                    let aw = a.spec.mem_request - a.memory_bytes;
                    let bw = b.spec.mem_request - b.memory_bytes;
                    bw.partial_cmp(&aw).unwrap_or(std::cmp::Ordering::Equal)
                });
                for p in wasters.iter().take(5) {
                    let suggested = (p.memory_bytes * 2.0).max(64.0 * 1024.0 * 1024.0);
                    lines.push(Line::from(vec![
                        Span::styled("    ", dim),
                        Span::styled(format!("{:<40}", truncate(&p.owner, 38)), value),
                        Span::styled(
                            format!(
                                "using {} / {} req",
                                crate::k8s::format_memory_gb_from_bytes(p.memory_bytes),
                                crate::k8s::format_memory_gb_from_bytes(p.spec.mem_request)
                            ),
                            over_prov,
                        ),
                        Span::styled(
                            format!("  → lower to {}", crate::k8s::format_memory_gb_from_bytes(suggested)),
                            label,
                        ),
                    ]));
                }
            }

            // Missing requests/limits
            if !no_req.is_empty() {
                lines.push(Line::from(vec![
                    Span::styled("  ", dim),
                    Span::styled(
                        "CONFIG    ",
                        Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(
                        format!(
                            "{} pod(s) with no resource requests — BestEffort QoS, evicted first",
                            no_req.len()
                        ),
                        warn,
                    ),
                ]));
            }
            if !no_lim.is_empty() {
                lines.push(Line::from(vec![
                    Span::styled("  ", dim),
                    Span::styled(
                        "CONFIG    ",
                        Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(
                        format!(
                            "{} pod(s) with no resource limits — can consume unbounded resources",
                            no_lim.len()
                        ),
                        warn,
                    ),
                ]));
            }
            lines.push(Line::from(""));
        }
    }

    let paragraph = Paragraph::new(lines)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(border_color))
                .title(" Resource Map "),
        )
        .scroll((app.resource_map.scroll, 0));
    f.render_widget(paragraph, area);
}

fn render_resource_filter_bar(f: &mut Frame, app: &App, area: Rect) {
    let visible = app.visible_resource_indices();
    let total = app.resources.entries.len();
    let count_suffix = if visible.len() < total {
        format!(" ({}/{})", visible.len(), total)
    } else {
        String::new()
    };

    let display = if app.resources.filter.editing {
        format!(" /{}▏{}", app.resources.filter.buf, count_suffix)
    } else {
        format!(" /{}/{}", app.resources.filter.text, count_suffix)
    };
    let style = if app.resources.filter.editing {
        Style::default().fg(Color::Yellow)
    } else {
        Style::default().fg(Color::DarkGray)
    };
    f.render_widget(Paragraph::new(Line::from(Span::styled(display, style))), area);
}

// ---------------------------------------------------------------------------
// Events log view (log-style rendering for events)
// ---------------------------------------------------------------------------

/// Known event reasons that indicate trouble — rendered with warning style.
fn is_warning_reason(reason: &str) -> bool {
    matches!(
        reason,
        "BackOff"
            | "Failed"
            | "FailedScheduling"
            | "FailedMount"
            | "FailedAttachVolume"
            | "FailedCreate"
            | "Unhealthy"
            | "Evicted"
            | "OOMKilling"
            | "ExceededGracePeriod"
            | "NodeNotReady"
            | "Rebooted"
            | "FailedSync"
            | "FailedValidation"
    )
}

fn render_events_log(f: &mut Frame, app: &mut App, area: Rect) {
    let focused = app.focus == Focus::Resources;
    let border_color = if focused { Color::Cyan } else { Color::DarkGray };
    let inner_height = area.height.saturating_sub(2) as usize; // minus top/bottom borders
    let all_ns = app.kube.context.namespace.is_none();
    let visible_indices = app.visible_resource_indices();
    let total = visible_indices.len();
    let cursor = app.events.cursor.min(total.saturating_sub(1));
    app.events.cursor = cursor;

    // Auto-scroll keeps cursor at the bottom (newest event)
    if app.events.auto_scroll && total > 0 {
        app.events.cursor = total - 1;
    }

    // Scroll follows cursor: ensure cursor is always visible
    let mut scroll = app.events.scroll;
    if app.events.cursor < scroll {
        scroll = app.events.cursor;
    } else if inner_height > 0 && app.events.cursor >= scroll + inner_height {
        scroll = app.events.cursor - inner_height + 1;
    }
    scroll = scroll.min(total.saturating_sub(inner_height));
    app.events.scroll = scroll;

    let lines: Vec<Line> = visible_indices
        .iter()
        .enumerate()
        .skip(scroll)
        .take(inner_height)
        .map(|(vis_idx, &real_idx)| {
            let entry = &app.resources.entries[real_idx];
            let is_selected = vis_idx == app.events.cursor;
            // columns: [0]=TYPE  [1]=REASON  [2]=OBJECT  [3]=AGE  [4]=MESSAGE  [5]=COUNT
            let event_type = entry.columns.first().map(|s| s.as_str()).unwrap_or("");
            let reason = entry.columns.get(1).map(|s| s.as_str()).unwrap_or("");
            let object = entry.columns.get(2).map(|s| s.as_str()).unwrap_or("");
            let age = entry.columns.get(3).map(|s| s.as_str()).unwrap_or("");
            let message = entry.columns.get(4).map(|s| s.as_str()).unwrap_or("");
            let count: i32 = entry.columns.get(5).and_then(|s| s.parse().ok()).unwrap_or(1);

            let is_warning = event_type == "Warning";
            let is_known_bad = is_warning_reason(reason);

            // Type indicator with icon
            let (type_icon, type_color) = if is_warning {
                ("⚠ ", Color::Yellow)
            } else {
                ("● ", Color::Green)
            };

            // Reason color: red for known-bad reasons, bold white otherwise
            let reason_style = if is_known_bad {
                Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)
            } else if is_warning {
                Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::White).add_modifier(Modifier::BOLD)
            };

            // Message color: dimmer for Normal, brighter for Warning
            let msg_color = if is_warning { Color::White } else { Color::Gray };

            let mut spans = vec![
                Span::styled(format!("{:>5} ", age), Style::default().fg(Color::DarkGray)),
                Span::styled(type_icon, Style::default().fg(type_color)),
            ];

            // Namespace prefix when in all-namespaces mode
            if all_ns && !entry.namespace.is_empty() {
                spans.push(Span::styled(
                    format!("{}/", entry.namespace),
                    Style::default().fg(Color::DarkGray),
                ));
            }

            spans.push(Span::styled(
                format!("{:<32}", object),
                Style::default().fg(Color::Cyan),
            ));
            spans.push(Span::styled(reason.to_string(), reason_style));

            // Count badge for repeated events
            if count > 1 {
                spans.push(Span::styled(
                    format!(" (x{})", count),
                    Style::default().fg(Color::Magenta),
                ));
            }

            spans.push(Span::styled(format!("  {}", message), Style::default().fg(msg_color)));

            let mut line = Line::from(spans);
            if is_selected {
                line = line.style(Style::default().add_modifier(Modifier::REVERSED));
            }
            line
        })
        .collect();

    let visible_end = scroll + lines.len();
    let count_info = format!(" {}/{} ", visible_end, total);
    let paragraph = Paragraph::new(lines).block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(border_color))
            .title(" Events ")
            .title_bottom(count_info),
    );
    f.render_widget(paragraph, area);
}

// ---------------------------------------------------------------------------
// Detail view — dispatches between Smart and YAML mode
// ---------------------------------------------------------------------------

fn render_detail(f: &mut Frame, app: &mut App) {
    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // breadcrumb
            Constraint::Min(3),    // content
            Constraint::Length(1), // hotkey bar
        ])
        .split(f.area());

    render_breadcrumb(f, app, outer[0]);

    let mode_label = match app.detail.mode {
        | DetailMode::Smart => "Smart",
        | DetailMode::Yaml => "YAML",
    };
    let title = format!(" [{}] ", mode_label);

    let lines: Vec<Line> = match app.detail.mode {
        | DetailMode::Smart => render_smart_lines(app),
        | DetailMode::Yaml => render_yaml_lines(&app.detail.yaml),
    };

    // Store inner height for scroll-to-cursor calculations (minus 2 for borders)
    app.detail.area_height = outer[1].height.saturating_sub(2) as usize;

    let paragraph = Paragraph::new(lines)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Cyan))
                .title(title),
        )
        .scroll((app.detail.scroll, 0));
    f.render_widget(paragraph, outer[1]);

    let bar = build_hotkey_bar(app);
    f.render_widget(Paragraph::new(bar), outer[2]);
}

// ---------------------------------------------------------------------------
// Edit diff view
// ---------------------------------------------------------------------------

fn render_edit_diff(f: &mut Frame, app: &mut App) {
    use super::app::{
        DiffKind,
        DiffMode,
    };

    let ctx = match &app.edit_ctx {
        | Some(c) => c,
        | None => return,
    };

    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // breadcrumb
            Constraint::Min(3),    // diff content
            Constraint::Length(1), // hotkey bar
        ])
        .split(f.area());

    render_breadcrumb(f, app, outer[0]);

    // Diff content
    let added = ctx.diff_lines.iter().filter(|(k, _)| *k == DiffKind::Added).count();
    let removed = ctx.diff_lines.iter().filter(|(k, _)| *k == DiffKind::Removed).count();
    let summary = format!(" +{} -{} ", added, removed);

    match ctx.diff_mode {
        | DiffMode::Inline => {
            let lines: Vec<Line> = ctx
                .diff_lines
                .iter()
                .map(|(kind, text)| {
                    let style = match kind {
                        | DiffKind::Added => Style::default().fg(Color::Green),
                        | DiffKind::Removed => Style::default().fg(Color::Red),
                        | DiffKind::Context => Style::default().fg(Color::DarkGray),
                    };
                    Line::from(Span::styled(text.clone(), style))
                })
                .collect();

            let paragraph = Paragraph::new(lines)
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .border_style(Style::default().fg(Color::Cyan))
                        .title(" Diff ")
                        .title_bottom(summary),
                )
                .scroll((ctx.scroll, 0));
            f.render_widget(paragraph, outer[1]);
        },
        | DiffMode::SideBySide => {
            // Split the diff into left (removed/context) and right (added/context) columns
            let mut left: Vec<(DiffKind, String)> = Vec::new();
            let mut right: Vec<(DiffKind, String)> = Vec::new();

            let mut i = 0;
            let dl = &ctx.diff_lines;
            while i < dl.len() {
                match dl[i].0 {
                    | DiffKind::Context => {
                        let text = dl[i]
                            .1
                            .strip_prefix("  ")
                            .or_else(|| dl[i].1.strip_prefix(" "))
                            .unwrap_or(&dl[i].1)
                            .to_string();
                        left.push((DiffKind::Context, text.clone()));
                        right.push((DiffKind::Context, text));
                        i += 1;
                    },
                    | DiffKind::Removed => {
                        let mut removes = Vec::new();
                        while i < dl.len() && dl[i].0 == DiffKind::Removed {
                            removes.push(
                                dl[i]
                                    .1
                                    .strip_prefix("- ")
                                    .or_else(|| dl[i].1.strip_prefix("-"))
                                    .unwrap_or(&dl[i].1)
                                    .to_string(),
                            );
                            i += 1;
                        }
                        let mut adds = Vec::new();
                        while i < dl.len() && dl[i].0 == DiffKind::Added {
                            adds.push(
                                dl[i]
                                    .1
                                    .strip_prefix("+ ")
                                    .or_else(|| dl[i].1.strip_prefix("+"))
                                    .unwrap_or(&dl[i].1)
                                    .to_string(),
                            );
                            i += 1;
                        }
                        // Pair them up
                        let max_len = removes.len().max(adds.len());
                        for j in 0..max_len {
                            left.push(
                                removes
                                    .get(j)
                                    .map(|s| (DiffKind::Removed, s.clone()))
                                    .unwrap_or((DiffKind::Context, String::new())),
                            );
                            right.push(
                                adds.get(j)
                                    .map(|s| (DiffKind::Added, s.clone()))
                                    .unwrap_or((DiffKind::Context, String::new())),
                            );
                        }
                    },
                    | DiffKind::Added => {
                        left.push((DiffKind::Context, String::new()));
                        right.push((
                            DiffKind::Added,
                            dl[i]
                                .1
                                .strip_prefix("+ ")
                                .or_else(|| dl[i].1.strip_prefix("+"))
                                .unwrap_or(&dl[i].1)
                                .to_string(),
                        ));
                        i += 1;
                    },
                }
            }

            // Render side-by-side
            let content_area = outer[1];
            let inner_w = content_area.width.saturating_sub(2) as usize; // minus borders
            let half_w = inner_w / 2;
            let sep_dim = Style::default().fg(Color::DarkGray);

            let lines: Vec<Line> = left
                .iter()
                .zip(right.iter())
                .map(|((lk, lt), (rk, rt))| {
                    let left_style = match lk {
                        | DiffKind::Removed => Style::default().fg(Color::Red),
                        | _ => Style::default().fg(Color::DarkGray),
                    };
                    let right_style = match rk {
                        | DiffKind::Added => Style::default().fg(Color::Green),
                        | _ => Style::default().fg(Color::DarkGray),
                    };
                    // Truncate/pad each side to half width
                    let left_text = if lt.len() > half_w.saturating_sub(1) {
                        format!("{:.w$}", lt, w = half_w.saturating_sub(1))
                    } else {
                        format!("{:<w$}", lt, w = half_w.saturating_sub(1))
                    };
                    let right_text = if rt.len() > half_w {
                        format!("{:.w$}", rt, w = half_w)
                    } else {
                        format!("{:<w$}", rt, w = half_w)
                    };
                    Line::from(vec![
                        Span::styled(left_text, left_style),
                        Span::styled("│", sep_dim),
                        Span::styled(right_text, right_style),
                    ])
                })
                .collect();

            let paragraph = Paragraph::new(lines)
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .border_style(Style::default().fg(Color::Cyan))
                        .title(" Diff — Side by Side ")
                        .title_bottom(summary),
                )
                .scroll((ctx.scroll, 0));
            f.render_widget(paragraph, content_area);
        },
    }

    // Hotkey bar
    let bar = build_hotkey_bar(app);
    f.render_widget(Paragraph::new(bar), outer[2]);
}

fn render_smart_lines(app: &mut App) -> Vec<Line<'static>> {
    let mut ds = smart::DictState {
        entries: Vec::new(),
        line_offsets: Vec::new(),
        cursor: app.detail.dict.cursor,
        expanded: app.detail.dict.expanded_keys.clone(),
    };
    let lines = if let Some(rt) = app.selected_resource_type() {
        smart::render(rt, &app.detail.value, app.detail.secret.as_mut(), &mut ds)
    } else if let Some(crd) = app.selected_crd_info().cloned() {
        smart::render_custom_resource(&app.detail.value, &crd, &mut ds)
    } else {
        return vec![];
    };
    // Sync state back to App
    app.detail.dict.entries = ds.entries;
    app.detail.dict.line_offsets = ds.line_offsets;
    app.detail.dict.expanded_keys = ds.expanded;
    // Clamp cursor if entries changed
    if let Some(c) = app.detail.dict.cursor {
        if c >= app.detail.dict.entries.len() {
            app.detail.dict.cursor = if app.detail.dict.entries.is_empty() {
                None
            } else {
                Some(app.detail.dict.entries.len() - 1)
            };
        }
    }

    let mut all_lines = lines;

    // Related resources — tabbed by category
    if !app.detail.related.resources.is_empty() {
        all_lines.push(Line::from(""));

        // Tab bar
        let cats = app.related_categories();
        let in_related = app.detail.related.cursor.is_some();
        let mut tab_spans: Vec<Span> = vec![Span::styled("  ", Style::default())];
        for (ci, cat) in cats.iter().enumerate() {
            let count = app
                .detail
                .related
                .resources
                .iter()
                .filter(|r| r.category == *cat)
                .count();
            let is_active = ci == app.detail.related.tab;
            let style = if in_related && is_active {
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Cyan)
                    .add_modifier(Modifier::BOLD)
            } else if is_active {
                Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::DarkGray)
            };
            tab_spans.push(Span::styled(format!(" {} ({}) ", cat, count), style));
            if ci + 1 < cats.len() {
                tab_spans.push(Span::styled("│", Style::default().fg(Color::DarkGray)));
            }
        }
        if !in_related {
            tab_spans.push(Span::styled("   [Tab] select", Style::default().fg(Color::DarkGray)));
        } else {
            tab_spans.push(Span::styled(
                "   ◀▶ switch  Enter=open",
                Style::default().fg(Color::DarkGray),
            ));
        }
        all_lines.push(Line::from(tab_spans));

        // Items for current tab only
        app.detail.related.line_start = all_lines.len();
        let tab_indices = app.related_tab_indices();
        for &idx in &tab_indices {
            let rel = &app.detail.related.resources[idx];
            let is_selected = app.detail.related.cursor == Some(idx);
            let marker = if is_selected { "▶ " } else { "  " };
            let type_name = rel.resource_type.singular_name();

            if is_selected {
                all_lines.push(Line::from(Span::styled(
                    format!("  {}{}/{}  ({})", marker, type_name, rel.name, rel.info),
                    Style::default().fg(Color::Cyan).add_modifier(Modifier::REVERSED),
                )));
            } else {
                all_lines.push(Line::from(vec![
                    Span::styled(format!("  {}", marker), Style::default()),
                    Span::styled(format!("{}/", type_name), Style::default().fg(Color::Magenta)),
                    Span::styled(rel.name.clone(), Style::default().fg(Color::White)),
                    Span::styled(format!("  ({})", rel.info), Style::default().fg(Color::DarkGray)),
                ]));
            }
        }
    }

    // Related events (describe-style)
    if !app.detail.related.events.is_empty() {
        all_lines.push(Line::from(""));
        all_lines.push(Line::from(Span::styled(
            "Events:",
            Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD),
        )));

        all_lines.push(Line::from(vec![
            Span::styled(
                format!("  {:<8} {:<6} {:<18} ", "AGE", "COUNT", "REASON"),
                Style::default().fg(Color::DarkGray),
            ),
            Span::styled("MESSAGE", Style::default().fg(Color::DarkGray)),
        ]));

        for ev in &app.detail.related.events {
            let type_style = if ev.type_ == "Warning" {
                Style::default().fg(Color::Yellow)
            } else {
                Style::default().fg(Color::Green)
            };
            let icon = if ev.type_ == "Warning" { "⚠" } else { "●" };
            let count_str = if ev.count > 1 {
                format!("x{}", ev.count)
            } else {
                String::new()
            };
            all_lines.push(Line::from(vec![
                Span::styled(format!("  {:<8} ", ev.last_seen), Style::default().fg(Color::DarkGray)),
                Span::styled(format!("{:<6} ", count_str), Style::default().fg(Color::White)),
                Span::styled(format!("{} {:<16} ", icon, ev.reason), type_style),
                Span::styled(ev.message.clone(), Style::default().fg(Color::White)),
            ]));
        }
    }

    all_lines
}

fn render_yaml_lines(yaml: &str) -> Vec<Line<'_>> {
    yaml.lines()
        .map(|l| {
            if l.starts_with("---") {
                Line::from(Span::styled(l, Style::default().fg(Color::DarkGray)))
            } else if l.contains(':') && !l.trim_start().starts_with('-') {
                let parts: Vec<&str> = l.splitn(2, ':').collect();
                if parts.len() == 2 {
                    Line::from(vec![
                        Span::styled(parts[0], Style::default().fg(Color::Cyan)),
                        Span::styled(":", Style::default().fg(Color::DarkGray)),
                        Span::styled(parts[1], Style::default().fg(Color::White)),
                    ])
                } else {
                    Line::from(l)
                }
            } else {
                Line::from(Span::styled(l, Style::default().fg(Color::White)))
            }
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Log view
// ---------------------------------------------------------------------------

fn render_logs(f: &mut Frame, app: &mut App) {
    let has_filter_bar = app
        .log
        .as_ref()
        .is_some_and(|state| state.filter.editing || !state.filter.text.is_empty());
    let filter_height = if has_filter_bar { 1 } else { 0 };

    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),             // breadcrumb
            Constraint::Length(1),             // log status line
            Constraint::Min(1),                // log content (no borders)
            Constraint::Length(filter_height), // filter bar
            Constraint::Length(1),             // hotkey bar
        ])
        .split(f.area());

    render_breadcrumb(f, app, outer[0]);

    if let Some(state) = &mut app.log {
        state.viewport_rows = outer[2].height as usize;
        state.viewport_cols = outer[2].width as usize;
        if state.auto_scroll {
            state.stick_to_tail();
        } else {
            state.clamp_navigation();
        }
    }
    let state = match &app.log {
        | Some(s) => s,
        | None => return,
    };

    // Status line: mode badges + line count
    {
        let dim = Style::default().fg(Color::DarkGray);
        let badge = Style::default().fg(Color::Cyan);
        let mut spans: Vec<Span> = Vec::new();
        if state.following {
            spans.push(Span::styled("[Following] ", badge));
        }
        if state.wrap {
            spans.push(Span::styled("[Wrap] ", badge));
        }
        let vis_count = state.visible_len();
        let cursor = state
            .selected_line
            .unwrap_or(state.scroll)
            .min(vis_count.saturating_sub(1));
        let line_info = if let Some((start, end)) = state.selection_range() {
            format!("{} selected  {}/{}", end - start + 1, cursor + 1, vis_count)
        } else if vis_count == 0 {
            "0/0".to_string()
        } else {
            format!("{}/{}", cursor + 1, vis_count)
        };
        let left_len: usize = spans.iter().map(|s| s.width()).sum();
        let pad = (outer[1].width as usize).saturating_sub(left_len + line_info.len());
        spans.push(Span::styled(" ".repeat(pad), dim));
        spans.push(Span::styled(line_info, dim));
        f.render_widget(Paragraph::new(Line::from(spans)), outer[1]);
    }

    // Log lines (filtered) — no borders for clean text selection
    let visible = state.visible_lines();
    let area_height = outer[2].height as usize;
    let sel_range = state.selection_range();

    // When wrapping, compute how many terminal rows each logical line takes
    let wrapped_heights: Vec<usize> = visible.iter().map(|line| state.display_rows(line)).collect();

    let scroll_offset = state.scroll.min(visible.len().saturating_sub(1));

    // Collect logical lines that fit in the viewport (accounting for wrap)
    let mut rows_used = 0;
    let lines: Vec<Line> = visible
        .iter()
        .enumerate()
        .skip(scroll_offset)
        .take_while(|(idx, _)| {
            let h = wrapped_heights.get(*idx).copied().unwrap_or(1);
            if rows_used >= area_height {
                return false;
            }
            rows_used += h;
            true
        })
        .map(|(idx, l)| {
            let is_cursor = state.selected_line == Some(idx);
            let is_in_selection = sel_range
                .map(|(start, end)| idx >= start && idx <= end)
                .unwrap_or(false);
            let pod_color = state.color_for_pod(&l.pod);
            if is_cursor {
                let style = Style::default().fg(Color::Cyan).add_modifier(Modifier::REVERSED);
                return Line::from(vec![
                    Span::styled(l.prefix(), style),
                    Span::styled(l.message.as_str(), style),
                ]);
            }
            if is_in_selection {
                let style = Style::default().fg(Color::White).bg(Color::DarkGray);
                return Line::from(vec![
                    Span::styled(l.prefix(), style),
                    Span::styled(l.message.as_str(), style),
                ]);
            }
            // Highlight filter matches in the message portion
            if let Some(re) = &state.filter.regex {
                if let Some(m) = re.find(&l.message) {
                    return Line::from(vec![
                        Span::styled(l.prefix(), Style::default().fg(pod_color)),
                        Span::styled(l.message[..m.start()].to_string(), Style::default().fg(Color::White)),
                        Span::styled(
                            l.message[m.start()..m.end()].to_string(),
                            Style::default().fg(Color::Black).bg(Color::Yellow),
                        ),
                        Span::styled(l.message[m.end()..].to_string(), Style::default().fg(Color::White)),
                    ]);
                }
            }
            Line::from(vec![
                Span::styled(l.prefix(), Style::default().fg(pod_color)),
                Span::styled(l.message.as_str(), Style::default().fg(Color::White)),
            ])
        })
        .collect();

    let mut paragraph = Paragraph::new(lines);
    if state.wrap {
        paragraph = paragraph.wrap(Wrap { trim: false });
    }
    f.render_widget(paragraph, outer[2]);

    // Log detail popup (selected line expanded)
    if let Some(detail) = &app.log_detail_line {
        let popup_area = centered_rect(80, 50, f.area());
        f.render_widget(Clear, popup_area);
        let detail_paragraph = Paragraph::new(detail.as_str()).wrap(Wrap { trim: false }).block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Yellow))
                .title(" Log Line Detail — Esc to close "),
        );
        f.render_widget(detail_paragraph, popup_area);
    }

    // Filter bar
    if has_filter_bar {
        let filter_display = if state.filter.editing {
            format!(" /{}▏", state.filter.buf)
        } else {
            format!(" /{}/", state.filter.text)
        };
        let filter_style = if state.filter.editing {
            Style::default().fg(Color::Yellow)
        } else {
            Style::default().fg(Color::DarkGray)
        };
        let filter_line = Paragraph::new(Line::from(Span::styled(filter_display, filter_style)));
        f.render_widget(filter_line, outer[3]);
    }

    // Hotkey bar
    let bar = build_hotkey_bar(app);
    f.render_widget(Paragraph::new(bar), outer[4]);
}

/// Build the hotkey bar for any context from the command registry.
/// Dynamic labels (Follow/Unfollow, Pause/Resume, etc.) are applied here.
fn build_hotkey_bar(app: &App) -> Line<'static> {
    use super::command::{
        self,
        Ctx,
    };

    let key_style = Style::default().fg(Color::Black).bg(Color::Yellow);
    let label_style = Style::default().fg(Color::White);
    let badge_style = Style::default().fg(Color::Black).bg(Color::Green);
    let sep = Span::styled("  ", Style::default());

    let ctx = app.current_context();
    let flags = app.cmd_flags();
    let commands = command::hotkey_bar(ctx, &flags);
    let mut spans: Vec<Span> = Vec::new();

    for cmd in &commands {
        // Dynamic label overrides based on current state
        let label: String = match (cmd.key, ctx) {
            | ("f", Ctx::Logs) => {
                if flags.following {
                    " Unfollow".into()
                } else {
                    " Follow".into()
                }
            },
            | ("w", Ctx::Logs) => {
                if flags.wrapping {
                    " Unwrap".into()
                } else {
                    " Wrap".into()
                }
            },
            | ("t", Ctx::Logs) => {
                if flags.has_since {
                    " Time*".into()
                } else {
                    " Time".into()
                }
            },
            | ("w", Ctx::Detail) => {
                if flags.detail_auto_refresh {
                    " Unwatch".into()
                } else {
                    " Watch".into()
                }
            },
            | ("p", Ctx::Logs) => {
                if let Some(s) = &app.log {
                    format!(" Pod: {}", s.pod_label())
                } else {
                    " Pod".into()
                }
            },
            | ("c", Ctx::Logs) => {
                if let Some(s) = &app.log {
                    format!(" Container: {}", s.container_label())
                } else {
                    " Container".into()
                }
            },
            | ("K", _) => {
                if flags.node_cordoned {
                    " Uncordon".into()
                } else {
                    " Cordon".into()
                }
            },
            | ("d", Ctx::Resources) => {
                if flags.diff_mark_set {
                    " Diff*".into()
                } else {
                    " Diff".into()
                }
            },
            | ("c", Ctx::Nav | Ctx::Resources | Ctx::ClusterStats) => {
                format!(" {}", app.kube.context.name.as_str())
            },
            | ("n", Ctx::Nav | Ctx::Resources | Ctx::ClusterStats) => {
                if app.kube.context.namespace.is_none() {
                    " All".into()
                } else {
                    format!(" {}", app.kube.context.namespace.as_deref().unwrap_or("All"))
                }
            },
            | _ => format!(" {}", cmd.label),
        };

        spans.push(Span::styled(format!(" {} ", cmd.key), key_style));
        spans.push(Span::styled(label, label_style));
        spans.push(sep.clone());
    }

    // State badges at the end
    if flags.paused
        && matches!(
            ctx,
            Ctx::Nav | Ctx::Resources | Ctx::ClusterStats | Ctx::Detail | Ctx::Events
        )
    {
        spans.push(Span::styled(
            " ⏸ PAUSED ",
            Style::default().fg(Color::Black).bg(Color::Yellow),
        ));
        spans.push(sep.clone());
    }
    if flags.detail_auto_refresh && ctx == Ctx::Detail {
        spans.push(Span::styled(" ⟳ WATCH ", badge_style));
        spans.push(sep.clone());
    }

    // Actions live in the palette. Keep the entry point on every bar.
    spans.push(Span::styled(" Ctrl+P ", key_style));
    spans.push(Span::styled(" Actions", label_style));
    spans.push(sep.clone());
    spans.push(Span::styled(" ? ", key_style));
    spans.push(Span::styled(" Help", label_style));

    Line::from(spans)
}

// ---------------------------------------------------------------------------
// Popup overlay
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Port forwards view
// ---------------------------------------------------------------------------

fn render_profiles(f: &mut Frame, app: &mut App, area: Rect) {
    let focused = app.focus == Focus::Resources;
    let border_color = if focused { Color::Cyan } else { Color::DarkGray };

    let mut names: Vec<String> = app.config.profiles.keys().cloned().collect();
    names.sort();

    if names.is_empty() {
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(border_color))
            .title(" Profiles ");
        let text = Paragraph::new(vec![
            Line::from(""),
            Line::from(Span::styled("  No profiles", Style::default().fg(Color::DarkGray))),
        ])
        .block(block);
        f.render_widget(text, area);
        return;
    }

    app.profiles.table_state.select(if names.is_empty() {
        None
    } else {
        Some(app.profiles.cursor)
    });

    let header = Row::new(vec!["NAME", "CONTEXT", "FAVORITES", "PORT FORWARDS"])
        .style(Style::default().fg(Color::DarkGray))
        .bottom_margin(0);

    let rows: Vec<Row> = names
        .iter()
        .map(|name| {
            let profile = app.config.profiles.get(name);
            let is_active = name == &app.config.active_profile;

            let fav_count = profile.map(|p| p.favorites.len()).unwrap_or(0);
            let pf_count = profile.map(|p| p.port_forwards.len()).unwrap_or(0);
            let context = profile.and_then(|p| p.context.as_deref()).unwrap_or("(default)");

            let name_style = if is_active {
                Style::default().fg(Color::Green).add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            };

            let name_display = if is_active {
                format!("● {}", name)
            } else {
                format!("  {}", name)
            };

            Row::new(vec![
                Cell::from(Span::styled(name_display, name_style)),
                Cell::from(context),
                Cell::from(fav_count.to_string()),
                Cell::from(pf_count.to_string()),
            ])
        })
        .collect();

    let table = Table::new(rows, [
        Constraint::Min(20),
        Constraint::Length(20),
        Constraint::Length(12),
        Constraint::Length(14),
    ])
    .header(header)
    .block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(border_color))
            .title(format!(" Profiles ({}) ", names.len())),
    )
    .row_highlight_style(Style::default().add_modifier(Modifier::REVERSED).fg(Color::Cyan))
    .highlight_symbol("▶ ");

    f.render_stateful_widget(table, area, &mut app.profiles.table_state);
}

fn type_color(kind: &FavoriteType) -> Color {
    match kind.builtin_type().map(ResourceType::category) {
        | Some(Category::Workloads) => Color::Green,
        | Some(Category::Network) => Color::Cyan,
        | Some(Category::Config) => Color::Yellow,
        | Some(Category::Storage) => Color::Blue,
        | Some(Category::Rbac) => Color::Magenta,
        | Some(Category::Cluster) => Color::White,
        | None if kind.custom_ref().is_some() => Color::LightCyan,
        | None => Color::DarkGray,
    }
}

fn render_choice_list(f: &mut Frame, area: Rect, title: &str, items: &[String], state: &mut ListState) {
    let list_items: Vec<ListItem> = items
        .iter()
        .enumerate()
        .map(|(i, item)| {
            let style = if i == 0 {
                Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            };
            ListItem::new(item.as_str()).style(style)
        })
        .collect();
    let list = List::new(list_items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Cyan))
                .title(title),
        )
        .highlight_style(Style::default().add_modifier(Modifier::REVERSED).fg(Color::Cyan))
        .highlight_symbol("▶ ");
    f.render_stateful_widget(list, area, state);
}

fn render_favorites(f: &mut Frame, app: &mut App, area: Rect) {
    let favorites = &app.config.active_profile().favorites;

    let focused = app.focus == Focus::Resources;
    let border_color = if focused { Color::Cyan } else { Color::DarkGray };

    if favorites.is_empty() {
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(border_color))
            .title(" ★ Favorites ");
        let text = Paragraph::new(vec![
            Line::from(""),
            Line::from(Span::styled("  No favorites yet", Style::default().fg(Color::DarkGray))),
            Line::from(""),
            Line::from(Span::styled(
                "  Press [*] on any resource to add it",
                Style::default().fg(Color::DarkGray),
            )),
        ])
        .block(block);
        f.render_widget(text, area);
        return;
    }

    use crate::tui::app::DisplayItem;

    let display = app.favorites_display_items();

    // Sync table state with cursor
    app.favorites.table_state.select(if display.is_empty() {
        None
    } else {
        Some(app.favorites.cursor)
    });

    let current_context = app.kube.context.name.as_str().to_string();
    let available_contexts = app.kube.contexts();

    // Look up cached resource entries for basic status info
    let cached = &app.resources.entries;

    let rows: Vec<Row> = display
        .iter()
        .map(|item| {
            match item {
                | DisplayItem::Header(label) => {
                    Row::new(vec![
                        Cell::from(Span::styled("─", Style::default().fg(Color::DarkGray))),
                        Cell::from(Span::styled(
                            label.to_uppercase(),
                            Style::default().fg(Color::DarkGray).add_modifier(Modifier::BOLD),
                        )),
                        Cell::from(""),
                        Cell::from(""),
                        Cell::from(""),
                    ])
                },
                | DisplayItem::Entry(idx) => {
                    let fav = &favorites[*idx];
                    let missing = !available_contexts.iter().any(|c| c == &fav.context);
                    let is_current_ctx = fav.context == current_context;
                    let is_diff_marked = app
                        .diff_mark
                        .as_ref()
                        .is_some_and(|mark| mark.name == fav.name && mark.namespace == fav.namespace);

                    let type_label = fav.resource_type.short_label();
                    let t_color = type_color(&fav.resource_type);
                    let type_cell = Cell::from(Span::styled(
                        type_label.to_string(),
                        Style::default().fg(if missing { Color::DarkGray } else { t_color }),
                    ));

                    // Name
                    let name_cell = if is_diff_marked {
                        Cell::from(Span::styled(
                            format!("* {}", fav.name),
                            Style::default().fg(Color::Magenta).add_modifier(Modifier::BOLD),
                        ))
                    } else {
                        let name_style = if missing {
                            Style::default().fg(Color::DarkGray)
                        } else if is_current_ctx {
                            Style::default().fg(Color::Green)
                        } else {
                            Style::default()
                        };
                        let name_prefix = if missing { "⚠ " } else { "★ " };
                        Cell::from(Span::styled(format!("{}{}", name_prefix, fav.name), name_style))
                    };

                    // Namespace
                    let ns_cell = Cell::from(Span::styled(
                        fav.namespace.as_str(),
                        if missing {
                            Style::default().fg(Color::DarkGray)
                        } else {
                            Style::default()
                        },
                    ));

                    // Status — try to find a matching cached resource entry
                    // for current-context favorites to show live status
                    let status_cell = if missing {
                        Cell::from(Span::styled("unavailable", Style::default().fg(Color::Red)))
                    } else if !is_current_ctx {
                        Cell::from(Span::styled(
                            format!("ctx:{}", truncate(&fav.context, 12)),
                            Style::default().fg(Color::DarkGray),
                        ))
                    } else {
                        // Look up in cached entries for a status column
                        let entry = cached
                            .iter()
                            .find(|e| e.name == fav.name && e.namespace == fav.namespace);
                        match entry {
                            | Some(e) if !e.columns.is_empty() => {
                                // First column is typically the most useful status
                                // (ready count, phase, replicas, etc.)
                                let status = e.columns.iter().take(2).cloned().collect::<Vec<_>>().join("  ");
                                let color = if status.contains("Running")
                                    || status.contains("Active")
                                    || status.contains("Bound")
                                {
                                    Color::Green
                                } else if status.contains("Pending")
                                    || status.contains("ContainerCreating")
                                    || status.contains("Terminating")
                                {
                                    Color::Yellow
                                } else if status.contains("Failed")
                                    || status.contains("Error")
                                    || status.contains("CrashLoop")
                                {
                                    Color::Red
                                } else {
                                    Color::White
                                };
                                Cell::from(Span::styled(status, Style::default().fg(color)))
                            },
                            | _ => Cell::from(Span::styled("–", Style::default().fg(Color::DarkGray))),
                        }
                    };

                    // Context indicator — only show if different from current
                    let ctx_cell = if is_current_ctx {
                        Cell::from(Span::styled("", Style::default()))
                    } else if missing {
                        Cell::from(Span::styled(
                            format!("{} ✗", truncate(&fav.context, 12)),
                            Style::default().fg(Color::Red),
                        ))
                    } else {
                        Cell::from(Span::styled(
                            truncate(&fav.context, 14).to_string(),
                            Style::default().fg(Color::DarkGray),
                        ))
                    };

                    Row::new(vec![type_cell, name_cell, ns_cell, status_cell, ctx_cell])
                },
            }
        })
        .collect();

    let table = Table::new(rows, [
        Constraint::Length(12), // TYPE
        Constraint::Min(20),    // NAME
        Constraint::Length(16), // NAMESPACE
        Constraint::Length(20), // STATUS
        Constraint::Length(14), // CONTEXT
    ])
    .header(
        Row::new(vec![
            Cell::from(Span::styled("TYPE", Style::default().fg(Color::DarkGray))),
            Cell::from(Span::styled("NAME", Style::default().fg(Color::DarkGray))),
            Cell::from(Span::styled("NAMESPACE", Style::default().fg(Color::DarkGray))),
            Cell::from(Span::styled("STATUS", Style::default().fg(Color::DarkGray))),
            Cell::from(Span::styled("CONTEXT", Style::default().fg(Color::DarkGray))),
        ])
        .bottom_margin(0),
    )
    .block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(border_color))
            .title(format!(" ★ Favorites ({}) ", favorites.len())),
    )
    .row_highlight_style(Style::default().add_modifier(Modifier::REVERSED).fg(Color::Yellow))
    .highlight_symbol("▶ ");

    f.render_stateful_widget(table, area, &mut app.favorites.table_state);
}

fn render_port_forwards(f: &mut Frame, app: &mut App, area: Rect) {
    let entries = app.pf.manager.entries();

    let focused = app.focus == Focus::Resources;
    let border_color = if focused { Color::Cyan } else { Color::DarkGray };

    if entries.is_empty() {
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(border_color))
            .title(" Port Forwards ");
        let text = Paragraph::new(vec![
            Line::from(""),
            Line::from(Span::styled(
                "  No active port forwards",
                Style::default().fg(Color::DarkGray),
            )),
            Line::from(""),
            Line::from(Span::styled(
                "  Press [F] on a resource to create one",
                Style::default().fg(Color::DarkGray),
            )),
        ])
        .block(block);
        f.render_widget(text, area);
        return;
    }

    use crate::tui::app::DisplayItem;

    let display = app.pf_display_items();

    // Sync table state with cursor
    app.pf
        .table_state
        .select(if display.is_empty() { None } else { Some(app.pf.cursor) });

    let rows: Vec<Row> = display
        .iter()
        .map(|item| {
            match item {
                | DisplayItem::Header(label) => {
                    Row::new(vec![
                        Cell::from(Span::styled(
                            label.to_uppercase(),
                            Style::default().fg(Color::DarkGray).add_modifier(Modifier::BOLD),
                        )),
                        Cell::from(""),
                        Cell::from(""),
                        Cell::from(""),
                        Cell::from(""),
                        Cell::from(""),
                        Cell::from(""),
                        Cell::from(""),
                    ])
                },
                | DisplayItem::Entry(idx) => {
                    let entry = &entries[*idx];
                    let status_style = match &entry.status {
                        | PortForwardStatus::Active => Style::default().fg(Color::Green),
                        | PortForwardStatus::Paused => Style::default().fg(Color::Yellow),
                        | PortForwardStatus::Reconnecting { .. } => Style::default().fg(Color::Yellow),
                        | PortForwardStatus::Error(_) | PortForwardStatus::AuthExpired => {
                            Style::default().fg(Color::Red)
                        },
                        | PortForwardStatus::Starting => Style::default().fg(Color::Cyan),
                        | PortForwardStatus::Cancelled => Style::default().fg(Color::DarkGray),
                    };
                    let status_text = match &entry.status {
                        | PortForwardStatus::Reconnecting { attempt } => format!("Retry({})", attempt),
                        | PortForwardStatus::Error(msg) => msg.clone(),
                        | PortForwardStatus::AuthExpired => "Login expired".to_string(),
                        | other => other.display().to_string(),
                    };
                    Row::new(vec![
                        Cell::from(Span::styled(status_text, status_style)),
                        Cell::from(Span::styled(
                            entry.identity.type_label(),
                            Style::default().fg(Color::DarkGray),
                        )),
                        Cell::from(format!(":{}", entry.port.local)),
                        Cell::from(format!(":{}", entry.port.remote)),
                        Cell::from(entry.context.as_str()),
                        Cell::from(entry.identity.resource_label()),
                        Cell::from(entry.pod_name.as_str()),
                        Cell::from(entry.connections.to_string()),
                    ])
                },
            }
        })
        .collect();

    let table = Table::new(rows, [
        Constraint::Length(12), // STATUS
        Constraint::Length(10), // TYPE
        Constraint::Length(8),  // LOCAL
        Constraint::Length(8),  // REMOTE
        Constraint::Min(14),    // CONTEXT
        Constraint::Min(18),    // RESOURCE
        Constraint::Min(18),    // POD
        Constraint::Length(6),  // CONNS
    ])
    .header(
        Row::new(vec![
            Cell::from(Span::styled("STATUS", Style::default().fg(Color::DarkGray))),
            Cell::from(Span::styled("TYPE", Style::default().fg(Color::DarkGray))),
            Cell::from(Span::styled("LOCAL", Style::default().fg(Color::DarkGray))),
            Cell::from(Span::styled("REMOTE", Style::default().fg(Color::DarkGray))),
            Cell::from(Span::styled("CONTEXT", Style::default().fg(Color::DarkGray))),
            Cell::from(Span::styled("RESOURCE", Style::default().fg(Color::DarkGray))),
            Cell::from(Span::styled("POD", Style::default().fg(Color::DarkGray))),
            Cell::from(Span::styled("CONNS", Style::default().fg(Color::DarkGray))),
        ])
        .bottom_margin(0),
    )
    .row_highlight_style(Style::default().add_modifier(Modifier::REVERSED).fg(Color::Cyan))
    .block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(border_color))
            .title(" Port Forwards "),
    );

    f.render_stateful_widget(table, area, &mut app.pf.table_state);
}

// ---------------------------------------------------------------------------
// Command palette
// ---------------------------------------------------------------------------

fn render_help(f: &mut Frame, app: &mut App) {
    if app.help.is_none() {
        return;
    }
    let area = f.area();
    let width = (area.width * 70 / 100).max(50).min(area.width);
    let x = (area.width.saturating_sub(width)) / 2;
    let max_rows = 20u16;
    let height = (max_rows + 3).min(area.height);
    let help_area = ratatui::layout::Rect::new(x, 1, width, height);

    f.render_widget(Clear, help_area);

    // Compute these before mutable borrow of help
    let entries = app.filtered_help_entries();
    let context_label = app.current_context().label().to_string();

    let h = app.help.as_mut().unwrap();
    let mut lines: Vec<Line> = Vec::new();

    // Search input
    lines.push(Line::from(vec![
        Span::styled("  ", Style::default()),
        Span::styled(
            format!("{}|", h.buf),
            Style::default().fg(Color::White).add_modifier(Modifier::BOLD),
        ),
    ]));

    let visible_rows = (height.saturating_sub(3)) as usize;
    // Edge-only scrolling: only scroll when cursor hits the boundary
    if h.cursor < h.scroll {
        h.scroll = h.cursor;
    } else if visible_rows > 0 && h.cursor >= h.scroll + visible_rows {
        h.scroll = h.cursor - visible_rows + 1;
    }
    h.scroll = h.scroll.min(entries.len().saturating_sub(visible_rows));

    for (i, cmd) in entries.iter().skip(h.scroll).take(visible_rows).enumerate() {
        let actual_idx = i + h.scroll;
        let is_selected = actual_idx == h.cursor;
        let ctx_label = cmd.contexts.iter().map(|c| c.label()).collect::<Vec<_>>().join(", ");
        let row_style = if is_selected {
            Style::default().add_modifier(Modifier::REVERSED).fg(Color::Cyan)
        } else {
            Style::default()
        };
        lines.push(Line::from(vec![
            Span::styled(
                format!("  {:<14}", cmd.key),
                if is_selected {
                    row_style
                } else {
                    Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)
                },
            ),
            Span::styled(format!("{:<36}", cmd.description), row_style),
            Span::styled(
                ctx_label,
                if is_selected {
                    row_style
                } else {
                    Style::default().fg(Color::DarkGray)
                },
            ),
        ]));
    }

    if entries.is_empty() && !h.buf.is_empty() {
        lines.push(Line::from(Span::styled(
            "    No matching keybinds",
            Style::default().fg(Color::DarkGray),
        )));
    }

    let count = entries.len();
    let mode_label = if h.context_only {
        context_label
    } else {
        "All".to_string()
    };
    let toggle_hint = if h.context_only { "All" } else { "Context" };
    let title = format!(
        " Keybindings — {} ({})  |  Tab: {}  |  Esc to close ",
        mode_label, count, toggle_hint
    );

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Yellow))
        .title(title);

    let paragraph = Paragraph::new(lines).block(block);
    f.render_widget(paragraph, help_area);
}

fn render_palette(f: &mut Frame, app: &mut App) {
    let p = match &app.palette {
        | Some(p) => p,
        | None => return,
    };
    // Top-centered palette, like VS Code
    let area = f.area();
    let width = (area.width * 60 / 100).max(40).min(area.width);
    let x = (area.width.saturating_sub(width)) / 2;
    let max_results = 12u16;
    let height = (max_results + 3).min(area.height); // input + border + results
    let palette_area = ratatui::layout::Rect::new(x, 1, width, height);

    f.render_widget(Clear, palette_area);

    let mut lines: Vec<Line> = Vec::new();

    // Input line
    let prefix = if p.buf.starts_with('>') { "" } else { "  " };
    lines.push(Line::from(vec![
        Span::styled(prefix, Style::default()),
        Span::styled(
            format!("{}|", p.buf),
            Style::default().fg(Color::White).add_modifier(Modifier::BOLD),
        ),
    ]));

    // Results
    let visible_results = (height.saturating_sub(3)) as usize;
    // Scroll to keep cursor visible
    let scroll = if p.cursor >= visible_results {
        p.cursor - visible_results + 1
    } else {
        0
    };

    for (i, entry) in p.results.iter().skip(scroll).take(visible_results).enumerate() {
        let actual_idx = i + scroll;
        let is_selected = actual_idx == p.cursor;
        let style = if is_selected {
            Style::default().fg(Color::Cyan).add_modifier(Modifier::REVERSED)
        } else {
            Style::default().fg(Color::White)
        };
        let marker = if is_selected { "▶ " } else { "  " };

        let desc = if entry.description.is_empty() {
            String::new()
        } else {
            format!("  {}", entry.description)
        };

        lines.push(Line::from(vec![
            Span::styled(format!("  {}{}", marker, entry.label), style),
            Span::styled(desc, Style::default().fg(Color::DarkGray)),
        ]));
    }

    if p.results.is_empty() && !p.buf.is_empty() {
        lines.push(Line::from(Span::styled(
            "    No matches",
            Style::default().fg(Color::DarkGray),
        )));
    }

    let hint = " Actions  |  type to filter  |  Enter runs  |  Esc closes ";

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan))
        .title(hint);

    let paragraph = Paragraph::new(lines).block(block);
    f.render_widget(paragraph, palette_area);
}

// ---------------------------------------------------------------------------
// Popup overlays
// ---------------------------------------------------------------------------

fn render_popup(f: &mut Frame, app: &mut App) {
    let current_context = app.kube.context.name.as_str().to_string();
    let current_namespace = app.kube.context.namespace.as_deref().map(|s| s.to_string());

    let popup = match &mut app.popup {
        | Some(p) => p,
        | None => return,
    };

    // PortForwardCreate / ConfirmDelete / ScaleInput / ExecShell use their own
    // smaller area
    let area = if matches!(popup, Popup::ExecShell { .. } | Popup::KubeconfigInput { .. }) {
        let a = centered_rect(60, 65, f.area());
        f.render_widget(Clear, a);
        a
    } else if matches!(
        popup,
        Popup::PortForwardCreate(_)
            | Popup::ConfirmDelete { .. }
            | Popup::ConfirmDrain { .. }
            | Popup::ConfirmQuit { .. }
            | Popup::TriggerCronJob { .. }
            | Popup::ScaleInput { .. }
            | Popup::TimeFilter { .. }
            | Popup::ProfileSave { .. }
            | Popup::PortForwardEditPort { .. }
            | Popup::ProfileClone { .. }
            | Popup::ConfirmDeleteProfile { .. }
            | Popup::Confirm { .. }
            | Popup::AuthExpired { .. }
    ) {
        let a = centered_rect(45, 50, f.area());
        f.render_widget(Clear, a);
        a
    } else {
        let a = centered_rect(50, 60, f.area());
        f.render_widget(Clear, a);
        a
    };

    match popup {
        | Popup::ContextSelect { items, state } => {
            let list_items: Vec<ListItem> = items
                .iter()
                .map(|i| {
                    let style = if *i == current_context {
                        Style::default().fg(Color::Green)
                    } else {
                        Style::default()
                    };
                    ListItem::new(i.as_str()).style(style)
                })
                .collect();

            let list = List::new(list_items)
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .border_style(Style::default().fg(Color::Cyan))
                        .title(" Select Context "),
                )
                .highlight_style(Style::default().add_modifier(Modifier::REVERSED).fg(Color::Cyan))
                .highlight_symbol("▶ ");

            f.render_stateful_widget(list, area, state);
        },
        | Popup::NamespaceSelect { items, state } => {
            let list_items: Vec<ListItem> = items
                .iter()
                .map(|i| {
                    let is_selected = if *i == ALL_NAMESPACES_LABEL {
                        current_namespace.is_none()
                    } else {
                        current_namespace.as_deref() == Some(i.as_str())
                    };
                    let style = if is_selected {
                        Style::default().fg(Color::Green)
                    } else if *i == ALL_NAMESPACES_LABEL {
                        Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)
                    } else {
                        Style::default()
                    };
                    ListItem::new(i.as_str()).style(style)
                })
                .collect();

            let list = List::new(list_items)
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .border_style(Style::default().fg(Color::Cyan))
                        .title(" Select Namespace "),
                )
                .highlight_style(Style::default().add_modifier(Modifier::REVERSED).fg(Color::Cyan))
                .highlight_symbol("▶ ");

            f.render_stateful_widget(list, area, state);
        },
        | Popup::PodSelect { items, state } => {
            render_choice_list(f, area, " Select Pod ", items, state);
        },
        | Popup::ContainerSelect { items, state } => {
            render_choice_list(f, area, " Select Container ", items, state);
        },
        | Popup::PortForwardCreate(dialog) => {
            let mut lines: Vec<Line> = Vec::new();
            lines.push(Line::from(Span::styled(
                format!(" {}/{}", dialog.resource_type.display_name(), dialog.resource_name),
                Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
            )));
            lines.push(Line::from(""));

            // Port list
            for (i, port) in dialog.ports.iter().enumerate() {
                let marker = if i == dialog.port_cursor { "▶ " } else { "  " };
                let port_label = if port.name.is_empty() {
                    format!("{}{}/{}", marker, port.container_port, port.protocol)
                } else {
                    format!("{}{}/{} ({})", marker, port.container_port, port.protocol, port.name)
                };
                let style = if i == dialog.port_cursor {
                    Style::default().fg(Color::Green).add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(Color::White)
                };
                lines.push(Line::from(Span::styled(port_label, style)));
            }

            lines.push(Line::from(""));
            lines.push(Line::from(vec![
                Span::styled(" Local port: ", Style::default().fg(Color::Yellow)),
                Span::styled(
                    format!("{}_", dialog.local_port_buf),
                    Style::default().fg(Color::White).add_modifier(Modifier::BOLD),
                ),
            ]));
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                " Enter confirms  Esc=Cancel",
                Style::default().fg(Color::DarkGray),
            )));

            let block = Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Cyan))
                .title(" Port Forward ");

            let paragraph = Paragraph::new(lines).block(block);
            f.render_widget(paragraph, area);
        },
        | Popup::ConfirmDelete { name, kind, .. } => {
            let display = kind.display_name();
            let mut lines: Vec<Line> = Vec::new();
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                format!("  Delete {}/{}?", display, name),
                Style::default().fg(Color::Red),
            )));
            lines.push(Line::from(""));
            lines.push(Line::from("  Press [Enter/y] to confirm, [Esc/n] to cancel"));

            let block = Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Red))
                .title(" Confirm Delete ");

            let paragraph = Paragraph::new(lines).block(block);
            f.render_widget(paragraph, area);
        },
        | Popup::ConfirmDrain { node_name } => {
            let lines = vec![
                Line::from(""),
                Line::from(Span::styled(
                    format!("  Drain node {}?", node_name),
                    Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
                )),
                Line::from(""),
                Line::from(Span::styled(
                    "  This will cordon the node and evict",
                    Style::default().fg(Color::Yellow),
                )),
                Line::from(Span::styled(
                    "  all non-DaemonSet pods.",
                    Style::default().fg(Color::Yellow),
                )),
                Line::from(""),
                Line::from("  [Enter/y] confirm  [Esc/n] cancel"),
            ];

            let block = Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Red))
                .title(" Confirm Drain ");

            f.render_widget(Paragraph::new(lines).block(block), area);
        },
        | Popup::ConfirmQuit { pf_count } => {
            let mut lines = vec![
                Line::from(""),
                Line::from(Span::styled(
                    "  Quit qb?",
                    Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD),
                )),
            ];
            if *pf_count > 0 {
                lines.push(Line::from(""));
                lines.push(Line::from(Span::styled(
                    format!(
                        "  {} active port forward{} will be stopped.",
                        pf_count,
                        if *pf_count == 1 { "" } else { "s" }
                    ),
                    Style::default().fg(Color::Yellow),
                )));
            }
            lines.push(Line::from(""));
            lines.push(Line::from("  [Enter/y] quit  [Esc/n] cancel"));

            let block = Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Yellow))
                .title(" Confirm Quit ");

            f.render_widget(Paragraph::new(lines).block(block), area);
        },
        | Popup::TriggerCronJob { cronjob_name, buf, .. } => {
            let lines = vec![
                Line::from(""),
                Line::from(Span::styled(
                    format!("  Trigger CronJob/{}", cronjob_name),
                    Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
                )),
                Line::from(""),
                Line::from(format!("  Job name: {}▎", buf)),
                Line::from(""),
                Line::from("  [Enter] confirm create  [Esc] cancel"),
            ];

            let block = Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Cyan))
                .title(" Trigger CronJob ");

            f.render_widget(Paragraph::new(lines).block(block), area);
        },
        | Popup::ScaleInput {
            name,
            resource_type,
            current,
            buf,
            ..
        } => {
            let mut lines: Vec<Line> = Vec::new();
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                format!("  {}/{}", resource_type.display_name(), name),
                Style::default().fg(Color::Cyan),
            )));
            lines.push(Line::from(format!("  Current replicas: {}", current)));
            lines.push(Line::from(""));
            lines.push(Line::from(format!("  New replicas: {}▎", buf)));
            lines.push(Line::from(""));
            lines.push(Line::from("  [Enter] confirm count  [Esc] cancel"));

            let block = Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Cyan))
                .title(" Scale ");

            let paragraph = Paragraph::new(lines).block(block);
            f.render_widget(paragraph, area);
        },
        | Popup::ExecShell {
            pod_name,
            containers,
            container_cursor,
            command_buf,
            terminal_buf,
            editing_terminal,
            ..
        } => {
            let cmd_style = if !*editing_terminal {
                Style::default().fg(Color::White).add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::DarkGray)
            };
            let term_style = if *editing_terminal {
                Style::default().fg(Color::White).add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::DarkGray)
            };

            let mut lines: Vec<Line> = Vec::new();
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                format!("  Pod: {}", pod_name),
                Style::default().fg(Color::Cyan),
            )));
            lines.push(Line::from(""));
            lines.push(Line::from("  Container:"));
            for (i, c) in containers.iter().enumerate() {
                let marker = if i == *container_cursor { "▶ " } else { "  " };
                let style = if i == *container_cursor {
                    Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)
                } else {
                    Style::default()
                };
                lines.push(Line::from(Span::styled(format!("    {}{}", marker, c), style)));
            }
            lines.push(Line::from(""));
            let cmd_cursor = if !*editing_terminal { "▎" } else { "" };
            lines.push(Line::from(vec![
                Span::styled("  Command:  ", Style::default().fg(Color::Yellow)),
                Span::styled(format!("{}{}", command_buf, cmd_cursor), cmd_style),
            ]));
            let term_cursor = if *editing_terminal { "▎" } else { "" };
            lines.push(Line::from(vec![
                Span::styled("  Terminal:  ", Style::default().fg(Color::Yellow)),
                Span::styled(format!("{}{}", terminal_buf, term_cursor), term_style),
            ]));
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                "  [Tab] switch field  [Up/Down] container  [Enter] exec  [Esc] cancel",
                Style::default().fg(Color::DarkGray),
            )));

            let block = Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Cyan))
                .title(" Exec Shell ");

            let paragraph = Paragraph::new(lines).block(block);
            f.render_widget(paragraph, area);
        },
        | Popup::KubeconfigInput { buf } => {
            let lines = vec![
                Line::from(""),
                Line::from(vec![
                    Span::styled("  Path: ", Style::default().fg(Color::Yellow)),
                    Span::styled(
                        format!("{}▎", buf),
                        Style::default().fg(Color::White).add_modifier(Modifier::BOLD),
                    ),
                ]),
                Line::from(""),
                Line::from(Span::styled(
                    "  [Enter] load  [Esc] cancel",
                    Style::default().fg(Color::DarkGray),
                )),
            ];

            let block = Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Cyan))
                .title(" Open Kubeconfig ");

            let paragraph = Paragraph::new(lines).block(block);
            f.render_widget(paragraph, area);
        },
        | Popup::TimeFilter { buf } => {
            let lines = vec![
                Line::from(""),
                Line::from(Span::styled("  Log time range:", Style::default().fg(Color::Cyan))),
                Line::from(""),
                Line::from(vec![
                    Span::styled("  Duration: ", Style::default().fg(Color::Yellow)),
                    Span::styled(
                        format!("{}▎", buf),
                        Style::default().fg(Color::White).add_modifier(Modifier::BOLD),
                    ),
                ]),
                Line::from(""),
                Line::from(Span::styled(
                    "  Examples: 30m, 2h, 1h30m, 1d",
                    Style::default().fg(Color::DarkGray),
                )),
                Line::from(Span::styled(
                    "  [Enter] apply  [Esc] cancel",
                    Style::default().fg(Color::DarkGray),
                )),
            ];

            let block = Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Cyan))
                .title(" Time Filter ");

            let paragraph = Paragraph::new(lines).block(block);
            f.render_widget(paragraph, area);
        },
        | Popup::ProfileSave { buf } => {
            let lines = vec![
                Line::from(""),
                Line::from(Span::styled("  Save profile as:", Style::default().fg(Color::Cyan))),
                Line::from(""),
                Line::from(vec![
                    Span::styled("  Name: ", Style::default().fg(Color::Yellow)),
                    Span::styled(
                        format!("{}▎", buf),
                        Style::default().fg(Color::White).add_modifier(Modifier::BOLD),
                    ),
                ]),
                Line::from(""),
                Line::from(Span::styled(
                    "  [Enter] save  [Esc] cancel",
                    Style::default().fg(Color::DarkGray),
                )),
            ];

            let block = Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Cyan))
                .title(" Save Profile ");

            let paragraph = Paragraph::new(lines).block(block);
            f.render_widget(paragraph, area);
        },
        | Popup::ProfileLoad { items, state } => {
            let list_items: Vec<ListItem> = items
                .iter()
                .map(|name| {
                    let style = if name == &app.config.active_profile {
                        Style::default().fg(Color::Green).add_modifier(Modifier::BOLD)
                    } else {
                        Style::default()
                    };
                    ListItem::new(format!("  {}", name)).style(style)
                })
                .collect();

            let list = List::new(list_items)
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .border_style(Style::default().fg(Color::Cyan))
                        .title(" Load Profile "),
                )
                .highlight_style(Style::default().add_modifier(Modifier::REVERSED).fg(Color::Cyan))
                .highlight_symbol("▶ ");

            f.render_stateful_widget(list, area, state);
        },
        | Popup::PortForwardEditPort { old_port, buf, .. } => {
            let lines = vec![
                Line::from(""),
                Line::from(Span::styled(
                    format!("  Change local port (current: :{}):", old_port),
                    Style::default().fg(Color::Cyan),
                )),
                Line::from(""),
                Line::from(vec![
                    Span::styled("  Port: ", Style::default().fg(Color::Yellow)),
                    Span::styled(
                        format!("{}▎", buf),
                        Style::default().fg(Color::White).add_modifier(Modifier::BOLD),
                    ),
                ]),
                Line::from(""),
                Line::from(Span::styled(
                    "  [Enter] apply  [Esc] cancel",
                    Style::default().fg(Color::DarkGray),
                )),
            ];

            let block = Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Cyan))
                .title(" Edit Local Port ");

            let paragraph = Paragraph::new(lines).block(block);
            f.render_widget(paragraph, area);
        },
        | Popup::ProfileClone { source_name, buf } => {
            let lines = vec![
                Line::from(""),
                Line::from(Span::styled(
                    format!("  Clone profile '{}':", source_name),
                    Style::default().fg(Color::Cyan),
                )),
                Line::from(""),
                Line::from(vec![
                    Span::styled("  New name: ", Style::default().fg(Color::Yellow)),
                    Span::styled(
                        format!("{}▎", buf),
                        Style::default().fg(Color::White).add_modifier(Modifier::BOLD),
                    ),
                ]),
                Line::from(""),
                Line::from(Span::styled(
                    "  [Enter] clone  [Esc] cancel",
                    Style::default().fg(Color::DarkGray),
                )),
            ];

            let block = Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Cyan))
                .title(" Clone Profile ");

            let paragraph = Paragraph::new(lines).block(block);
            f.render_widget(paragraph, area);
        },
        | Popup::ConfirmDeleteProfile { profile_name } => {
            let lines = vec![
                Line::from(""),
                Line::from(Span::styled(
                    format!("  Delete profile '{}'?", profile_name),
                    Style::default().fg(Color::Red),
                )),
                Line::from(""),
                Line::from(Span::styled(
                    "  This cannot be undone.",
                    Style::default().fg(Color::DarkGray),
                )),
                Line::from(""),
                Line::from(Span::styled(
                    "  [Enter/y] confirm  [Esc/n] cancel",
                    Style::default().fg(Color::DarkGray),
                )),
            ];

            let block = Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Red))
                .title(" Delete Profile ");

            let paragraph = Paragraph::new(lines).block(block);
            f.render_widget(paragraph, area);
        },
        | Popup::AuthExpired { detail } => {
            let mut lines = vec![
                Line::from(""),
                Line::from(Span::styled(
                    "  Google Cloud credentials for this cluster are no longer valid.",
                    Style::default().fg(Color::Yellow),
                )),
                Line::from(Span::styled(
                    "  Cluster calls are paused. The display will stay intact.",
                    Style::default().fg(Color::White),
                )),
                Line::from(""),
                Line::from("  Log in again in another terminal:"),
                Line::from(Span::styled(
                    "    gcloud auth login",
                    Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
                )),
                Line::from(""),
                Line::from("  Then press r here to retry."),
                Line::from("  Press q to quit, log in, and start qb again."),
            ];
            if !detail.is_empty() {
                lines.push(Line::from(""));
                let shown: String = detail.chars().take(100).collect();
                lines.push(Line::from(Span::styled(
                    format!("  {shown}"),
                    Style::default().fg(Color::DarkGray),
                )));
            }
            let block = Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Red))
                .title(" Cluster login expired ");
            f.render_widget(Paragraph::new(lines).block(block), area);
        },
        | Popup::Confirm { title, body, .. } => {
            let mut lines = vec![
                Line::from(""),
                Line::from(Span::styled(format!("  {body}"), Style::default().fg(Color::Red))),
            ];
            lines.push(Line::from(""));
            lines.push(Line::from("  [Enter/y] confirm  [Esc/n] cancel"));
            let block = Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Red))
                .title(format!(" {title} "));
            f.render_widget(Paragraph::new(lines).block(block), area);
        },
    }
}

fn centered_rect(percent_x: u16, percent_y: u16, area: Rect) -> Rect {
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(area);
    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(vertical[1])[1]
}
