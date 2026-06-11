mod commands;
mod core;

use std::{env, path::PathBuf, process::ExitCode};

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
    },
    /// Manage workflow phases
    #[command(subcommand_value_name = "command")]
    Phase {
        #[command(subcommand)]
        command: PhaseCommands,
    },
    /// Manage global agira configuration
    #[command(
        subcommand_value_name = "command",
        long_about = "Manage global agira configuration.\n\nValid config keys:\n\n  config get shows default-max-retries and hook-debug.\n  Only hook-debug is settable with value true or false."
    )]
    Config {
        #[command(subcommand)]
        command: ConfigCommands,
    },
    /// Manage lifecycle hooks
    #[command(
        subcommand_value_name = "command",
        long_about = "Manage lifecycle hooks.\n\nValid events are *, task_added, all_tasks_done, failed, and configured phase names.\n\nHook commands inject the following environment variables into every hook script:\n\n  AGIRA_TASK_ID             task ID (e.g. task-001)\n  AGIRA_TASK_TITLE          task title\n  AGIRA_TASK_DESCRIPTION    task description\n  AGIRA_TASK_STATE          current task state after the lifecycle event\n  AGIRA_TASK_DEPENDENCIES   comma-separated dependency IDs\n  AGIRA_TASK_RETRY_COUNT    current retry count\n  AGIRA_TASK_MAX_RETRIES    configured maximum retries for the task\n  AGIRA_TASK_CREATED_AT     RFC3339 creation timestamp\n  AGIRA_PROJECT_SLUG        lowercased git-root basename\n  AGIRA_PROJECT_PATH        canonical git root path\n  AGIRA_FROM_PHASE          phase the task is leaving (empty string for task_added)\n  AGIRA_TO_PHASE            phase/event target (initial phase for task_added)\n  AGIRA_ARTIFACT            --artifact text from 'agira task todo --artifact' (empty if not provided)\n\nDebug logging:\n\n  Use `agira config set hook-debug true` to enable hook debug logging.\n\nExample hook script:\n\n  echo \"$AGIRA_TASK_ID transitioned to $AGIRA_TO_PHASE\""
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
    /// List and manage named workflows defined in the project config
    #[command(subcommand_value_name = "command")]
    Workflow {
        #[command(subcommand)]
        command: WorkflowCommands,
    },
    /// Manage the tmux-backed project runner
    #[command(subcommand_value_name = "command")]
    Runner {
        #[command(subcommand)]
        command: RunnerCommands,
    },
    /// Print prompts to install or uninstall an agira task-add personal skill
    #[command(subcommand_value_name = "command")]
    Skill {
        #[command(subcommand)]
        command: SkillCommands,
    },
    /// Update agira to the latest GitHub release
    Update,
    /// Print version information
    Version,
}

#[derive(Subcommand)]
enum PhaseCommands {
    /// List global phase definitions
    List,
    /// Add a global phase definition
    Add {
        #[arg(value_name = "name")]
        name: String,
        #[arg(long, value_name = "model")]
        model: Option<String>,
        #[arg(long, value_name = "duty")]
        duty: Option<String>,
    },
    /// Update a global phase definition
    Update {
        #[arg(value_name = "name")]
        name: String,
        #[arg(long = "set-model", value_name = "model")]
        set_model: Option<String>,
        #[arg(long = "set-duty", value_name = "duty")]
        set_duty: Option<String>,
        #[arg(long = "clear-model")]
        clear_model: bool,
        #[arg(long = "clear-duty")]
        clear_duty: bool,
        #[arg(long = "set-gate", value_name = "cmd")]
        set_gate: Option<String>,
        #[arg(long = "unset-gate")]
        unset_gate: bool,
    },
    /// Remove a global phase definition
    Remove {
        #[arg(value_name = "name")]
        name: String,
    },
}

#[derive(Subcommand)]
enum ProjectCommands {
    /// List initialized projects and their state directories
    List,
}

