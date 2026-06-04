use std::{
    env, fs, io,
    io::Write,
    path::{Path, PathBuf},
    process,
    time::{SystemTime, UNIX_EPOCH},
};

use serde::Deserialize;
use thiserror::Error;

const LATEST_RELEASE_URL: &str = "https://api.github.com/repos/siwei-lu/agira/releases/latest";

#[derive(Debug, Error)]
pub enum SelfUpdateError {
    #[error("unsupported platform: {os}/{arch}")]
    UnsupportedPlatform { os: String, arch: String },

    #[error("no asset found for {os}/{arch} in release {tag}")]
    AssetNotFound {
        os: String,
        arch: String,
        tag: String,
    },

    #[error("failed to build http client: {0}")]
    HttpClient(#[source] reqwest::Error),

    #[error("failed to fetch latest release: {0}")]
    FetchLatest(#[source] reqwest::Error),

    #[error("failed to parse latest release: {0}")]
    ParseLatest(#[source] serde_json::Error),

    #[error("failed to download asset: {0}")]
    DownloadAsset(#[source] reqwest::Error),

    #[error("failed to find current executable: {0}")]
    CurrentExe(#[source] io::Error),

    #[error("current executable has no parent directory: {0}")]
    MissingExeParent(PathBuf),

    #[error("failed to create temporary file {path}: {source}")]
    CreateTemp {
        path: PathBuf,
        #[source]
        source: io::Error,
    },

    #[error("failed to write temporary file {path}: {source}")]
    WriteTemp {
        path: PathBuf,
        #[source]
        source: io::Error,
    },

    #[error("failed to set executable permissions on {path}: {source}")]
    SetPermissions {
        path: PathBuf,
        #[source]
        source: io::Error,
    },

    #[error("failed to replace current executable {path}: {source}")]
    ReplaceCurrentExe {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

#[derive(Debug, Deserialize)]
struct GithubRelease {
    tag_name: String,
    assets: Vec<GithubAsset>,
}

#[derive(Debug, Deserialize)]
struct GithubAsset {
    name: String,
    browser_download_url: String,
}

pub fn run_self_update() -> Result<(), SelfUpdateError> {
    let os = env::consts::OS;
    let arch = env::consts::ARCH;
    let asset_name = platform_asset_name(os, arch)?;
    let client = build_client()?;
    let release = fetch_latest_release(&client)?;
    let tag_version = release_version(&release.tag_name).to_owned();

    if is_current_version(&release.tag_name, env!("CARGO_PKG_VERSION")) {
        println!("{}", already_up_to_date_message(&tag_version));
        return Ok(());
    }

    let asset = find_asset(&release, asset_name).ok_or_else(|| SelfUpdateError::AssetNotFound {
        os: os.to_owned(),
        arch: arch.to_owned(),
        tag: release.tag_name.clone(),
    })?;
    let asset_url = asset.browser_download_url.clone();
    let current_exe = env::current_exe().map_err(SelfUpdateError::CurrentExe)?;
    let temp_path = temp_download_path(&current_exe)?;

    let result = (|| {
        download_asset(&client, &asset_url, &temp_path)?;
        set_executable(&temp_path)?;
        replace_current_exe(&temp_path, &current_exe)?;
        Ok(())
    })();

    match result {
        Ok(()) => {
            println!("{}", updated_message(&tag_version));
            Ok(())
        }
        Err(error) => {
            let _ = fs::remove_file(&temp_path);
            Err(error)
        }
    }
}

fn platform_asset_name(os: &str, arch: &str) -> Result<&'static str, SelfUpdateError> {
    match (os, arch) {
        ("macos", "aarch64") => Ok("agira-aarch64-apple-darwin"),
        ("macos", "x86_64") => Ok("agira-x86_64-apple-darwin"),
        ("linux", "x86_64") => Ok("agira-x86_64-unknown-linux-gnu"),
        _ => Err(SelfUpdateError::UnsupportedPlatform {
            os: os.to_owned(),
            arch: arch.to_owned(),
        }),
    }
}

fn build_client() -> Result<reqwest::blocking::Client, SelfUpdateError> {
    reqwest::blocking::Client::builder()
        .user_agent(format!("agira/{}", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(SelfUpdateError::HttpClient)
}

fn fetch_latest_release(
    client: &reqwest::blocking::Client,
) -> Result<GithubRelease, SelfUpdateError> {
    let body = client
        .get(LATEST_RELEASE_URL)
        .send()
        .and_then(reqwest::blocking::Response::error_for_status)
        .and_then(reqwest::blocking::Response::text)
        .map_err(SelfUpdateError::FetchLatest)?;

    serde_json::from_str(&body).map_err(SelfUpdateError::ParseLatest)
}

fn download_asset(
    client: &reqwest::blocking::Client,
    url: &str,
    path: &Path,
) -> Result<(), SelfUpdateError> {
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|source| SelfUpdateError::CreateTemp {
            path: path.to_path_buf(),
            source,
        })?;

    let mut response = client
        .get(url)
        .send()
        .and_then(reqwest::blocking::Response::error_for_status)
        .map_err(SelfUpdateError::DownloadAsset)?;

    io::copy(&mut response, &mut file).map_err(|source| SelfUpdateError::WriteTemp {
        path: path.to_path_buf(),
        source,
    })?;
    file.flush().map_err(|source| SelfUpdateError::WriteTemp {
        path: path.to_path_buf(),
        source,
    })?;

    Ok(())
}

fn replace_current_exe(temp_path: &Path, current_exe: &Path) -> Result<(), SelfUpdateError> {
    fs::rename(temp_path, current_exe).map_err(|source| SelfUpdateError::ReplaceCurrentExe {
        path: current_exe.to_path_buf(),
        source,
    })
}

fn set_executable(path: &Path) -> Result<(), SelfUpdateError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).map_err(|source| {
            SelfUpdateError::SetPermissions {
                path: path.to_path_buf(),
                source,
            }
        })?;
    }

    #[cfg(not(unix))]
    {
        let _ = path;
    }

    Ok(())
}

fn temp_download_path(current_exe: &Path) -> Result<PathBuf, SelfUpdateError> {
    let parent = current_exe
        .parent()
        .ok_or_else(|| SelfUpdateError::MissingExeParent(current_exe.to_path_buf()))?;
    let timestamp_like_counter = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();

    Ok(parent.join(format!(
        "agira-update-{}-{}.tmp",
        process::id(),
        timestamp_like_counter
    )))
}

fn release_version(tag_name: &str) -> &str {
    tag_name.strip_prefix('v').unwrap_or(tag_name)
}

fn is_current_version(tag_name: &str, current_version: &str) -> bool {
    release_version(tag_name) == current_version
}

fn find_asset<'a>(release: &'a GithubRelease, asset_name: &str) -> Option<&'a GithubAsset> {
    release.assets.iter().find(|asset| asset.name == asset_name)
}

fn already_up_to_date_message(version: &str) -> String {
    format!("agira is already up to date (v{version})")
}

fn updated_message(version: &str) -> String {
    format!("updated to v{version}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn platform_asset_name_maps_supported_targets() {
        assert_eq!(
            platform_asset_name("macos", "aarch64").unwrap(),
            "agira-aarch64-apple-darwin"
        );
        assert_eq!(
            platform_asset_name("macos", "x86_64").unwrap(),
            "agira-x86_64-apple-darwin"
        );
        assert_eq!(
            platform_asset_name("linux", "x86_64").unwrap(),
            "agira-x86_64-unknown-linux-gnu"
        );
    }

    #[test]
    fn platform_asset_name_rejects_unsupported_targets() {
        assert!(matches!(
            platform_asset_name("linux", "aarch64"),
            Err(SelfUpdateError::UnsupportedPlatform { .. })
        ));
        assert!(matches!(
            platform_asset_name("windows", "x86_64"),
            Err(SelfUpdateError::UnsupportedPlatform { .. })
        ));
    }

    #[test]
    fn release_version_strips_single_leading_v() {
        assert_eq!(release_version("v0.4.1"), "0.4.1");
        assert_eq!(release_version("0.4.1"), "0.4.1");
    }

    #[test]
    fn is_current_version_matches_normalized_tag() {
        assert!(is_current_version("v0.4.1", "0.4.1"));
    }

    #[test]
    fn is_current_version_detects_new_release() {
        assert!(!is_current_version("v0.4.2", "0.4.1"));
    }

    #[test]
    fn find_asset_matches_exact_name() {
        let release = GithubRelease {
            tag_name: "v0.4.2".to_owned(),
            assets: vec![
                GithubAsset {
                    name: "agira-aarch64-apple-darwin".to_owned(),
                    browser_download_url: "https://example.test/aarch64".to_owned(),
                },
                GithubAsset {
                    name: "agira-x86_64-apple-darwin".to_owned(),
                    browser_download_url: "https://example.test/x86_64".to_owned(),
                },
            ],
        };

        let asset = find_asset(&release, "agira-x86_64-apple-darwin").unwrap();

        assert_eq!(asset.browser_download_url, "https://example.test/x86_64");
    }

    #[test]
    fn find_asset_returns_none_for_missing_name() {
        let release = GithubRelease {
            tag_name: "v0.4.2".to_owned(),
            assets: vec![GithubAsset {
                name: "agira-aarch64-apple-darwin".to_owned(),
                browser_download_url: "https://example.test/aarch64".to_owned(),
            }],
        };

        assert!(find_asset(&release, "agira-x86_64-apple-darwin").is_none());
    }

    #[test]
    fn already_up_to_date_message_formats_exactly() {
        assert_eq!(
            already_up_to_date_message("0.4.1"),
            "agira is already up to date (v0.4.1)"
        );
    }

    #[test]
    fn updated_message_formats_exactly() {
        assert_eq!(updated_message("0.4.2"), "updated to v0.4.2");
    }
}
