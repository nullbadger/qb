use {
    clap::{
        Parser,
        Subcommand,
        ValueEnum,
    },
    std::path::PathBuf,
};

#[derive(Parser)]
#[command(name = "qb", about = "Kubernetes resource browser")]
pub struct Cli {
    /// Enable experimental features (exec shell, etc.)
    #[arg(short = 'e', long = "experimental")]
    pub experimental: bool,

    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Subcommand)]
pub enum Command {
    /// Print version information
    Version,
    /// Renders the manual.
    Man {
        /// Directory to write into.
        #[arg(short = 'o', long = "out")]
        out: PathBuf,
        /// `manpages` or `markdown`.
        #[arg(short = 'f', long = "format", value_enum)]
        format: ManualFormat,
    },
    /// Renders shell completion scripts.
    Autocomplete {
        /// Directory to write into.
        #[arg(short = 'o', long = "out")]
        out: PathBuf,
        /// bash, zsh, fish, elvish, or powershell.
        #[arg(short = 's', long = "shell", value_enum)]
        shell: clap_complete::Shell,
    },
}

#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum ManualFormat {
    Manpages,
    Markdown,
}
