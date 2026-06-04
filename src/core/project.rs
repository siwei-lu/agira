use std::{
    env, fs, io,
    path::{Path, PathBuf},
};

use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::core::global_config::{GlobalConfig, GlobalConfigError, load_or_create};

#[derive(Debug, Clone)]
pub struct Project {
    pub git_root: PathBuf,
    pub slug: String,
    pub state_dir: PathBuf,
    pub global_config: GlobalConfig,
}

#[derive(Debug, Error)]
pub enum ProjectError {
    #[error("not inside a git repository")]
    NotInGitRepository,

    #[error("home directory missing")]
    HomeDirectoryMissing,

    #[error("failed to canonicalize git root {0}")]
    GitRootCanonicalize(PathBuf, #[source] io::Error),

    #[error("failed to create state directory {0}")]
    CreateStateDir(PathBuf, #[source] io::Error),

    #[error("failed to read source path {0}")]
    SourcePathRead(PathBuf, #[source] io::Error),

    #[error("failed to write source path {0}")]
    SourcePathWrite(PathBuf, #[source] io::Error),

    #[error("hashed slug collision at {0}")]
    HashSlugCollision(PathBuf),

    #[error(transparent)]
    GlobalConfig(#[from] GlobalConfigError),
}

pub fn resolve_project() -> Result<Project, ProjectError> {
    let current_dir = env::current_dir().map_err(|_| ProjectError::NotInGitRepository)?;
    let home = env::var_os("HOME")
        .filter(|value| !value.as_os_str().is_empty())
        .ok_or(ProjectError::HomeDirectoryMissing)?;
    let agira_root = PathBuf::from(home).join(".agira");

    resolve_project_from(&current_dir, &agira_root)
}

pub fn resolve_project_from(start_dir: &Path, agira_root: &Path) -> Result<Project, ProjectError> {
    let git_root = find_git_root(start_dir)?;

    fs::create_dir_all(agira_root)
        .map_err(|error| ProjectError::CreateStateDir(agira_root.to_path_buf(), error))?;
    let global_config = load_or_create(agira_root)?;

    let base_slug = slugify(&git_root_basename(&git_root));
    let source_path_content = source_path_content(&git_root);
    let base_state_dir = agira_root.join(&base_slug);

    let (slug, state_dir) = match prepare_candidate(&base_state_dir, &source_path_content)? {
        CandidateResolution::Use => (base_slug, base_state_dir),
        CandidateResolution::Collision => {
            let hash_slug = format!("{}-{}", base_slug, hash_suffix(&git_root));
            let hash_state_dir = agira_root.join(&hash_slug);

            match prepare_candidate(&hash_state_dir, &source_path_content)? {
                CandidateResolution::Use => (hash_slug, hash_state_dir),
                CandidateResolution::Collision => {
                    return Err(ProjectError::HashSlugCollision(hash_state_dir));
                }
            }
        }
    };

    let project = Project {
        git_root,
        slug,
        state_dir,
        global_config,
    };
    debug_assert_eq!(
        project.state_dir.file_name().and_then(|name| name.to_str()),
        Some(project.slug.as_str())
    );

    Ok(project)
}

enum CandidateResolution {
    Use,
    Collision,
}

fn find_git_root(start_dir: &Path) -> Result<PathBuf, ProjectError> {
    let mut current = start_dir.to_path_buf();

    loop {
        let git_path = current.join(".git");
        if git_path.is_dir() || git_path.is_file() {
            return current
                .canonicalize()
                .map_err(|error| ProjectError::GitRootCanonicalize(current, error));
        }

        if !current.pop() {
            return Err(ProjectError::NotInGitRepository);
        }
    }
}

fn git_root_basename(git_root: &Path) -> String {
    git_root
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default()
}

fn slugify(name: &str) -> String {
    name.chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect()
}

fn hash_suffix(git_root: &Path) -> String {
    let mut hasher = Sha256::new();
    hasher.update(git_root.to_string_lossy().as_bytes());
    let hash = format!("{:x}", hasher.finalize());

    hash[..6].to_owned()
}

fn source_path_content(git_root: &Path) -> String {
    format!("{}\n", git_root.to_string_lossy())
}

fn prepare_candidate(
    state_dir: &Path,
    source_path_content: &str,
) -> Result<CandidateResolution, ProjectError> {
    match fs::metadata(state_dir) {
        Ok(metadata) if metadata.is_dir() => {
            prepare_existing_candidate(state_dir, source_path_content)
        }
        Ok(_) => Err(ProjectError::CreateStateDir(
            state_dir.to_path_buf(),
            io::Error::new(
                io::ErrorKind::AlreadyExists,
                "state path exists and is not a directory",
            ),
        )),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            fs::create_dir_all(state_dir)
                .map_err(|error| ProjectError::CreateStateDir(state_dir.to_path_buf(), error))?;
            write_source_path(state_dir, source_path_content)?;
            Ok(CandidateResolution::Use)
        }
        Err(error) => Err(ProjectError::CreateStateDir(state_dir.to_path_buf(), error)),
    }
}

fn prepare_existing_candidate(
    state_dir: &Path,
    source_path_content: &str,
) -> Result<CandidateResolution, ProjectError> {
    let source_path = state_dir.join(".source_path");

    match fs::read_to_string(&source_path) {
        Ok(existing_content) => {
            if existing_content == source_path_content {
                Ok(CandidateResolution::Use)
            } else {
                Ok(CandidateResolution::Collision)
            }
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            if is_empty_dir(state_dir)? {
                write_source_path(state_dir, source_path_content)?;
                Ok(CandidateResolution::Use)
            } else {
                Ok(CandidateResolution::Collision)
            }
        }
        Err(error) => Err(ProjectError::SourcePathRead(source_path, error)),
    }
}

fn is_empty_dir(state_dir: &Path) -> Result<bool, ProjectError> {
    let mut entries = fs::read_dir(state_dir)
        .map_err(|error| ProjectError::CreateStateDir(state_dir.to_path_buf(), error))?;

    match entries.next() {
        Some(Ok(_)) => Ok(false),
        Some(Err(error)) => Err(ProjectError::CreateStateDir(state_dir.to_path_buf(), error)),
        None => Ok(true),
    }
}

fn write_source_path(state_dir: &Path, source_path_content: &str) -> Result<(), ProjectError> {
    let source_path = state_dir.join(".source_path");
    let temporary_path = state_dir.join(".source_path.tmp");

    fs::write(&temporary_path, source_path_content)
        .and_then(|_| fs::rename(&temporary_path, &source_path))
        .map_err(|error| ProjectError::SourcePathWrite(source_path, error))
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::TempDir;

    use super::*;

    #[test]
    fn slug_unit_tests() {
        let cases = [
            ("My App", "my-app"),
            ("My_App", "my-app"),
            ("My--App", "my--app"),
            ("App  01", "app--01"),
            ("München", "m-nchen"),
        ];

        for (input, expected) in cases {
            assert_eq!(slugify(input), expected);
        }
    }

    #[test]
    fn resolve_project_from_creates_state_dir_and_source_path() {
        let git_root = TempDir::new().unwrap();
        let agira_root = TempDir::new().unwrap();
        let nested_dir = git_root.path().join("nested").join("dir");

        fs::create_dir(git_root.path().join(".git")).unwrap();
        fs::create_dir_all(&nested_dir).unwrap();

        let project = resolve_project_from(&nested_dir, agira_root.path()).unwrap();

        assert!(project.state_dir.is_dir());
        assert!(project.state_dir.join(".source_path").exists());
    }

    #[test]
    fn same_project_twice_uses_same_slug() {
        let git_root = TempDir::new().unwrap();
        let agira_root = TempDir::new().unwrap();

        fs::create_dir(git_root.path().join(".git")).unwrap();

        let first = resolve_project_from(git_root.path(), agira_root.path()).unwrap();
        let second = resolve_project_from(git_root.path(), agira_root.path()).unwrap();

        assert_eq!(first.slug, second.slug);
    }

    #[test]
    fn collision_uses_hashed_slug_for_second_project() {
        let parent_one = TempDir::new().unwrap();
        let parent_two = TempDir::new().unwrap();
        let agira_root = TempDir::new().unwrap();
        let first_git_root = parent_one.path().join("same");
        let second_git_root = parent_two.path().join("same");

        fs::create_dir(&first_git_root).unwrap();
        fs::create_dir(&second_git_root).unwrap();
        fs::create_dir(first_git_root.join(".git")).unwrap();
        fs::create_dir(second_git_root.join(".git")).unwrap();

        let first = resolve_project_from(&first_git_root, agira_root.path()).unwrap();
        let second = resolve_project_from(&second_git_root, agira_root.path()).unwrap();
        let expected_second_slug = format!("same-{}", hash_suffix(&second.git_root));

        assert_eq!(first.slug, "same");
        assert_eq!(second.slug, expected_second_slug);
        assert_ne!(first.slug, second.slug);
    }
}
