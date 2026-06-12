use std::{
    env, fs, io,
    path::{Path, PathBuf},
};

use serde::Serialize;
use thiserror::Error;

use crate::core::{
    config::{Config, PhaseDef, TERMINAL_PHASE_NAME, load_project_config},
    global_config::GlobalConfig,
    pick::select_next_task,
    tasks::{Task, load_tasks_file_read_only},
};

const PROJECT_WIDTH: usize = 18;
const PATH_WIDTH: usize = 42;
const PROGRESS_WIDTH: usize = 12;
const ACTIVE_WIDTH: usize = 24;
const BLOCKED_WIDTH: usize = 7;
const FAILED_WIDTH: usize = 6;
const INVALID_TASKS_JSON: &str = "invalid tasks.json";

#[derive(Debug, Clone, PartialEq, Eq)]
struct ProjectEntry {
    name: String,
    state_dir: PathBuf,
    source_path: Option<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ProjectListRow {
    slug: String,
    source_path: Option<PathBuf>,
    state_dir: PathBuf,
    total: Option<usize>,
    done: Option<usize>,
    blocked: Option<usize>,
    failed: Option<usize>,
    active: Option<ActiveTask>,
    stale: bool,
    error: Option<String>,
    fully_done: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct ActiveTask {
    id: String,
    phase: String,
}

#[derive(Debug, Serialize)]
struct ProjectListJsonRow {
    slug: String,
    source_path: Option<String>,
    state_dir: String,
    total: Option<usize>,
    done: Option<usize>,
    blocked: Option<usize>,
    failed: Option<usize>,
    active: Option<ActiveTask>,
    stale: bool,
    error: Option<String>,
}

#[derive(Debug, Error)]
pub enum ProjectListError {
    #[error("home directory missing")]
    HomeDirectoryMissing,

    #[error("failed to read {path}")]
    Read {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

pub fn run_project_list(json: bool, stale: bool) -> Result<(), ProjectListError> {
    let agira_root = agira_root_from_home()?;
    run_project_list_from(&agira_root, json, stale)
}

fn agira_root_from_home() -> Result<PathBuf, ProjectListError> {
    let home = env::var_os("HOME")
        .filter(|value| !value.as_os_str().is_empty())
        .ok_or(ProjectListError::HomeDirectoryMissing)?;

    Ok(PathBuf::from(home).join(".agira"))
}

fn run_project_list_from(
    agira_root: &Path,
    json: bool,
    stale: bool,
) -> Result<(), ProjectListError> {
    let output = format_project_list_output(agira_root, json, stale)?;

    if !output.is_empty() {
        println!("{output}");
    }

    Ok(())
}

fn format_project_list_output(
    agira_root: &Path,
    json: bool,
    show_stale: bool,
) -> Result<String, ProjectListError> {
    let projects = list_initialized_projects(agira_root)?;
    let rows = collect_project_rows(&projects);

    if json {
        return Ok(format_project_list_json(&rows));
    }

    Ok(format_project_list_table(&rows, show_stale))
}

fn collect_project_rows(projects: &[ProjectEntry]) -> Vec<ProjectListRow> {
    let global_config = GlobalConfig::default();
    let mut rows: Vec<_> = projects
        .iter()
        .map(|project| collect_project_row(project, &global_config))
        .collect();

    rows.sort_by(|left, right| {
        left.fully_done
            .cmp(&right.fully_done)
            .then_with(|| left.slug.cmp(&right.slug))
    });
    rows
}

fn collect_project_row(project: &ProjectEntry, global_config: &GlobalConfig) -> ProjectListRow {
    let stale = project
        .source_path
        .as_ref()
        .is_some_and(|source_path| !source_path.exists());

    let config = load_project_config(&project.state_dir.join("config.json"), global_config)
        .unwrap_or_else(|_| fallback_project_config());
    let terminal_phase = config
        .terminal_phase()
        .unwrap_or(TERMINAL_PHASE_NAME)
        .to_owned();

    match read_project_tasks(&project.state_dir, &config) {
        Ok(tasks) => {
            let total = tasks.len();
            let done = tasks
                .iter()
                .filter(|task| task.state == terminal_phase)
                .count();
            let blocked = tasks.iter().filter(|task| task.state == "blocked").count();
            let failed = tasks.iter().filter(|task| task.state == "failed").count();
            let active = select_active_task(&tasks, &config, &terminal_phase);
            let fully_done = total == 0 || tasks.iter().all(|task| task.state == terminal_phase);

            ProjectListRow {
                slug: project.name.clone(),
                source_path: project.source_path.clone(),
                state_dir: project.state_dir.clone(),
                total: Some(total),
                done: Some(done),
                blocked: Some(blocked),
                failed: Some(failed),
                active,
                stale,
                error: None,
                fully_done,
            }
        }
        Err(()) => ProjectListRow {
            slug: project.name.clone(),
            source_path: project.source_path.clone(),
            state_dir: project.state_dir.clone(),
            total: None,
            done: None,
            blocked: None,
            failed: None,
            active: None,
            stale,
            error: Some(INVALID_TASKS_JSON.to_owned()),
            fully_done: false,
        },
    }
}

fn read_project_tasks(state_dir: &Path, config: &Config) -> Result<Vec<Task>, ()> {
    let tasks_path = state_dir.join("tasks.json");
    match fs::metadata(&tasks_path) {
        Ok(_) => load_tasks_file_read_only(&tasks_path, config)
            .map(|tasks_file| tasks_file.tasks)
            .map_err(|_| ()),
        Err(source) if source.kind() == io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(_) => Err(()),
    }
}

fn fallback_project_config() -> Config {
    Config::new_single_workflow("unknown", Vec::<(String, PhaseDef)>::new(), 3)
}

fn select_active_task(tasks: &[Task], config: &Config, terminal_phase: &str) -> Option<ActiveTask> {
    let task = select_next_task(tasks, config).or_else(|| {
        tasks
            .iter()
            .find(|task| task.state != terminal_phase && task.state != "failed")
    })?;

    Some(ActiveTask {
        id: task.id.clone(),
        phase: task.state.clone(),
    })
}

fn format_project_list_table(rows: &[ProjectListRow], show_stale: bool) -> String {
    let visible_rows: Vec<_> = rows.iter().filter(|row| show_stale || !row.stale).collect();
    let hidden_stale_count = rows.iter().filter(|row| row.stale).count();
    let mut output = String::new();

    if !visible_rows.is_empty() {
        let mut lines = vec![
            format!(
                "{:<PROJECT_WIDTH$}  {:<PATH_WIDTH$}  {:<PROGRESS_WIDTH$}  {:<ACTIVE_WIDTH$}  {:>BLOCKED_WIDTH$}  {:>FAILED_WIDTH$}",
                "PROJECT", "PATH", "PROGRESS", "ACTIVE", "BLOCKED", "FAILED"
            ),
            format!(
                "{:-<PROJECT_WIDTH$}  {:-<PATH_WIDTH$}  {:-<PROGRESS_WIDTH$}  {:-<ACTIVE_WIDTH$}  {:-<BLOCKED_WIDTH$}  {:-<FAILED_WIDTH$}",
                "", "", "", "", "", ""
            ),
        ];
        lines.extend(
            visible_rows
                .iter()
                .map(|row| format_project_row(row, show_stale)),
        );
        output.push_str(&lines.join("\n"));
    }

    if !show_stale && hidden_stale_count > 0 {
        if !output.is_empty() {
            output.push('\n');
        }
        let noun = if hidden_stale_count == 1 {
            "project"
        } else {
            "projects"
        };
        output.push_str(&format!(
            "{hidden_stale_count} stale {noun} hidden (use --stale to show)"
        ));
    }

    output
}

fn format_project_row(row: &ProjectListRow, show_stale: bool) -> String {
    let path = format_display_path(row, show_stale);

    if row.error.is_some() {
        return format!(
            "{:<PROJECT_WIDTH$}  {:<PATH_WIDTH$}  {}",
            row.slug, path, "error: invalid tasks.json"
        );
    }

    let progress = format!(
        "{}/{}",
        row.done.expect("done count present"),
        row.total.expect("total count present")
    );
    let active = row
        .active
        .as_ref()
        .map(|active| format!("{} ({})", active.id, active.phase))
        .unwrap_or_else(|| "-".to_owned());

    format!(
        "{:<PROJECT_WIDTH$}  {:<PATH_WIDTH$}  {:<PROGRESS_WIDTH$}  {:<ACTIVE_WIDTH$}  {:>BLOCKED_WIDTH$}  {:>FAILED_WIDTH$}",
        row.slug,
        path,
        progress,
        active,
        row.blocked.expect("blocked count present"),
        row.failed.expect("failed count present")
    )
}

fn format_display_path(row: &ProjectListRow, show_stale: bool) -> String {
    let display_path = row.source_path.as_deref().unwrap_or(&row.state_dir);
    let mut path = display_path.display().to_string();
    if show_stale && row.stale {
        path.push_str(" (missing)");
    }
    path
}

fn format_project_list_json(rows: &[ProjectListRow]) -> String {
    let json_rows: Vec<_> = rows
        .iter()
        .map(|row| ProjectListJsonRow {
            slug: row.slug.clone(),
            source_path: row
                .source_path
                .as_ref()
                .map(|path| path.display().to_string()),
            state_dir: row.state_dir.display().to_string(),
            total: row.total,
            done: row.done,
            blocked: row.blocked,
            failed: row.failed,
            active: row.active.clone(),
            stale: row.stale,
            error: row.error.clone(),
        })
        .collect();

    serde_json::to_string_pretty(&json_rows).expect("project list json serializes")
}

fn list_initialized_projects(agira_root: &Path) -> Result<Vec<ProjectEntry>, ProjectListError> {
    let entries = match fs::read_dir(agira_root) {
        Ok(entries) => entries,
        Err(source) if source.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(source) => {
            return Err(ProjectListError::Read {
                path: agira_root.to_path_buf(),
                source,
            });
        }
    };

    let mut projects = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|source| ProjectListError::Read {
            path: agira_root.to_path_buf(),
            source,
        })?;
        let state_dir = entry.path();
        let file_type = entry.file_type().map_err(|source| ProjectListError::Read {
            path: state_dir.clone(),
            source,
        })?;

        if !file_type.is_dir() || !state_dir.join("config.json").is_file() {
            continue;
        }

        let name = entry.file_name().to_string_lossy().into_owned();
        let source_path = read_source_path(&state_dir);
        projects.push(ProjectEntry {
            name,
            state_dir,
            source_path,
        });
    }

    projects.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(projects)
}

fn read_source_path(state_dir: &Path) -> Option<PathBuf> {
    let content = fs::read_to_string(state_dir.join(".source_path")).ok()?;
    let trimmed = content.trim_end_matches('\n');
    if trimmed.is_empty() {
        None
    } else {
        Some(PathBuf::from(trimmed))
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use serde_json::{Value, json};

    use super::*;

    fn write_project(
        agira_root: &Path,
        slug: &str,
        source_path: Option<&Path>,
        tasks: Option<Value>,
    ) -> PathBuf {
        let state_dir = agira_root.join(slug);
        fs::create_dir_all(&state_dir).expect("create project state dir");
        let config = Config::new_single_workflow(
            "test",
            vec![
                ("enriching".to_owned(), PhaseDef::default()),
                ("implementing".to_owned(), PhaseDef::default()),
                ("reviewing".to_owned(), PhaseDef::default()),
            ],
            3,
        );
        fs::write(
            state_dir.join("config.json"),
            serde_json::to_string_pretty(&config).expect("serialize config"),
        )
        .expect("write config");

        if let Some(source_path) = source_path {
            fs::write(
                state_dir.join(".source_path"),
                format!("{}\n", source_path.display()),
            )
            .expect("write source path");
        }

        if let Some(tasks) = tasks {
            fs::write(
                state_dir.join("tasks.json"),
                serde_json::to_string_pretty(&tasks).expect("serialize tasks"),
            )
            .expect("write tasks");
        }

        state_dir
    }

    fn task(id: &str, state: &str) -> Value {
        json!({
            "id": id,
            "title": format!("title {id}"),
            "description": "description",
            "state": state,
            "dependencies": [],
            "retry_count": 0,
            "max_retries": 3,
            "phases": BTreeMap::<String, Value>::new(),
            "history": [],
            "created_at": "2026-01-01T00:00:00Z",
            "workflow": "default"
        })
    }

    fn render_human(agira_root: &Path, show_stale: bool) -> String {
        format_project_list_output(agira_root, false, show_stale).expect("format human")
    }

    fn render_json(agira_root: &Path) -> Value {
        serde_json::from_str(
            &format_project_list_output(agira_root, true, false).expect("format json"),
        )
        .expect("parse json output")
    }

    #[test]
    fn normal_multi_project_progress_active_counts_and_sorting() {
        let temp = tempfile::TempDir::new().expect("temp dir");
        let agira_root = temp.path().join(".agira");
        let alpha_source = temp.path().join("alpha-src");
        let done_source = temp.path().join("done-src");
        let zeta_source = temp.path().join("zeta-src");
        fs::create_dir_all(&alpha_source).expect("create alpha source");
        fs::create_dir_all(&done_source).expect("create done source");
        fs::create_dir_all(&zeta_source).expect("create zeta source");

        write_project(
            &agira_root,
            "zeta",
            Some(&zeta_source),
            Some(json!({ "tasks": [task("task-020", "implementing")] })),
        );
        write_project(
            &agira_root,
            "alpha",
            Some(&alpha_source),
            Some(json!({
                "tasks": [
                    task("task-001", "done"),
                    task("task-002", "blocked"),
                    task("task-003", "failed"),
                    task("task-004", "implementing")
                ]
            })),
        );
        write_project(
            &agira_root,
            "done",
            Some(&done_source),
            Some(json!({ "tasks": [task("task-030", "done"), task("task-031", "done")] })),
        );

        let output = render_human(&agira_root, false);
        let alpha_idx = output.find("alpha").expect("alpha row");
        let zeta_idx = output.find("zeta").expect("zeta row");
        let done_idx = output.find("done").expect("done row");
        assert!(alpha_idx < zeta_idx, "active projects sort alphabetically");
        assert!(
            zeta_idx < done_idx,
            "done projects sort after active projects"
        );
        assert!(output.contains("alpha"));
        assert!(output.contains("1/4"));
        assert!(output.contains("task-004 (implementing)"));
        assert!(output.contains("      1       1"));
        assert!(output.contains("done"));
        assert!(output.contains("2/2"));
    }

    #[test]
    fn stale_projects_hide_by_default_and_show_with_marker() {
        let temp = tempfile::TempDir::new().expect("temp dir");
        let agira_root = temp.path().join(".agira");
        let live_source = temp.path().join("live-src");
        let stale_source = temp.path().join("missing-src");
        fs::create_dir_all(&live_source).expect("create live source");

        write_project(
            &agira_root,
            "live",
            Some(&live_source),
            Some(json!({ "tasks": [] })),
        );
        write_project(
            &agira_root,
            "stale",
            Some(&stale_source),
            Some(json!({ "tasks": [] })),
        );

        let hidden = render_human(&agira_root, false);
        assert!(hidden.contains("live"));
        assert!(!hidden.contains(&stale_source.display().to_string()));
        assert!(hidden.ends_with("1 stale project hidden (use --stale to show)"));

        let shown = render_human(&agira_root, true);
        assert!(shown.contains("stale"));
        assert!(shown.contains(&format!("{} (missing)", stale_source.display())));
        assert!(!shown.contains("hidden (use --stale to show)"));
    }

    #[test]
    fn corrupt_tasks_json_renders_inline_error_and_continues() {
        let temp = tempfile::TempDir::new().expect("temp dir");
        let agira_root = temp.path().join(".agira");
        let good_source = temp.path().join("good-src");
        let bad_source = temp.path().join("bad-src");
        fs::create_dir_all(&good_source).expect("create good source");
        fs::create_dir_all(&bad_source).expect("create bad source");

        write_project(
            &agira_root,
            "bad",
            Some(&bad_source),
            Some(json!({ "tasks": [] })),
        );
        fs::write(agira_root.join("bad").join("tasks.json"), "{not json").expect("corrupt tasks");
        write_project(
            &agira_root,
            "good",
            Some(&good_source),
            Some(json!({ "tasks": [task("task-001", "implementing")] })),
        );

        let output = render_human(&agira_root, false);
        assert!(output.contains("bad"));
        assert!(output.contains("error: invalid tasks.json"));
        assert!(output.contains("good"));
        assert!(output.contains("task-001 (implementing)"));
    }

    #[test]
    fn json_output_shape_includes_stale_and_nullable_error_fields() {
        let temp = tempfile::TempDir::new().expect("temp dir");
        let agira_root = temp.path().join(".agira");
        let source = temp.path().join("src");
        let stale_source = temp.path().join("missing-src");
        fs::create_dir_all(&source).expect("create source");

        let state_dir = write_project(
            &agira_root,
            "alpha",
            Some(&source),
            Some(json!({ "tasks": [task("task-001", "implementing"), task("task-002", "done")] })),
        );
        write_project(
            &agira_root,
            "bad",
            Some(&stale_source),
            Some(json!({ "tasks": [] })),
        );
        fs::write(agira_root.join("bad").join("tasks.json"), "{not json").expect("corrupt tasks");

        assert_eq!(
            format_project_list_output(&agira_root, true, false).expect("json without stale flag"),
            format_project_list_output(&agira_root, true, true).expect("json with stale flag")
        );

        let output = render_json(&agira_root);
        let rows = output.as_array().expect("json array");
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0]["slug"], "alpha");
        assert_eq!(rows[0]["source_path"], source.display().to_string());
        assert_eq!(rows[0]["state_dir"], state_dir.display().to_string());
        assert_eq!(rows[0]["total"], 2);
        assert_eq!(rows[0]["done"], 1);
        assert_eq!(rows[0]["blocked"], 0);
        assert_eq!(rows[0]["failed"], 0);
        assert_eq!(rows[0]["active"]["id"], "task-001");
        assert_eq!(rows[0]["active"]["phase"], "implementing");
        assert_eq!(rows[0]["stale"], false);
        assert!(rows[0]["error"].is_null());

        assert_eq!(rows[1]["slug"], "bad");
        assert!(rows[1]["total"].is_null());
        assert!(rows[1]["done"].is_null());
        assert!(rows[1]["blocked"].is_null());
        assert!(rows[1]["failed"].is_null());
        assert!(rows[1]["active"].is_null());
        assert_eq!(rows[1]["stale"], true);
        assert_eq!(rows[1]["error"], "invalid tasks.json");
    }

    #[test]
    fn empty_agira_root_outputs_empty_human_and_empty_json() {
        let temp = tempfile::TempDir::new().expect("temp dir");
        let agira_root = temp.path().join(".agira");
        fs::create_dir_all(&agira_root).expect("create empty root");

        assert_eq!(render_human(&agira_root, false), "");
        assert_eq!(
            format_project_list_output(&agira_root, true, false).expect("format json"),
            "[]"
        );
    }
}
