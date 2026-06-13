use std::{
    fs::{File, OpenOptions},
    io,
    os::fd::AsRawFd,
    path::{Path, PathBuf},
};

use thiserror::Error;

#[derive(Debug, Error)]
pub enum FileLockError {
    #[error("failed to open lock file {path}")]
    Open {
        path: PathBuf,
        #[source]
        source: io::Error,
    },

    #[error("failed to acquire lock file {path}")]
    Acquire {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

#[derive(Debug)]
pub struct FileLock {
    file: File,
}

impl FileLock {
    pub fn acquire(path: impl AsRef<Path>) -> Result<Self, FileLockError> {
        Self::acquire_with_flags(path.as_ref(), libc::LOCK_EX).map(Option::unwrap)
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub fn try_acquire(path: impl AsRef<Path>) -> Result<Option<Self>, FileLockError> {
        Self::acquire_with_flags(path.as_ref(), libc::LOCK_EX | libc::LOCK_NB)
    }

    fn acquire_with_flags(path: &Path, flags: libc::c_int) -> Result<Option<Self>, FileLockError> {
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(path)
            .map_err(|source| FileLockError::Open {
                path: path.to_path_buf(),
                source,
            })?;

        if unsafe { libc::flock(file.as_raw_fd(), flags) } == 0 {
            return Ok(Some(Self { file }));
        }

        let source = io::Error::last_os_error();
        if flags & libc::LOCK_NB != 0
            && matches!(
                source.raw_os_error(),
                Some(code) if code == libc::EWOULDBLOCK || code == libc::EAGAIN
            )
        {
            return Ok(None);
        }

        Err(FileLockError::Acquire {
            path: path.to_path_buf(),
            source,
        })
    }
}

impl Drop for FileLock {
    fn drop(&mut self) {
        let _ = unsafe { libc::flock(self.file.as_raw_fd(), libc::LOCK_UN) };
    }
}

#[cfg(test)]
mod tests {
    use super::FileLock;

    #[test]
    fn held_lock_blocks_nonblocking_acquire_until_dropped() {
        let dir = tempfile::TempDir::new().expect("create temp dir");
        let lock_path = dir.path().join("tasks.lock");

        let lock = FileLock::acquire(&lock_path).expect("acquire lock");
        assert!(
            FileLock::try_acquire(&lock_path)
                .expect("try acquire lock")
                .is_none(),
            "second non-blocking acquire should report contention"
        );

        drop(lock);

        assert!(
            FileLock::try_acquire(&lock_path)
                .expect("try acquire after drop")
                .is_some(),
            "lock should be acquirable after the first guard drops"
        );
    }
}
