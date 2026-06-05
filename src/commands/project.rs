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

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::*;

    #[test]
    fn list_initialized_projects_returns_configured_state_dirs_sorted_by_name() {
        let agira_root = TempDir::new().unwrap();
        let zebra = agira_root.path().join("zebra");
        let alpha = agira_root.path().join("alpha");

        fs::create_dir(&zebra).unwrap();
        fs::create_dir(&alpha).unwrap();
        fs::write(zebra.join("config.json"), "{}").unwrap();
        fs::write(alpha.join("config.json"), "{}").unwrap();
        fs::write(
            agira_root.path().join("config.toml"),
            "default_max_retries = 3\n",
        )
        .unwrap();

        let projects = list_initialized_projects(agira_root.path()).unwrap();

        assert_eq!(
            projects,
            vec![
                ProjectEntry {
                    name: "alpha".to_owned(),
                    state_dir: alpha,
                    source_path: None,
                },
                ProjectEntry {
                    name: "zebra".to_owned(),
                    state_dir: zebra,
                    source_path: None,
                },
            ]
        );
    }

    #[test]
    fn list_initialized_projects_reads_source_path_when_present() {
        let agira_root = TempDir::new().unwrap();
        let myproject = agira_root.path().join("myproject");

        fs::create_dir(&myproject).unwrap();
        fs::write(myproject.join("config.json"), "{}").unwrap();
        fs::write(
            myproject.join(".source_path"),
            "/Users/alice/workspace/myproject\n",
        )
        .unwrap();

        let projects = list_initialized_projects(agira_root.path()).unwrap();

        assert_eq!(projects.len(), 1);
        assert_eq!(
            projects[0].source_path,
            Some(PathBuf::from("/Users/alice/workspace/myproject"))
        );
    }

    #[test]
    fn list_initialized_projects_ignores_uninitialized_state_dirs() {
        let agira_root = TempDir::new().unwrap();
        let uninitialized = agira_root.path().join("uninitialized");

        fs::create_dir(&uninitialized).unwrap();
        fs::write(uninitialized.join(".source_path"), "/tmp/repo\n").unwrap();

        let projects = list_initialized_projects(agira_root.path()).unwrap();

        assert!(projects.is_empty());
    }

    #[test]
    fn missing_agira_root_lists_no_projects() {
        let parent = TempDir::new().unwrap();

        let projects = list_initialized_projects(&parent.path().join(".agira")).unwrap();

        assert!(projects.is_empty());
    }

    #[test]
    fn format_project_list_outputs_source_path_when_present() {
        let project = ProjectEntry {
            name: "alpha".to_owned(),
            state_dir: PathBuf::from("/tmp/.agira/alpha"),
            source_path: Some(PathBuf::from("/Users/alice/workspace/alpha")),
        };

        assert_eq!(
            format_project_list(&[project]),
            "alpha  /Users/alice/workspace/alpha"
        );
    }

    #[test]
    fn format_project_list_falls_back_to_state_dir_when_no_source_path() {
        let project = ProjectEntry {
            name: "alpha".to_owned(),
            state_dir: PathBuf::from("/tmp/.agira/alpha"),
            source_path: None,
        };

        assert_eq!(format_project_list(&[project]), "alpha  /tmp/.agira/alpha");
    }
}
