mod config;
mod done;
mod fail;
mod init;
mod next;
mod project;
mod tasks;

use std::{path::PathBuf, process::ExitCode};

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
    Next {
        #[arg(long)]
        prd: Option<PathBuf>,
    },
    Done {
        id: String,
        #[arg(long)]
        artifact: Option<String>,
    },
    Fail {
        id: String,
        #[arg(long)]
        reason: Option<String>,
    },
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
        Commands::Next { prd } => match resolve_project() {
            Ok(project) => match next::run_next(&project, prd.as_deref()) {
                Ok(()) => ExitCode::SUCCESS,
                Err(error) => {
                    eprintln!("error: {error}");
                    ExitCode::from(1)
                }
            },
            Err(error) => {
                eprintln!("error: {error}");
                exit_code_for(&error)
            }
        },
        Commands::Done { id, artifact } => match resolve_project() {
            Ok(project) => match done::run_done(&project, &id, artifact.as_deref()) {
                Ok(()) => ExitCode::SUCCESS,
                Err(error) => {
                    eprintln!("error: {error}");
                    exit_code_for_done(&error)
                }
            },
            Err(error) => {
                eprintln!("error: {error}");
                exit_code_for(&error)
            }
        },
        Commands::Fail { id, reason } => match resolve_project() {
            Ok(project) => match fail::run_fail(&project, &id, reason.as_deref()) {
                Ok(()) => ExitCode::SUCCESS,
                Err(error) => {
                    eprintln!("error: {error}");
                    exit_code_for_fail(&error)
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

fn exit_code_for_done(error: &done::DoneError) -> ExitCode {
    use done::DoneError::*;

    match error {
        MissingArtifact
        | EmptyArtifact
        | TaskNotFound { .. }
        | AlreadyTerminal { .. }
        | ConfigNotFound { .. }
        | ConfigLoad { .. }
        | InvalidConfig { .. } => ExitCode::from(1),
        ConfigRead { .. } => ExitCode::from(2),
        StoreError(store_error) => match store_error {
            crate::tasks::StoreError::Io { .. }
            | crate::tasks::StoreError::Serialize(_)
            | crate::tasks::StoreError::Deserialize(_) => ExitCode::from(2),
            _ => ExitCode::from(1),
        },
    }
}

fn exit_code_for_fail(error: &fail::FailError) -> ExitCode {
    use fail::FailError::*;

    match error {
        MissingReason
        | EmptyReason
        | TaskNotFound { .. }
        | AlreadyFailed { .. }
        | AlreadyTerminal { .. }
        | ConfigNotFound { .. }
        | ConfigLoad { .. }
        | InvalidConfig { .. } => ExitCode::from(1),
        ConfigRead { .. } => ExitCode::from(2),
        StoreError(store_error) => match store_error {
            crate::tasks::StoreError::Io { .. }
            | crate::tasks::StoreError::Serialize(_)
            | crate::tasks::StoreError::Deserialize(_) => ExitCode::from(2),
            _ => ExitCode::from(1),
        },
    }
}
