use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque},
    env, fs, io,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{
    commands::add::{AddError, dispatch_task_added_hooks, map_config_error},
    core::{
        config::{Config, ConfigError, load_project_config},
        project::Project,
        tasks::{StoreError, Task, TaskStore},
    },
};

#[derive(Debug, Error)]
pub enum AddBatchError {
    #[error("{message}")]
    User { message: String },

    #[error("failed to read manifest from stdin")]
    StdinRead(#[source] io::Error),

    #[error("failed to read manifest {path}")]
    ManifestRead {
        path: PathBuf,
        #[source]
        source: io::Error,
    },

    #[error("failed to parse manifest: {source}")]
    ManifestParse {
        #[source]
        source: toml::de::Error,
    },

    #[error(transparent)]
    Add(#[from] AddError),

    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

#[derive(Debug, Clone)]
pub struct AddBatchOptions {
    pub file: Option<PathBuf>,
    pub stdin: bool,
    pub dry_run: bool,
    pub json: bool,
    pub workflow: Option<String>,
    pub phase: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    #[serde(default)]
    task: Vec<ManifestTask>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct ManifestTask {
    id: Option<String>,
    title: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    depends_on: Vec<String>,
    phase: Option<String>,
    workflow: Option<String>,
}

#[derive(Debug, Clone)]
struct PlannedTask {
    index: usize,
    real_id: String,
    resolved_dependencies: Vec<String>,
    workflow: String,
    phase: Option<String>,
}

#[derive(Debug)]
struct BatchPlan {
    entries: Vec<ManifestTask>,
    planned: Vec<PlannedTask>,
    alias_to_real_id: BTreeMap<String, String>,
}

#[derive(Debug, Serialize)]
struct BatchOutput<'a> {
    dry_run: bool,
    aliases: &'a BTreeMap<String, String>,
    tasks: Vec<BatchOutputTask>,
}

#[derive(Debug, Serialize)]
struct BatchOutputTask {
    alias: Option<String>,
    id: String,
    title: String,
    depends_on: Vec<String>,
    workflow: String,
    phase: Option<String>,
}

pub fn run_add_batch(project: &Project, options: AddBatchOptions) -> Result<(), AddBatchError> {
    if options.file.is_some() == options.stdin {
        return Err(AddBatchError::User {
            message: "supply exactly one of --file or --stdin".to_owned(),
        });
    }

    let contents = read_manifest(options.file.as_deref(), options.stdin)?;
    let manifest: Manifest =
        toml::from_str(&contents).map_err(|source| AddBatchError::ManifestParse { source })?;

    let config_path = project.state_dir.join("config.json");
    let config =
        load_project_config(&config_path, &project.global_config).map_err(map_config_error)?;
    let mut store = TaskStore::new(&project.state_dir, &config).map_err(AddError::from)?;
    let plan = build_plan(&manifest.task, &store, &config, &options)?;

    if options.dry_run {
        print_plan(&plan, true, options.json)?;
        return Ok(());
    }

    add_batch_flow_with_ensure(
        project,
        &mut store,
        &plan,
        project.global_config.runner.auto_start,
        inside_runner(),
        &project.global_config.runner.runner_type,
        &mut |proj, rt| {
            let mut tmux = crate::commands::runner::ProcessTmux;
            crate::commands::runner::ensure_runner_with_tmux(proj, rt, &mut tmux)
                .map(|_| ())
                .map_err(|error| error.to_string())
        },
    )?;

    print_plan(&plan, false, options.json)?;
    Ok(())
}

fn read_manifest(path: Option<&Path>, use_stdin: bool) -> Result<String, AddBatchError> {
    if use_stdin {
        let mut contents = String::new();
        io::Read::read_to_string(&mut io::stdin(), &mut contents)
            .map_err(AddBatchError::StdinRead)?;
        return Ok(contents);
    }

    let path = path.expect("file/stdin validated");
    fs::read_to_string(path).map_err(|source| AddBatchError::ManifestRead {
        path: path.to_path_buf(),
        source,
    })
}

fn build_plan(
    entries: &[ManifestTask],
    store: &TaskStore,
    config: &Config,
    options: &AddBatchOptions,
) -> Result<BatchPlan, AddBatchError> {
    let mut errors = Vec::new();
    if entries.is_empty() {
        errors.push("manifest must include at least one [[task]] entry".to_owned());
    }

    let mut alias_to_index = HashMap::new();
    let mut duplicate_aliases = BTreeSet::new();
    for (index, entry) in entries.iter().enumerate() {
        if let Some(alias) = entry.id.as_deref() {
            if alias_to_index.insert(alias.to_owned(), index).is_some() {
                duplicate_aliases.insert(alias.to_owned());
            }
        }
    }
    for alias in duplicate_aliases {
        errors.push(format!("duplicate local id alias: {alias}"));
    }

    collect_duplicate_title_errors(entries, store, config, &mut errors);
    collect_workflow_and_phase_errors(entries, config, options, &mut errors);
    collect_unknown_dependency_errors(entries, store, &alias_to_index, &mut errors);

    let topo_order = topo_sort(entries, &alias_to_index, &mut errors);

    if !errors.is_empty() {
        return Err(AddBatchError::User {
            message: errors.join("\n"),
        });
    }

    let start = next_task_number(store);
    let mut real_ids_by_index = HashMap::new();
    for (offset, index) in topo_order.iter().enumerate() {
        real_ids_by_index.insert(*index, format!("task-{:03}", start + offset));
    }

    let mut alias_to_real_id = BTreeMap::new();
    for (alias, index) in &alias_to_index {
        alias_to_real_id.insert(alias.clone(), real_ids_by_index[index].clone());
    }

    let mut planned = Vec::with_capacity(entries.len());
    for index in topo_order {
        let entry = &entries[index];
        let resolved_dependencies = entry
            .depends_on
            .iter()
            .map(|dependency| {
                alias_to_index
                    .get(dependency)
                    .map(|dependency_index| real_ids_by_index[dependency_index].clone())
                    .unwrap_or_else(|| dependency.clone())
            })
            .collect();
        let workflow = entry
            .workflow
            .clone()
            .or_else(|| options.workflow.clone())
            .unwrap_or_else(|| config.default_workflow.clone());
        let phase = entry.phase.clone().or_else(|| options.phase.clone());
        planned.push(PlannedTask {
            index,
            real_id: real_ids_by_index[&index].clone(),
            resolved_dependencies,
            workflow,
            phase,
        });
    }

    Ok(BatchPlan {
        entries: entries.to_vec(),
        planned,
        alias_to_real_id,
    })
}

fn collect_duplicate_title_errors(
    entries: &[ManifestTask],
    store: &TaskStore,
    config: &Config,
    errors: &mut Vec<String>,
) {
    let mut manifest_titles: HashMap<String, String> = HashMap::new();
    let mut duplicate_manifest_titles = BTreeSet::new();
    for entry in entries {
        let lowercase = entry.title.to_lowercase();
        if manifest_titles
            .insert(lowercase.clone(), entry.title.clone())
            .is_some()
        {
            duplicate_manifest_titles.insert(lowercase);
        }
    }
    for title in duplicate_manifest_titles {
        errors.push(format!("duplicate title in manifest: {title}"));
    }

    for entry in entries {
        let title_lowercase = entry.title.to_lowercase();
        if let Some(task) = store.all_tasks().iter().find(|task| {
            Some(task.state.as_str()) != config.terminal_phase()
                && task.title.to_lowercase() == title_lowercase
        }) {
            errors.push(format!(
                "a task with this title already exists: {} \"{}\"",
                task.id, task.title
            ));
        }
    }
}

fn collect_workflow_and_phase_errors(
    entries: &[ManifestTask],
    config: &Config,
    options: &AddBatchOptions,
    errors: &mut Vec<String>,
) {
    let mut unknown_workflows = BTreeSet::new();
    let mut unknown_phases = BTreeSet::new();
    for entry in entries {
        let workflow = entry
            .workflow
            .as_deref()
            .or(options.workflow.as_deref())
            .unwrap_or(&config.default_workflow);
        let sequence = config.sequence(workflow);
        if sequence.is_empty() {
            unknown_workflows.insert(workflow.to_owned());
            if let Some(phase) = entry.phase.as_deref().or(options.phase.as_deref()) {
                if !config.phases.contains_key(phase) {
                    unknown_phases.insert(phase.to_owned());
                }
            }
            continue;
        }
        if let Some(phase) = entry.phase.as_deref().or(options.phase.as_deref()) {
            if !sequence.iter().any(|candidate| candidate == phase) {
                unknown_phases.insert(phase.to_owned());
            }
        }
    }
    for workflow in unknown_workflows {
        errors.push(format!("unknown workflow '{workflow}'"));
    }
    for phase in unknown_phases {
        errors.push(format!("unknown phase: {phase}"));
    }
}

fn collect_unknown_dependency_errors(
    entries: &[ManifestTask],
    store: &TaskStore,
    alias_to_index: &HashMap<String, usize>,
    errors: &mut Vec<String>,
) {
    let mut unknown = BTreeSet::new();
    for entry in entries {
        for dependency in &entry.depends_on {
            if !alias_to_index.contains_key(dependency) && store.get_task(dependency).is_none() {
                unknown.insert(dependency.clone());
            }
        }
    }
    for dependency in unknown {
        errors.push(format!("unknown dependency reference: {dependency}"));
    }
}

fn topo_sort(
    entries: &[ManifestTask],
    alias_to_index: &HashMap<String, usize>,
    errors: &mut Vec<String>,
) -> Vec<usize> {
    let mut indegree = vec![0usize; entries.len()];
    let mut dependents: Vec<Vec<usize>> = vec![Vec::new(); entries.len()];
    for (index, entry) in entries.iter().enumerate() {
        let mut seen_deps = HashSet::new();
        for dependency in &entry.depends_on {
            if let Some(dependency_index) = alias_to_index.get(dependency) {
                if seen_deps.insert(*dependency_index) {
                    indegree[index] += 1;
                    dependents[*dependency_index].push(index);
                }
            }
        }
    }

    let mut queue = VecDeque::new();
    for (index, degree) in indegree.iter().enumerate() {
        if *degree == 0 {
            queue.push_back(index);
        }
    }

    let mut order = Vec::with_capacity(entries.len());
    while let Some(index) = queue.pop_front() {
        order.push(index);
        for dependent in &dependents[index] {
            indegree[*dependent] -= 1;
            if indegree[*dependent] == 0 {
                queue.push_back(*dependent);
            }
        }
    }

    if order.len() != entries.len() {
        errors.push("dependency cycle among manifest tasks".to_owned());
    }
    order
}

fn next_task_number(store: &TaskStore) -> usize {
    store
        .all_tasks()
        .iter()
        .filter_map(|task| {
            task.id
                .strip_prefix("task-")
                .and_then(|suffix| suffix.parse::<usize>().ok())
        })
        .max()
        .unwrap_or(0)
        + 1
}

#[allow(clippy::too_many_arguments)]
fn add_batch_flow_with_ensure(
    project: &Project,
    store: &mut TaskStore,
    plan: &BatchPlan,
    auto_start: bool,
    inside_runner: bool,
    runner_type: &str,
    ensure_runner: &mut dyn FnMut(&Project, &str) -> Result<(), String>,
) -> Result<Vec<Task>, AddBatchError> {
    if auto_start && !inside_runner {
        if let Err(message) = ensure_runner(project, runner_type) {
            eprintln!("warning: ensure-runner failed: {message}");
        }
    }

    let mut tasks = Vec::with_capacity(plan.planned.len());
    for planned in &plan.planned {
        let entry = &plan.entries[planned.index];
        let task = store
            .add_task(
                &entry.title,
                &entry.description,
                planned.resolved_dependencies.clone(),
                planned.phase.as_deref(),
                planned.workflow.clone(),
            )
            .map_err(map_store_error)?;
        dispatch_task_added_hooks(project, &task);
        tasks.push(task);
    }
    Ok(tasks)
}

fn map_store_error(error: StoreError) -> AddBatchError {
    AddBatchError::Add(match error {
        StoreError::DependencyBlocked { blocking_id, .. } => {
            AddError::UnknownDependency { id: blocking_id }
        }
        StoreError::UnknownPhase { phase } => AddError::UnknownPhase { phase },
        other => AddError::StoreError(other),
    })
}

fn print_plan(plan: &BatchPlan, dry_run: bool, json: bool) -> Result<(), AddBatchError> {
    if json {
        let output = BatchOutput {
            dry_run,
            aliases: &plan.alias_to_real_id,
            tasks: output_tasks(plan),
        };
        println!("{}", serde_json::to_string_pretty(&output)?);
        return Ok(());
    }

    for (alias, real_id) in &plan.alias_to_real_id {
        println!("{alias} -> {real_id}");
    }
    for task in output_tasks(plan) {
        if dry_run {
            println!("would add {}: {}", task.id, task.title);
        } else {
            println!("added {}: {}", task.id, task.title);
        }
    }
    Ok(())
}

fn output_tasks(plan: &BatchPlan) -> Vec<BatchOutputTask> {
    plan.planned
        .iter()
        .map(|planned| {
            let entry = &plan.entries[planned.index];
            BatchOutputTask {
                alias: entry.id.clone(),
                id: planned.real_id.clone(),
                title: entry.title.clone(),
                depends_on: planned.resolved_dependencies.clone(),
                workflow: planned.workflow.clone(),
                phase: planned.phase.clone(),
            }
        })
        .collect()
}

fn inside_runner() -> bool {
    env::var("AGIRA_RUNNER_ID")
        .ok()
        .is_some_and(|runner_id| !runner_id.trim().is_empty())
}

impl From<ConfigError> for AddBatchError {
    fn from(error: ConfigError) -> Self {
        AddBatchError::Add(map_config_error(error))
    }
}

#[cfg(test)]
mod tests {
    use std::{fs, path::Path};

    use crate::core::{
        config::{Config, PhaseDef},
        global_config::{GlobalConfig, RunnerConfig},
        hooks::HookConfig,
        project::Project,
        tasks::TaskStore,
    };

    use super::{AddBatchOptions, add_batch_flow_with_ensure, build_plan};

    fn test_config() -> Config {
        Config::new_single_workflow(
            "rust",
            vec![(
                "implementing".to_owned(),
                PhaseDef {
                    model: None,
                    duty: None,
                    gate: None,
                },
            )],
            3,
        )
    }

    fn make_project(dir: &Path, auto_start: bool) -> Project {
        let state_dir = dir.join(".agira").join("test-repo");
        fs::create_dir_all(&state_dir).expect("state dir");
        fs::write(
            state_dir.join("config.json"),
            serde_json::to_string_pretty(&test_config()).expect("serialize config"),
        )
        .expect("write config");
        Project {
            git_root: Path::new("/tmp/test-repo").to_path_buf(),
            slug: "test-repo".to_owned(),
            state_dir,
            global_config: GlobalConfig {
                runner: RunnerConfig {
                    auto_start,
                    runner_type: "claude-tmux".to_owned(),
                    ..RunnerConfig::default()
                },
                ..GlobalConfig::default()
            },
            global_hooks: HookConfig::default(),
            project_hooks: HookConfig::default(),
        }
    }

    #[test]
    fn runner_ensure_runs_at_most_once_for_batch() {
        let dir = tempfile::TempDir::new().expect("temp dir");
        let project = make_project(dir.path(), true);
        let config = test_config();
        let mut store = TaskStore::new(&project.state_dir, &config).expect("task store");
        let manifest: super::Manifest = toml::from_str(
            r#"
[[task]]
id = "a"
title = "a"

[[task]]
id = "b"
title = "b"
depends_on = ["a"]
"#,
        )
        .unwrap();
        let options = AddBatchOptions {
            file: None,
            stdin: true,
            dry_run: false,
            json: false,
            workflow: None,
            phase: None,
        };
        let plan = build_plan(&manifest.task, &store, &config, &options).unwrap();

        let mut calls = 0;
        add_batch_flow_with_ensure(
            &project,
            &mut store,
            &plan,
            true,
            false,
            "claude-tmux",
            &mut |_project, _runner_type| {
                calls += 1;
                Ok(())
            },
        )
        .unwrap();

        assert_eq!(calls, 1);
        assert_eq!(store.all_tasks().len(), 2);
    }
}
