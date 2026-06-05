use std::{
    env, fs, io,
    io::{Read, Write},
    path::{Path, PathBuf},
    process,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use serde::Deserialize;
use thiserror::Error;

const LATEST_RELEASE_URL: &str = "https://api.github.com/repos/siwei-lu/agira/releases/latest";
const DOWNLOAD_BUFFER_SIZE: usize = 8 * 1024;
const PROGRESS_BAR_WIDTH: usize = 20;
const UNKNOWN_TOTAL_BAR: &str = "????????????????????";

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

    #[error("failed to write download progress: {0}")]
    WriteProgress(#[source] io::Error),

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
    let total_bytes = response.content_length();

    let stdout = io::stdout();
    let mut progress_output = stdout.lock();
    let started_at = Instant::now();
    copy_with_download_progress(
        &mut response,
        &mut file,
        total_bytes,
        &mut progress_output,
        || started_at.elapsed(),
    )
    .map_err(|error| match error {
        DownloadCopyError::Data(source) => SelfUpdateError::WriteTemp {
            path: path.to_path_buf(),
            source,
        },
        DownloadCopyError::Progress(source) => SelfUpdateError::WriteProgress(source),
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

#[derive(Debug)]
enum DownloadCopyError {
    Data(io::Error),
    Progress(io::Error),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ProgressMarker {
    KnownBarSegments(usize),
    UnknownTotalBucket(u64),
}

struct DownloadProgressReporter<'a, W, E>
where
    W: Write,
    E: FnMut() -> Duration,
{
    output: &'a mut W,
    total_bytes: Option<u64>,
    elapsed: E,
    last_marker: Option<ProgressMarker>,
    last_downloaded: Option<u64>,
}

impl<'a, W, E> DownloadProgressReporter<'a, W, E>
where
    W: Write,
    E: FnMut() -> Duration,
{
    fn new(output: &'a mut W, total_bytes: Option<u64>, elapsed: E) -> Self {
        Self {
            output,
            total_bytes,
            elapsed,
            last_marker: None,
            last_downloaded: None,
        }
    }

    fn start(&mut self) -> Result<(), DownloadCopyError> {
        self.render(0)
    }

    fn record(&mut self, downloaded: u64) -> Result<(), DownloadCopyError> {
        let marker = progress_marker(downloaded, self.total_bytes);

        if self.last_marker != Some(marker) {
            self.render(downloaded)?;
        }

        Ok(())
    }

    fn finish(&mut self, downloaded: u64) -> Result<(), DownloadCopyError> {
        if self.last_downloaded != Some(downloaded) {
            self.render(downloaded)?;
        }

        Ok(())
    }

    fn render(&mut self, downloaded: u64) -> Result<(), DownloadCopyError> {
        let elapsed = (self.elapsed)();
        writeln!(
            self.output,
            "{}",
            format_download_progress(downloaded, self.total_bytes, elapsed)
        )
        .map_err(DownloadCopyError::Progress)?;
        self.last_marker = Some(progress_marker(downloaded, self.total_bytes));
        self.last_downloaded = Some(downloaded);
        Ok(())
    }
}

fn copy_with_download_progress<R, W, P, E>(
    reader: &mut R,
    writer: &mut W,
    total_bytes: Option<u64>,
    progress_output: &mut P,
    elapsed: E,
) -> Result<u64, DownloadCopyError>
where
    R: Read,
    W: Write,
    P: Write,
    E: FnMut() -> Duration,
{
    let mut reporter = DownloadProgressReporter::new(progress_output, total_bytes, elapsed);
    reporter.start()?;

    let mut downloaded = 0;
    let mut buffer = [0; DOWNLOAD_BUFFER_SIZE];
    loop {
        let bytes_read = reader.read(&mut buffer).map_err(DownloadCopyError::Data)?;
        if bytes_read == 0 {
            break;
        }

        writer
            .write_all(&buffer[..bytes_read])
            .map_err(DownloadCopyError::Data)?;
        downloaded += bytes_read as u64;
        reporter.record(downloaded)?;
    }

    reporter.finish(downloaded)?;
    Ok(downloaded)
}

fn progress_marker(downloaded: u64, total_bytes: Option<u64>) -> ProgressMarker {
    match total_bytes {
        Some(total) => {
            ProgressMarker::KnownBarSegments(filled_progress_segments(downloaded, total))
        }
        None => ProgressMarker::UnknownTotalBucket(downloaded / (1024 * 1024)),
    }
}

fn format_download_progress(
    downloaded: u64,
    total_bytes: Option<u64>,
    elapsed: Duration,
) -> String {
    let bar = progress_bar(downloaded, total_bytes);
    let total_display = total_bytes
        .map(|total| format!("{total} B"))
        .unwrap_or_else(|| "unknown".to_owned());
    let percentage = match total_bytes {
        Some(total) => format!("{}%", progress_percentage(downloaded, total)),
        None => "--%".to_owned(),
    };
    let speed = download_speed(downloaded, elapsed);

    format!("download [{bar}] {downloaded} B / {total_display} ({percentage}) {speed} B/s")
}

fn progress_bar(downloaded: u64, total_bytes: Option<u64>) -> String {
    match total_bytes {
        Some(total) => {
            let filled = filled_progress_segments(downloaded, total);
            format!(
                "{}{}",
                "#".repeat(filled),
                "-".repeat(PROGRESS_BAR_WIDTH - filled)
            )
        }
        None => UNKNOWN_TOTAL_BAR.to_owned(),
    }
}

fn filled_progress_segments(downloaded: u64, total: u64) -> usize {
    if total == 0 {
        return PROGRESS_BAR_WIDTH;
    }

    ((downloaded as u128 * PROGRESS_BAR_WIDTH as u128) / total as u128)
        .min(PROGRESS_BAR_WIDTH as u128) as usize
}

fn progress_percentage(downloaded: u64, total: u64) -> u64 {
    if total == 0 {
        return 100;
    }

    ((downloaded as u128 * 100) / total as u128).min(100) as u64
}

fn download_speed(downloaded: u64, elapsed: Duration) -> u64 {
    let elapsed_millis = elapsed.as_millis();
    if elapsed_millis == 0 {
        return 0;
    }

    ((downloaded as u128 * 1000) / elapsed_millis) as u64
}

fn already_up_to_date_message(version: &str) -> String {
    format!("agira is already up to date (v{version})")
}

fn updated_message(version: &str) -> String {
    format!("updated to v{version}")
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

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

    #[test]
    fn download_progress_formats_bytes_percentage_bar_and_speed() {
        assert_eq!(
            format_download_progress(1024, Some(2048), Duration::from_secs(2)),
            "download [##########----------] 1024 B / 2048 B (50%) 512 B/s"
        );
    }

    #[test]
    fn download_progress_handles_unknown_total_without_ansi_sequences() {
        let output = format_download_progress(1536, None, Duration::from_secs(1));

        assert_eq!(
            output,
            "download [????????????????????] 1536 B / unknown (--%) 1536 B/s"
        );
        assert!(!output.contains('\x1b'));
        assert!(!output.contains('\r'));
    }

    #[test]
    fn copy_with_download_progress_reports_start_intermediate_and_finish() {
        let body = vec![7; DOWNLOAD_BUFFER_SIZE * 2];
        let mut reader = io::Cursor::new(body.clone());
        let mut writer = Vec::new();
        let mut progress = Vec::new();

        let copied = copy_with_download_progress(
            &mut reader,
            &mut writer,
            Some(body.len() as u64),
            &mut progress,
            || Duration::from_secs(2),
        )
        .unwrap();

        let progress = String::from_utf8(progress).unwrap();
        assert_eq!(copied, body.len() as u64);
        assert_eq!(writer, body);
        assert!(progress.contains("download [--------------------] 0 B / 16384 B (0%) 0 B/s\n"));
        assert!(
            progress.contains("download [##########----------] 8192 B / 16384 B (50%) 4096 B/s\n")
        );
        assert!(
            progress
                .contains("download [####################] 16384 B / 16384 B (100%) 8192 B/s\n")
        );
        assert!(!progress.contains('\x1b'));
        assert!(!progress.contains('\r'));
    }
}
