use std::{
    fmt::Write as FmtWrite,
    io::{self, Write},
    path::Path,
};

use crate::{
    commands::status::{StatusError, sort_tasks_by_id_asc},
    core::{
        config::{ConfigError, load_project_config},
        project::Project,
        tasks::{StoreError, Task, TaskStore},
    },
};

const NO_BLOCKED_TASKS_MESSAGE: &str = "No blocked tasks.";

pub fn run_blocked(project: &Project, json: bool) -> Result<(), StatusError> {
    let config_path = project.state_dir.join("config.json");
    let config =
        load_project_config(&config_path, &project.global_config).map_err(map_config_error)?;
    let store = TaskStore::new(&project.state_dir, &config)?;
    let sorted_tasks = sort_tasks_by_id_asc(store.all_tasks());
    let blocked_tasks: Vec<&Task> = sorted_tasks
        .into_iter()
        .filter(|task| task.state == "blocked")
        .collect();

    if json {
        let json_str =
            serde_json::to_string_pretty(&blocked_tasks).map_err(StoreError::Serialize)?;
        return write_blocked_output(&json_str, &project.state_dir.join("tasks.json"));
    }

    if blocked_tasks.is_empty() {
        print_blocked_output(NO_BLOCKED_TASKS_MESSAGE);
        return Ok(());
    }

    print_blocked_output(&format_blocked_tasks(&blocked_tasks));
    Ok(())
}

fn map_config_error(error: ConfigError) -> StatusError {
    match error {
        ConfigError::NotFound { path } => StatusError::ConfigNotFound { path },
        ConfigError::Read { path, source } => StatusError::ConfigRead { path, source },
        ConfigError::Parse { path, source } => StatusError::ConfigLoad { path, source },
        ConfigError::Invalid { path, reason } => StatusError::InvalidConfig { path, reason },
    }
}

