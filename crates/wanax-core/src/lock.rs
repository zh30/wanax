use crate::error::{ErrorCode, WanaxError};
use crate::timeutil::now_rfc3339;
use fs2::FileExt;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct LockInfo {
    pub run_id: String,
    pub pid: u32,
    pub started_at: String,
    pub lock_path: PathBuf,
}

pub struct RepoLock {
    pub info: LockInfo,
    file: File,
}

impl RepoLock {
    pub fn lock_path(repo_root: &Path) -> PathBuf {
        repo_root.join(".wanax").join("LOCK")
    }

    pub fn acquire(repo_root: &Path, run_id: &str) -> Result<Self, WanaxError> {
        let wanax = repo_root.join(".wanax");
        fs::create_dir_all(&wanax).map_err(|e| WanaxError::with_detail(ErrorCode::Db, e))?;
        let path = Self::lock_path(repo_root);
        if path.exists() {
            if let Ok(existing) = read_lock(&path) {
                if pid_alive(existing.pid) {
                    return Err(WanaxError::new(
                        ErrorCode::RepoLocked,
                        format!("repo locked by run {}", existing.run_id),
                    ));
                }
                return Err(WanaxError::new(
                    ErrorCode::RepoLocked,
                    format!(
                        "repo locked by run {} (stale lock pid={})",
                        existing.run_id, existing.pid
                    ),
                ));
            }
            return Err(WanaxError::new(
                ErrorCode::RepoLocked,
                format!("repo locked by run {run_id}"),
            ));
        }
        let mut file = OpenOptions::new()
            .create_new(true)
            .read(true)
            .write(true)
            .open(&path)
            .map_err(|e| {
                if e.kind() == std::io::ErrorKind::AlreadyExists {
                    if let Ok(existing) = read_lock(&path) {
                        WanaxError::new(
                            ErrorCode::RepoLocked,
                            format!("repo locked by run {}", existing.run_id),
                        )
                    } else {
                        WanaxError::from_code(ErrorCode::RepoLocked)
                    }
                } else {
                    WanaxError::with_detail(ErrorCode::Db, e)
                }
            })?;
        file.lock_exclusive()
            .map_err(|e| WanaxError::with_detail(ErrorCode::Db, e))?;
        let info = LockInfo {
            run_id: run_id.to_string(),
            pid: std::process::id(),
            started_at: now_rfc3339(),
            lock_path: path.clone(),
        };
        write_lock(&mut file, &info)?;
        Ok(Self { info, file })
    }

    /// Take over a stale LOCK for the same run, or create one if missing.
    pub fn acquire_for_resume(repo_root: &Path, run_id: &str) -> Result<Self, WanaxError> {
        let path = Self::lock_path(repo_root);
        if path.exists() {
            let existing = read_lock(&path)?;
            if existing.run_id != run_id {
                return Err(WanaxError::new(
                    ErrorCode::RepoLocked,
                    format!("repo locked by run {}", existing.run_id),
                ));
            }
            if pid_alive(existing.pid) {
                return Err(WanaxError::new(
                    ErrorCode::RepoLocked,
                    format!("repo locked by run {}", existing.run_id),
                ));
            }
            fs::remove_file(&path).map_err(|e| WanaxError::with_detail(ErrorCode::Db, e))?;
        }
        Self::acquire(repo_root, run_id)
    }

    pub fn release(self) -> Result<(), WanaxError> {
        let path = self.info.lock_path.clone();
        drop(self);
        if path.exists() {
            fs::remove_file(&path).map_err(|e| WanaxError::with_detail(ErrorCode::Db, e))?;
        }
        Ok(())
    }
}

impl Drop for RepoLock {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.file);
    }
}

pub fn read_lock(path: &Path) -> Result<LockInfo, WanaxError> {
    let mut s = String::new();
    File::open(path)
        .and_then(|mut f| f.read_to_string(&mut s))
        .map_err(|e| WanaxError::with_detail(ErrorCode::Db, e))?;
    let mut run_id = String::new();
    let mut pid = 0u32;
    let mut started_at = String::new();
    for line in s.lines() {
        if let Some((k, v)) = line.split_once('=') {
            match k.trim() {
                "run_id" => run_id = v.trim().to_string(),
                "pid" => pid = v.trim().parse().unwrap_or(0),
                "started_at" => started_at = v.trim().to_string(),
                _ => {}
            }
        }
    }
    if run_id.is_empty() {
        return Err(WanaxError::with_detail(ErrorCode::Db, "malformed LOCK"));
    }
    Ok(LockInfo {
        run_id,
        pid,
        started_at,
        lock_path: path.to_path_buf(),
    })
}

fn write_lock(file: &mut File, info: &LockInfo) -> Result<(), WanaxError> {
    let body = format!(
        "run_id={}\npid={}\nstarted_at={}\n",
        info.run_id, info.pid, info.started_at
    );
    file.set_len(0)
        .and_then(|_| file.write_all(body.as_bytes()))
        .and_then(|_| file.flush())
        .map_err(|e| WanaxError::with_detail(ErrorCode::Db, e))?;
    Ok(())
}

pub fn pid_alive(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    std::process::Command::new("kill")
        .args(["-0", &pid.to_string()])
        .status()
        .map(|s| s.success())
        .unwrap_or(true)
}

pub fn inspect_lock(repo_root: &Path) -> Result<Option<(LockInfo, bool)>, WanaxError> {
    let path = RepoLock::lock_path(repo_root);
    if !path.exists() {
        return Ok(None);
    }
    let info = read_lock(&path)?;
    let alive = pid_alive(info.pid);
    Ok(Some((info, alive)))
}

/// doctor --fix-lock: only when pid is dead.
pub fn clear_stale_lock(repo_root: &Path) -> Result<LockInfo, WanaxError> {
    let path = RepoLock::lock_path(repo_root);
    if !path.exists() {
        return Err(WanaxError::with_detail(
            ErrorCode::LockStale,
            "no LOCK file",
        ));
    }
    let info = read_lock(&path)?;
    if pid_alive(info.pid) {
        return Err(WanaxError::new(
            ErrorCode::RepoLocked,
            format!("repo locked by run {}", info.run_id),
        ));
    }
    fs::remove_file(&path).map_err(|e| WanaxError::with_detail(ErrorCode::Db, e))?;
    Ok(info)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn acquire_and_second_start_refuses() {
        let tmp = TempDir::new().unwrap();
        std::fs::create_dir_all(tmp.path().join(".wanax")).unwrap();
        let lock = RepoLock::acquire(tmp.path(), "wx_01AAAAAAAAAAAAAAAAAAAAAAAA").unwrap();
        let err = match RepoLock::acquire(tmp.path(), "wx_01BBBBBBBBBBBBBBBBBBBBBBBB") {
            Ok(_) => panic!("second acquire should fail"),
            Err(e) => e,
        };
        assert_eq!(err.code, ErrorCode::RepoLocked);
        assert!(err.message.contains("wx_01AAAAAAAAAAAAAAAAAAAAAAAA"));
        lock.release().unwrap();
        assert!(!RepoLock::lock_path(tmp.path()).exists());
    }
}
