use crate::error::{ErrorCode, WanaxError};
use crate::glob_overlap::peer_glob_sets_overlap;
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
    pub exclusive: bool,
    pub paths: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct LockAcquire {
    pub exclusive: bool,
    pub paths: Vec<String>,
    pub resume: bool,
}

pub struct RepoLock {
    pub info: LockInfo,
    file: File,
}

impl RepoLock {
    pub fn lock_path(repo_root: &Path) -> PathBuf {
        repo_root.join(".wanax").join("LOCK")
    }

    pub fn locks_dir(repo_root: &Path) -> PathBuf {
        repo_root.join(".wanax").join("locks")
    }

    pub fn lockset_path(repo_root: &Path) -> PathBuf {
        repo_root.join(".wanax").join("LOCKSET")
    }

    pub fn holder_path(repo_root: &Path, run_id: &str) -> PathBuf {
        Self::locks_dir(repo_root).join(run_id)
    }

    pub fn acquire(repo_root: &Path, run_id: &str) -> Result<Self, WanaxError> {
        Self::acquire_with(
            repo_root,
            run_id,
            &LockAcquire {
                exclusive: true,
                paths: Vec::new(),
                resume: false,
            },
        )
    }

    pub fn acquire_for_resume(repo_root: &Path, run_id: &str) -> Result<Self, WanaxError> {
        Self::acquire_with(
            repo_root,
            run_id,
            &LockAcquire {
                exclusive: true,
                paths: Vec::new(),
                resume: true,
            },
        )
    }

