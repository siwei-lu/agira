mod commands;
mod core;

use std::{path::PathBuf, process::ExitCode};

use clap::{Parser, Subcommand};

use crate::core::{ProjectError, resolve_initialized_project, resolve_project};

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
    #[command(subcommand_value_name = "command")]
    Task {
        #[command(subcommand)]
        command: TaskCommands,
    },
    /// Initialize project configuration
    Init {
        #[arg(long, value_name = "stack")]
        stack: Option<String>,
        #[arg(long, value_name = "phases")]
        phases: Option<String>,
        #[arg(long = "verification-commands", value_name = "verification-commands")]
        verification_commands: Option<String>,
        #[arg(long = "acceptance-testing", value_name = "acceptance-testing")]
        acceptance_testing: Option<String>,
        #[arg(long = "prd-path", value_name = "prd-path")]
        prd_path: Option<String>,
    },
    /// Manage workflow phases
    #[command(subcommand_value_name = "command")]
    Phase {
        #[command(subcommand)]
        command: PhaseCommands,
    },
    /// Manage lifecycle hooks
    #[command(
        subcommand_value_name = "command",
        long_about = "Manage lifecycle hooks.\n\nValid events are *, task_added, failed, and configured phase names.\n\nHook commands inject the following environment variables into every hook script:\n\n  AGIRA_TASK_ID             task ID (e.g. task-001)\n  AGIRA_TASK_TITLE          task title\n  AGIRA_TASK_DESCRIPTION    task description\n  AGIRA_TASK_STATE          current task state after the lifecycle event\n  AGIRA_TASK_PRD_MODULE_ID  PRD module ID (empty if not set)\n  AGIRA_TASK_DEPENDENCIES   comma-separated dependency IDs\n  AGIRA_TASK_RETRY_COUNT    current retry count\n  AGIRA_TASK_MAX_RETRIES    configured maximum retries for the task\n  AGIRA_TASK_CREATED_AT     RFC3339 creation timestamp\n  AGIRA_PROJECT_SLUG        lowercased git-root basename\n  AGIRA_PROJECT_PATH        canonical git root path\n  AGIRA_FROM_PHASE          phase the task is leaving (empty string for task_added)\n  AGIRA_TO_PHASE            phase/event target (initial phase for task_added)\n  AGIRA_ARTIFACT            --artifact text from 'agira task todo --artifact' (empty if not provided)\n\nExample hook script:\n\n  echo \"$AGIRA_TASK_ID transitioned to $AGIRA_TO_PHASE\""
    )]
    Hook {
        #[command(subcommand)]
        command: HookCommands,
    },
    /// List and manage initialized projects
    #[command(alias = "projects", subcommand_value_name = "command")]
    Project {
        #[command(subcommand)]
        command: ProjectCommands,
    },
    /// Update agira to the latest GitHub release
    Update,
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
        #[arg(long, value_name = "add")]
        add: Option<String>,
        /// Insert the new phase after this existing phase
        #[arg(long, value_name = "after")]
        after: Option<String>,
        /// Insert the new phase before this existing phase
        #[arg(long, value_name = "before")]
        before: Option<String>,
        /// Phase name to remove (fails if any task is currently in that phase)
        #[arg(long, value_name = "remove")]
        remove: Option<String>,
        /// Change the model of an existing phase: --set-model <phase> <model>
        #[arg(long, num_args = 2, value_names = ["phase", "model"])]
        set_model: Option<Vec<String>>,
    },
}

#[derive(Subcommand)]
enum ProjectCommands {
    /// List initialized projects and their state directories
    List,
}

