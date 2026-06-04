mod add;
mod config;
mod done;
mod fail;
mod global_config;
mod init;
mod next;
mod project;
mod status;
mod tasks;

use std::{path::PathBuf, process::ExitCode};

use clap::{Parser, Subcommand};
use project::{resolve_project, ProjectError};

#[derive(Parser)]
#[command(name = "agira", version = concat!(env!("CARGO_PKG_VERSION"), " (", env!("BUILD_TARGET"), ")"), disable_version_flag = true, color = clap::ColorChoice::Never, about = "Orchestrate AI-assisted software development workflows", long_version = concat!(env!("CARGO_PKG_VERSION"), " (", env!("BUILD_TARGET"), ")"))]
struct Cli {
    /// Print version
    #[arg(short = 'v', long = "version", action = clap::ArgAction::Version)]
    version: Option<bool>,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Show current task status table
    Status {
        /// Output raw JSON instead of the formatted table
        #[arg(long)]
        json: bool,
    },
    /// Initialize project configuration interactively
    Init,
    /// Print the next actionable task prompt for the AI agent
    Next {
        /// Path to a PRD file to inject as requirements context
        #[arg(long)]
        prd: Option<PathBuf>,
    },
    /// Advance a task to its next phase, recording an artifact as evidence
    Done {
        /// Task ID to advance (e.g. task-001)
        id: String,
        /// Evidence of completion for this phase
        #[arg(long)]
        artifact: Option<String>,
    },
    /// Record a task failure and retry or terminate based on retry count
    Fail {
        /// Task ID to fail (e.g. task-001)
        id: String,
        /// Reason for the failure
        #[arg(long)]
        reason: Option<String>,
    },
    /// Add a new task to the project
    Add {
        /// Task title
        title: String,
        /// Optional longer description of the task
        #[arg(long)]
        description: Option<String>,
        /// PRD module ID this task implements (e.g. FM-001)
        #[arg(long)]
        prd: Option<String>,
        /// Comma-separated task IDs this task depends on
        #[arg(long, value_delimiter = ',')]
        depends_on: Vec<String>,
    },
    /// Print version information
    Version,
}

fn main() -> ExitCode {
    let cli = Cli::parse();

    match cli.command {
        Commands::Status { json } => match resolve_project() {
            Ok(project) => match status::run_status(&project, json) {
                Ok(()) => ExitCode::SUCCESS,
                Err(error) => {
                    eprintln!("error: {error}");
                    exit_code_for_status(&error)
                }
            },
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
                    exit_code_for_next(&error)
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
        Commands::Add {
            title,
            description,
            prd,
            depends_on,
        } => match resolve_project() {
            Ok(project) => match add::run_add(
                &project,
                &title,
                description.as_deref(),
                prd.as_deref(),
                &depends_on,
            ) {
                Ok(()) => ExitCode::SUCCESS,
                Err(error) => {
                    eprintln!("error: {error}");
                    exit_code_for_add(&error)
                }
            },
            Err(error) => {
                eprintln!("error: {error}");
                exit_code_for(&error)
            }
        },
        Commands::Version => {
            println!("agira {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
    }
}

fn exit_code_for(error: &ProjectError) -> ExitCode {
    match error {
        ProjectError::NotInGitRepository
        | ProjectError::CreateStateDir(_, _)
        | ProjectError::GlobalConfig(crate::global_config::GlobalConfigError::Parse { .. }) => {
            ExitCode::from(1)
        }
        _ => ExitCode::from(2),
    }
}

fn exit_code_for_init(error: &init::InitError) -> ExitCode {
    match error {
        init::InitError::NonInteractive | init::InitError::InputEnded => ExitCode::from(1),
        _ => ExitCode::from(2),
    }
}

fn exit_code_for_status(error: &status::StatusError) -> ExitCode {
    use status::StatusError::*;

    match error {
        ConfigNotFound { .. } | ConfigLoad { .. } | InvalidConfig { .. } => ExitCode::from(1),
        ConfigRead { .. } | JsonOutput { .. } => ExitCode::from(2),
        StoreError(store_error) => match store_error {
            crate::tasks::StoreError::Io { .. }
            | crate::tasks::StoreError::Serialize(_)
            | crate::tasks::StoreError::Deserialize(_) => ExitCode::from(2),
            _ => ExitCode::from(1),
        },
    }
}

fn exit_code_for_next(error: &next::NextError) -> ExitCode {
    match error {
        next::NextError::PrdNotFound { .. } | next::NextError::ConfigLoad { .. } => {
            ExitCode::from(1)
        }
        next::NextError::Io { .. }
        | next::NextError::StoreError(crate::tasks::StoreError::Io { .. })
        | next::NextError::StoreError(crate::tasks::StoreError::Serialize(_))
        | next::NextError::StoreError(crate::tasks::StoreError::Deserialize(_)) => {
            ExitCode::from(2)
        }
        next::NextError::StoreError(_) => ExitCode::from(1),
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

fn exit_code_for_add(error: &add::AddError) -> ExitCode {
    match error {
        add::AddError::UnknownDependency { .. }
        | add::AddError::ConfigNotFound { .. }
        | add::AddError::ConfigLoad { .. }
        | add::AddError::InvalidConfig { .. } => ExitCode::from(1),
        add::AddError::ConfigRead { .. } => ExitCode::from(2),
        add::AddError::StoreError(store_error) => match store_error {
            crate::tasks::StoreError::Io { .. }
            | crate::tasks::StoreError::Serialize(_)
            | crate::tasks::StoreError::Deserialize(_) => ExitCode::from(2),
            _ => ExitCode::from(1),
        },
    }
}
