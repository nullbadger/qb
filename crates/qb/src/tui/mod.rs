pub mod app;
pub mod command;
pub mod jobs;
pub mod logs;
pub mod smart;
pub mod ui;

use {
    crate::{
        config::QbConfig,
        k8s::KubeClient,
    },
    anyhow::Result,
    crossterm::{
        event,
        execute,
        terminal::{
            disable_raw_mode,
            enable_raw_mode,
            EnterAlternateScreen,
            LeaveAlternateScreen,
        },
    },
    ratatui::prelude::*,
    std::io,
};

pub async fn run(
    kubeconfig: Option<String>,
    context: Option<String>,
    namespace: Option<String>,
    experimental: bool,
    config: QbConfig,
) -> Result<()> {
    let kube_client = KubeClient::new(kubeconfig, context, namespace).await?;

    run_tui(kube_client, experimental, config).await
}

async fn run_tui(kube_client: KubeClient, experimental: bool, config: QbConfig) -> Result<()> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let (result, mut config) = run_event_loop(&mut terminal, kube_client, experimental, config).await;

    if let Err(e) = config.save() {
        eprintln!("Warning: Failed to save config: {}", e);
    }

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    result
}

async fn run_event_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    kube_client: KubeClient,
    experimental: bool,
    config: QbConfig,
) -> (Result<()>, QbConfig) {
    let mut app = app::App::new(kube_client, experimental, config);
    let result = run_event_loop_inner(terminal, &mut app).await;
    (result, app.config)
}

async fn run_event_loop_inner(terminal: &mut Terminal<CrosstermBackend<io::Stdout>>, app: &mut app::App) -> Result<()> {
    use {
        crossterm::event::EventStream,
        futures::StreamExt,
    };

    let mut event_stream = EventStream::new();
    let mut wake = jobs::WakeClock::new();

    loop {
        // A wall-clock jump with a frozen monotonic clock is a sleep/wake.
        // Drop streams before drawing or polling so a backlog is never parsed.
        if wake.jumped(std::time::Duration::from_secs(2)) {
            app.on_wake();
        }

        terminal.draw(|f| ui::render(f, app))?;

        // Apply finished background work. Never await cluster I/O here.
        let held = app.auth_blocked();
        app.poll_jobs();
        app.poll_cluster_stats();
        app.poll_metrics();
        app.poll_log_stream();
        app.poll_port_forwards();
        app.maybe_auto_refresh();
        app.kick_pending();
        // Draw the login prompt on this tick, not after the next key.
        if app.auth_blocked() && !held {
            terminal.draw(|f| ui::render(f, app))?;
        }

        // Wait for input or tick (50ms timeout for UI responsiveness)
        tokio::select! {
            maybe_event = event_stream.next() => {
                if let Some(Ok(event::Event::Key(key))) = maybe_event {
                    if key.kind == event::KeyEventKind::Press {
                        app.handle_key(key);
                    }
                }
            }
            _ = tokio::time::sleep(std::time::Duration::from_millis(50)) => {}
        }
        app.kick_pending();

        // Handle pending editor invocation — suspend TUI, run editor, resume
        if let Some(edit) = app.pending_edit.take() {
            if let Some((edit, edited_yaml)) = run_external_editor(terminal, app, edit)? {
                app.handle_edit_result(edit, edited_yaml);
            }
        }

        // Spawn exec in a new terminal window
        if app.pending_exec.is_some() {
            app.spawn_exec_terminal();
        }

        // Handle pending create — suspend TUI, open editor, apply on save
        if let Some(create) = app.pending_create.take() {
            if let Some(yaml) = run_create_editor(terminal, app, create)? {
                app.handle_create_result(yaml);
            }
        }

        // Handle pending metadata edit — suspend TUI, open editor, apply on save
        if let Some(meta_edit) = app.pending_metadata_edit.take() {
            if let Some((edit, edited_yaml)) = run_metadata_editor(terminal, app, meta_edit)? {
                app.handle_metadata_edit_result(edit, edited_yaml);
            }
        }

        if app.should_quit {
            return Ok(());
        }
    }
}

enum EditorRun {
    Saved(String),
    Failed(String),
}

/// Suspend the TUI, run `$EDITOR` on `yaml`, and resume.
///
/// `$EDITOR` may include arguments (`code --wait`). Split is whitespace-only —
/// quoted paths in the variable are not supported.
fn run_editor(terminal: &mut Terminal<CrosstermBackend<io::Stdout>>, yaml: &str) -> Result<EditorRun> {
    let tmp = tempfile::Builder::new().suffix(".yaml").tempfile()?;
    std::fs::write(tmp.path(), yaml)?;

    let (program, args) = resolve_editor();

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    let mut cmd = std::process::Command::new(&program);
    cmd.args(&args).arg(tmp.path());
    let status = cmd.status();

    let edited = std::fs::read_to_string(tmp.path());

    enable_raw_mode()?;
    execute!(terminal.backend_mut(), EnterAlternateScreen)?;
    terminal.clear()?;

    match (status, edited) {
        | (Ok(s), Ok(text)) if s.success() => Ok(EditorRun::Saved(text)),
        | (Ok(s), _) => Ok(EditorRun::Failed(format!("Editor exited with status: {s}"))),
        | (Err(e), _) => Ok(EditorRun::Failed(format!("Failed to run editor '{program}': {e}"))),
    }
}

fn resolve_editor() -> (String, Vec<String>) {
    if let Some(split) = std::env::var("EDITOR").ok().and_then(|raw| split_editor(&raw)) {
        return split;
    }
    let vim_exists = std::process::Command::new("vim")
        .arg("--version")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if vim_exists {
        ("vim".into(), Vec::new())
    } else {
        ("vi".into(), Vec::new())
    }
}

fn split_editor(raw: &str) -> Option<(String, Vec<String>)> {
    let mut parts = raw.split_whitespace();
    let program = parts.next()?.to_string();
    Some((program, parts.map(str::to_string).collect()))
}

fn run_external_editor(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    app: &mut app::App,
    edit: app::PendingEdit,
) -> Result<Option<(app::PendingEdit, String)>> {
    match run_editor(terminal, &edit.yaml)? {
        | EditorRun::Saved(yaml) => Ok(Some((edit, yaml))),
        | EditorRun::Failed(message) => {
            app.error = Some(message);
            Ok(None)
        },
    }
}

fn run_create_editor(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    app: &mut app::App,
    create: app::PendingCreate,
) -> Result<Option<String>> {
    match run_editor(terminal, &create.yaml)? {
        | EditorRun::Saved(yaml) => Ok(Some(yaml)),
        | EditorRun::Failed(message) => {
            app.error = Some(message);
            Ok(None)
        },
    }
}

fn run_metadata_editor(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    app: &mut app::App,
    edit: app::PendingMetadataEdit,
) -> Result<Option<(app::PendingMetadataEdit, String)>> {
    match run_editor(terminal, &edit.yaml)? {
        | EditorRun::Saved(yaml) => Ok(Some((edit, yaml))),
        | EditorRun::Failed(message) => {
            app.error = Some(message);
            Ok(None)
        },
    }
}

#[cfg(test)]
mod tests {
    use super::split_editor;

    #[test]
    fn editor_args_split_on_whitespace() {
        assert_eq!(
            split_editor("code --wait"),
            Some(("code".into(), vec!["--wait".into()]))
        );
        assert_eq!(split_editor("  vim  "), Some(("vim".into(), Vec::new())));
        assert_eq!(split_editor("   "), None);
    }
}
