use std::{
    collections::BTreeMap,
    fs, io,
    path::{Path, PathBuf},
};

use chrono::Utc;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::core::config::Config;

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct TaskPhase {
    pub artifact: String,
    pub completed_at: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct HistoryEntry {
    pub from: Option<String>,
    pub to: String,
    pub timestamp: String,
    pub reason: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct Task {
    pub id: String,
    pub title: String,
    pub description: String,
    pub state: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blocked_at_phase: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blocked_reason: Option<String>,
    pub dependencies: Vec<String>,
    pub retry_count: u32,
    #[serde(default = "default_max_retries")]
    pub max_retries: u32,
    pub phases: BTreeMap<String, TaskPhase>,
    pub history: Vec<HistoryEntry>,
    pub created_at: String,
}

fn default_max_retries() -> u32 {
    3
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct TasksFile {
    pub tasks: Vec<Task>,
}

#[derive(Debug, Error)]
pub enum StoreError {
    #[error("task not found")]
    NotFound,

    #[error("invalid transition: {from} -> {to}")]
    InvalidTransition { from: String, to: String },

    #[error("task {task_id} blocked by dependency {blocking_id}")]
    DependencyBlocked {
        task_id: String,
        blocking_id: String,
    },

    #[error("task is already terminal")]
    AlreadyTerminal,

    #[error("task is already blocked")]
    AlreadyBlocked,

    #[error("task is not blocked")]
    NotBlocked,

    #[error("task {id} is {state} and cannot be updated")]
    CannotUpdateTerminal { id: String, state: String },

    #[error("task {id} is not in the pending phase")]
    NotInPendingPhase { id: String },

    #[error("task {id} is depended on by {dependent_id}")]
    DependedOnBy { id: String, dependent_id: String },

    #[error("unknown phase: {phase}")]
    UnknownPhase { phase: String },

    #[error("failed to write or read {path}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },

    #[error("failed to serialize tasks")]
    Serialize(#[source] serde_json::Error),

    #[error("failed to deserialize tasks")]
    Deserialize(#[source] serde_json::Error),
}

pub struct TaskStore {
    tasks_path: PathBuf,
    tasks_file: TasksFile,
    state_machine: Vec<String>,
    terminal_phase: String,
    max_retries: u32,
}

impl TaskStore {
    pub fn new(state_dir: impl AsRef<Path>, config: &Config) -> Result<Self, StoreError> {
        let tasks_path = state_dir.as_ref().join("tasks.json");
        let state_machine: Vec<String> = config.phases.iter().map(|p| p.name.clone()).collect();
        let terminal_phase =
            state_machine
                .last()
                .cloned()
                .ok_or_else(|| StoreError::InvalidTransition {
                    from: String::new(),
                    to: String::new(),
                })?;
        let tasks_file = Self::load_from_file(&tasks_path)?;

        Ok(Self {
            tasks_path,
            tasks_file,
            state_machine,
            terminal_phase,
            max_retries: config.max_retries,
        })
    }

    fn load_from_file(path: &Path) -> Result<TasksFile, StoreError> {
        match fs::read_to_string(path) {
            Ok(contents) => serde_json::from_str(&contents).map_err(StoreError::Deserialize),
            Err(source) if source.kind() == io::ErrorKind::NotFound => {
                Ok(TasksFile { tasks: vec![] })
            }
            Err(source) => Err(StoreError::Io {
                path: path.to_path_buf(),
                source,
            }),
        }
    }

    pub fn save(&mut self, tasks_file: TasksFile) -> Result<(), StoreError> {
        let bytes = serde_json::to_vec_pretty(&tasks_file).map_err(StoreError::Serialize)?;
        let temporary_path = self.tasks_path.with_extension("json.tmp");

        fs::write(&temporary_path, bytes).map_err(|source| StoreError::Io {
            path: self.tasks_path.clone(),
            source,
        })?;
        fs::rename(&temporary_path, &self.tasks_path).map_err(|source| StoreError::Io {
            path: self.tasks_path.clone(),
            source,
        })?;

        self.tasks_file = tasks_file;
        Ok(())
    }

    pub fn add_task(
        &mut self,
        title: &str,
        description: &str,
        dependencies: Vec<String>,
        phase_override: Option<&str>,
    ) -> Result<Task, StoreError> {
        let next_num = self
            .tasks_file
            .tasks
            .iter()
            .filter_map(|t| {
                t.id.strip_prefix("task-")
                    .and_then(|suffix| suffix.parse::<usize>().ok())
            })
            .max()
            .unwrap_or(0)
            + 1;
        let id = format!("task-{next_num:03}");

        for dependency_id in &dependencies {
            if self.get_task(dependency_id).is_none() {
                return Err(StoreError::DependencyBlocked {
                    task_id: id,
                    blocking_id: dependency_id.clone(),
                });
            }
        }

        let initial_phase = if let Some(phase) = phase_override {
            if !self.state_machine.iter().any(|s| s == phase) {
                return Err(StoreError::UnknownPhase {
                    phase: phase.to_owned(),
                });
            }
            phase.to_owned()
        } else {
            self.state_machine
                .first()
                .cloned()
                .ok_or_else(|| StoreError::InvalidTransition {
                    from: String::new(),
                    to: String::new(),
                })?
        };
        let created_at = Utc::now().to_rfc3339();
        let task = Task {
            id,
            title: title.to_owned(),
            description: description.to_owned(),
            state: initial_phase.clone(),
            blocked_at_phase: None,
            blocked_reason: None,
            dependencies,
            retry_count: 0,
            max_retries: self.max_retries,
            phases: BTreeMap::new(),
            history: vec![HistoryEntry {
                from: None,
                to: initial_phase,
                timestamp: created_at.clone(),
                reason: "task created".to_owned(),
            }],
            created_at,
        };

        let mut tasks_file = self.tasks_file.clone();
        tasks_file.tasks.push(task.clone());
        self.save(tasks_file)?;

        Ok(task)
    }

    pub fn get_task(&self, id: &str) -> Option<&Task> {
        self.tasks_file.tasks.iter().find(|task| task.id == id)
    }

    pub fn record_phase_artifact(
        &mut self,
        id: &str,
        artifact: &str,
        completed_at: String,
    ) -> Result<String, StoreError> {
        let mut tasks_file = self.tasks_file.clone();
        let task = tasks_file
            .tasks
            .iter_mut()
            .find(|task| task.id == id)
            .ok_or(StoreError::NotFound)?;
        let current_phase = task.state.clone();
        task.phases.insert(
            current_phase.clone(),
            TaskPhase {
                artifact: artifact.to_owned(),
                completed_at,
            },
        );
        self.save(tasks_file)?;
        Ok(current_phase)
    }

    pub fn all_tasks(&self) -> &[Task] {
        &self.tasks_file.tasks
    }

    pub fn next_phase(&mut self, id: &str) -> Result<(), StoreError> {
        let mut tasks_file = self.tasks_file.clone();
        let task_index = tasks_file
            .tasks
            .iter()
            .position(|task| task.id == id)
            .ok_or(StoreError::NotFound)?;
        let previous_state = tasks_file.tasks[task_index].state.clone();

        if previous_state == "failed" || previous_state == self.terminal_phase {
            return Err(StoreError::AlreadyTerminal);
        }

        let current_index = self
            .state_machine
            .iter()
            .position(|state| state == &previous_state)
            .ok_or_else(|| StoreError::InvalidTransition {
                from: previous_state.clone(),
                to: String::new(),
            })?;
        let target_index = current_index + 1;

        if target_index >= self.state_machine.len() {
            return Err(StoreError::AlreadyTerminal);
        }

        if target_index > 0 {
            for dependency_id in &tasks_file.tasks[task_index].dependencies {
                match tasks_file
                    .tasks
                    .iter()
                    .find(|task| task.id == *dependency_id)
                {
                    Some(dependency) if dependency.state == self.terminal_phase => {}
                    _ => {
                        return Err(StoreError::DependencyBlocked {
                            task_id: id.to_owned(),
                            blocking_id: dependency_id.clone(),
                        });
                    }
                }
            }
        }

        let target_state = self.state_machine[target_index].clone();
        let task = &mut tasks_file.tasks[task_index];
        task.state = target_state.clone();
        task.history.push(HistoryEntry {
            from: Some(previous_state),
            to: target_state,
            timestamp: Utc::now().to_rfc3339(),
            reason: "advanced to next phase".to_owned(),
        });

        self.save(tasks_file)
    }

    pub fn fail_task(&mut self, id: &str, reason: &str) -> Result<(), StoreError> {
        let mut tasks_file = self.tasks_file.clone();
        let task_index = tasks_file
            .tasks
            .iter()
            .position(|task| task.id == id)
            .ok_or(StoreError::NotFound)?;
        let previous_state = tasks_file.tasks[task_index].state.clone();

        if previous_state == "failed" || previous_state == self.terminal_phase {
            return Err(StoreError::AlreadyTerminal);
        }

        if !self
            .state_machine
            .iter()
            .any(|state| state == &previous_state)
        {
            return Err(StoreError::InvalidTransition {
                from: previous_state,
                to: "failed".to_owned(),
            });
        }

        let task = &mut tasks_file.tasks[task_index];
        task.state = "failed".to_owned();
        task.history.push(HistoryEntry {
            from: Some(previous_state),
            to: "failed".to_owned(),
            timestamp: Utc::now().to_rfc3339(),
            reason: reason.to_owned(),
        });

        self.save(tasks_file)
    }

    pub fn block_task(&mut self, id: &str, reason: &str) -> Result<(), StoreError> {
        let mut tasks_file = self.tasks_file.clone();
        let task_index = tasks_file
            .tasks
            .iter()
            .position(|task| task.id == id)
            .ok_or(StoreError::NotFound)?;
        let previous_state = tasks_file.tasks[task_index].state.clone();

        if previous_state == "blocked" {
            return Err(StoreError::AlreadyBlocked);
        }

        if previous_state == "failed" || previous_state == self.terminal_phase {
            return Err(StoreError::AlreadyTerminal);
        }

        if !self
            .state_machine
            .iter()
            .any(|state| state == &previous_state)
        {
            return Err(StoreError::InvalidTransition {
                from: previous_state,
                to: "blocked".to_owned(),
            });
        }

        let task = &mut tasks_file.tasks[task_index];
        task.state = "blocked".to_owned();
        task.blocked_at_phase = Some(previous_state.clone());
        task.blocked_reason = Some(reason.to_owned());
        task.history.push(HistoryEntry {
            from: Some(previous_state),
            to: "blocked".to_owned(),
            timestamp: Utc::now().to_rfc3339(),
            reason: reason.to_owned(),
        });

        self.save(tasks_file)
    }

    pub fn unblock_task(&mut self, id: &str) -> Result<(), StoreError> {
        let mut tasks_file = self.tasks_file.clone();
        let task_index = tasks_file
            .tasks
            .iter()
            .position(|task| task.id == id)
            .ok_or(StoreError::NotFound)?;

        if tasks_file.tasks[task_index].state != "blocked" {
            return Err(StoreError::NotBlocked);
        }

        let target_state = tasks_file.tasks[task_index]
            .blocked_at_phase
            .clone()
            .ok_or_else(|| StoreError::InvalidTransition {
                from: "blocked".to_owned(),
                to: String::new(),
            })?;

        let task = &mut tasks_file.tasks[task_index];
        task.state = target_state.clone();
        task.blocked_at_phase = None;
        task.blocked_reason = None;
        task.history.push(HistoryEntry {
            from: Some("blocked".to_owned()),
            to: target_state,
            timestamp: Utc::now().to_rfc3339(),
            reason: "unblocked".to_owned(),
        });

        self.save(tasks_file)
    }

    pub fn update_task(
        &mut self,
        id: &str,
        title: Option<&str>,
        description: Option<&str>,
        depends_on: Option<&[String]>,
    ) -> Result<(), StoreError> {
        let target_task = self.get_task(id).ok_or(StoreError::NotFound)?;
        if target_task.state == self.terminal_phase || target_task.state == "failed" {
            return Err(StoreError::CannotUpdateTerminal {
                id: id.to_owned(),
                state: target_task.state.clone(),
            });
        }

        if let Some(deps) = depends_on {
            for dep_id in deps {
                if self.get_task(dep_id).is_none() {
                    return Err(StoreError::DependencyBlocked {
                        task_id: id.to_owned(),
                        blocking_id: dep_id.clone(),
                    });
                }
            }
        }

        let mut tasks_file = self.tasks_file.clone();
        let task = tasks_file
            .tasks
            .iter_mut()
            .find(|task| task.id == id)
            .ok_or(StoreError::NotFound)?;

        if let Some(t) = title {
            task.title = t.to_owned();
        }
        if let Some(d) = description {
            task.description = d.to_owned();
        }
        if let Some(deps) = depends_on {
            task.dependencies = deps.to_vec();
        }

        self.save(tasks_file)
    }

    pub fn remove_task(&mut self, id: &str) -> Result<(), StoreError> {
        let task = self.get_task(id).ok_or(StoreError::NotFound)?;
        let pending_phase =
            self.state_machine
                .first()
                .ok_or_else(|| StoreError::InvalidTransition {
                    from: task.state.clone(),
                    to: String::new(),
                })?;

        if task.state != *pending_phase {
            return Err(StoreError::NotInPendingPhase { id: id.to_owned() });
        }

        if let Some(dependent) = self.tasks_file.tasks.iter().find(|task| {
            task.id != id && task.dependencies.iter().any(|dependency| dependency == id)
        }) {
            return Err(StoreError::DependedOnBy {
                id: id.to_owned(),
                dependent_id: dependent.id.clone(),
            });
        }

        let mut tasks_file = self.tasks_file.clone();
        tasks_file.tasks.retain(|t| t.id != id);
        self.save(tasks_file)
    }

    pub fn retry_task(&mut self, id: &str, reason: &str) -> Result<(u32, u32), StoreError> {
        let mut tasks_file = self.tasks_file.clone();
        let task_index = tasks_file
            .tasks
            .iter()
            .position(|task| task.id == id)
            .ok_or(StoreError::NotFound)?;
        let previous_state = tasks_file.tasks[task_index].state.clone();

        if previous_state == "failed" || previous_state == self.terminal_phase {
            return Err(StoreError::AlreadyTerminal);
        }

        let first_phase =
            self.state_machine
                .first()
                .cloned()
                .ok_or_else(|| StoreError::InvalidTransition {
                    from: previous_state.clone(),
                    to: String::new(),
                })?;

        if !self
            .state_machine
            .iter()
            .any(|state| state == &previous_state)
        {
            return Err(StoreError::InvalidTransition {
                from: previous_state,
                to: first_phase,
            });
        }

        let task = &mut tasks_file.tasks[task_index];
        let new_retry_count = task.retry_count + 1;
        let max_retries = task.max_retries;
        task.retry_count = new_retry_count;
        task.state = first_phase.clone();
        task.history.push(HistoryEntry {
            from: Some(previous_state),
            to: first_phase,
            timestamp: Utc::now().to_rfc3339(),
            reason: format!("retry {new_retry_count}/{max_retries}: {reason}"),
        });

        self.save(tasks_file)?;

        Ok((new_retry_count, max_retries))
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use chrono::DateTime;
    use serde_json::Value;
    use tempfile::TempDir;

    use super::*;
    use crate::core::config::{PhaseConfig, VerificationConfig};

    fn test_config() -> Config {
        Config {
            stack: "rust".to_owned(),
            phases: vec![
                PhaseConfig {
                    name: "pending".to_owned(),
                    model: None,
                    duty: None,
                },
                PhaseConfig {
                    name: "enriching".to_owned(),
                    model: Some("opus".to_owned()),
                    duty: None,
                },
                PhaseConfig {
                    name: "done".to_owned(),
                    model: None,
                    duty: None,
                },
            ],
            default_model: None,
            verification: VerificationConfig { commands: vec![] },
            max_retries: 3,
        }
    }

    fn test_store(temp_dir: &TempDir) -> TaskStore {
        TaskStore::new(temp_dir.path(), &test_config()).unwrap()
    }

    #[test]
    fn add_task_creates_task_with_first_phase() {
        let temp_dir = TempDir::new().unwrap();
        let mut store = test_store(&temp_dir);

        let task = store
            .add_task("First task", "Description", vec![], None)
            .unwrap();

        assert_eq!(task.id, "task-001");
        assert_eq!(task.state, "pending");
        assert_eq!(task.retry_count, 0);
        assert_eq!(task.history.len(), 1);
        assert_eq!(task.history[0].to, "pending");
        assert!(DateTime::parse_from_rfc3339(&task.history[0].timestamp).is_ok());

        let tasks_path = temp_dir.path().join("tasks.json");
        assert!(tasks_path.exists());
        let contents = fs::read_to_string(tasks_path).unwrap();
        let value: Value = serde_json::from_str(&contents).unwrap();
        assert!(value.get("tasks").is_some());
    }

    #[test]
    fn add_task_with_phase_override_places_task_in_that_phase() {
        let temp_dir = TempDir::new().unwrap();
        let mut store = test_store(&temp_dir);

        let task = store
            .add_task("First task", "", vec![], Some("enriching"))
            .unwrap();

        assert_eq!(task.state, "enriching");
        assert_eq!(task.history.len(), 1);
        assert_eq!(task.history[0].to, "enriching");
        assert_eq!(store.get_task("task-001").unwrap().state, "enriching");
    }

    #[test]
    fn add_task_with_unknown_phase_returns_error() {
        let temp_dir = TempDir::new().unwrap();
        let mut store = test_store(&temp_dir);

        let error = store
            .add_task("First task", "", vec![], Some("reviewing"))
            .unwrap_err();

        match error {
            StoreError::UnknownPhase { phase } => assert_eq!(phase, "reviewing"),
            other => panic!("unexpected error: {other}"),
        }
        assert!(store.all_tasks().is_empty());
        assert!(!temp_dir.path().join("tasks.json").exists());
    }

    #[test]
    fn sequential_ids() {
        let temp_dir = TempDir::new().unwrap();
        let mut store = test_store(&temp_dir);

        let first = store.add_task("First", "", vec![], None).unwrap();
        let second = store.add_task("Second", "", vec![], None).unwrap();
        let third = store.add_task("Third", "", vec![], None).unwrap();

        assert_eq!(first.id, "task-001");
        assert_eq!(second.id, "task-002");
        assert_eq!(third.id, "task-003");
    }

    #[test]
    fn next_phase_advances_state() {
        let temp_dir = TempDir::new().unwrap();
        let mut store = test_store(&temp_dir);

        store.add_task("First", "", vec![], None).unwrap();
        store.next_phase("task-001").unwrap();
        let task = store.get_task("task-001").unwrap();

        assert_eq!(task.state, "enriching");
        assert_eq!(task.history.len(), 2);
        let last = task.history.last().unwrap();
        assert_eq!(last.from.as_deref(), Some("pending"));
        assert_eq!(last.to, "enriching");
    }

    #[test]
    fn next_phase_on_terminal_returns_already_terminal() {
        let temp_dir = TempDir::new().unwrap();
        let mut store = test_store(&temp_dir);

        store.add_task("First", "", vec![], None).unwrap();
        store.next_phase("task-001").unwrap();
        store.next_phase("task-001").unwrap();

        let error = store.next_phase("task-001").unwrap_err();
        assert!(matches!(error, StoreError::AlreadyTerminal));
    }

    #[test]
    fn next_phase_on_failed_returns_already_terminal() {
        let temp_dir = TempDir::new().unwrap();
        let mut store = test_store(&temp_dir);

        store.add_task("First", "", vec![], None).unwrap();
        store.fail_task("task-001", "failed").unwrap();

        let error = store.next_phase("task-001").unwrap_err();
        assert!(matches!(error, StoreError::AlreadyTerminal));
    }

    #[test]
    fn fail_task_sets_failed() {
        let temp_dir = TempDir::new().unwrap();
        let mut store = test_store(&temp_dir);

        store.add_task("First", "", vec![], None).unwrap();
        store.fail_task("task-001", "provided reason").unwrap();
        let task = store.get_task("task-001").unwrap();

        assert_eq!(task.state, "failed");
        let last = task.history.last().unwrap();
        assert_eq!(last.to, "failed");
        assert_eq!(last.reason, "provided reason");
    }

    #[test]
    fn fail_task_on_terminal_returns_already_terminal() {
        let temp_dir = TempDir::new().unwrap();
        let mut store = test_store(&temp_dir);

        store.add_task("First", "", vec![], None).unwrap();
        store.next_phase("task-001").unwrap();
        store.next_phase("task-001").unwrap();

        let error = store.fail_task("task-001", "too late").unwrap_err();
        assert!(matches!(error, StoreError::AlreadyTerminal));
    }

    #[test]
    fn retry_task_resets_state_and_increments_retry_count() {
        let temp_dir = TempDir::new().unwrap();
        let mut store = test_store(&temp_dir);

        store.add_task("First", "", vec![], None).unwrap();
        store.next_phase("task-001").unwrap();

        let (retry_count, max_retries) = store.retry_task("task-001", "try again").unwrap();
        let task = store.get_task("task-001").unwrap();

        assert_eq!((retry_count, max_retries), (1, 3));
        assert_eq!(task.state, "pending");
        assert_eq!(task.retry_count, 1);
        let last = task.history.last().unwrap();
        assert_eq!(last.from.as_deref(), Some("enriching"));
        assert_eq!(last.to, "pending");
        assert_eq!(last.reason, "retry 1/3: try again");
        assert!(DateTime::parse_from_rfc3339(&last.timestamp).is_ok());
    }

    #[test]
    fn retry_task_rejects_terminal_states() {
        let temp_dir = TempDir::new().unwrap();
        let mut store = test_store(&temp_dir);

        store.add_task("Done", "", vec![], None).unwrap();
        store.next_phase("task-001").unwrap();
        store.next_phase("task-001").unwrap();
        let before_done = store.get_task("task-001").unwrap().clone();

        let error = store.retry_task("task-001", "too late").unwrap_err();
        assert!(matches!(error, StoreError::AlreadyTerminal));
        assert_eq!(store.get_task("task-001").unwrap(), &before_done);

        store.add_task("Failed", "", vec![], None).unwrap();
        store.fail_task("task-002", "failed").unwrap();
        let before_failed = store.get_task("task-002").unwrap().clone();

        let error = store.retry_task("task-002", "too late").unwrap_err();
        assert!(matches!(error, StoreError::AlreadyTerminal));
        assert_eq!(store.get_task("task-002").unwrap(), &before_failed);
    }

    #[test]
    fn update_task_rejects_done() {
        let temp_dir = TempDir::new().unwrap();
        let mut store = test_store(&temp_dir);

        store.add_task("First", "", vec![], None).unwrap();
        store.next_phase("task-001").unwrap();
        store.next_phase("task-001").unwrap();
        let before = store.get_task("task-001").unwrap().clone();

        let error = store
            .update_task("task-001", Some("Updated"), None, None)
            .unwrap_err();

        match error {
            StoreError::CannotUpdateTerminal { id, state } => {
                assert_eq!(id, "task-001");
                assert_eq!(state, "done");
            }
            other => panic!("unexpected error: {other}"),
        }
        assert_eq!(store.get_task("task-001").unwrap(), &before);
    }

    #[test]
    fn update_task_rejects_failed() {
        let temp_dir = TempDir::new().unwrap();
        let mut store = test_store(&temp_dir);

        store.add_task("First", "", vec![], None).unwrap();
        store.fail_task("task-001", "failed").unwrap();
        let before = store.get_task("task-001").unwrap().clone();

        let error = store
            .update_task("task-001", Some("Updated"), None, None)
            .unwrap_err();

        match error {
            StoreError::CannotUpdateTerminal { id, state } => {
                assert_eq!(id, "task-001");
                assert_eq!(state, "failed");
            }
            other => panic!("unexpected error: {other}"),
        }
        assert_eq!(store.get_task("task-001").unwrap(), &before);
    }

    #[test]
    fn dependency_blocks_advance() {
        let temp_dir = TempDir::new().unwrap();
        let mut store = test_store(&temp_dir);

        store.add_task("Dependency", "", vec![], None).unwrap();
        store
            .add_task("Dependent", "", vec!["task-001".to_owned()], None)
            .unwrap();

        let error = store.next_phase("task-002").unwrap_err();
        match error {
            StoreError::DependencyBlocked {
                task_id,
                blocking_id,
            } => {
                assert_eq!(task_id, "task-002");
                assert_eq!(blocking_id, "task-001");
            }
            other => panic!("unexpected error: {other}"),
        }
    }

    #[test]
    fn dependency_unblocked_when_dep_terminal() {
        let temp_dir = TempDir::new().unwrap();
        let mut store = test_store(&temp_dir);

        store.add_task("Dependency", "", vec![], None).unwrap();
        store
            .add_task("Dependent", "", vec!["task-001".to_owned()], None)
            .unwrap();
        store.next_phase("task-001").unwrap();
        store.next_phase("task-001").unwrap();

        store.next_phase("task-002").unwrap();

        assert_eq!(store.get_task("task-002").unwrap().state, "enriching");
    }

    #[test]
    fn failed_dependency_blocks() {
        let temp_dir = TempDir::new().unwrap();
        let mut store = test_store(&temp_dir);

        store.add_task("Dependency", "", vec![], None).unwrap();
        store
            .add_task("Dependent", "", vec!["task-001".to_owned()], None)
            .unwrap();
        store.fail_task("task-001", "dependency failed").unwrap();

        let error = store.next_phase("task-002").unwrap_err();
        assert!(matches!(error, StoreError::DependencyBlocked { .. }));
    }

    #[test]
    fn block_task_sets_blocked_metadata_and_history() {
        let temp_dir = TempDir::new().unwrap();
        let mut store = test_store(&temp_dir);

        store.add_task("First", "", vec![], None).unwrap();
        store.block_task("task-001", "waiting on api").unwrap();
        let task = store.get_task("task-001").unwrap();

        assert_eq!(task.state, "blocked");
        assert_eq!(task.blocked_at_phase.as_deref(), Some("pending"));
        assert_eq!(task.blocked_reason.as_deref(), Some("waiting on api"));
        let last = task.history.last().unwrap();
        assert_eq!(last.from.as_deref(), Some("pending"));
        assert_eq!(last.to, "blocked");
        assert_eq!(last.reason, "waiting on api");
        assert!(DateTime::parse_from_rfc3339(&last.timestamp).is_ok());
    }

    #[test]
    fn block_task_rejects_terminal_done() {
        let temp_dir = TempDir::new().unwrap();
        let mut store = test_store(&temp_dir);

        store.add_task("First", "", vec![], None).unwrap();
        store.next_phase("task-001").unwrap();
        store.next_phase("task-001").unwrap();

        let error = store.block_task("task-001", "reason").unwrap_err();
        assert!(matches!(error, StoreError::AlreadyTerminal));
    }

    #[test]
    fn block_task_rejects_failed() {
        let temp_dir = TempDir::new().unwrap();
        let mut store = test_store(&temp_dir);

        store.add_task("First", "", vec![], None).unwrap();
        store.fail_task("task-001", "failed").unwrap();

        let error = store.block_task("task-001", "reason").unwrap_err();
        assert!(matches!(error, StoreError::AlreadyTerminal));
    }

    #[test]
    fn block_task_rejects_already_blocked() {
        let temp_dir = TempDir::new().unwrap();
        let mut store = test_store(&temp_dir);

        store.add_task("First", "", vec![], None).unwrap();
        store.block_task("task-001", "first reason").unwrap();

        let error = store.block_task("task-001", "second reason").unwrap_err();
        assert!(matches!(error, StoreError::AlreadyBlocked));
    }

    #[test]
    fn block_unknown_task_returns_not_found() {
        let temp_dir = TempDir::new().unwrap();
        let mut store = test_store(&temp_dir);

        let error = store.block_task("task-999", "reason").unwrap_err();
        assert!(matches!(error, StoreError::NotFound));
    }

    #[test]
    fn unblock_task_restores_phase_and_clears_metadata() {
        let temp_dir = TempDir::new().unwrap();
        let mut store = test_store(&temp_dir);

        store.add_task("First", "", vec![], None).unwrap();
        store.next_phase("task-001").unwrap();
        store.block_task("task-001", "waiting").unwrap();
        store.unblock_task("task-001").unwrap();
        let task = store.get_task("task-001").unwrap();

        assert_eq!(task.state, "enriching");
        assert!(task.blocked_at_phase.is_none());
        assert!(task.blocked_reason.is_none());
        let last = task.history.last().unwrap();
        assert_eq!(last.from.as_deref(), Some("blocked"));
        assert_eq!(last.to, "enriching");
        assert_eq!(last.reason, "unblocked");
    }

    #[test]
    fn unblock_non_blocked_task_returns_not_blocked() {
        let temp_dir = TempDir::new().unwrap();
        let mut store = test_store(&temp_dir);

        store.add_task("First", "", vec![], None).unwrap();

        let error = store.unblock_task("task-001").unwrap_err();
        assert!(matches!(error, StoreError::NotBlocked));
    }

    #[test]
    fn unblock_unknown_task_returns_not_found() {
        let temp_dir = TempDir::new().unwrap();
        let mut store = test_store(&temp_dir);

        let error = store.unblock_task("task-999").unwrap_err();
        assert!(matches!(error, StoreError::NotFound));
    }

    #[test]
    fn remove_task_removes_pending_task_and_saves() {
        let temp_dir = TempDir::new().unwrap();
        let mut store = test_store(&temp_dir);

        store.add_task("First", "", vec![], None).unwrap();
        store.add_task("Second", "", vec![], None).unwrap();

        store.remove_task("task-001").unwrap();

        assert!(store.get_task("task-001").is_none());
        assert_eq!(store.all_tasks().len(), 1);
        assert_eq!(store.all_tasks()[0].id, "task-002");

        let reloaded = test_store(&temp_dir);
        assert!(reloaded.get_task("task-001").is_none());
        assert_eq!(reloaded.all_tasks().len(), 1);
        assert_eq!(reloaded.all_tasks()[0].id, "task-002");
    }

    #[test]
    fn remove_unknown_task_returns_not_found() {
        let temp_dir = TempDir::new().unwrap();
        let mut store = test_store(&temp_dir);

        let error = store.remove_task("task-999").unwrap_err();

        assert!(matches!(error, StoreError::NotFound));
    }

    #[test]
    fn remove_task_rejects_non_pending_states() {
        fn assert_rejected_after<F>(make_non_pending: F, expected_state: &str)
        where
            F: FnOnce(&mut TaskStore),
        {
            let temp_dir = TempDir::new().unwrap();
            let mut store = test_store(&temp_dir);
            store.add_task("First", "", vec![], None).unwrap();
            make_non_pending(&mut store);
            let before = store.get_task("task-001").unwrap().clone();

            let error = store.remove_task("task-001").unwrap_err();

            assert!(matches!(
                error,
                StoreError::NotInPendingPhase { ref id } if id == "task-001"
            ));
            assert_eq!(store.get_task("task-001").unwrap(), &before);
            assert_eq!(store.get_task("task-001").unwrap().state, expected_state);
        }

        assert_rejected_after(
            |store| {
                store.next_phase("task-001").unwrap();
            },
            "enriching",
        );
        assert_rejected_after(
            |store| {
                store.block_task("task-001", "waiting").unwrap();
            },
            "blocked",
        );
        assert_rejected_after(
            |store| {
                store.fail_task("task-001", "failed").unwrap();
            },
            "failed",
        );
        assert_rejected_after(
            |store| {
                store.next_phase("task-001").unwrap();
                store.next_phase("task-001").unwrap();
            },
            "done",
        );
    }

    #[test]
    fn remove_task_rejects_tasks_with_dependents() {
        let temp_dir = TempDir::new().unwrap();
        let mut store = test_store(&temp_dir);

        store.add_task("First", "", vec![], None).unwrap();
        store
            .add_task("Second", "", vec!["task-001".to_owned()], None)
            .unwrap();

        let error = store.remove_task("task-001").unwrap_err();

        assert!(matches!(
            error,
            StoreError::DependedOnBy {
                ref id,
                ref dependent_id,
            } if id == "task-001" && dependent_id == "task-002"
        ));
        assert!(store.get_task("task-001").is_some());
        assert!(store.get_task("task-002").is_some());
        assert_eq!(store.all_tasks().len(), 2);
    }

    #[test]
    fn atomic_write_leaves_valid_json() {
        let temp_dir = TempDir::new().unwrap();
        let mut store = test_store(&temp_dir);

        store.add_task("First", "", vec![], None).unwrap();

        let contents = fs::read_to_string(temp_dir.path().join("tasks.json")).unwrap();
        let value: Value = serde_json::from_str(&contents).unwrap();
        assert!(value.get("tasks").is_some());
    }

    #[test]
    fn add_task_with_phase_override_uses_specified_phase() {
        let temp_dir = TempDir::new().unwrap();
        let mut store = test_store(&temp_dir);

        let task = store
            .add_task("Backfilled", "", vec![], Some("enriching"))
            .unwrap();

        assert_eq!(task.state, "enriching");
        assert_eq!(task.history.len(), 1);
        assert_eq!(task.history[0].from, None);
        assert_eq!(task.history[0].to, "enriching");
        assert_eq!(task.history[0].reason, "task created");
    }

    #[test]
    fn add_task_with_phase_override_to_terminal_phase() {
        let temp_dir = TempDir::new().unwrap();
        let mut store = test_store(&temp_dir);

        let task = store
            .add_task("Already done", "", vec![], Some("done"))
            .unwrap();

        assert_eq!(task.state, "done");
        assert_eq!(task.history[0].to, "done");
    }

    #[test]
    fn add_task_with_unknown_phase_override_returns_error() {
        let temp_dir = TempDir::new().unwrap();
        let mut store = test_store(&temp_dir);

        let error = store
            .add_task("Bad phase", "", vec![], Some("nonexistent"))
            .unwrap_err();

        assert!(matches!(
            error,
            StoreError::UnknownPhase { ref phase } if phase == "nonexistent"
        ));
        assert_eq!(error.to_string(), "unknown phase: nonexistent");
        assert!(!temp_dir.path().join("tasks.json").exists());
    }

    #[test]
    fn add_after_remove_does_not_reuse_id() {
        let temp_dir = TempDir::new().unwrap();
        let mut store = test_store(&temp_dir);

        store.add_task("First", "", vec![], None).unwrap();
        store.add_task("Second", "", vec![], None).unwrap();
        store.add_task("Third", "", vec![], None).unwrap();
        store.remove_task("task-002").unwrap();

        let task = store.add_task("Fourth", "", vec![], None).unwrap();
        assert_eq!(task.id, "task-004");
    }
}