#[derive(Subcommand)]
enum HookCommands {
    /// List effective lifecycle hooks
    List,
    /// Add a lifecycle hook
    #[command(
        after_help = "Valid events are *, task_added, failed, and configured phase names.\n\nEnvironment variables injected into every hook script:\n\n  AGIRA_TASK_ID             task ID (e.g. task-001)\n  AGIRA_TASK_TITLE          task title\n  AGIRA_TASK_DESCRIPTION    task description\n  AGIRA_TASK_STATE          current task state after the lifecycle event\n  AGIRA_TASK_PRD_MODULE_ID  PRD module ID (empty if not set)\n  AGIRA_TASK_DEPENDENCIES   comma-separated dependency IDs\n  AGIRA_TASK_RETRY_COUNT    current retry count\n  AGIRA_TASK_MAX_RETRIES    configured maximum retries for the task\n  AGIRA_TASK_CREATED_AT     RFC3339 creation timestamp\n  AGIRA_PROJECT_SLUG        lowercased git-root basename\n  AGIRA_PROJECT_PATH        canonical git root path\n  AGIRA_FROM_PHASE          phase the task is leaving (empty string for task_added)\n  AGIRA_TO_PHASE            phase/event target (initial phase for task_added)\n  AGIRA_ARTIFACT            --artifact text from 'agira task todo --artifact' (empty if not provided)\n\nExample:\n\n  agira hook add task_added echo \"$AGIRA_TASK_ID created in $AGIRA_TO_PHASE\""
    )]
    Add {
        /// Write the hook to ~/.agira/config.toml instead of the current project
        #[arg(long = "global")]
        global: bool,
        /// Hook event name: *, task_added, failed, or a configured phase
        #[arg(value_name = "event")]
        event: String,
        /// Shell command to run for the hook
        #[arg(value_name = "command", num_args = 1.., trailing_var_arg = true, allow_hyphen_values = true)]
        command: Vec<String>,
    },
    /// Update lifecycle hooks for an event
    #[command(
        after_help = "Valid events are *, task_added, failed, and configured phase names.\n\nEnvironment variables injected into every hook script:\n\n  AGIRA_TASK_ID             task ID (e.g. task-001)\n  AGIRA_TASK_TITLE          task title\n  AGIRA_TASK_DESCRIPTION    task description\n  AGIRA_TASK_STATE          current task state after the lifecycle event\n  AGIRA_TASK_PRD_MODULE_ID  PRD module ID (empty if not set)\n  AGIRA_TASK_DEPENDENCIES   comma-separated dependency IDs\n  AGIRA_TASK_RETRY_COUNT    current retry count\n  AGIRA_TASK_MAX_RETRIES    configured maximum retries for the task\n  AGIRA_TASK_CREATED_AT     RFC3339 creation timestamp\n  AGIRA_PROJECT_SLUG        lowercased git-root basename\n  AGIRA_PROJECT_PATH        canonical git root path\n  AGIRA_FROM_PHASE          phase the task is leaving (empty string for task_added)\n  AGIRA_TO_PHASE            phase/event target (initial phase for task_added)\n  AGIRA_ARTIFACT            --artifact text from 'agira task todo --artifact' (empty if not provided)\n\nExample:\n\n  agira hook update task_added echo \"$AGIRA_TASK_ID created in $AGIRA_TO_PHASE\""
    )]
    Update {
        /// Update hooks in ~/.agira/config.toml instead of the current project
        #[arg(long = "global")]
        global: bool,
        /// Hook event name: *, task_added, failed, or a configured phase
        event: String,
        /// Replacement shell command to run for the hook
        #[arg(value_name = "command", num_args = 1.., trailing_var_arg = true, allow_hyphen_values = true)]
        command: Vec<String>,
    },
    /// Remove all lifecycle hooks for an event
    Remove {
        /// Remove hooks from ~/.agira/config.toml instead of the current project
        #[arg(long = "global")]
        global: bool,
        /// Hook event name: *, task_added, failed, or a configured phase
        #[arg(value_name = "event")]
        event: String,
    },
}

