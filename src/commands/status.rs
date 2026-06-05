use std::{
    cmp::Ordering,
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

const NO_TASKS_MESSAGE: &str =
    "No tasks. Run `agira task add` or `agira task work --prd <path>` to get started.";
const TITLE_LIMIT: usize = 40;
const LAST_ACTION_LIMIT: usize = 30;
const STATE_LIMIT: usize = 13;

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
    limit: usize,
    offset: usize,
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
        let sorted_tasks = sort_tasks_by_id_desc(tasks);
        let paginated_tasks = paginate_tasks(&sorted_tasks, limit, offset);
        let mut output = format_status_table(&paginated_tasks, &terminal_phase);

        if limit != 0 && tasks.len() > limit {
            output.push('\n');
            output.push_str(&format!(
                "Showing {limit} of {} tasks. Use --offset or --limit to see more.",
                tasks.len()
            ));
        }

        print_status_output(&output);
    }
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

fn format_status_table(tasks: &[&Task], terminal_phase: &str) -> String {
    let mut lines = vec![
        format!(
            "{:<10}  {:<41}  {:<15}  {:>7}  {}",
            "ID", "Title", "State", "Retries", "Last Action"
        ),
        format!(
            "{:-<10}  {:-<41}  {:-<15}  {:-<7}  {:-<30}",
            "", "", "", "", ""
        ),
    ];

    lines.extend(
        tasks
            .iter()
            .map(|task| format_status_row(task, terminal_phase)),
    );
    lines.join("\n")
}

