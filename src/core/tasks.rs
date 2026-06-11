use std::{
    collections::BTreeMap,
    fs, io,
    path::{Path, PathBuf},
};

use chrono::Utc;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::core::config::{Config, TERMINAL_PHASE_NAME};

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
    pub workflow: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub locked_at: Option<String>,
}

fn default_max_retries() -> u32 {
    3
}

pub fn all_tasks_done(tasks: &[Task]) -> bool {
    !tasks.is_empty() && tasks.iter().all(is_terminal_task)
}

fn is_terminal_task(task: &Task) -> bool {
    task.state == TERMINAL_PHASE_NAME || task.state == "failed"
}

#[derive(Serialize, Debug, Clone, PartialEq, Eq)]
pub struct TasksFile {
    pub tasks: Vec<Task>,
}

#[derive(Deserialize)]
struct TasksFileWire {
    tasks: Vec<TaskWire>,
}

#[derive(Deserialize)]
struct TaskWire {
    id: String,
    title: String,
    description: String,
    state: String,
    #[serde(default)]
    blocked_at_phase: Option<String>,
    #[serde(default)]
    blocked_reason: Option<String>,
    dependencies: Vec<String>,
    retry_count: u32,
    #[serde(default = "default_max_retries")]
    max_retries: u32,
    phases: BTreeMap<String, TaskPhase>,
    history: Vec<HistoryEntry>,
    created_at: String,
    #[serde(default)]
    workflow: Option<String>,
    #[serde(default)]
    locked_at: Option<String>,
    #[serde(default, rename = "state_machine")]
    state_machine_snapshot: Option<serde_json::Value>,
}

struct LoadedTasksFile {
    tasks_file: TasksFile,
    migrated: bool,
}

impl TaskWire {
    fn needs_migration(&self) -> bool {
        self.workflow.as_deref().is_none_or(str::is_empty) || self.state_machine_snapshot.is_some()
    }