#[derive(Subcommand)]
enum SkillCommands {
    /// Print a prompt asking an agent to write the agira task-add personal skill
    Install,
    /// Print a prompt asking an agent to delete the agira task-add personal skill
    Uninstall,
}

#[derive(Subcommand)]
enum WorkflowCommands {
    /// List all named workflows defined in the project config
    List {
        /// Output raw JSON instead of the formatted table
        #[arg(long)]
        json: bool,
    },
    /// Add a named workflow from existing phase references
    Add {
        #[arg(value_name = "name")]
        name: String,
        #[arg(long, value_delimiter = ',', value_name = "phases")]
        phases: Vec<String>,
    },
    /// Update a workflow's ordered phase references
    Update {
        #[arg(value_name = "name")]
        name: String,
        #[arg(long, value_name = "phase")]
        add: Option<String>,
        #[arg(long, value_name = "phase")]
        after: Option<String>,
        #[arg(long, value_name = "phase")]
        remove: Option<String>,
    },
    /// Remove a named workflow
    Remove {
        #[arg(value_name = "name")]
        name: String,
    },
    /// Set the default workflow
    SetDefault {
        #[arg(value_name = "name")]
        name: String,
    },
}

#[derive(Subcommand)]
enum RunnerCommands {
    /// Start the tmux-backed runner
    Start {
        /// Runner type recorded in the registry
        #[arg(long = "type", value_name = "type", default_value = "claude-tmux")]
        runner_type: String,
    },
    /// Stop the tmux-backed runner
    Stop,
    /// Show runner liveness and registry state
    Status,
    /// Attach to the tmux-backed runner
    Attach,
    /// Print the tmux pipe-pane log
    Logs {
        /// Follow the log file
        #[arg(short = 'f', long)]
        follow: bool,
    },
}

#[derive(Subcommand)]
enum ConfigCommands {
    /// List global config settings
    #[command(after_help = "Displayed config keys:\n\n  default-max-retries\n  hook-debug")]
    Get,
    /// Set a global config setting
    #[command(
        after_help = "valid keys: hook-debug\n\nOnly hook-debug is settable. Value must be true or false."
    )]
    Set {
        /// Config key to update
        #[arg(value_name = "key")]
        key: String,
        /// Config value to write
        #[arg(value_name = "value")]
        value: String,
    },
}