#[derive(Subcommand)]
enum TaskCommands {
    /// Show current task status table
    Status {
        /// Output raw JSON instead of the formatted table
        #[arg(long)]
        json: bool,
        /// Number of tasks to show, or 0 to show all
        #[arg(long, default_value_t = 20, value_name = "limit")]
        limit: usize,
        /// Number of tasks to skip from the latest-first list
        #[arg(long, default_value_t = 0, value_name = "offset")]
        offset: usize,
        /// Show only this task ID
        #[arg(value_name = "task-id")]
        filter: Option<String>,
    },
    /// Print the current actionable task prompt, or advance it when --artifact is given
    Todo {
        /// Path to a PRD file to inject as requirements context (print mode only)
        #[arg(long, value_name = "prd")]
        prd: Option<PathBuf>,
        /// Evidence of completion for this phase; advances the current task when provided
        #[arg(long, value_name = "artifact")]
        artifact: Option<String>,
    },
    /// Record a task failure and retry or terminate based on retry count
    Fail {
        /// Task ID to fail (e.g. task-001)
        #[arg(value_name = "id")]
        id: String,
        /// Reason for the failure
        #[arg(long, value_name = "reason")]
        reason: Option<String>,
    },
    /// Mark a task as blocked
    Block {
        /// Task ID to block (e.g. task-001)
        #[arg(value_name = "id")]
        id: String,
        /// Reason the task is blocked
        #[arg(long, value_name = "reason")]
        reason: Option<String>,
    },
    /// Resume a blocked task
    Unblock {
        /// Task ID to unblock (e.g. task-001)
        #[arg(value_name = "id")]
        id: String,
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
        /// Place the task directly into this phase instead of the default starting phase
        #[arg(long, value_name = "phase")]
        phase: Option<String>,
    },
    /// Update editable fields of an existing task
    Update {
        /// Task ID to update (e.g. task-001)
        #[arg(value_name = "id")]
        id: String,
        /// New title
        #[arg(long, value_name = "title")]
        title: Option<String>,
        /// New description
        #[arg(long, value_name = "description")]
        description: Option<String>,
        /// New PRD module ID (e.g. FM-001)
        #[arg(long, value_name = "prd")]
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
            TaskCommands::Status {
                json,
                limit,
                offset,
                filter,
            } => match resolve_initialized_project() {
                Ok(project) => {
                    match commands::run_status(&project, json, filter.as_deref(), limit, offset) {
                        Ok(()) => ExitCode::SUCCESS,
                        Err(error) => {
                            eprintln!("error: {error}");
                            exit_code_for_status(&error)
                        }
                    }
                }
                Err(error) => {
                    eprintln!("error: {error}");
                    exit_code_for(&error)
                }
            },
            TaskCommands::Todo { prd, artifact } => match resolve_initialized_project() {
                Ok(project) => {
                    match commands::run_todo(&project, prd.as_deref(), artifact.as_deref()) {
                        Ok(()) => ExitCode::SUCCESS,
                        Err(error) => {
                            eprintln!("error: {error}");
                            exit_code_for_todo(&error)
                        }
                    }
                }
                Err(error) => {
                    eprintln!("error: {error}");
                    exit_code_for(&error)
                }
            },
            TaskCommands::Fail { id, reason } => match resolve_initialized_project() {
                Ok(project) => match commands::run_fail(&project, &id, reason.as_deref()) {
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
            TaskCommands::Block { id, reason } => match resolve_initialized_project() {
                Ok(project) => match commands::run_block(&project, &id, reason.as_deref()) {
                    Ok(()) => ExitCode::SUCCESS,
                    Err(error) => {
                        eprintln!("error: {error}");
                        exit_code_for_block(&error)
                    }
                },
                Err(error) => {
                    eprintln!("error: {error}");
                    exit_code_for(&error)
                }
            },
            TaskCommands::Unblock { id } => match resolve_initialized_project() {
                Ok(project) => match commands::run_unblock(&project, &id) {
                    Ok(()) => ExitCode::SUCCESS,
                    Err(error) => {
                        eprintln!("error: {error}");
                        exit_code_for_unblock(&error)
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
                phase,
            } => match resolve_initialized_project() {
                Ok(project) => match commands::run_add(
                    &project,
                    &title,
                    description.as_deref(),
                    prd.as_deref(),
                    &depends_on,
                    phase.as_deref(),
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
            } => match resolve_initialized_project() {
                Ok(project) => match commands::run_update(
                    &project,
                    &id,
                    commands::UpdateInput {
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
            Ok(project) => match commands::run_init(
                &project,
                commands::InitFlags {
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
                Ok(project) => match commands::run_phase_get(&project) {
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
                Ok(project) => match commands::run_phase_update(
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
        Commands::Hook { command } => match command {
            HookCommands::List => match resolve_project() {
                Ok(project) => match commands::run_hook_list(&project) {
                    Ok(()) => ExitCode::SUCCESS,
                    Err(error) => {
                        eprintln!("error: {error}");
                        exit_code_for_hook(&error)
                    }
                },
                Err(error) => {
                    eprintln!("error: {error}");
                    exit_code_for(&error)
                }
            },
            HookCommands::Add {
                global,
                event,
                command,
            } => match resolve_project() {
                Ok(project) => match commands::run_hook_add(&project, &event, &command, global) {
                    Ok(()) => ExitCode::SUCCESS,
                    Err(error) => {
                        eprintln!("error: {error}");
                        exit_code_for_hook(&error)
                    }
                },
                Err(error) => {
                    eprintln!("error: {error}");
                    exit_code_for(&error)
                }
            },
            HookCommands::Update {
                global,
                event,
                command,
            } => match resolve_project() {
                Ok(project) => {
                    match commands::run_hook_update(&project, &event, &command, global) {
                        Ok(()) => ExitCode::SUCCESS,
                        Err(error) => {
                            eprintln!("error: {error}");
                            exit_code_for_hook(&error)
                        }
                    }
                }
                Err(error) => {
                    eprintln!("error: {error}");
                    exit_code_for(&error)
                }
            },
            HookCommands::Remove { global, event } => match resolve_project() {
                Ok(project) => match commands::run_hook_remove(&project, &event, global) {
                    Ok(()) => ExitCode::SUCCESS,
                    Err(error) => {
                        eprintln!("error: {error}");
                        exit_code_for_hook(&error)
                    }
                },
                Err(error) => {
                    eprintln!("error: {error}");
                    exit_code_for(&error)
                }
            },
        },
        Commands::Project { command } => match command {
            ProjectCommands::List => match commands::run_project_list() {
                Ok(()) => ExitCode::SUCCESS,
                Err(error) => {
                    eprintln!("error: {error}");
                    exit_code_for_project_list(&error)
                }
            },
        },
        Commands::Update => match commands::run_self_update() {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("error: {error}");
                exit_code_for_self_update(&error)
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
        | ProjectError::ProjectNotInitialized
        | ProjectError::CreateStateDir(_, _)
        | ProjectError::GlobalConfig(crate::core::GlobalConfigError::Parse { .. })
        | ProjectError::HookConfig(crate::core::HookConfigError::Parse { .. }) => ExitCode::from(1),
        _ => ExitCode::from(2),
    }
}

fn exit_code_for_init(error: &commands::InitError) -> ExitCode {
    match error {
        commands::InitError::MissingFlags { .. }
        | commands::InitError::InvalidPhases
        | commands::InitError::InvalidAcceptanceTesting => ExitCode::from(1),
        _ => ExitCode::from(2),
    }
}

fn exit_code_for_status(error: &commands::StatusError) -> ExitCode {
    use commands::StatusError::*;

    match error {
        ConfigNotFound { .. } | ConfigLoad { .. } | InvalidConfig { .. } | TaskNotFound { .. } => {
            ExitCode::from(1)
        }
        ConfigRead { .. } | JsonOutput { .. } => ExitCode::from(2),
        StoreError(store_error) => match store_error {
            crate::core::StoreError::Io { .. }
            | crate::core::StoreError::Serialize(_)
            | crate::core::StoreError::Deserialize(_) => ExitCode::from(2),
            _ => ExitCode::from(1),
        },
    }
}

fn exit_code_for_todo(error: &commands::TodoError) -> ExitCode {
    use commands::TodoError::*;

    match error {
        NoActionableTask
        | EmptyArtifact
        | PrdNotFound { .. }
        | ConfigLoad { .. }
        | InvalidConfig { .. } => ExitCode::from(1),
        Io { .. } => ExitCode::from(2),
        StoreError(store_error) => match store_error {
            crate::core::StoreError::Io { .. }
            | crate::core::StoreError::Serialize(_)
            | crate::core::StoreError::Deserialize(_) => ExitCode::from(2),
            _ => ExitCode::from(1),
        },
    }
}

fn exit_code_for_fail(error: &commands::FailError) -> ExitCode {
    use commands::FailError::*;

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
            crate::core::StoreError::Io { .. }
            | crate::core::StoreError::Serialize(_)
            | crate::core::StoreError::Deserialize(_) => ExitCode::from(2),
            _ => ExitCode::from(1),
        },
    }
}

fn exit_code_for_block(error: &commands::BlockError) -> ExitCode {
    use commands::BlockError::*;

    match error {
        MissingReason
        | EmptyReason
        | TaskNotFound { .. }
        | AlreadyTerminal { .. }
        | ConfigNotFound { .. }
        | ConfigLoad { .. }
        | InvalidConfig { .. } => ExitCode::from(1),
        ConfigRead { .. } => ExitCode::from(2),
        StoreError(store_error) => match store_error {
            crate::core::StoreError::Io { .. }
            | crate::core::StoreError::Serialize(_)
            | crate::core::StoreError::Deserialize(_) => ExitCode::from(2),
            _ => ExitCode::from(1),
        },
    }
}

fn exit_code_for_unblock(error: &commands::UnblockError) -> ExitCode {
    use commands::UnblockError::*;

    match error {
        TaskNotFound { .. }
        | NotBlocked { .. }
        | ConfigNotFound { .. }
        | ConfigLoad { .. }
        | InvalidConfig { .. } => ExitCode::from(1),
        ConfigRead { .. } => ExitCode::from(2),
        StoreError(store_error) => match store_error {
            crate::core::StoreError::Io { .. }
            | crate::core::StoreError::Serialize(_)
            | crate::core::StoreError::Deserialize(_) => ExitCode::from(2),
            _ => ExitCode::from(1),
        },
    }
}

fn exit_code_for_update(error: &commands::UpdateError) -> ExitCode {
    use commands::UpdateError::*;

    match error {
        NoFields
        | TaskNotFound { .. }
        | UnknownDependency { .. }
        | CannotUpdate { .. }
        | ConfigNotFound { .. }
        | ConfigLoad { .. }
        | InvalidConfig { .. } => ExitCode::from(1),
        ConfigRead { .. } => ExitCode::from(2),
        StoreError(store_error) => match store_error {
            crate::core::StoreError::Io { .. }
            | crate::core::StoreError::Serialize(_)
            | crate::core::StoreError::Deserialize(_) => ExitCode::from(2),
            _ => ExitCode::from(1),
        },
    }
}

fn exit_code_for_self_update(error: &commands::SelfUpdateError) -> ExitCode {
    match error {
        commands::SelfUpdateError::UnsupportedPlatform { .. }
        | commands::SelfUpdateError::AssetNotFound { .. } => ExitCode::from(1),
        _ => ExitCode::from(2),
    }
}

fn exit_code_for_project_list(error: &commands::ProjectListError) -> ExitCode {
    match error {
        commands::ProjectListError::HomeDirectoryMissing => ExitCode::from(1),
        commands::ProjectListError::Read { .. } => ExitCode::from(2),
    }
}

fn exit_code_for_add(error: &commands::AddError) -> ExitCode {
    match error {
        commands::AddError::UnknownDependency { .. }
        | commands::AddError::UnknownPhase { .. }
        | commands::AddError::ConfigNotFound { .. }
        | commands::AddError::ConfigLoad { .. }
        | commands::AddError::InvalidConfig { .. } => ExitCode::from(1),
        commands::AddError::ConfigRead { .. } => ExitCode::from(2),
        commands::AddError::StoreError(store_error) => match store_error {
            crate::core::StoreError::Io { .. }
            | crate::core::StoreError::Serialize(_)
            | crate::core::StoreError::Deserialize(_) => ExitCode::from(2),
            _ => ExitCode::from(1),
        },
    }
}

fn exit_code_for_phase_get(error: &commands::PhaseGetError) -> ExitCode {
    use commands::PhaseGetError::*;

    match error {
        NotFound { .. } | Load { .. } | InvalidConfig { .. } => ExitCode::from(1),
        Read { .. } => ExitCode::from(2),
    }
}

fn exit_code_for_phase_update(error: &commands::PhaseUpdateError) -> ExitCode {
    use commands::PhaseUpdateError::*;

    match error {
        NoOperation
        | ConflictingPositionFlags
        | InvalidAddFormat
        | UnknownModel { .. }
        | PhaseNotFound { .. }
        | DuplicatePhase { .. }
        | MandatoryPhase { .. }
        | MandatoryPhaseNoModel { .. }
        | CannotInsertBeforeInitial { .. }
        | CannotInsertAfterTerminal { .. }
        | PhaseBusy { .. }
        | ConfigNotFound { .. }
        | ConfigLoad { .. }
        | InvalidConfig { .. } => ExitCode::from(1),
        ConfigRead { .. } | ConfigWrite { .. } => ExitCode::from(2),
        StoreError(store_error) => match store_error {
            crate::core::StoreError::Io { .. }
            | crate::core::StoreError::Serialize(_)
            | crate::core::StoreError::Deserialize(_) => ExitCode::from(2),
            _ => ExitCode::from(1),
        },
    }
}

fn exit_code_for_hook(error: &commands::HookError) -> ExitCode {
    use commands::HookError::*;

    match error {
        InvalidEventName
        | UnknownEvent { .. }
        | EmptyCommand
        | HookNotFound { .. }
        | ConfigNotFound { .. }
        | ConfigLoad { .. }
        | InvalidConfig { .. } => ExitCode::from(1),
        ConfigRead { .. } | Delete { .. } => ExitCode::from(2),
        Hooks(hook_error) => match hook_error {
            crate::core::HookConfigError::Parse { .. } => ExitCode::from(1),
            crate::core::HookConfigError::Read { .. }
            | crate::core::HookConfigError::Serialize { .. }
            | crate::core::HookConfigError::Write { .. } => ExitCode::from(2),
        },
    }
}
