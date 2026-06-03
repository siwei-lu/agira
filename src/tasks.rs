#![allow(dead_code)]

use std::{
    collections::BTreeMap,
    fs, io,
    path::{Path, PathBuf},
};

use chrono::Utc;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::config::Config;

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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prd_module_id: Option<String>,
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
        let state_machine = config.state_machine.clone();
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
        prd_module_id: Option<String>,
        dependencies: Vec<String>,
    ) -> Result<Task, StoreError> {
        let id = format!("task-{:03}", self.tasks_file.tasks.len() + 1);

        for dependency_id in &dependencies {
            if self.get_task(dependency_id).is_none() {
                return Err(StoreError::DependencyBlocked {
                    task_id: id,
                    blocking_id: dependency_id.clone(),
                });
            }
        }

        let first_phase =
            self.state_machine
                .first()
                .cloned()
                .ok_or_else(|| StoreError::InvalidTransition {
                    from: String::new(),
                    to: String::new(),
                })?;
        let created_at = Utc::now().to_rfc3339();
        let history_timestamp = Utc::now().to_rfc3339();
        let task = Task {
            id,
            title: title.to_owned(),
            description: description.to_owned(),
            state: first_phase.clone(),
            prd_module_id,
            dependencies,
            retry_count: 0,
            max_retries: self.max_retries,
            phases: BTreeMap::new(),
            history: vec![HistoryEntry {
                from: None,
                to: first_phase,
                timestamp: history_timestamp,
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
}

#[cfg(test)]
mod tests {
    use std::{collections::BTreeMap, fs};

    use chrono::DateTime;
    use serde_json::Value;
    use tempfile::TempDir;

    use super::*;
    use crate::config::VerificationConfig;

    fn test_config() -> Config {
        Config {
            stack: "rust".to_owned(),
            state_machine: vec![
                "pending".to_owned(),
                "enriching".to_owned(),
                "done".to_owned(),
            ],
            models: BTreeMap::new(),
            verification: VerificationConfig { commands: vec![] },
            acceptance_testing: "cli".to_owned(),
            max_retries: 3,
            prd_path: None,
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
            .add_task("First task", "Description", None, vec![])
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
    fn sequential_ids() {
        let temp_dir = TempDir::new().unwrap();
        let mut store = test_store(&temp_dir);

        let first = store.add_task("First", "", None, vec![]).unwrap();
        let second = store.add_task("Second", "", None, vec![]).unwrap();
        let third = store.add_task("Third", "", None, vec![]).unwrap();

        assert_eq!(first.id, "task-001");
        assert_eq!(second.id, "task-002");
        assert_eq!(third.id, "task-003");
    }

    #[test]
    fn next_phase_advances_state() {
        let temp_dir = TempDir::new().unwrap();
        let mut store = test_store(&temp_dir);

        store.add_task("First", "", None, vec![]).unwrap();
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

        store.add_task("First", "", None, vec![]).unwrap();
        store.next_phase("task-001").unwrap();
        store.next_phase("task-001").unwrap();

        let error = store.next_phase("task-001").unwrap_err();
        assert!(matches!(error, StoreError::AlreadyTerminal));
    }

    #[test]
    fn next_phase_on_failed_returns_already_terminal() {
        let temp_dir = TempDir::new().unwrap();
        let mut store = test_store(&temp_dir);

        store.add_task("First", "", None, vec![]).unwrap();
        store.fail_task("task-001", "failed").unwrap();

        let error = store.next_phase("task-001").unwrap_err();
        assert!(matches!(error, StoreError::AlreadyTerminal));
    }

    #[test]
    fn fail_task_sets_failed() {
        let temp_dir = TempDir::new().unwrap();
        let mut store = test_store(&temp_dir);

        store.add_task("First", "", None, vec![]).unwrap();
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

        store.add_task("First", "", None, vec![]).unwrap();
        store.next_phase("task-001").unwrap();
        store.next_phase("task-001").unwrap();

        let error = store.fail_task("task-001", "too late").unwrap_err();
        assert!(matches!(error, StoreError::AlreadyTerminal));
    }

    #[test]
    fn dependency_blocks_advance() {
        let temp_dir = TempDir::new().unwrap();
        let mut store = test_store(&temp_dir);

        store.add_task("Dependency", "", None, vec![]).unwrap();
        store
            .add_task("Dependent", "", None, vec!["task-001".to_owned()])
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

        store.add_task("Dependency", "", None, vec![]).unwrap();
        store
            .add_task("Dependent", "", None, vec!["task-001".to_owned()])
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

        store.add_task("Dependency", "", None, vec![]).unwrap();
        store
            .add_task("Dependent", "", None, vec!["task-001".to_owned()])
            .unwrap();
        store.fail_task("task-001", "dependency failed").unwrap();

        let error = store.next_phase("task-002").unwrap_err();
        assert!(matches!(error, StoreError::DependencyBlocked { .. }));
    }

    #[test]
    fn atomic_write_leaves_valid_json() {
        let temp_dir = TempDir::new().unwrap();
        let mut store = test_store(&temp_dir);

        store.add_task("First", "", None, vec![]).unwrap();

        let contents = fs::read_to_string(temp_dir.path().join("tasks.json")).unwrap();
        let value: Value = serde_json::from_str(&contents).unwrap();
        assert!(value.get("tasks").is_some());
    }
}