fn format_blocked_tasks(tasks: &[&Task]) -> String {
    tasks
        .iter()
        .map(|task| {
            let mut output = String::new();
            writeln!(output, "ID: {}", task.id).unwrap();
            writeln!(output, "Title: {}", task.title).unwrap();
            writeln!(
                output,
                "Blocked at phase: {}",
                task.blocked_at_phase.as_deref().unwrap_or("-")
            )
            .unwrap();
            writeln!(output, "Blocked reason:").unwrap();
            write!(
                output,
                "{}",
                format_multiline_value(task.blocked_reason.as_deref().unwrap_or("-"), 2)
            )
            .unwrap();
            output
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

fn format_multiline_value(value: &str, indent: usize) -> String {
    let prefix = " ".repeat(indent);
    value
        .lines()
        .map(|line| {
            if line.is_empty() {
                format!("{prefix}-")
            } else {
                format!("{prefix}{line}")
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn print_blocked_output(message: &str) {
    #[cfg(test)]
    OUTPUT_CAPTURE.with(|capture| {
        if let Some(output) = capture.borrow_mut().as_mut() {
            output.push_str(message);
            output.push('\n');
        }
    });

    println!("{message}");
}

fn write_blocked_output(contents: &str, path: &Path) -> Result<(), StatusError> {
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

#[cfg(test)]
mod tests {
    use std::{collections::BTreeMap, fs, path::Path};

    use crate::core::{
        config::{Config, PhaseDef},
        global_config::GlobalConfig,
        hooks::HookConfig,
        project::Project,
        tasks::{HistoryEntry, Task},
    };

    use super::{OUTPUT_CAPTURE, run_blocked};

    fn test_config() -> Config {
        Config::new_single_workflow(
            "rust",
            vec![
                ("pending".to_owned(), PhaseDef::default()),
                ("implementing".to_owned(), PhaseDef::default()),
                ("done".to_owned(), PhaseDef::default()),
            ],
            3,
        )
    }

    fn make_project(state_dir: &Path) -> Project {
        Project {
            git_root: Path::new("/tmp/agira-blocked-test").to_path_buf(),
            slug: "agira-blocked-test".to_owned(),
            state_dir: state_dir.to_path_buf(),
            global_config: GlobalConfig::default(),
            global_hooks: HookConfig::default(),
            project_hooks: HookConfig::default(),
        }
    }

    fn setup_project() -> (tempfile::TempDir, Project) {
        let dir = tempfile::TempDir::new().expect("temp dir");
        let state_dir = dir.path().join(".agira").join("agira-blocked-test");
        fs::create_dir_all(&state_dir).expect("state dir");
        fs::write(
            state_dir.join("config.json"),
            serde_json::to_string_pretty(&test_config()).expect("serialize config"),
        )
        .expect("write config");

        (dir, make_project(&state_dir))
    }

    fn task(id: &str, title: &str, state: &str) -> Task {
        Task {
            id: id.to_owned(),
            title: title.to_owned(),
            description: "description".to_owned(),
            state: state.to_owned(),
            blocked_at_phase: None,
            blocked_reason: None,
            clarifications: Vec::new(),
            dependencies: Vec::new(),
            retry_count: 0,
            max_retries: 3,
            phases: BTreeMap::new(),
            history: vec![HistoryEntry {
                from: None,
                to: state.to_owned(),
                timestamp: "2026-06-13T10:00:00Z".to_owned(),
                reason: "task created".to_owned(),
            }],
            created_at: "2026-06-13T10:00:00Z".to_owned(),
            workflow: "default".to_owned(),
            locked_at: None,
            acceptance_criteria: None,
        }
    }

    fn blocked_task(id: &str, title: &str, phase: &str, reason: &str) -> Task {
        let mut task = task(id, title, "blocked");
        task.blocked_at_phase = Some(phase.to_owned());
        task.blocked_reason = Some(reason.to_owned());
        task
    }

    fn write_tasks(project: &Project, tasks: Vec<Task>) {
        fs::write(
            project.state_dir.join("tasks.json"),
            serde_json::to_string_pretty(&serde_json::json!({ "tasks": tasks }))
                .expect("serialize tasks"),
        )
        .expect("write tasks");
    }

    fn capture_output<F>(f: F) -> String
    where
        F: FnOnce(),
    {
        OUTPUT_CAPTURE.with(|capture| *capture.borrow_mut() = Some(String::new()));
        f();
        OUTPUT_CAPTURE.with(|capture| capture.borrow_mut().take().expect("captured output"))
    }

    #[test]
    fn text_lists_blocked_tasks_with_complete_reasons_ordered_by_id() {
        let (_dir, project) = setup_project();
        write_tasks(
            &project,
            vec![
                blocked_task(
                    "task-010",
                    "later blocked",
                    "reviewing",
                    "Question one?\nQuestion two needs a longer human decision.",
                ),
                blocked_task("task-002", "first blocked", "implementing", "Pick A or B?"),
            ],
        );

        let output = capture_output(|| run_blocked(&project, false).expect("run blocked"));

        assert!(output.contains("task-002"));
        assert!(output.contains("first blocked"));
        assert!(output.contains("implementing"));
        assert!(output.contains("Pick A or B?"));
        assert!(output.contains("task-010"));
        assert!(output.contains("Question one?\n  Question two needs a longer human decision."));
        assert!(output.find("task-002") < output.find("task-010"));
    }

    #[test]
    fn text_excludes_non_blocked_tasks() {
        let (_dir, project) = setup_project();
        write_tasks(
            &project,
            vec![
                task("task-001", "pending task", "pending"),
                blocked_task("task-002", "blocked task", "implementing", "Need input?"),
                task("task-003", "done task", "done"),
                task("task-004", "failed task", "failed"),
            ],
        );

        let output = capture_output(|| run_blocked(&project, false).expect("run blocked"));

        assert!(output.contains("task-002"));
        assert!(!output.contains("pending task"));
        assert!(!output.contains("done task"));
        assert!(!output.contains("failed task"));
    }

    #[test]
    fn json_outputs_blocked_tasks_as_full_task_objects() {
        let (_dir, project) = setup_project();
        write_tasks(
            &project,
            vec![
                task("task-001", "pending task", "pending"),
                blocked_task("task-002", "blocked task", "implementing", "Need input?"),
            ],
        );

        let output = capture_output(|| run_blocked(&project, true).expect("run blocked"));
        let value: serde_json::Value = serde_json::from_str(&output).expect("valid json");
        let array = value.as_array().expect("json array");

        assert_eq!(array.len(), 1);
        assert_eq!(array[0]["id"], "task-002");
        assert_eq!(array[0]["title"], "blocked task");
        assert_eq!(array[0]["state"], "blocked");
        assert_eq!(array[0]["blocked_at_phase"], "implementing");
        assert_eq!(array[0]["blocked_reason"], "Need input?");
        assert!(array[0].get("description").is_some());
        assert!(array[0].get("history").is_some());
    }

    #[test]
    fn empty_outputs_clear_text_line_or_empty_json_array() {
        let (_dir, project) = setup_project();
        write_tasks(&project, vec![task("task-001", "pending task", "pending")]);

        let text = capture_output(|| run_blocked(&project, false).expect("run blocked"));
        assert_eq!(text, "No blocked tasks.\n");

        let json = capture_output(|| run_blocked(&project, true).expect("run blocked"));
        assert_eq!(json, "[]");
    }

    #[test]
    fn missing_tasks_file_is_empty() {
        let (_dir, project) = setup_project();

        let text = capture_output(|| run_blocked(&project, false).expect("run blocked"));
        assert_eq!(text, "No blocked tasks.\n");

        let json = capture_output(|| run_blocked(&project, true).expect("run blocked"));
        assert_eq!(json, "[]");
    }
}