    fn into_task(self, config: &Config) -> Task {
        Task {
            id: self.id,
            title: self.title,
            description: self.description,
            state: self.state,
            blocked_at_phase: self.blocked_at_phase,
            blocked_reason: self.blocked_reason,
            dependencies: self.dependencies,
            retry_count: self.retry_count,
            max_retries: self.max_retries,
            phases: self.phases,
            history: self.history,
            created_at: self.created_at,
            workflow: self
                .workflow
                .filter(|name| !name.is_empty())
                .unwrap_or_else(|| config.default_workflow.clone()),
            locked_at: self.locked_at,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::config::PhaseDef;

    fn retry_test_store() -> (tempfile::TempDir, TaskStore) {
        let dir = tempfile::TempDir::new().expect("create temp dir");
        let state_dir = dir.path().join(".agira");
        fs::create_dir_all(&state_dir).expect("create state dir");
        let config = Config::new_single_workflow(
            "test",
            vec![
                ("enriching".to_owned(), PhaseDef::default()),
                ("implementing".to_owned(), PhaseDef::default()),
                ("reviewing".to_owned(), PhaseDef::default()),
            ],
            3,
        );
        let store = TaskStore::new(&state_dir, &config).expect("create store");
        (dir, store)
    }

    #[test]
    fn retry_from_reviewing_reenters_at_producing_phase() {
        let (_dir, mut store) = retry_test_store();
        let task = store
            .add_task(
                "retry target",
                "description",
                Vec::new(),
                Some("reviewing"),
                "default".to_owned(),
            )
            .expect("add task");
        store.lock_task(&task.id).expect("lock task");

        assert_eq!(
            store
                .retry_task(&task.id, "needs changes")
                .expect("retry task"),
            (1, 3)
        );

        let task = store.get_task(&task.id).expect("get task");
        assert_eq!(task.state, "implementing");
        assert_eq!(task.retry_count, 1);
        assert_eq!(task.locked_at, None);
        let history = task.history.last().expect("retry history");
        assert_eq!(history.from.as_deref(), Some("reviewing"));
        assert_eq!(history.to, "implementing");
        assert_eq!(history.reason, "retry 1/3: needs changes");
    }

    #[test]
    fn retry_first_attempt_uses_previous_phase() {
        let (_dir, mut store) = retry_test_store();
        let task = store
            .add_task(
                "first retry target",
                "description",
                Vec::new(),
                Some("reviewing"),
                "default".to_owned(),
            )
            .expect("add task");

        store
            .retry_task(&task.id, "wrong output")
            .expect("retry task");

        let task = store.get_task(&task.id).expect("get task");
        assert_eq!(task.state, "implementing");
        assert_eq!(task.retry_count, 1);
        let history = task.history.last().expect("retry history");
        assert_eq!(history.reason, "retry 1/3: wrong output");
    }

    #[test]
    fn retry_second_attempt_escalates_to_first_real_phase() {
        let (_dir, mut store) = retry_test_store();
        let task = store
            .add_task(
                "second retry target",
                "description",
                Vec::new(),
                Some("reviewing"),
                "default".to_owned(),
            )
            .expect("add task");

        store
            .retry_task(&task.id, "first retry")
            .expect("retry task");
        store.next_phase(&task.id).expect("return to reviewing");
        store
            .retry_task(&task.id, "second retry")
            .expect("retry task");

        let task = store.get_task(&task.id).expect("get task");
        assert_eq!(task.state, "enriching");
        assert_eq!(task.retry_count, 2);
        let history = task.history.last().expect("retry history");
        assert_eq!(history.from.as_deref(), Some("reviewing"));
        assert_eq!(history.to, "enriching");
        assert_eq!(history.reason, "retry 2/3: second retry");
    }

    #[test]
    fn retry_from_first_real_phase_clamps_to_itself() {
        let (_dir, mut store) = retry_test_store();
        let task = store
            .add_task(
                "first real phase",
                "description",
                Vec::new(),
                Some("enriching"),
                "default".to_owned(),
            )
            .expect("add task");

        store.retry_task(&task.id, "try again").expect("retry task");

        let task = store.get_task(&task.id).expect("get task");
        assert_eq!(task.state, "enriching");
        assert_eq!(task.retry_count, 1);
        let history = task.history.last().expect("retry history");
        assert_eq!(history.from.as_deref(), Some("enriching"));
        assert_eq!(history.to, "enriching");
        assert_eq!(history.reason, "retry 1/3: try again");
    }

    // ---------------------------------------------------------------------------
    // advance_with_artifact: single-write atomic advance
    // ---------------------------------------------------------------------------

    fn advance_test_store() -> (tempfile::TempDir, TaskStore) {
        let dir = tempfile::TempDir::new().expect("create temp dir");
        let state_dir = dir.path().join(".agira");
        fs::create_dir_all(&state_dir).expect("create state dir");
        let config = Config::new_single_workflow(
            "test",
            vec![
                ("enriching".to_owned(), PhaseDef::default()),
                ("implementing".to_owned(), PhaseDef::default()),
                ("reviewing".to_owned(), PhaseDef::default()),
            ],
            3,
        );
        let store = TaskStore::new(&state_dir, &config).expect("create store");
        (dir, store)
    }

    #[test]
    fn advance_with_artifact_writes_once() {
        let (dir, mut store) = advance_test_store();
        let tasks_path = dir.path().join(".agira").join("tasks.json");

        let task = store
            .add_task(
                "atomic advance",
                "description",
                Vec::new(),
                Some("enriching"),
                "default".to_owned(),
            )
            .expect("add task");

        // Lock the task to verify locked_at is cleared
        store.lock_task(&task.id).expect("lock task");
        assert!(
            store.get_task(&task.id).unwrap().locked_at.is_some(),
            "locked_at must be set before advance"
        );

        let completed_at = "2026-06-11T10:00:00Z".to_owned();
        let from_phase = store
            .advance_with_artifact(&task.id, "my artifact", completed_at.clone())
            .expect("advance_with_artifact");

        // (a) from-phase returned correctly
        assert_eq!(from_phase, "enriching");

        // (b) artifact is stored under from-phase
        let updated = store.get_task(&task.id).expect("task exists");
        let phase_entry = updated
            .phases
            .get("enriching")
            .expect("artifact recorded under enriching");
        assert_eq!(phase_entry.artifact, "my artifact");
        assert_eq!(phase_entry.completed_at, "2026-06-11T10:00:00Z");

        // (c) state is advanced to next phase
        assert_eq!(updated.state, "implementing");

        // (d) locked_at is cleared
        assert_eq!(updated.locked_at, None, "locked_at must be cleared");

        // (e) last history entry has correct from/to/reason
        let last_history = updated.history.last().expect("history entry");
        assert_eq!(last_history.from.as_deref(), Some("enriching"));
        assert_eq!(last_history.to, "implementing");
        assert_eq!(last_history.reason, "advanced to next phase");

        // (f) single write: reload from disk and verify consistent state
        // (artifact + advanced state both present — no partial write possible)
        let config = Config::new_single_workflow(
            "test",
            vec![
                ("enriching".to_owned(), PhaseDef::default()),
                ("implementing".to_owned(), PhaseDef::default()),
                ("reviewing".to_owned(), PhaseDef::default()),
            ],
            3,
        );
        let fresh_store = TaskStore::new(dir.path().join(".agira"), &config).expect("reload store");
        let disk_task = fresh_store.get_task(&task.id).expect("task on disk");
        assert_eq!(
            disk_task.state, "implementing",
            "disk state must be advanced"
        );
        assert!(
            disk_task.phases.contains_key("enriching"),
            "disk artifact must be present"
        );
        assert_eq!(disk_task.locked_at, None, "disk locked_at must be None");

        // Confirm tasks.json file exists (sanity)
        assert!(tasks_path.exists(), "tasks.json must exist after advance");
    }

    #[test]
    fn advance_with_artifact_not_found() {
        let (_dir, mut store) = advance_test_store();
        let result =
            store.advance_with_artifact("task-999", "artifact", "2026-06-11T10:00:00Z".to_owned());
        assert!(
            matches!(result, Err(StoreError::NotFound)),
            "expected NotFound, got: {result:?}"
        );
    }

    #[test]
    fn advance_with_artifact_already_terminal() {
        let (_dir, mut store) = advance_test_store();
        let task = store
            .add_task(
                "terminal",
                "desc",
                Vec::new(),
                Some("done"),
                "default".to_owned(),
            )
            .expect("add task");
        let result =
            store.advance_with_artifact(&task.id, "artifact", "2026-06-11T10:00:00Z".to_owned());
        assert!(
            matches!(result, Err(StoreError::AlreadyTerminal)),
            "expected AlreadyTerminal, got: {result:?}"
        );
    }

    #[test]
    fn advance_with_artifact_dependency_blocked() {
        let dir = tempfile::TempDir::new().expect("create temp dir");
        let state_dir = dir.path().join(".agira");
        fs::create_dir_all(&state_dir).expect("create state dir");
        let config = Config::new_single_workflow(
            "test",
            vec![
                ("enriching".to_owned(), PhaseDef::default()),
                ("implementing".to_owned(), PhaseDef::default()),
            ],
            3,
        );
        let mut store = TaskStore::new(&state_dir, &config).expect("create store");

        // dep is not terminal (still in enriching)
        let dep = store
            .add_task(
                "dep",
                "desc",
                Vec::new(),
                Some("enriching"),
                "default".to_owned(),
            )
            .expect("add dep");
        let task = store
            .add_task(
                "dependent",
                "desc",
                vec![dep.id.clone()],
                Some("enriching"),
                "default".to_owned(),
            )
            .expect("add task");

        let result =
            store.advance_with_artifact(&task.id, "artifact", "2026-06-11T10:00:00Z".to_owned());
        assert!(
            matches!(result, Err(StoreError::DependencyBlocked { .. })),
            "expected DependencyBlocked, got: {result:?}"
        );
    }
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

    #[error("unknown workflow: {workflow}")]
    UnknownWorkflow { workflow: String },

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
    config: Config,
    max_retries: u32,
}

impl TaskStore {
    pub fn new(state_dir: impl AsRef<Path>, config: &Config) -> Result<Self, StoreError> {
        let tasks_path = state_dir.as_ref().join("tasks.json");
        let loaded = Self::load_from_file(&tasks_path, config)?;

        let mut store = Self {
            tasks_path,
            tasks_file: loaded.tasks_file,
            config: config.clone(),
            max_retries: config.max_retries,
        };

        if loaded.migrated {
            let current = store.tasks_file.clone();
            store.save(current)?;
        }

        Ok(store)
    }

    fn load_from_file(path: &Path, config: &Config) -> Result<LoadedTasksFile, StoreError> {
        match fs::read_to_string(path) {
            Ok(contents) => {
                let wire: TasksFileWire =
                    serde_json::from_str(&contents).map_err(StoreError::Deserialize)?;
                let mut migrated = false;
                let tasks = wire
                    .tasks
                    .into_iter()
                    .map(|task| {
                        migrated |= task.needs_migration();
                        task.into_task(config)
                    })
                    .collect();
                Ok(LoadedTasksFile {
                    tasks_file: TasksFile { tasks },
                    migrated,
                })
            }
            Err(source) if source.kind() == io::ErrorKind::NotFound => Ok(LoadedTasksFile {
                tasks_file: TasksFile { tasks: vec![] },
                migrated: false,
            }),
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
        workflow: String,
    ) -> Result<Task, StoreError> {
        let sequence = self.sequence_for_workflow(&workflow)?;
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
            if !sequence.iter().any(|candidate| candidate == phase) {
                return Err(StoreError::UnknownPhase {
                    phase: phase.to_owned(),
                });
            }
            phase.to_owned()
        } else {
            sequence
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
            workflow,
            locked_at: None,
        };

        let mut tasks_file = self.tasks_file.clone();
        tasks_file.tasks.push(task.clone());
        self.save(tasks_file)?;

        Ok(task)
    }

    pub fn get_task(&self, id: &str) -> Option<&Task> {
        self.tasks_file.tasks.iter().find(|task| task.id == id)
    }

    #[allow(dead_code)]
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

    #[allow(dead_code)]
    pub fn next_phase(&mut self, id: &str) -> Result<(), StoreError> {
        let mut tasks_file = self.tasks_file.clone();
        let task_index = self.task_index(&tasks_file, id)?;
        let previous_state = tasks_file.tasks[task_index].state.clone();
        let terminal_phase = self.terminal_for(&tasks_file.tasks[task_index])?;

        if previous_state == "failed" || previous_state == terminal_phase {
            return Err(StoreError::AlreadyTerminal);
        }

        let sequence = self.sequence_for(&tasks_file.tasks[task_index])?;
        let current_index = sequence
            .iter()
            .position(|state| state == &previous_state)
            .ok_or_else(|| StoreError::InvalidTransition {
                from: previous_state.clone(),
                to: String::new(),
            })?;
        let target_index = current_index + 1;

        if target_index >= sequence.len() {
            return Err(StoreError::AlreadyTerminal);
        }

        if target_index > 0 {
            for dependency_id in &tasks_file.tasks[task_index].dependencies {
                match tasks_file
                    .tasks
                    .iter()
                    .find(|task| task.id == *dependency_id)
                {
                    Some(dependency) if dependency.state == self.terminal_for(dependency)? => {}
                    _ => {
                        return Err(StoreError::DependencyBlocked {
                            task_id: id.to_owned(),
                            blocking_id: dependency_id.clone(),
                        });
                    }
                }
            }
        }

        let target_state = sequence[target_index].clone();
        let task = &mut tasks_file.tasks[task_index];
        task.state = target_state.clone();
        task.locked_at = None;
        task.history.push(HistoryEntry {
            from: Some(previous_state),
            to: target_state,
            timestamp: Utc::now().to_rfc3339(),
            reason: "advanced to next phase".to_owned(),
        });

        self.save(tasks_file)
    }

    /// Record the current phase's artifact, advance the task to the next phase,
    /// clear `locked_at`, and append a history entry — all in a single `save()`.
    ///
    /// Returns the name of the phase that was completed (the from-phase).
    pub fn advance_with_artifact(
        &mut self,
        id: &str,
        artifact: &str,
        completed_at: String,
    ) -> Result<String, StoreError> {
        let mut tasks_file = self.tasks_file.clone();
        let task_index = self.task_index(&tasks_file, id)?;
        let from_phase = tasks_file.tasks[task_index].state.clone();
        let terminal_phase = self.terminal_for(&tasks_file.tasks[task_index])?;

        if from_phase == "failed" || from_phase == terminal_phase {
            return Err(StoreError::AlreadyTerminal);
        }

        let sequence = self.sequence_for(&tasks_file.tasks[task_index])?;
        let current_index = sequence
            .iter()
            .position(|s| s == &from_phase)
            .ok_or_else(|| StoreError::InvalidTransition {
                from: from_phase.clone(),
                to: String::new(),
            })?;
        let target_index = current_index + 1;

        if target_index >= sequence.len() {
            return Err(StoreError::AlreadyTerminal);
        }

        // Validate dependencies (same logic as next_phase)
        if target_index > 0 {
            for dependency_id in &tasks_file.tasks[task_index].dependencies {
                match tasks_file
                    .tasks
                    .iter()
                    .find(|task| task.id == *dependency_id)
                {
                    Some(dependency) if dependency.state == self.terminal_for(dependency)? => {}
                    _ => {
                        return Err(StoreError::DependencyBlocked {
                            task_id: id.to_owned(),
                            blocking_id: dependency_id.clone(),
                        });
                    }
                }
            }
        }

        let target_phase = sequence[target_index].clone();

        // Apply all mutations to the single cloned tasks_file
        let task = &mut tasks_file.tasks[task_index];
        task.phases.insert(
            from_phase.clone(),
            TaskPhase {
                artifact: artifact.to_owned(),
                completed_at,
            },
        );
        task.state = target_phase.clone();
        task.locked_at = None;
        task.history.push(HistoryEntry {
            from: Some(from_phase.clone()),
            to: target_phase,
            timestamp: Utc::now().to_rfc3339(),
            reason: "advanced to next phase".to_owned(),
        });

        // Single save — no intermediate writes
        self.save(tasks_file)?;
        Ok(from_phase)
    }

    pub fn fail_task(&mut self, id: &str, reason: &str) -> Result<(), StoreError> {
        self.transition_to_terminal_like(id, "failed", reason)
    }

    pub fn block_task(&mut self, id: &str, reason: &str) -> Result<(), StoreError> {
        let mut tasks_file = self.tasks_file.clone();
        let task_index = self.task_index(&tasks_file, id)?;
        let previous_state = tasks_file.tasks[task_index].state.clone();
        let terminal_phase = self.terminal_for(&tasks_file.tasks[task_index])?;

        if previous_state == "blocked" {
            return Err(StoreError::AlreadyBlocked);
        }

        if previous_state == "failed" || previous_state == terminal_phase {
            return Err(StoreError::AlreadyTerminal);
        }

        self.ensure_state_in_workflow(&tasks_file.tasks[task_index], &previous_state, "blocked")?;

        let task = &mut tasks_file.tasks[task_index];
        task.state = "blocked".to_owned();
        task.blocked_at_phase = Some(previous_state.clone());
        task.blocked_reason = Some(reason.to_owned());
        task.locked_at = None;
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
        let task_index = self.task_index(&tasks_file, id)?;

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
        let terminal_phase = self.terminal_for(target_task)?;
        if target_task.state == terminal_phase || target_task.state == "failed" {
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
        let sequence = self.sequence_for(task)?;
        let pending_phase = sequence
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
        let task_index = self.task_index(&tasks_file, id)?;
        let previous_state = tasks_file.tasks[task_index].state.clone();
        let terminal_phase = self.terminal_for(&tasks_file.tasks[task_index])?;

        if previous_state == "failed" || previous_state == terminal_phase {
            return Err(StoreError::AlreadyTerminal);
        }

        let sequence = self.sequence_for(&tasks_file.tasks[task_index])?;
        let first_phase =
            sequence
                .first()
                .cloned()
                .ok_or_else(|| StoreError::InvalidTransition {
                    from: previous_state.clone(),
                    to: String::new(),
                })?;
        let previous_index = sequence
            .iter()
            .position(|phase| phase == &previous_state)
            .ok_or(StoreError::InvalidTransition {
                from: previous_state.clone(),
                to: first_phase,
            })?;
        let new_retry_count = tasks_file.tasks[task_index].retry_count + 1;
        let target_index = if new_retry_count == 1 {
            previous_index.saturating_sub(1).max(1)
        } else {
            1
        };
        let target_phase =
            sequence
                .get(target_index)
                .cloned()
                .ok_or_else(|| StoreError::InvalidTransition {
                    from: previous_state.clone(),
                    to: String::new(),
                })?;

        let task = &mut tasks_file.tasks[task_index];
        let max_retries = task.max_retries;
        task.retry_count = new_retry_count;
        task.state = target_phase.clone();
        task.locked_at = None;
        task.history.push(HistoryEntry {
            from: Some(previous_state),
            to: target_phase,
            timestamp: Utc::now().to_rfc3339(),
            reason: format!("retry {new_retry_count}/{max_retries}: {reason}"),
        });

        self.save(tasks_file)?;

        Ok((new_retry_count, max_retries))
    }

    pub fn lock_task(&mut self, id: &str) -> Result<(), StoreError> {
        let mut tasks_file = self.tasks_file.clone();
        let task = tasks_file
            .tasks
            .iter_mut()
            .find(|t| t.id == id)
            .ok_or(StoreError::NotFound)?;
        task.locked_at = Some(Utc::now().to_rfc3339());
        self.save(tasks_file)
    }

    pub fn unlock_task(&mut self, id: &str) -> Result<(), StoreError> {
        let mut tasks_file = self.tasks_file.clone();
        let task = tasks_file
            .tasks
            .iter_mut()
            .find(|t| t.id == id)
            .ok_or(StoreError::NotFound)?;
        task.locked_at = None;
        self.save(tasks_file)
    }

    fn transition_to_terminal_like(
        &mut self,
        id: &str,
        target: &str,
        reason: &str,
    ) -> Result<(), StoreError> {
        let mut tasks_file = self.tasks_file.clone();
        let task_index = self.task_index(&tasks_file, id)?;
        let previous_state = tasks_file.tasks[task_index].state.clone();
        let terminal_phase = self.terminal_for(&tasks_file.tasks[task_index])?;

        if previous_state == "failed" || previous_state == terminal_phase {
            return Err(StoreError::AlreadyTerminal);
        }

        self.ensure_state_in_workflow(&tasks_file.tasks[task_index], &previous_state, target)?;

        let task = &mut tasks_file.tasks[task_index];
        task.state = target.to_owned();
        task.locked_at = None;
        task.history.push(HistoryEntry {
            from: Some(previous_state),
            to: target.to_owned(),
            timestamp: Utc::now().to_rfc3339(),
            reason: reason.to_owned(),
        });

        self.save(tasks_file)
    }

    fn task_index(&self, tasks_file: &TasksFile, id: &str) -> Result<usize, StoreError> {
        tasks_file
            .tasks
            .iter()
            .position(|task| task.id == id)
            .ok_or(StoreError::NotFound)
    }

    fn sequence_for(&self, task: &Task) -> Result<Vec<String>, StoreError> {
        self.sequence_for_workflow(&task.workflow)
    }

    fn sequence_for_workflow(&self, workflow: &str) -> Result<Vec<String>, StoreError> {
        let sequence = self.config.sequence(workflow);
        if sequence.is_empty() {
            return Err(StoreError::UnknownWorkflow {
                workflow: workflow.to_owned(),
            });
        }
        Ok(sequence.to_vec())
    }

    fn terminal_for(&self, task: &Task) -> Result<String, StoreError> {
        self.sequence_for(task)?
            .last()
            .cloned()
            .ok_or_else(|| StoreError::InvalidTransition {
                from: task.state.clone(),
                to: String::new(),
            })
    }

    fn ensure_state_in_workflow(
        &self,
        task: &Task,
        state: &str,
        to: &str,
    ) -> Result<(), StoreError> {
        if self.sequence_for(task)?.iter().any(|phase| phase == state) {
            Ok(())
        } else {
            Err(StoreError::InvalidTransition {
                from: state.to_owned(),
                to: to.to_owned(),
            })
        }
    }
}
