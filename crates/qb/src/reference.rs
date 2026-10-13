use {
    crate::args::Cli,
    anyhow::{
        Context,
        Result,
    },
    clap::CommandFactory,
    clap_complete::Shell,
    clap_mangen::Man,
    std::{
        fs::File,
        io::Write,
        path::Path,
    },
};

/// Manpages, markdown, and shell completions generated from the CLI definition.
pub(crate) struct ReferenceBuilder;

impl ReferenceBuilder {
    pub(crate) fn build_shell_completion(&self, outdir: &Path, shell: Shell) -> Result<()> {
        let mut app = Cli::command();
        clap_complete::generate_to(shell, &mut app, env!("CARGO_PKG_NAME"), outdir)
            .with_context(|| format!("Failed to generate {} completion in '{}'", shell, outdir.display()))?;
        Ok(())
    }

    pub(crate) fn build_markdown(&self, outdir: &Path) -> Result<()> {
        for (name, command) in self.collect_commands() {
            let path = outdir.join(format!("{name}.md"));
            let mut file = File::create(&path)
                .with_context(|| format!("Failed to create Markdown reference '{}'", path.display()))?;
            file.write_all(clap_markdown::help_markdown_command(&command).as_bytes())
                .with_context(|| format!("Failed to write Markdown reference '{}'", path.display()))?;
        }
        Ok(())
    }

    pub(crate) fn build_manpages(&self, outdir: &Path) -> Result<()> {
        for (name, command) in self.collect_commands() {
            let path = outdir.join(format!("{name}.1"));
            let mut file =
                File::create(&path).with_context(|| format!("Failed to create manpage '{}'", path.display()))?;
            Man::new(command)
                .render(&mut file)
                .with_context(|| format!("Failed to render manpage '{}'", path.display()))?;
        }
        Ok(())
    }

    fn collect_commands(&self) -> Vec<(String, clap::Command)> {
        let root = Cli::command();
        let mut pending = vec![(root.get_name().to_string(), root)];
        let mut commands = Vec::new();
        while let Some((name, command)) = pending.pop() {
            let subcommands: Vec<_> = command.get_subcommands().cloned().collect();
            for subcommand in subcommands.into_iter().rev() {
                pending.push((format!("{name}-{}", subcommand.get_name()), subcommand));
            }
            commands.push((name, command));
        }
        commands
    }
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        clap::ValueEnum,
    };

    #[test]
    fn reference_files_cover_every_shell_qop_ships() {
        let shells = ["bash", "zsh", "fish", "elvish", "powershell"];
        for name in shells {
            assert!(Shell::from_str(name, true).is_ok(), "{name}");
        }
    }

    #[test]
    fn manpages_and_completions_write_into_the_output_dir() {
        let docs = tempfile::tempdir().unwrap();
        let builder = ReferenceBuilder;
        builder.build_manpages(docs.path()).unwrap();
        builder.build_markdown(docs.path()).unwrap();
        assert!(docs.path().join("qb.1").is_file());
        assert!(docs.path().join("qb-man.1").is_file());
        assert!(docs.path().join("qb.md").is_file());

        let completions = tempfile::tempdir().unwrap();
        builder.build_shell_completion(completions.path(), Shell::Bash).unwrap();
        assert!(completions.path().read_dir().unwrap().next().is_some());
    }
}
