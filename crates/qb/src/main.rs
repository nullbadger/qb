use {
    anyhow::{
        Context,
        Result,
    },
    clap::Parser,
};

mod args;
mod config;
mod k8s;
mod portforward;
mod reference;
mod tui;

#[tokio::main]
async fn main() -> Result<()> {
    let cli = args::Cli::parse();
    match cli.command {
        | Some(args::Command::Version) => {
            println!("qb {}", env!("CARGO_PKG_VERSION"));
        },
        | Some(args::Command::Man { out, format }) => {
            std::fs::create_dir_all(&out).with_context(|| format!("Failed to create directory: {}", out.display()))?;
            let builder = reference::ReferenceBuilder;
            match format {
                | args::ManualFormat::Manpages => builder.build_manpages(&out)?,
                | args::ManualFormat::Markdown => builder.build_markdown(&out)?,
            }
        },
        | Some(args::Command::Autocomplete { out, shell }) => {
            std::fs::create_dir_all(&out).with_context(|| format!("Failed to create directory: {}", out.display()))?;
            reference::ReferenceBuilder.build_shell_completion(&out, shell)?;
        },
        | None => {
            let config = match config::QbConfig::load() {
                | Ok(c) => c,
                | Err(e) => {
                    eprintln!("Warning: Failed to load config: {}", e);
                    eprintln!("Using default configuration.");
                    config::QbConfig::default_config()
                },
            };
            let saved_kubeconfig = config.active_profile().kubeconfig.clone();
            let saved_context = config.active_profile().context.clone();
            tui::run(saved_kubeconfig, saved_context, None, cli.experimental, config).await?;
        },
    }
    Ok(())
}
