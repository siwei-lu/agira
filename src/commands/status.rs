use std::{
    cmp::Ordering,
    fmt::Write as FmtWrite,
    fs, io,
    io::Write,
    path::{Path, PathBuf},
};

use thiserror::Error;

use crate::core::{
    config::{ConfigError, load_project_config},
    project::Project,
    tasks::{StoreError, Task, TaskStore},
};

const NO_TASKS_MESSAGE: &str = "No tasks. Run `agira task add` to get started.";
const TITLE_LIMIT: usize = 40;
const LAST_ACTION_LIMIT: usize = 30;
const STATE_LIMIT: usize = 13;
const WORKFLOW_LIMIT: usize = 14;

#[derive(Debug, Error)]
pub enum StatusError {
    #[error("config file not found: {path}")]
    ConfigNotFound { path: PathBuf },

    #[error("failed to read {path}")]
    ConfigRead {
        path: PathBuf,
        #[source]
        source: io::Error,
    },

    #[error("failed to load config {path}: {source}")]
    ConfigLoad {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },

    #[error("invalid config {path}: {reason}")]
    InvalidConfig { path: PathBuf, reason: String },

    #[error(transparent)]
    StoreError(#[from] StoreError),

    #[error("failed to read {path}")]
    JsonOutput {
        path: PathBuf,
        #[source]
        source: io::Error,
    },

    #[error("task not found: {id}")]
    TaskNotFound { id: String },
}

pub fn run_status(
    project: &Project,
    json: bool,
    filter: Option<&str>,
    limit: Option<usize>,
    offset: Option<usize>,
) -> Result<(), StatusError> {
    if json {
        if let Some(id) = filter {
            return output_task_json(project, id);
        }
        return output_raw_json(project);
    }

    let tasks_path = project.state_dir.join("tasks.json");
    match fs::metadata(&tasks_path) {
        Ok(_) => {}
        Err(source) if source.kind() == io::ErrorKind::NotFound => {
            if let Some(id) = filter {
                return Err(StatusError::TaskNotFound { id: id.to_owned() });
            }
            print_status_output(NO_TASKS_MESSAGE);
            return Ok(());
        }
        Err(source) => {
            return Err(StoreError::Io {
                path: tasks_path,
                source,
            }
            .into());
        }
    }

    let config_path = project.state_dir.join("config.json");
    let config =
        load_project_config(&config_path, &project.global_config).map_err(map_config_error)?;
    let terminal_phase = config
        .terminal_phase()
        .ok_or_else(|| StatusError::InvalidConfig {
            path: config_path,
            reason: "phases must not be empty".to_owned(),
        })?
        .to_owned();

    let store = TaskStore::new(&project.state_dir, &config)?;
    let tasks = store.all_tasks();
    if tasks.is_empty() {
        if let Some(id) = filter {
            return Err(StatusError::TaskNotFound { id: id.to_owned() });
        }
        print_status_output(NO_TASKS_MESSAGE);
        return Ok(());
    }

    if let Some(id) = filter {
        let task = tasks
            .iter()
            .find(|t| t.id == id)
            .ok_or_else(|| StatusError::TaskNotFound { id: id.to_owned() })?;
        print_status_output(&format_status_table(&[task], &terminal_phase));
    } else {
        let sorted_tasks = sort_tasks_by_id_asc(tasks);
        let total = sorted_tasks.len();
        let explicit = limit.is_some() || offset.is_some();
        let limit_eff = limit.unwrap_or(20);
        let offset_eff = if explicit {
            offset.unwrap_or(0)
        } else {
            total.saturating_sub(20)
        };
        let paginated_tasks = paginate_tasks(&sorted_tasks, limit_eff, offset_eff);
        let mut output = format_status_table(&paginated_tasks, &terminal_phase);

        if limit_eff != 0 && total > limit_eff {
            output.push('\n');
            if explicit {
                output.push_str(&format!(
                    "Showing {limit_eff} of {total} tasks. Use --offset or --limit to see more."
                ));
            } else {
                output.push_str(&format!(
                    "Showing latest {limit_eff} of {total} tasks. Use --offset or --limit to see more."
                ));
            }
        }

        print_status_output(&output);
    }
    Ok(())
}

pub fn run_inspect(project: &Project, id: &str) -> Result<(), StatusError> {
    let tasks_path = project.state_dir.join("tasks.json");
    match fs::metadata(&tasks_path) {
        Ok(_) => {}
        Err(source) if source.kind() == io::ErrorKind::NotFound => {
            return Err(StatusError::TaskNotFound { id: id.to_owned() });
        }
        Err(source) => {
            return Err(StoreError::Io {
                path: tasks_path,
                source,
            }
            .into());
        }
    }

    let config_path = project.state_dir.join("config.json");
    let config =
        load_project_config(&config_path, &project.global_config).map_err(map_config_error)?;
    let store = TaskStore::new(&project.state_dir, &config)?;
    let task = store
        .all_tasks()
        .iter()
        .find(|task| task.id == id)
        .ok_or_else(|| StatusError::TaskNotFound { id: id.to_owned() })?;

    let detail = format_task_detail(task);
    print_status_output(&detail);
    Ok(())
}

fn output_raw_json(project: &Project) -> Result<(), StatusError> {
    let path = project.state_dir.join("tasks.json");
    let contents = fs::read_to_string(&path).map_err(|source| StatusError::JsonOutput {
        path: path.clone(),
        source,
    })?;

    write_status_output(&contents, &path)
}

fn output_task_json(project: &Project, id: &str) -> Result<(), StatusError> {
    let tasks_path = project.state_dir.join("tasks.json");
    match fs::metadata(&tasks_path) {
        Ok(_) => {}
        Err(source) if source.kind() == io::ErrorKind::NotFound => {
            return Err(StatusError::TaskNotFound { id: id.to_owned() });
        }
        Err(source) => {
            return Err(StoreError::Io {
                path: tasks_path,
                source,
            }
            .into());
        }
    }

    let config_path = project.state_dir.join("config.json");
    let config =
        load_project_config(&config_path, &project.global_config).map_err(map_config_error)?;
    let store = TaskStore::new(&project.state_dir, &config)?;
    let task = store
        .all_tasks()
        .iter()
        .find(|task| task.id == id)
        .ok_or_else(|| StatusError::TaskNotFound { id: id.to_owned() })?;
    let json_str = serde_json::to_string_pretty(task).map_err(StoreError::Serialize)?;

    write_status_output(&json_str, &tasks_path)
}

fn map_config_error(error: ConfigError) -> StatusError {
    match error {
        ConfigError::NotFound { path } => StatusError::ConfigNotFound { path },
        ConfigError::Read { path, source } => StatusError::ConfigRead { path, source },
        ConfigError::Parse { path, source } => StatusError::ConfigLoad { path, source },
        ConfigError::Invalid { path, reason } => StatusError::InvalidConfig { path, reason },
    }
}

fn format_task_detail(task: &Task) -> String {
    let mut output = String::new();
    let dependencies = if task.dependencies.is_empty() {
        "—".to_owned()
    } else {
        task.dependencies.join(", ")
    };
    let blocked = format_blocked(task);
    let description = format_multiline_value(&task.description, 2);
    let workflow = task.workflow.as_str();

    writeln!(output, "ID:           {}", task.id).unwrap();
    writeln!(output, "Title:        {}", task.title).unwrap();
    writeln!(output, "State:        {}", task.state).unwrap();
    writeln!(output, "Workflow:     {workflow}").unwrap();
    writeln!(output, "Created:      {}", task.created_at).unwrap();
    writeln!(
        output,
        "Retries:      {}/{}",
        task.retry_count, task.max_retries
    )
    .unwrap();
    writeln!(output, "Depends on:   {dependencies}").unwrap();
    writeln!(output, "Blocked:      {blocked}").unwrap();
    writeln!(output, "Description:").unwrap();
    writeln!(output, "{description}").unwrap();
    writeln!(output, "Phases:").unwrap();
    if task.phases.is_empty() {
        writeln!(output, "  —").unwrap();
    } else {
        for (name, phase) in &task.phases {
            writeln!(output, "  {name} (completed {}):", phase.completed_at).unwrap();
            writeln!(output, "{}", format_multiline_value(&phase.artifact, 4)).unwrap();
        }
    }
    writeln!(output, "History:").unwrap();
    if task.history.is_empty() {
        write!(output, "  —").unwrap();
    } else {
        for entry in &task.history {
            let from = entry.from.as_deref().unwrap_or("(none)");
            writeln!(
                output,
                "  {from} → {}  {}  {}",
                entry.to, entry.timestamp, entry.reason
            )
            .unwrap();
        }
        output.pop();
    }

    output
}

fn format_blocked(task: &Task) -> String {
    match (
        task.blocked_at_phase.as_deref(),
        task.blocked_reason.as_deref(),
    ) {
        (None, None) => "—".to_owned(),
        (Some(phase), Some(reason)) => format!("{phase} — {reason}"),
        (Some(phase), None) => format!("{phase} — —"),
        (None, Some(reason)) => format!("— — {reason}"),
    }
}

fn format_multiline_value(value: &str, indent: usize) -> String {
    let prefix = " ".repeat(indent);
    if value.is_empty() {
        return format!("{prefix}—");
    }

    value
        .lines()
        .map(|line| {
            if line.is_empty() {
                format!("{prefix}—")
            } else {
                format!("{prefix}{line}")
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn format_status_table(tasks: &[&Task], terminal_phase: &str) -> String {
    let mut lines = vec![
        format!(
            "{:<10}  {:<41}  {:<15}  {:>7}  {:<15}  {}",
            "ID", "Title", "State", "Retries", "Workflow", "Last Action"
        ),
        format!(
            "{:-<10}  {:-<41}  {:-<15}  {:-<7}  {:-<15}  {:-<30}",
            "", "", "", "", "", ""
        ),
    ];

    lines.extend(
        tasks
            .iter()
            .map(|task| format_status_row(task, terminal_phase)),
    );
    lines.join("\n")
}

fn sort_tasks_by_id_asc(tasks: &[Task]) -> Vec<&Task> {
    let mut sorted_tasks: Vec<&Task> = tasks.iter().collect();
    sorted_tasks.sort_by(|left, right| compare_task_ids_asc(&left.id, &right.id));
    sorted_tasks
}

fn paginate_tasks<'a>(tasks: &[&'a Task], limit: usize, offset: usize) -> Vec<&'a Task> {
    let window = tasks.iter().skip(offset).copied();

    if limit == 0 {
        window.collect()
    } else {
        window.take(limit).collect()
    }
}

fn format_status_row(task: &Task, terminal_phase: &str) -> String {
    let title = format_title(&task.title);
    let state = format_state(&task.state, terminal_phase);
    let retries = format!("{}/{}", task.retry_count, task.max_retries);
    let workflow = truncate_chars(&task.workflow, WORKFLOW_LIMIT);
    let last_action = task
        .history
        .last()
        .map(|entry| entry.reason.as_str())
        .unwrap_or("-");
    let last_action = truncate_chars(last_action, LAST_ACTION_LIMIT);

    format!(
        "{:<10}  {:<41}  {:<15}  {:>7}  {:<15}  {}",
        task.id, title, state, retries, workflow, last_action
    )
}

fn format_title(title: &str) -> String {
    truncate_chars(title, TITLE_LIMIT)
}

fn format_state(state: &str, terminal_phase: &str) -> String {
    let display = if state == "blocked" {
        "⊘ blocked".to_owned()
    } else if state == "failed" {
        "✗ failed".to_owned()
    } else if state == terminal_phase {
        format!("✓ {state}")
    } else {
        state.to_owned()
    };

    truncate_chars(&display, STATE_LIMIT)
}

fn truncate_chars(value: &str, limit: usize) -> String {
    if value.chars().count() > limit {
        value
            .chars()
            .take(limit)
            .chain(std::iter::once('…'))
            .collect()
    } else {
        value.to_owned()
    }
}

fn compare_task_ids_asc(left: &str, right: &str) -> Ordering {
    match (task_id_number(left), task_id_number(right)) {
        (Some(left_number), Some(right_number)) => {
            left_number.cmp(&right_number).then_with(|| left.cmp(right))
        }
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => left.cmp(right),
    }
}

fn task_id_number(id: &str) -> Option<u64> {
    id.rsplit_once('-')?.1.parse().ok()
}

fn print_status_output(message: &str) {
    #[cfg(test)]
    OUTPUT_CAPTURE.with(|capture| {
        if let Some(output) = capture.borrow_mut().as_mut() {
            output.push_str(message);
            output.push('\n');
        }
    });

    println!("{message}");
}

fn write_status_output(contents: &str, path: &Path) -> Result<(), StatusError> {
    #[cfg(test)]
    {
        let captured = OUTPUT_CAPTURE.with(|capture| {
            if let Some(output) = capture.borrow_mut().as_mut() {
                output.push_str(contents);
                true
            } else {
                false
            }
        });

        if captured {
            return Ok(());
        }
    }

    io::stdout()
        .write_all(contents.as_bytes())
        .map_err(|source| StatusError::JsonOutput {
            path: path.to_path_buf(),
            source,
        })
}

#[cfg(test)]
thread_local! {
    static OUTPUT_CAPTURE: std::cell::RefCell<Option<String>> = const { std::cell::RefCell::new(None) };
}