#[derive(Subcommand)]
enum HookCommands {
    /// List effective lifecycle hooks
    List,
    /// Add a lifecycle hook
    #[command(
        after_help = "Valid events are *, task_added, all_tasks_done, failed, and configured phase names.\n\nEnvironment variables injected into every hook script:\n\n  AGIRA_TASK_ID             task ID (e.g. task-001)\n  AGIRA_TASK_TITLE          task title\n  AGIRA_TASK_DESCRIPTION    task description\n  AGIRA_TASK_STATE          current task state after the lifecycle event\n  AGIRA_TASK_DEPENDENCIES   comma-separated dependency IDs\n  AGIRA_TASK_RETRY_COUNT    current retry count\n  AGIRA_TASK_MAX_RETRIES    configured maximum retries for the task\n  AGIRA_TASK_CREATED_AT     RFC3339 creation timestamp\n  AGIRA_PROJECT_SLUG        lowercased git-root basename\n  AGIRA_PROJECT_PATH        canonical git root path\n  AGIRA_FROM_PHASE          phase the task is leaving (empty string for task_added)\n  AGIRA_TO_PHASE            phase/event target (initial phase for task_added)\n  AGIRA_ARTIFACT            --artifact text from 'agira task todo --artifact' (empty if not provided)\n\nDebug logging:\n\n  Use `agira config set hook-debug true` to enable hook debug logging.\n\nExample:\n\n  agira hook add task_added echo \"$AGIRA_TASK_ID created in $AGIRA_TO_PHASE\""
    )]
    Add {
        /// Write the hook to ~/.agira/config.toml instead of the current project
        #[arg(long = "global")]
        global: bool,
        /// Hook event name: *, task_added, all_tasks_done, failed, or a configured phase
        #[arg(long = "on", value_name = "event")]
        on: Option<String>,
        /// Hook event followed by command, or just command when --on is provided
        #[arg(value_name = "event command", num_args = 1.., trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Update lifecycle hooks for an event
    #[command(
        after_help = "Valid events are *, task_added, all_tasks_done, failed, and configured phase names.\n\nEnvironment variables injected into every hook script:\n\n  AGIRA_TASK_ID             task ID (e.g. task-001)\n  AGIRA_TASK_TITLE          task title\n  AGIRA_TASK_DESCRIPTION    task description\n  AGIRA_TASK_STATE          current task state after the lifecycle event\n  AGIRA_TASK_DEPENDENCIES   comma-separated dependency IDs\n  AGIRA_TASK_RETRY_COUNT    current retry count\n  AGIRA_TASK_MAX_RETRIES    configured maximum retries for the task\n  AGIRA_TASK_CREATED_AT     RFC3339 creation timestamp\n  AGIRA_PROJECT_SLUG        lowercased git-root basename\n  AGIRA_PROJECT_PATH        canonical git root path\n  AGIRA_FROM_PHASE          phase the task is leaving (empty string for task_added)\n  AGIRA_TO_PHASE            phase/event target (initial phase for task_added)\n  AGIRA_ARTIFACT            --artifact text from 'agira task todo --artifact' (empty if not provided)\n\nDebug logging:\n\n  Use `agira config set hook-debug true` to enable hook debug logging.\n\nExample:\n\n  agira hook update task_added echo \"$AGIRA_TASK_ID created in $AGIRA_TO_PHASE\""
    )]
    Update {
        /// Update hooks in ~/.agira/config.toml instead of the current project
        #[arg(long = "global")]
        global: bool,
        /// Hook event name: *, task_added, all_tasks_done, failed, or a configured phase
        #[arg(value_name = "event")]
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
        /// Hook event name: *, task_added, all_tasks_done, failed, or a configured phase
        #[arg(value_name = "event")]
        event: String,
    },
}

