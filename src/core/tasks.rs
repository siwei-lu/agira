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
    _state_machine: serde_json::Value,
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
        let mut tasks_file = Self::load_from_file(&tasks_path, config)?;

        let mut migrated = false;
        for task in &mut tasks_file.tasks {
            if task.workflow.is_empty() {
                task.workflow = config.default_workflow.clone();
                migrated = true;
            }
        }

        let mut store = Self {
            tasks_path,
            tasks_file,
            config: config.clone(),
            max_retries: config.max_retries,
        };

        if migrated {
            let current = store.tasks_file.clone();
            store.save(current)?;
        }

        Ok(store)
    }

    fn load_from_file(path: &Path, config: &Config) -> Result<TasksFile, StoreError> {
        match fs::read_to_string(path) {
            Ok(contents) => {
                let wire: TasksFileWire =
                    serde_json::from_str(&contents).map_err(StoreError::Deserialize)?;
                Ok(TasksFile {
                    tasks: wire
                        .tasks
                        .into_iter()
                        .map(|task| Task {
                            id: task.id,
                            title: task.title,
                            description: task.description,
                            state: task.state,
                            blocked_at_phase: task.blocked_at_phase,
                            blocked_reason: task.blocked_reason,
                            dependencies: task.dependencies,
                            retry_count: task.retry_count,
                            max_retries: task.max_retries,
                            phases: task.phases,
                            history: task.history,
                            created_at: task.created_at,
                            workflow: task
                                .workflow
                                .filter(|name| !name.is_empty())
                                .unwrap_or_else(|| config.default_workflow.clone()),
                            locked_at: task.locked_at,
                        })
                        .collect(),
                })
            }
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

        if !sequence.contains(&previous_state) {
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
        task.locked_at = None;
        task.history.push(HistoryEntry {
            from: Some(previous_state),
            to: first_phase,
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
