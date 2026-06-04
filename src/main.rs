mod add;
mod advance;
mod config;
mod fail;
mod global_config;
mod init;
mod phase;
mod pick;
mod project;
mod status;
mod tasks;
mod update;
mod work;

use std::{path::PathBuf, process::ExitCode};

use clap::{Parser, Subcommand};
use project::{ProjectError, resolve_project};

#[derive(Parser)]
#[command(name = "agira", version = concat!(env!("CARGO_PKG_VERSION"), " (", env!("BUILD_TARGET"), ")"), disable_version_flag = true, color = clap::ColorChoice::Never, about = "Orchestrate AI-assisted software development workflows", long_version = concat!(env!("CARGO_PKG_VERSION"), " (", env!("BUILD_TARGET"), ")"), subcommand_value_name = "command")]
struct Cli {
    /// Print version
    #[arg(short = 'v', long = "version", action = clap::ArgAction::Version)]
    version: Option<bool>,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Manage project tasks
    Task {
        #[command(subcommand)]
        command: TaskCommands,
    },
    /// Initialize project configuration
    Init {
        #[arg(long)]
        stack: Option<String>,
        #[arg(long)]
        phases: Option<String>,
        #[arg(long = "verification-commands")]
        verification_commands: Option<String>,
        #[arg(long = "acceptance-testing")]
        acceptance_testing: Option<String>,
        #[arg(long = "prd-path")]
        prd_path: Option<String>,
    },
    /// Manage workflow phases
    Phase {
        #[command(subcommand)]
        command: PhaseCommands,
    },
    /// Print version information
    Version,
}

#[derive(Subcommand)]
enum PhaseCommands {
    /// List current phases in the state machine
    Get,
    /// Add, insert, or remove phases in the state machine
    Update {
        /// Phase to add in phase:model format (e.g. review:opus); appended at end unless --after or --before
        #[arg(long)]
        add: Option<String>,
        /// Insert the new phase after this existing phase
        #[arg(long)]
        after: Option<String>,
        /// Insert the new phase before this existing phase
        #[arg(long)]
        before: Option<String>,
        /// Phase name to remove (fails if any task is currently in that phase)
        #[arg(long)]
        remove: Option<String>,
        /// Change the model of an existing phase: --set-model <phase> <model>
        #[arg(long, num_args = 2, value_names = ["phase", "model"])]
        set_model: Option<Vec<String>>,
    },
}