#[derive(Subcommand)]
enum TaskCommands {
    /// Show a detailed view of a single task
    Inspect {
        /// Task ID to inspect (e.g. task-001)
        #[arg(value_name = "task-id")]
        id: String,
    },
    /// List tasks as a table; defaults to the latest 20 tasks
    List {
        /// Output raw JSON instead of the formatted table
        #[arg(long)]
        json: bool,
        /// Number of tasks to show, or 0 to show all; default shows the latest 20 tasks
        #[arg(long, value_name = "limit")]
        limit: Option<usize>,
        /// Number of tasks to skip from the start of the ascending list
        #[arg(long, value_name = "offset")]
        offset: Option<usize>,
        /// Show only this task ID
        #[arg(value_name = "task-id")]
        filter: Option<String>,
    },
    /// Print the current actionable task prompt, or advance it when --artifact is given
    Todo {
        /// Evidence of completion for this phase; advances the current task when provided
        #[arg(long, value_name = "artifact")]
        artifact: Option<String>,
        /// Runner identity used to claim the selected task lease
        #[arg(long, value_name = "id")]
        runner: Option<String>,
        /// Target a specific task by ID instead of the automatically selected next task
        #[arg(long = "task", value_name = "id")]
        task_id: Option<String>,
        /// Expected current phase of the task; used for compare-and-swap safety when combined with --artifact
        #[arg(long, value_name = "phase")]
        from: Option<String>,
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
        /// Comma-separated task IDs this task depends on
        #[arg(long, value_delimiter = ',', value_name = "depends-on")]
        depends_on: Vec<String>,
        /// Place the task directly into this phase instead of the default starting phase
        #[arg(long, value_name = "phase")]
        phase: Option<String>,
        /// Use a named workflow from the project config to execute this task
        #[arg(long, value_name = "workflow")]
        workflow: Option<String>,
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
        /// Replacement comma-separated dependency list
        #[arg(long, value_delimiter = ',', value_name = "depends-on")]
        depends_on: Option<Vec<String>>,
    },
    /// Remove a pending task from the project
    Remove {
        /// Task ID to remove (e.g. task-001)
        #[arg(value_name = "id")]
        id: String,
    },
    /// Mark a task as worker-locked (advisory; skipped by task todo until lock expires or is cleared)
    #[command(hide = true)]
    Lock {
        /// Task ID to lock (e.g. task-001)
        #[arg(value_name = "id")]
        id: String,
    },
    /// Clear the worker lock on a task
    #[command(hide = true)]
    Unlock {
        /// Task ID to unlock (e.g. task-001)
        #[arg(value_name = "id")]
        id: String,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();

    match cli.command {
        Commands::Task { command } => match command {
            TaskCommands::Inspect { id } => match resolve_initialized_project() {
                Ok(project) => match commands::run_inspect(&project, &id) {
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
            TaskCommands::List {
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
            TaskCommands::Todo {
                artifact,
                runner,
                task_id,
                from,
            } => match resolve_initialized_project() {
                Ok(project) => match commands::run_todo(
                    &project,
                    artifact.as_deref(),
                    task_id.as_deref(),
                    from.as_deref(),
                    resolve_runner_id(runner.as_deref()).as_deref(),
                ) {
                    Ok(()) => ExitCode::SUCCESS,
                    Err(error) => {
                        eprintln!("error: {error}");
                        exit_code_for_todo(&error)
                    }
                },
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
                depends_on,
                phase,
                workflow,
            } => match resolve_initialized_project() {
                Ok(project) => match commands::run_add(
                    &project,
                    &title,
                    description.as_deref(),
                    &depends_on,
                    phase.as_deref(),
                    None,
                    None,
                    workflow.as_deref(),
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
                depends_on,
            } => match resolve_initialized_project() {
                Ok(project) => match commands::run_update(
                    &project,
                    &id,
                    commands::UpdateInput {
                        title,
                        description,
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
            TaskCommands::Remove { id } => match resolve_initialized_project() {
                Ok(project) => match commands::run_remove(&project, &id) {
                    Ok(()) => ExitCode::SUCCESS,
                    Err(error) => {
                        eprintln!("error: {error}");
                        exit_code_for_remove(&error)
                    }
                },
                Err(error) => {
                    eprintln!("error: {error}");
                    exit_code_for(&error)
                }
            },
            TaskCommands::Lock { id } => match resolve_initialized_project() {
                Ok(project) => match commands::run_lock(&project, &id) {
                    Ok(()) => ExitCode::SUCCESS,
                    Err(error) => {
                        eprintln!("error: {error}");
                        exit_code_for_lock(&error)
                    }
                },
                Err(error) => {
                    eprintln!("error: {error}");
                    exit_code_for(&error)
                }
            },
            TaskCommands::Unlock { id } => match resolve_initialized_project() {
                Ok(project) => match commands::run_unlock(&project, &id) {
                    Ok(()) => ExitCode::SUCCESS,
                    Err(error) => {
                        eprintln!("error: {error}");
                        exit_code_for_unlock(&error)
                    }
                },
                Err(error) => {
                    eprintln!("error: {error}");
                    exit_code_for(&error)
                }
            },
        },
        Commands::Init { stack, phases } => match resolve_project() {
            Ok(project) => {
                match commands::run_init(&project, commands::InitFlags { stack, phases }) {
                    Ok(()) => ExitCode::SUCCESS,
                    Err(error) => {
                        eprintln!("error: {error}");
                        exit_code_for_init(&error)
                    }
                }
            }
            Err(error) => {
                eprintln!("error: {error}");
                exit_code_for(&error)
            }
        },
        Commands::Phase { command } => match command {
            PhaseCommands::List => match resolve_project() {
                Ok(project) => match commands::run_phase_list(&project) {
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
            PhaseCommands::Add { name, model, duty } => match resolve_project() {
                Ok(project) => match commands::run_phase_add(
                    &project,
                    &name,
                    model.as_deref(),
                    duty.as_deref(),
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
            PhaseCommands::Update {
                name,
                set_model,
                set_duty,
                clear_model,
                clear_duty,
                set_gate,
                unset_gate,
            } => match resolve_project() {
                Ok(project) => match commands::run_phase_update(
                    &project,
                    &name,
                    set_model.as_deref(),
                    set_duty.as_deref(),
                    clear_model,
                    clear_duty,
                    set_gate.as_deref(),
                    unset_gate,
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
            PhaseCommands::Remove { name } => match resolve_project() {
                Ok(project) => match commands::run_phase_remove(&project, &name) {
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
        Commands::Config { command } => match agira_root_from_home() {
            Ok(agira_root) => match command {
                ConfigCommands::Get => match commands::run_config_get(&agira_root) {
                    Ok(()) => ExitCode::SUCCESS,
                    Err(error) => {
                        eprintln!("error: {error}");
                        exit_code_for_config_command(&error)
                    }
                },
                ConfigCommands::Set { key, value } => {
                    match commands::run_config_set(&agira_root, &key, &value) {
                        Ok(()) => ExitCode::SUCCESS,
                        Err(error) => {
                            eprintln!("error: {error}");
                            exit_code_for_config_command(&error)
                        }
                    }
                }
            },
            Err(error) => {
                eprintln!("error: {error}");
                exit_code_for_config_command(&error)
            }
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
            HookCommands::Add { global, on, args } => match resolve_project() {
                Ok(project) => {
                    let (event, command) = split_hook_add_args(on, args);
                    match commands::run_hook_add(&project, &event, &command, global) {
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
        Commands::Workflow { command } => match command {
            WorkflowCommands::List { json } => match resolve_initialized_project() {
                Ok(project) => match commands::run_workflow_list(&project, json) {
                    Ok(()) => ExitCode::SUCCESS,
                    Err(error) => {
                        eprintln!("error: {error}");
                        exit_code_for_workflow_list(&error)
                    }
                },
                Err(error) => {
                    eprintln!("error: {error}");
                    exit_code_for(&error)
                }
            },
            WorkflowCommands::Add { name, phases } => match resolve_initialized_project() {
                Ok(project) => match commands::run_workflow_add(&project, &name, phases) {
                    Ok(()) => ExitCode::SUCCESS,
                    Err(error) => {
                        eprintln!("error: {error}");
                        exit_code_for_workflow_list(&error)
                    }
                },
                Err(error) => {
                    eprintln!("error: {error}");
                    exit_code_for(&error)
                }
            },
            WorkflowCommands::Update {
                name,
                add,
                after,
                remove,
            } => match resolve_initialized_project() {
                Ok(project) => match commands::run_workflow_update(
                    &project,
                    &name,
                    add.as_deref(),
                    after.as_deref(),
                    remove.as_deref(),
                ) {
                    Ok(()) => ExitCode::SUCCESS,
                    Err(error) => {
                        eprintln!("error: {error}");
                        exit_code_for_workflow_list(&error)
                    }
                },
                Err(error) => {
                    eprintln!("error: {error}");
                    exit_code_for(&error)
                }
            },
            WorkflowCommands::Remove { name } => match resolve_initialized_project() {
                Ok(project) => match commands::run_workflow_remove(&project, &name) {
                    Ok(()) => ExitCode::SUCCESS,
                    Err(error) => {
                        eprintln!("error: {error}");
                        exit_code_for_workflow_list(&error)
                    }
                },
                Err(error) => {
                    eprintln!("error: {error}");
                    exit_code_for(&error)
                }
            },
            WorkflowCommands::SetDefault { name } => match resolve_initialized_project() {
                Ok(project) => match commands::run_workflow_set_default(&project, &name) {
                    Ok(()) => ExitCode::SUCCESS,
                    Err(error) => {
                        eprintln!("error: {error}");
                        exit_code_for_workflow_list(&error)
                    }
                },
                Err(error) => {
                    eprintln!("error: {error}");
                    exit_code_for(&error)
                }
            },
        },
        Commands::Runner { command } => match resolve_initialized_project() {
            Ok(project) => match command {
                RunnerCommands::Start { runner_type } => {
                    match commands::run_runner_start(&project, Some(&runner_type)) {
                        Ok(()) => ExitCode::SUCCESS,
                        Err(error) => {
                            eprintln!("error: {error}");
                            exit_code_for_runner(&error)
                        }
                    }
                }
                RunnerCommands::Stop => match commands::run_runner_stop(&project) {
                    Ok(()) => ExitCode::SUCCESS,
                    Err(error) => {
                        eprintln!("error: {error}");
                        exit_code_for_runner(&error)
                    }
                },
                RunnerCommands::Status => match commands::run_runner_status(&project) {
                    Ok(()) => ExitCode::SUCCESS,
                    Err(error) => {
                        eprintln!("error: {error}");
                        exit_code_for_runner(&error)
                    }
                },
                RunnerCommands::Attach => match commands::run_runner_attach(&project) {
                    Ok(()) => ExitCode::SUCCESS,
                    Err(error) => {
                        eprintln!("error: {error}");
                        exit_code_for_runner(&error)
                    }
                },
                RunnerCommands::Logs { follow } => {
                    match commands::run_runner_logs(&project, follow) {
                        Ok(()) => ExitCode::SUCCESS,
                        Err(error) => {
                            eprintln!("error: {error}");
                            exit_code_for_runner(&error)
                        }
                    }
                }
            },
            Err(error) => {
                eprintln!("error: {error}");
                exit_code_for(&error)
            }
        },
        Commands::Skill { command } => {
            let result = match command {
                SkillCommands::Install => commands::run_skill_install(),
                SkillCommands::Uninstall => commands::run_skill_uninstall(),
            };
            match result {
                Ok(()) => ExitCode::SUCCESS,
                Err(error) => {
                    eprintln!("error: {error}");
                    exit_code_for_skill(&error)
                }
            }
        }
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

fn resolve_runner_id(cli_runner: Option<&str>) -> Option<String> {
    cli_runner
        .filter(|id| !id.trim().is_empty())
        .map(|id| id.trim().to_owned())
        .or_else(|| {
            env::var("AGIRA_RUNNER_ID")
                .ok()
                .map(|id| id.trim().to_owned())
                .filter(|id| !id.is_empty())
        })
}

fn split_hook_add_args(on: Option<String>, args: Vec<String>) -> (String, Vec<String>) {
    match on {
        Some(event) => (event, args),
        None => {
            let mut args = args.into_iter();
            let event = args.next().unwrap_or_default();
            (event, args.collect())
        }
    }
}

fn agira_root_from_home() -> Result<PathBuf, commands::ConfigCommandError> {
    let home = env::var_os("HOME")
        .filter(|value| !value.as_os_str().is_empty())
        .ok_or(commands::ConfigCommandError::HomeDirectoryMissing)?;

    Ok(PathBuf::from(home).join(".agira"))
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
        commands::InitError::MissingFlags { .. } | commands::InitError::InvalidPhases => {
            ExitCode::from(1)
        }
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
        | TaskNotFound { .. }
        | AlreadyAdvancedPast { .. }
        | NotAdvanceable { .. }
        | GateFailed { .. }
        | ConfigLoad { .. }
        | InvalidConfig { .. } => ExitCode::from(1),
        Io { .. } => ExitCode::from(2),
        StoreError(store_error) => match store_error {
            crate::core::StoreError::Io { .. }
            | crate::core::StoreError::Serialize(_)
            | crate::core::StoreError::Deserialize(_) => ExitCode::from(2),
            _ => ExitCode::from(1),
        },
        RunnerStoreError(runner_error) => match runner_error {
            crate::core::RunnerStoreError::Io { .. }
            | crate::core::RunnerStoreError::Serialize(_)
            | crate::core::RunnerStoreError::Deserialize(_) => ExitCode::from(2),
            crate::core::RunnerStoreError::NotFound => ExitCode::from(1),
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

fn exit_code_for_lock(error: &commands::LockError) -> ExitCode {
    use commands::LockError::*;

    match error {
        TaskNotFound { .. } | ConfigNotFound { .. } | ConfigLoad { .. } | InvalidConfig { .. } => {
            ExitCode::from(1)
        }
        ConfigRead { .. } => ExitCode::from(2),
        StoreError(store_error) => match store_error {
            crate::core::StoreError::Io { .. }
            | crate::core::StoreError::Serialize(_)
            | crate::core::StoreError::Deserialize(_) => ExitCode::from(2),
            _ => ExitCode::from(1),
        },
    }
}

fn exit_code_for_unlock(error: &commands::UnlockError) -> ExitCode {
    use commands::UnlockError::*;

    match error {
        TaskNotFound { .. } | ConfigNotFound { .. } | ConfigLoad { .. } | InvalidConfig { .. } => {
            ExitCode::from(1)
        }
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

fn exit_code_for_skill(error: &commands::SkillError) -> ExitCode {
    // SkillError is currently uninhabited; this match is exhaustive and never runs.
    match *error {}
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

fn exit_code_for_config_command(error: &commands::ConfigCommandError) -> ExitCode {
    match error {
        commands::ConfigCommandError::HomeDirectoryMissing
        | commands::ConfigCommandError::UnknownKey { .. }
        | commands::ConfigCommandError::InvalidValue { .. }
        | commands::ConfigCommandError::GlobalConfig(crate::core::GlobalConfigError::Parse {
            ..
        }) => ExitCode::from(1),
        commands::ConfigCommandError::GlobalConfig(
            crate::core::GlobalConfigError::Read { .. }
            | crate::core::GlobalConfigError::Write { .. },
        )
        | commands::ConfigCommandError::Save(_) => ExitCode::from(2),
    }
}

fn exit_code_for_add(error: &commands::AddError) -> ExitCode {
    match error {
        commands::AddError::UnknownDependency { .. }
        | commands::AddError::UnknownPhase { .. }
        | commands::AddError::UnknownWorkflow { .. }
        | commands::AddError::DuplicateTitle { .. }
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
        | PhaseNotFound { .. }
        | DuplicatePhase { .. }
        | ReservedPhase { .. }
        | PhaseReferenced { .. }
        | ConfigNotFound { .. }
        | ConfigLoad { .. }
        | InvalidConfig { .. } => ExitCode::from(1),
        ConfigRead { .. } => ExitCode::from(2),
    }
}

fn exit_code_for_remove(error: &commands::RemoveError) -> ExitCode {
    use commands::RemoveError::*;

    match error {
        TaskNotFound { .. }
        | NotPending { .. }
        | HasDependents { .. }
        | ConfigNotFound { .. }
        | ConfigLoad { .. }
        | InvalidConfig { .. } => ExitCode::from(1),
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

fn exit_code_for_workflow_list(error: &commands::WorkflowListError) -> ExitCode {
    use commands::WorkflowListError::*;

    match error {
        NotFound { .. } | Load { .. } | InvalidConfig { .. } => ExitCode::from(1),
        Read { .. } | JsonOutput(_) => ExitCode::from(2),
    }
}

fn exit_code_for_runner(error: &commands::RunnerCommandError) -> ExitCode {
    use commands::RunnerCommandError::*;

    match error {
        UnregisteredLiveSession
        | NoRunnerRegistered
        | SessionNotAlive
        | LogFileNotFound { .. }
        | TmuxFailed { .. }
        | Config(
            crate::core::config::ConfigError::NotFound { .. }
            | crate::core::config::ConfigError::Parse { .. }
            | crate::core::config::ConfigError::Invalid { .. },
        ) => ExitCode::from(1),
        TmuxIo { .. }
        | Read { .. }
        | Write { .. }
        | Config(crate::core::config::ConfigError::Read { .. }) => ExitCode::from(2),
        RunnerStore(store_error) => match store_error {
            crate::core::RunnerStoreError::Io { .. }
            | crate::core::RunnerStoreError::Serialize(_)
            | crate::core::RunnerStoreError::Deserialize(_) => ExitCode::from(2),
            crate::core::RunnerStoreError::NotFound => ExitCode::from(1),
        },
    }
}