    pub fn acquire_with(
        repo_root: &Path,
        run_id: &str,
        req: &LockAcquire,
    ) -> Result<Self, WanaxError> {
        let exclusive = req.exclusive || req.paths.is_empty();
        with_lockset(repo_root, || {
            if req.resume {
                remove_stale_holder_for_run(repo_root, run_id)?;
            }
            let holders = list_holders_unlocked(repo_root)?;
            for existing in &holders {
                if req.resume && existing.run_id == run_id && !pid_alive(existing.pid) {
                    continue;
                }
                if existing.run_id == run_id && pid_alive(existing.pid) {
                    return Err(WanaxError::new(
                        ErrorCode::RepoLocked,
                        format!("repo locked by run {}", existing.run_id),
                    ));
                }
                if lock_conflicts(existing, exclusive, &req.paths) {
                    let stale = if pid_alive(existing.pid) {
                        String::new()
                    } else {
                        format!(" (stale lock pid={})", existing.pid)
                    };
                    return Err(WanaxError::new(
                        ErrorCode::RepoLocked,
                        format!("repo locked by run {}{stale}", existing.run_id),
                    ));
                }
            }
            create_holder(repo_root, run_id, exclusive, &req.paths)
        })
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

fn lock_conflicts(existing: &LockInfo, exclusive: bool, paths: &[String]) -> bool {
    if existing.exclusive || exclusive {
        return true;
    }
    peer_glob_sets_overlap(&existing.paths, paths)
}

fn create_holder(
    repo_root: &Path,
    run_id: &str,
    exclusive: bool,
    paths: &[String],
) -> Result<RepoLock, WanaxError> {
    let path = if exclusive {
        RepoLock::lock_path(repo_root)
    } else {
        fs::create_dir_all(RepoLock::locks_dir(repo_root))
            .map_err(|e| WanaxError::with_detail(ErrorCode::Db, e))?;
        RepoLock::holder_path(repo_root, run_id)
    };
    if path.exists() {
        if let Ok(existing) = read_lock(&path) {
            return Err(WanaxError::new(
                ErrorCode::RepoLocked,
                format!("repo locked by run {}", existing.run_id),
            ));
        }
        return Err(WanaxError::from_code(ErrorCode::RepoLocked));
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
        lock_path: path,
        exclusive,
        paths: if exclusive {
            Vec::new()
        } else {
            paths.to_vec()
        },
    };
    write_lock(&mut file, &info)?;
    Ok(RepoLock { info, file })
}

fn with_lockset<T>(
    repo_root: &Path,
    f: impl FnOnce() -> Result<T, WanaxError>,
) -> Result<T, WanaxError> {
    let wanax = repo_root.join(".wanax");
    fs::create_dir_all(&wanax).map_err(|e| WanaxError::with_detail(ErrorCode::Db, e))?;
    let guard = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(RepoLock::lockset_path(repo_root))
        .map_err(|e| WanaxError::with_detail(ErrorCode::Db, e))?;
    guard
        .lock_exclusive()
        .map_err(|e| WanaxError::with_detail(ErrorCode::Db, e))?;
    let result = f();
    let _ = FileExt::unlock(&guard);
    result
}

fn remove_stale_holder_for_run(repo_root: &Path, run_id: &str) -> Result<(), WanaxError> {
    for info in list_holders_unlocked(repo_root)? {
        if info.run_id != run_id {
            continue;
        }
        if pid_alive(info.pid) {
            return Err(WanaxError::new(
                ErrorCode::RepoLocked,
                format!("repo locked by run {}", info.run_id),
            ));
        }
        if info.lock_path.exists() {
            fs::remove_file(&info.lock_path)
                .map_err(|e| WanaxError::with_detail(ErrorCode::Db, e))?;
        }
    }
    Ok(())
}

fn list_holders_unlocked(repo_root: &Path) -> Result<Vec<LockInfo>, WanaxError> {
    let mut out = Vec::new();
    let exclusive = RepoLock::lock_path(repo_root);
    if exclusive.exists() {
        out.push(read_lock(&exclusive)?);
    }
    let dir = RepoLock::locks_dir(repo_root);
    if dir.is_dir() {
        let mut entries: Vec<PathBuf> = fs::read_dir(&dir)
            .map_err(|e| WanaxError::with_detail(ErrorCode::Db, e))?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.is_file())
            .collect();
        entries.sort();
        for path in entries {
            out.push(read_lock(&path)?);
        }
    }
    Ok(out)
}

pub fn list_holders(repo_root: &Path) -> Result<Vec<LockInfo>, WanaxError> {
    with_lockset(repo_root, || list_holders_unlocked(repo_root))
}

pub fn read_lock(path: &Path) -> Result<LockInfo, WanaxError> {
    let mut s = String::new();
    File::open(path)
        .and_then(|mut f| f.read_to_string(&mut s))
        .map_err(|e| WanaxError::with_detail(ErrorCode::Db, e))?;
    let mut run_id = String::new();
    let mut pid = 0u32;
    let mut started_at = String::new();
    let mut exclusive = true;
    let mut paths = Vec::new();
    let mut saw_exclusive = false;
    for line in s.lines() {
        if let Some((k, v)) = line.split_once('=') {
            match k.trim() {
                "run_id" => run_id = v.trim().to_string(),
                "pid" => pid = v.trim().parse().unwrap_or(0),
                "started_at" => started_at = v.trim().to_string(),
                "exclusive" => {
                    saw_exclusive = true;
                    exclusive = v.trim() == "true";
                }
                "path" => {
                    let p = v.trim();
                    if !p.is_empty() {
                        paths.push(p.to_string());
                    }
                }
                _ => {}
            }
        }
    }
    if run_id.is_empty() {
        return Err(WanaxError::with_detail(ErrorCode::Db, "malformed LOCK"));
    }
    if !saw_exclusive && !paths.is_empty() {
        exclusive = false;
    }
    Ok(LockInfo {
        run_id,
        pid,
        started_at,
        lock_path: path.to_path_buf(),
        exclusive,
        paths,
    })
}

fn write_lock(file: &mut File, info: &LockInfo) -> Result<(), WanaxError> {
    let mut body = format!(
        "run_id={}\npid={}\nstarted_at={}\nexclusive={}\n",
        info.run_id, info.pid, info.started_at, info.exclusive
    );
    for p in &info.paths {
        body.push_str("path=");
        body.push_str(p);
        body.push('\n');
    }
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
    Ok(inspect_locks(repo_root)?.into_iter().next())
}

pub fn inspect_locks(repo_root: &Path) -> Result<Vec<(LockInfo, bool)>, WanaxError> {
    Ok(list_holders(repo_root)?
        .into_iter()
        .map(|info| {
            let alive = pid_alive(info.pid);
            (info, alive)
        })
        .collect())
}

/// doctor --fix-lock: remove holders whose pid is dead.
pub fn clear_stale_lock(repo_root: &Path) -> Result<Vec<LockInfo>, WanaxError> {
    with_lockset(repo_root, || {
        let holders = list_holders_unlocked(repo_root)?;
        if holders.is_empty() {
            return Err(WanaxError::with_detail(
                ErrorCode::LockStale,
                "no LOCK file",
            ));
        }
        let mut cleared = Vec::new();
        let mut live = 0u32;
        for info in holders {
            if pid_alive(info.pid) {
                live += 1;
                continue;
            }
            if info.lock_path.exists() {
                fs::remove_file(&info.lock_path)
                    .map_err(|e| WanaxError::with_detail(ErrorCode::Db, e))?;
            }
            cleared.push(info);
        }
        if cleared.is_empty() {
            return Err(WanaxError::new(
                ErrorCode::RepoLocked,
                format!("repo locked by {live} live run(s)"),
            ));
        }
        Ok(cleared)
    })
}

pub fn release_run_lock(repo_root: &Path, run_id: &str) -> Result<(), WanaxError> {
    with_lockset(repo_root, || {
        for info in list_holders_unlocked(repo_root)? {
            if info.run_id == run_id && info.lock_path.exists() {
                fs::remove_file(&info.lock_path)
                    .map_err(|e| WanaxError::with_detail(ErrorCode::Db, e))?;
            }
        }
        Ok(())
    })
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

    #[test]
    fn path_set_allows_disjoint_and_rejects_overlap() {
        let tmp = TempDir::new().unwrap();
        std::fs::create_dir_all(tmp.path().join(".wanax")).unwrap();
        let a = RepoLock::acquire_with(
            tmp.path(),
            "wx_01AAAAAAAAAAAAAAAAAAAAAAAA",
            &LockAcquire {
                exclusive: false,
                paths: vec!["src/a.rs".into()],
                resume: false,
            },
        )
        .unwrap();
        let b = RepoLock::acquire_with(
            tmp.path(),
            "wx_01BBBBBBBBBBBBBBBBBBBBBBBB",
            &LockAcquire {
                exclusive: false,
                paths: vec!["src/b.rs".into()],
                resume: false,
            },
        )
        .unwrap();
        let err = match RepoLock::acquire_with(
            tmp.path(),
            "wx_01CCCCCCCCCCCCCCCCCCCCCCCC",
            &LockAcquire {
                exclusive: false,
                paths: vec!["src/a.rs".into()],
                resume: false,
            },
        ) {
            Ok(_) => panic!("overlap should fail"),
            Err(e) => e,
        };
        assert_eq!(err.code, ErrorCode::RepoLocked);
        let exclusive = match RepoLock::acquire(tmp.path(), "wx_01DDDDDDDDDDDDDDDDDDDDDDDD") {
            Ok(_) => panic!("exclusive should fail while path-set holders exist"),
            Err(e) => e,
        };
        assert_eq!(exclusive.code, ErrorCode::RepoLocked);
        a.release().unwrap();
        b.release().unwrap();
    }

    #[test]
    fn empty_path_set_fails_closed_as_exclusive() {
        let tmp = TempDir::new().unwrap();
        std::fs::create_dir_all(tmp.path().join(".wanax")).unwrap();
        let a = RepoLock::acquire_with(
            tmp.path(),
            "wx_01AAAAAAAAAAAAAAAAAAAAAAAA",
            &LockAcquire {
                exclusive: false,
                paths: Vec::new(),
                resume: false,
            },
        )
        .unwrap();
        assert!(a.info.exclusive);
        assert!(RepoLock::lock_path(tmp.path()).exists());
        a.release().unwrap();
    }
}
