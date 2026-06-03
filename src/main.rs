mod config;
mod init;
mod project;

use std::process::ExitCode;

use clap::{Parser, Subcommand};
use project::{ProjectError, resolve_project};

#[derive(Parser)]
#[command(name = "agira", color = clap::ColorChoice::Never)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    Status,
    Init,
}

fn main() -> ExitCode {
    let cli = Cli::parse();

    match cli.command {
        Commands::Status => match resolve_project() {
            Ok(project) => {
                let _resolved_project = (project.git_root, project.slug, project.state_dir);
                ExitCode::SUCCESS
            }
            Err(error) => {
                eprintln!("error: {error}");
                exit_code_for(&error)
            }
        },
        Commands::Init => match resolve_project() {
            Ok(project) => match init::run_init(&project) {
                Ok(()) => ExitCode::SUCCESS,
                Err(error) => {
                    eprintln!("error: {error}");
                    exit_code_for_init(&error)
                }
            },
            Err(error) => {
                eprintln!("error: {error}");
                exit_code_for(&error)
            }
        },
    }
}

fn exit_code_for(error: &ProjectError) -> ExitCode {
    match error {
        ProjectError::NotInGitRepository | ProjectError::CreateStateDir(_, _) => ExitCode::from(1),
        _ => ExitCode::from(2),
    }
}

fn exit_code_for_init(error: &init::InitError) -> ExitCode {
    match error {
        init::InitError::NonInteractive | init::InitError::InputEnded => ExitCode::from(1),
        _ => ExitCode::from(2),
    }
}
