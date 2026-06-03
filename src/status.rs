use std::{
    cmp::Ordering,
    fs, io,
    io::Write,
    path::{Path, PathBuf},
};

use thiserror::Error;

use crate::{
    config::{ConfigError, load_project_config},
    project::Project,
    tasks::{StoreError, Task, TaskStore},
};

const NO_TASKS_MESSAGE: &str =
    "No tasks. Run `agira add` or `agira next --prd <path>` to get started.";
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
}

pub fn run_status(project: &Project, json: bool) -> Result<(), StatusError> {
    if json {
        return output_raw_json(project);
    }

    let tasks_path = project.state_dir.join("tasks.json");
    match fs::metadata(&tasks_path) {
        Ok(_) => {}
        Err(source) if source.kind() == io::ErrorKind::NotFound => {
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
    let terminal_phase =
        config
            .state_machine
            .last()
            .cloned()
            .ok_or_else(|| StatusError::InvalidConfig {
                path: config_path,
                reason: "state_machine must not be empty".to_owned(),
            })?;

    let store = TaskStore::new(&project.state_dir, &config)?;
    let tasks = store.all_tasks();
    if tasks.is_empty() {
        print_status_output(NO_TASKS_MESSAGE);
        return Ok(());
    }

    print_status_output(&format_status_table(tasks, &terminal_phase));
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

fn map_config_error(error: ConfigError) -> StatusError {
    match error {
        ConfigError::NotFound { path } => StatusError::ConfigNotFound { path },
        ConfigError::Read { path, source } => StatusError::ConfigRead { path, source },
        ConfigError::Parse { path, source } => StatusError::ConfigLoad { path, source },
    }
}

fn format_status_table(tasks: &[Task], terminal_phase: &str) -> String {
    let mut sorted_tasks: Vec<&Task> = tasks.iter().collect();
    sorted_tasks.sort_by(|left, right| compare_task_ids(&left.id, &right.id));

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
        sorted_tasks
            .into_iter()
            .map(|task| format_status_row(task, terminal_phase)),
    );
    lines.join("\n")
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
    let display = if state == "failed" {
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

fn compare_task_ids(left: &str, right: &str) -> Ordering {
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
    static OUTPUT_CAPTURE: std::cell::RefCell<Option<String>> = std::cell::RefCell::new(None);
}

#[cfg(test)]
mod tests {
    use std::{collections::BTreeMap, fs, process::ExitCode};

    use tempfile::TempDir;

    use super::*;
    use crate::{
        config::{Config, VerificationConfig},
        global_config::GlobalConfig,
        tasks::{TaskStore, TasksFile},
    };

    fn test_config() -> Config {
        Config {
            stack: "rust".to_owned(),
            state_machine: vec![
                "enriching".to_owned(),
                "in_progress".to_owned(),
                "done".to_owned(),
            ],
            models: BTreeMap::new(),
            verification: VerificationConfig { commands: vec![] },
            acceptance_testing: "cli".to_owned(),
            max_retries: 3,
            default_model: "sonnet".to_owned(),
            prd_path: None,
        }
    }

    fn test_project(temp_dir: &TempDir) -> Project {
        Project {
            git_root: temp_dir.path().to_path_buf(),
            slug: "test".to_owned(),
            state_dir: temp_dir.path().to_path_buf(),
            global_config: GlobalConfig::default(),
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

    #[test]
    fn no_tasks_prints_no_tasks_message() {
        let (_temp_dir, project, _config) = test_project_with_config();
        write_empty_tasks(&project);

        let (result, output) = capture_output(|| run_status(&project, false));

        result.unwrap();
        assert_eq!(
            output,
            "No tasks. Run `agira add` or `agira next --prd <path>` to get started.\n"
        );
    }

    #[test]
    fn no_tasks_message_when_tasks_file_is_absent() {
        let temp_dir = TempDir::new().unwrap();
        let project = test_project(&temp_dir);

        let (result, output) = capture_output(|| run_status(&project, false));

        result.unwrap();
        assert_eq!(
            output,
            "No tasks. Run `agira add` or `agira next --prd <path>` to get started.\n"
        );
    }

    #[test]
    fn table_has_correct_columns() {
        let (temp_dir, project, config) = test_project_with_config();
        let mut store = test_store(&temp_dir, &config);
        store
            .add_task("Implement status command", "", None, vec![])
            .unwrap();

        let (result, output) = capture_output(|| run_status(&project, false));

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

        let (result, output) = capture_output(|| run_status(&project, false));

        result.unwrap();
        assert!(output.contains("✓ done"));
    }

    #[test]
    fn failed_task_shows_cross() {
        let (temp_dir, project, config) = test_project_with_config();
        let mut store = test_store(&temp_dir, &config);
        store.add_task("Fail status", "", None, vec![]).unwrap();
        store.fail_task("task-001", "blocked").unwrap();

        let (result, output) = capture_output(|| run_status(&project, false));

        result.unwrap();
        assert!(output.contains("✗ failed"));
    }

    #[test]
    fn title_truncation() {
        let (temp_dir, project, config) = test_project_with_config();
        let mut store = test_store(&temp_dir, &config);
        let title = "x".repeat(50);
        store.add_task(&title, "", None, vec![]).unwrap();

        let (result, output) = capture_output(|| run_status(&project, false));

        result.unwrap();
        let expected = format!("{}…", "x".repeat(40));
        assert_eq!(format_title(&title), expected);
        assert_eq!(format_title(&title).chars().nth(40), Some('…'));
        assert!(output.contains(&expected));
        assert!(!output.contains(&title));
    }

    #[test]
    fn sorted_by_id() {
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

        let (result, output) = capture_output(|| run_status(&project, false));

        result.unwrap();
        assert!(output.find("task-001").unwrap() < output.find("task-002").unwrap());
    }

    #[test]
    fn json_outputs_raw_tasks_file_exactly() {
        let temp_dir = TempDir::new().unwrap();
        let project = test_project(&temp_dir);
        let raw = "{\n  \"tasks\": []\n}";
        fs::write(project.state_dir.join("tasks.json"), raw).unwrap();

        let (result, output) = capture_output(|| run_status(&project, true));

        result.unwrap();
        assert_eq!(output, raw);
    }

    #[test]
    fn json_missing_tasks_file_returns_json_output() {
        let temp_dir = TempDir::new().unwrap();
        let project = test_project(&temp_dir);

        let (result, output) = capture_output(|| run_status(&project, true));
        let error = result.unwrap_err();

        assert!(matches!(error, StatusError::JsonOutput { .. }));
        assert_eq!(output, "");
    }

    #[test]
    fn config_load_and_invalid_config_errors() {
        let temp_dir = TempDir::new().unwrap();
        let project = test_project(&temp_dir);
        write_empty_tasks(&project);

        let error = run_status(&project, false).unwrap_err();
        assert!(matches!(error, StatusError::ConfigNotFound { .. }));

        fs::write(project.state_dir.join("config.json"), "{").unwrap();
        let error = run_status(&project, false).unwrap_err();
        assert!(matches!(error, StatusError::ConfigLoad { .. }));

        let mut config = test_config();
        config.state_machine.clear();
        write_config(&project, &config);
        let error = run_status(&project, false).unwrap_err();
        match error {
            StatusError::InvalidConfig { reason, .. } => {
                assert_eq!(reason, "state_machine must not be empty");
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
        let error = run_status(&project, true).unwrap_err();

        let code = match error {
            StatusError::JsonOutput { .. } => ExitCode::from(2),
            _ => ExitCode::from(1),
        };

        assert_eq!(code, ExitCode::from(2));
    }
}