fn sort_tasks_by_id_desc(tasks: &[Task]) -> Vec<&Task> {
    let mut sorted_tasks: Vec<&Task> = tasks.iter().collect();
    sorted_tasks.sort_by(|left, right| compare_task_ids_desc(&left.id, &right.id));
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
    let last_action = task
        .history
        .last()
        .map(|entry| entry.reason.as_str())
        .unwrap_or("-");
    let last_action = truncate_chars(last_action, LAST_ACTION_LIMIT);

    format!(
        "{:<10}  {:<41}  {:<15}  {:>7}  {}",
        task.id, title, state, retries, last_action
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

fn compare_task_ids_desc(left: &str, right: &str) -> Ordering {
    match (task_id_number(left), task_id_number(right)) {
        (Some(left_number), Some(right_number)) => {
            right_number.cmp(&left_number).then_with(|| right.cmp(left))
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
    static OUTPUT_CAPTURE: std::cell::RefCell<Option<String>> = std::cell::RefCell::new(None);
}

#[cfg(test)]
mod tests {
    use std::{fs, process::ExitCode};

    use tempfile::TempDir;

    use super::*;
    use crate::core::{
        config::{Config, PhaseConfig, VerificationConfig},
        global_config::GlobalConfig,
        tasks::{TaskStore, TasksFile},
    };

    fn test_config() -> Config {
        Config {
            stack: "rust".to_owned(),
            phases: vec![
                PhaseConfig {
                    name: "enriching".to_owned(),
                    model: "opus".to_owned(),
                },
                PhaseConfig {
                    name: "in_progress".to_owned(),
                    model: "sonnet".to_owned(),
                },
                PhaseConfig {
                    name: "done".to_owned(),
                    model: "haiku".to_owned(),
                },
            ],
            verification: VerificationConfig { commands: vec![] },
            acceptance_testing: "cli".to_owned(),
            max_retries: 3,
            prd_path: None,
        }
    }

    fn test_project(temp_dir: &TempDir) -> Project {
        Project {
            git_root: temp_dir.path().to_path_buf(),
            slug: "test".to_owned(),
            state_dir: temp_dir.path().to_path_buf(),
            global_config: GlobalConfig::default(),
            global_hooks: crate::core::hooks::HookConfig::default(),
            project_hooks: crate::core::hooks::HookConfig::default(),
        }
    }

    fn write_config(project: &Project, config: &Config) {
        let contents = serde_json::to_vec_pretty(config).unwrap();
        fs::write(project.state_dir.join("config.json"), contents).unwrap();
    }

    fn write_empty_tasks(project: &Project) {
        let contents = serde_json::to_vec_pretty(&TasksFile { tasks: vec![] }).unwrap();
        fs::write(project.state_dir.join("tasks.json"), contents).unwrap();
    }

    fn test_project_with_config() -> (TempDir, Project, Config) {
        let temp_dir = TempDir::new().unwrap();
        let project = test_project(&temp_dir);
        let config = test_config();
        write_config(&project, &config);

        (temp_dir, project, config)
    }

    fn test_store(temp_dir: &TempDir, config: &Config) -> TaskStore {
        TaskStore::new(temp_dir.path(), config).unwrap()
    }

    fn capture_output<F>(run: F) -> (Result<(), StatusError>, String)
    where
        F: FnOnce() -> Result<(), StatusError>,
    {
        OUTPUT_CAPTURE.with(|capture| {
            *capture.borrow_mut() = Some(String::new());
        });

        let result = run();
        let output = OUTPUT_CAPTURE.with(|capture| capture.borrow_mut().take().unwrap());

        (result, output)
    }

    fn add_tasks(store: &mut TaskStore, count: usize) {
        for index in 1..=count {
            store
                .add_task(&format!("Task {index:03}"), "", None, vec![])
                .unwrap();
        }
    }

    fn task_row_ids(output: &str) -> Vec<String> {
        output
            .lines()
            .filter(|line| line.starts_with("task-"))
            .map(|line| line.split_whitespace().next().unwrap().to_owned())
            .collect()
    }

    #[test]
    fn no_tasks_prints_no_tasks_message() {
        let (_temp_dir, project, _config) = test_project_with_config();
        write_empty_tasks(&project);

        let (result, output) = capture_output(|| run_status(&project, false, None, 20, 0));

        result.unwrap();
        assert_eq!(
            output,
            "No tasks. Run `agira task add` or `agira task work --prd <path>` to get started.\n"
        );
    }

    #[test]
    fn no_tasks_message_when_tasks_file_is_absent() {
        let temp_dir = TempDir::new().unwrap();
        let project = test_project(&temp_dir);

        let (result, output) = capture_output(|| run_status(&project, false, None, 20, 0));

        result.unwrap();
        assert_eq!(
            output,
            "No tasks. Run `agira task add` or `agira task work --prd <path>` to get started.\n"
        );
    }

    #[test]
    fn table_has_correct_columns() {
        let (temp_dir, project, config) = test_project_with_config();
        let mut store = test_store(&temp_dir, &config);
        store
            .add_task("Implement status command", "", None, vec![])
            .unwrap();

        let (result, output) = capture_output(|| run_status(&project, false, None, 20, 0));

        result.unwrap();
        assert!(output.contains("ID"));
        assert!(output.contains("Title"));
        assert!(output.contains("State"));
        assert!(output.contains("Retries"));
        assert!(output.contains("Last Action"));
        assert!(output.contains("task-001"));
        assert!(output.contains("Implement status command"));
        assert!(output.contains("enriching"));
        assert!(!output.contains("✓ enriching"));
        assert!(!output.contains("✗ enriching"));
    }

    #[test]
    fn terminal_done_shows_checkmark() {
        let (temp_dir, project, config) = test_project_with_config();
        let mut store = test_store(&temp_dir, &config);
        store.add_task("Finish status", "", None, vec![]).unwrap();
        store.next_phase("task-001").unwrap();
        store.next_phase("task-001").unwrap();

        let (result, output) = capture_output(|| run_status(&project, false, None, 20, 0));

        result.unwrap();
        assert!(output.contains("✓ done"));
    }

    #[test]
    fn failed_task_shows_cross() {
        let (temp_dir, project, config) = test_project_with_config();
        let mut store = test_store(&temp_dir, &config);
        store.add_task("Fail status", "", None, vec![]).unwrap();
        store.fail_task("task-001", "blocked").unwrap();

        let (result, output) = capture_output(|| run_status(&project, false, None, 20, 0));

        result.unwrap();
        assert!(output.contains("✗ failed"));
    }

    #[test]
    fn blocked_task_shows_blocked_prefix() {
        let (temp_dir, project, config) = test_project_with_config();
        let mut store = test_store(&temp_dir, &config);
        store.add_task("Blocked task", "", None, vec![]).unwrap();
        store.block_task("task-001", "waiting").unwrap();

        let (result, output) = capture_output(|| run_status(&project, false, None, 20, 0));

        result.unwrap();
        assert!(output.contains("⊘ blocked"));
        assert!(!output.contains("✓ blocked"));
        assert!(!output.contains("✗ blocked"));
    }

    #[test]
    fn title_truncation() {
        let (temp_dir, project, config) = test_project_with_config();
        let mut store = test_store(&temp_dir, &config);
        let title = "x".repeat(50);
        store.add_task(&title, "", None, vec![]).unwrap();

        let (result, output) = capture_output(|| run_status(&project, false, None, 20, 0));

        result.unwrap();
        let expected = format!("{}…", "x".repeat(40));
        assert_eq!(format_title(&title), expected);
        assert_eq!(format_title(&title).chars().nth(40), Some('…'));
        assert!(output.contains(&expected));
        assert!(!output.contains(&title));
    }

    #[test]
    fn sorted_by_id_descending() {
        let (temp_dir, project, config) = test_project_with_config();
        let mut store = test_store(&temp_dir, &config);
        store.add_task("Second in file", "", None, vec![]).unwrap();
        store.add_task("First in file", "", None, vec![]).unwrap();

        let mut tasks_file: TasksFile = serde_json::from_str(
            &fs::read_to_string(project.state_dir.join("tasks.json")).unwrap(),
        )
        .unwrap();
        tasks_file.tasks.reverse();
        fs::write(
            project.state_dir.join("tasks.json"),
            serde_json::to_vec_pretty(&tasks_file).unwrap(),
        )
        .unwrap();

        let (result, output) = capture_output(|| run_status(&project, false, None, 20, 0));

        result.unwrap();
        assert!(output.find("task-002").unwrap() < output.find("task-001").unwrap());
    }

    #[test]
    fn default_status_paginates_to_latest_twenty_tasks() {
        let (temp_dir, project, config) = test_project_with_config();
        let mut store = test_store(&temp_dir, &config);
        add_tasks(&mut store, 25);

        let (result, output) = capture_output(|| run_status(&project, false, None, 20, 0));

        result.unwrap();
        let ids = task_row_ids(&output);
        assert_eq!(ids.len(), 20);
        assert_eq!(ids.first().unwrap(), "task-025");
        assert_eq!(ids.last().unwrap(), "task-006");
        assert!(!output.contains("task-005"));
        assert!(!output.contains("task-001"));
    }

    #[test]
    fn limit_zero_shows_all_tasks() {
        let (temp_dir, project, config) = test_project_with_config();
        let mut store = test_store(&temp_dir, &config);
        add_tasks(&mut store, 25);

        let (result, output) = capture_output(|| run_status(&project, false, None, 0, 0));

        result.unwrap();
        let ids = task_row_ids(&output);
        assert_eq!(ids.len(), 25);
        assert_eq!(ids.first().unwrap(), "task-025");
        assert_eq!(ids.last().unwrap(), "task-001");
        assert!(!output.contains("Showing"));
    }

    #[test]
    fn offset_skips_tasks_from_descending_window() {
        let (temp_dir, project, config) = test_project_with_config();
        let mut store = test_store(&temp_dir, &config);
        add_tasks(&mut store, 25);

        let (result, output) = capture_output(|| run_status(&project, false, None, 5, 3));

        result.unwrap();
        assert_eq!(
            task_row_ids(&output),
            vec!["task-022", "task-021", "task-020", "task-019", "task-018"]
        );
    }

    #[test]
    fn footer_shown_when_total_exceeds_limit() {
        let (temp_dir, project, config) = test_project_with_config();
        let mut store = test_store(&temp_dir, &config);
        add_tasks(&mut store, 21);

        let (result, output) = capture_output(|| run_status(&project, false, None, 20, 0));

        result.unwrap();
        assert!(output.ends_with("Showing 20 of 21 tasks. Use --offset or --limit to see more.\n"));
    }

    #[test]
    fn footer_not_shown_when_total_does_not_exceed_limit() {
        let (temp_dir, project, config) = test_project_with_config();
        let mut store = test_store(&temp_dir, &config);
        add_tasks(&mut store, 20);

        let (result, output) = capture_output(|| run_status(&project, false, None, 20, 0));

        result.unwrap();
        assert!(!output.contains("Use --offset or --limit to see more."));
    }

    #[test]
    fn json_bypasses_pagination_and_outputs_full_tasks_array() {
        let (temp_dir, project, config) = test_project_with_config();
        let mut store = test_store(&temp_dir, &config);
        add_tasks(&mut store, 25);

        let (result, output) = capture_output(|| run_status(&project, true, None, 1, 10));

        result.unwrap();
        let value: serde_json::Value = serde_json::from_str(&output).unwrap();
        assert_eq!(value["tasks"].as_array().unwrap().len(), 25);
        assert!(output.contains("task-001"));
        assert!(output.contains("task-025"));
        assert!(!output.contains("Showing"));
    }

    #[test]
    fn json_outputs_raw_tasks_file_exactly() {
        let temp_dir = TempDir::new().unwrap();
        let project = test_project(&temp_dir);
        let raw = "{\n  \"tasks\": []\n}";
        fs::write(project.state_dir.join("tasks.json"), raw).unwrap();

        let (result, output) = capture_output(|| run_status(&project, true, None, 20, 0));

        result.unwrap();
        assert_eq!(output, raw);
    }

    #[test]
    fn json_missing_tasks_file_returns_json_output() {
        let temp_dir = TempDir::new().unwrap();
        let project = test_project(&temp_dir);

        let (result, output) = capture_output(|| run_status(&project, true, None, 20, 0));
        let error = result.unwrap_err();

        assert!(matches!(error, StatusError::JsonOutput { .. }));
        assert_eq!(output, "");
    }

    #[test]
    fn config_load_and_invalid_config_errors() {
        let temp_dir = TempDir::new().unwrap();
        let project = test_project(&temp_dir);
        write_empty_tasks(&project);

        let error = run_status(&project, false, None, 20, 0).unwrap_err();
        assert!(matches!(error, StatusError::ConfigNotFound { .. }));

        fs::write(project.state_dir.join("config.json"), "{").unwrap();
        let error = run_status(&project, false, None, 20, 0).unwrap_err();
        assert!(matches!(error, StatusError::ConfigLoad { .. }));

        let mut config = test_config();
        config.phases.clear();
        write_config(&project, &config);
        let error = run_status(&project, false, None, 20, 0).unwrap_err();
        match error {
            StatusError::InvalidConfig { reason, .. } => {
                assert_eq!(reason, "phases must not be empty");
            }
            other => panic!("unexpected error: {other}"),
        }
    }

    #[test]
    fn formatter_helpers_are_unicode_safe() {
        let title = "界".repeat(41);
        let action = "測".repeat(31);

        assert_eq!(format_title(&title), format!("{}…", "界".repeat(40)));
        assert_eq!(
            truncate_chars(&action, LAST_ACTION_LIMIT),
            format!("{}…", "測".repeat(30))
        );
        assert_eq!(format_title(&"界".repeat(40)), "界".repeat(40));
    }

    #[test]
    fn json_output_exit_code_contract_is_two() {
        let temp_dir = TempDir::new().unwrap();
        let project = test_project(&temp_dir);
        let error = run_status(&project, true, None, 20, 0).unwrap_err();

        let code = match error {
            StatusError::JsonOutput { .. } => ExitCode::from(2),
            _ => ExitCode::from(1),
        };

        assert_eq!(code, ExitCode::from(2));
    }

    #[test]
    fn json_with_filter_outputs_single_task_object() {
        let (temp_dir, project, config) = test_project_with_config();
        let mut store = test_store(&temp_dir, &config);
        store.add_task("First task", "", None, vec![]).unwrap();
        store.add_task("Second task", "", None, vec![]).unwrap();

        let (result, output) =
            capture_output(|| run_status(&project, true, Some("task-001"), 20, 0));

        result.unwrap();
        let value: serde_json::Value = serde_json::from_str(&output).unwrap();
        assert!(value.is_object());
        assert_eq!(value["id"], "task-001");
        assert_eq!(value["title"], "First task");
        assert!(!output.contains("task-002"));
        assert!(!output.contains("Second task"));
    }

    #[test]
    fn json_with_filter_task_not_found() {
        let (temp_dir, project, config) = test_project_with_config();
        let mut store = test_store(&temp_dir, &config);
        store.add_task("Some task", "", None, vec![]).unwrap();

        let error = run_status(&project, true, Some("task-999"), 20, 0).unwrap_err();

        match error {
            StatusError::TaskNotFound { id } => assert_eq!(id, "task-999"),
            other => panic!("expected TaskNotFound, got: {other}"),
        }
    }

    #[test]
    fn json_with_filter_no_tasks_file_returns_task_not_found() {
        let temp_dir = TempDir::new().unwrap();
        let project = test_project(&temp_dir);

        let error = run_status(&project, true, Some("task-001"), 20, 0).unwrap_err();

        assert!(matches!(error, StatusError::TaskNotFound { .. }));
    }

    #[test]
    fn filter_shows_only_matching_task() {
        let (temp_dir, project, config) = test_project_with_config();
        let mut store = test_store(&temp_dir, &config);
        store.add_task("First task", "", None, vec![]).unwrap();
        store.add_task("Second task", "", None, vec![]).unwrap();

        let (result, output) =
            capture_output(|| run_status(&project, false, Some("task-002"), 20, 0));

        result.unwrap();
        assert!(output.contains("task-002"));
        assert!(output.contains("Second task"));
        assert!(!output.contains("task-001"));
        assert!(!output.contains("First task"));
    }

    #[test]
    fn filter_unknown_id_returns_task_not_found() {
        let (temp_dir, project, config) = test_project_with_config();
        let mut store = test_store(&temp_dir, &config);
        store.add_task("Some task", "", None, vec![]).unwrap();

        let error = run_status(&project, false, Some("task-999"), 20, 0).unwrap_err();

        match error {
            StatusError::TaskNotFound { id } => assert_eq!(id, "task-999"),
            other => panic!("expected TaskNotFound, got: {other}"),
        }
    }

    #[test]
    fn filter_when_no_tasks_file_returns_task_not_found() {
        let temp_dir = TempDir::new().unwrap();
        let project = test_project(&temp_dir);

        let error = run_status(&project, false, Some("task-001"), 20, 0).unwrap_err();

        assert!(matches!(error, StatusError::TaskNotFound { .. }));
    }

    #[test]
    fn filter_when_empty_tasks_returns_task_not_found() {
        let (_temp_dir, project, _config) = test_project_with_config();
        write_empty_tasks(&project);

        let error = run_status(&project, false, Some("task-001"), 20, 0).unwrap_err();

        assert!(matches!(error, StatusError::TaskNotFound { .. }));
    }
}
