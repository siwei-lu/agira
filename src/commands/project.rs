use std::{
    env, fs, io,
    path::{Path, PathBuf},
};

use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq)]
struct ProjectEntry {
    name: String,
    state_dir: PathBuf,
    source_path: Option<PathBuf>,
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

pub fn run_project_list() -> Result<(), ProjectListError> {
    let agira_root = agira_root_from_home()?;
    run_project_list_from(&agira_root)
}

fn agira_root_from_home() -> Result<PathBuf, ProjectListError> {
    let home = env::var_os("HOME")
        .filter(|value| !value.as_os_str().is_empty())
        .ok_or(ProjectListError::HomeDirectoryMissing)?;

    Ok(PathBuf::from(home).join(".agira"))
}

fn run_project_list_from(agira_root: &Path) -> Result<(), ProjectListError> {
    let output = format_project_list(&list_initialized_projects(agira_root)?);

    if !output.is_empty() {
        println!("{output}");
    }

    Ok(())
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

fn format_project_list(projects: &[ProjectEntry]) -> String {
    projects
        .iter()
        .map(|project| {
            let display_path = project.source_path.as_deref().unwrap_or(&project.state_dir);
            format!("{}  {}", project.name, display_path.display())
        })
        .collect::<Vec<_>>()
        .join("\n")
}