#[derive(Subcommand)]
enum TaskCommands {
    /// Show current task status table
    Status {
        /// Output raw JSON instead of the formatted table
        #[arg(long)]
        json: bool,
        /// Show only this task ID
        #[arg(value_name = "task-id")]
        filter: Option<String>,
    },
    /// Print the current actionable task prompt, or advance it when --artifact is given
    Work {
        /// Path to a PRD file to inject as requirements context (print mode only)
        #[arg(long)]
        prd: Option<PathBuf>,
        /// Evidence of completion for this phase; advances the current task when provided
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
        #[arg(value_name = "title")]
        title: String,
        /// Optional longer description of the task
        #[arg(long, value_name = "description")]
        description: Option<String>,
        /// PRD module ID this task implements (e.g. FM-001)
        #[arg(long, value_name = "prd")]
        prd: Option<String>,
        /// Comma-separated task IDs this task depends on
        #[arg(long, value_delimiter = ',', value_name = "depends-on")]
        depends_on: Vec<String>,
    },
    /// Update editable fields of an existing task
    Update {
        /// Task ID to update (e.g. task-001)
        id: String,
        /// New title
        #[arg(long)]
        title: Option<String>,
        /// New description
        #[arg(long)]
        description: Option<String>,
        /// New PRD module ID (e.g. FM-001)
        #[arg(long)]
        prd: Option<String>,
        /// Replacement comma-separated dependency list
        #[arg(long, value_delimiter = ',', value_name = "depends-on")]
        depends_on: Option<Vec<String>>,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();

    match cli.command {
        Commands::Task { command } => match command {
            TaskCommands::Status { json, filter } => match resolve_project() {
                Ok(project) => match status::run_status(&project, json, filter.as_deref()) {
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
            TaskCommands::Work { prd, artifact } => match resolve_project() {
                Ok(project) => {
                    match work::run_work(&project, prd.as_deref(), artifact.as_deref()) {
                        Ok(()) => ExitCode::SUCCESS,
                        Err(error) => {
                            eprintln!("error: {error}");
                            exit_code_for_work(&error)
                        }
                    }
                }
                Err(error) => {
                    eprintln!("error: {error}");
                    exit_code_for(&error)
                }
            },
            TaskCommands::Fail { id, reason } => match resolve_project() {
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
            TaskCommands::Add {
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
            TaskCommands::Update {
                id,
                title,
                description,
                prd,
                depends_on,
            } => match resolve_project() {
                Ok(project) => match update::run_update(
                    &project,
                    &id,
                    update::UpdateInput {
                        title,
                        description,
                        prd,
                        depends_on,
                    },
                ) {
                    Ok(()) => ExitCode::SUCCESS,
                    Err(error) => {
                        eprintln!("error: {error}");
                        exit_code_for_update(&error)
                    }
                },
                Err(error) => {
                    eprintln!("error: {error}");
                    exit_code_for(&error)
                }
            },
        },
        Commands::Init {
            stack,
            phases,
            verification_commands,
            acceptance_testing,
            prd_path,
        } => match resolve_project() {
            Ok(project) => match init::run_init(
                &project,
                init::InitFlags {
                    stack,
                    phases,
                    verification_commands,
                    acceptance_testing,
                    prd_path,
                },
            ) {
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
        Commands::Phase { command } => match command {
            PhaseCommands::Get => match resolve_project() {
                Ok(project) => match phase::run_phase_get(&project) {
                    Ok(()) => ExitCode::SUCCESS,
                    Err(error) => {
                        eprintln!("error: {error}");
                        exit_code_for_phase_get(&error)
                    }
                },
                Err(error) => {
                    eprintln!("error: {error}");
                    exit_code_for(&error)
                }
            },
            PhaseCommands::Update {
                add,
                after,
                before,
                remove,
                set_model,
            } => match resolve_project() {
                Ok(project) => match phase::run_phase_update(
                    &project,
                    add.as_deref(),
                    after.as_deref(),
                    before.as_deref(),
                    remove.as_deref(),
                    set_model.as_deref(),
                ) {
                    Ok(()) => ExitCode::SUCCESS,
                    Err(error) => {
                        eprintln!("error: {error}");
                        exit_code_for_phase_update(&error)
                    }
                },
                Err(error) => {
                    eprintln!("error: {error}");
                    exit_code_for(&error)
                }
            },
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
        init::InitError::MissingFlags { .. }
        | init::InitError::InvalidPhases
        | init::InitError::InvalidAcceptanceTesting => ExitCode::from(1),
        _ => ExitCode::from(2),
    }
}

fn exit_code_for_status(error: &status::StatusError) -> ExitCode {
    use status::StatusError::*;

    match error {
        ConfigNotFound { .. } | ConfigLoad { .. } | InvalidConfig { .. } | TaskNotFound { .. } => {
            ExitCode::from(1)
        }
        ConfigRead { .. } | JsonOutput { .. } => ExitCode::from(2),
        StoreError(store_error) => match store_error {
            crate::tasks::StoreError::Io { .. }
            | crate::tasks::StoreError::Serialize(_)
            | crate::tasks::StoreError::Deserialize(_) => ExitCode::from(2),
            _ => ExitCode::from(1),
        },
    }
}

fn exit_code_for_work(error: &work::WorkError) -> ExitCode {
    use work::WorkError::*;

    match error {
        NoActionableTask
        | EmptyArtifact
        | PrdNotFound { .. }
        | ConfigLoad { .. }
        | InvalidConfig { .. } => ExitCode::from(1),
        Io { .. } => ExitCode::from(2),
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

fn exit_code_for_update(error: &update::UpdateError) -> ExitCode {
    use update::UpdateError::*;

    match error {
        NoFields
        | TaskNotFound { .. }
        | UnknownDependency { .. }
        | ConfigNotFound { .. }
        | ConfigLoad { .. } => ExitCode::from(1),
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

fn exit_code_for_phase_get(error: &phase::PhaseGetError) -> ExitCode {
    use phase::PhaseGetError::*;

    match error {
        NotFound { .. } | Load { .. } => ExitCode::from(1),
        Read { .. } => ExitCode::from(2),
    }
}

fn exit_code_for_phase_update(error: &phase::PhaseUpdateError) -> ExitCode {
    use phase::PhaseUpdateError::*;

    match error {
        NoOperation
        | ConflictingPositionFlags
        | InvalidAddFormat
        | UnknownModel { .. }
        | PhaseNotFound { .. }
        | DuplicatePhase { .. }
        | PhaseBusy { .. }
        | ConfigNotFound { .. }
        | ConfigLoad { .. } => ExitCode::from(1),
        ConfigRead { .. } | ConfigWrite { .. } => ExitCode::from(2),
        StoreError(store_error) => match store_error {
            crate::tasks::StoreError::Io { .. }
            | crate::tasks::StoreError::Serialize(_)
            | crate::tasks::StoreError::Deserialize(_) => ExitCode::from(2),
            _ => ExitCode::from(1),
        },
    }
}
