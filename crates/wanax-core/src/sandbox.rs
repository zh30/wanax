use crate::config::home_dir;
use crate::error::{ErrorCode, WanaxError};
use std::fs;
use std::path::{Path, PathBuf};

/// OS-level sandbox backend used to confine spawned children (worker CLIs,
/// test harness, commander CLI). `None` = unsandboxed fallback.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SandboxBackend {
    Seatbelt,
    Bwrap,
    None,
}

impl SandboxBackend {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Seatbelt => "seatbelt",
            Self::Bwrap => "bwrap",
            Self::None => "none",
        }
    }
}

/// Network egress confinement layered on the filesystem rules.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NetConfine {
    /// No network rule (previous behavior).
    Open,
    /// Outbound allowed only to `localhost:port` — children must use the
    /// domain-allowlist proxy; nothing else leaves the box, DNS included.
    ProxyOnly(u16),
    /// No network at all.
    Isolated,
}

/// Resolved per-spawn confinement: writable roots, hidden read paths, and the
/// chosen backend. `writable` must include the worktree and the real git dir
/// (linked worktrees write metadata under `<repo>/.git/worktrees/`).
#[derive(Debug, Clone)]
pub struct SandboxSpec {
    pub backend: SandboxBackend,
    pub writable: Vec<PathBuf>,
    pub deny_read: Vec<PathBuf>,
    pub net: NetConfine,
    pub seatbelt_profile: Option<PathBuf>,
}

impl SandboxSpec {
    pub fn none() -> Self {
        Self {
            backend: SandboxBackend::None,
            writable: Vec::new(),
            deny_read: Vec::new(),
            net: NetConfine::Open,
            seatbelt_profile: None,
        }
    }
}

/// Home-relative paths whose contents must not be readable by spawned
/// children. Agent self-config (`~/.claude`, `~/.codex`) deliberately absent —
/// the worker CLI needs its own credentials.
pub const SECRET_HOME_DIRS: &[&str] = &[
    ".ssh",
    ".aws",
    ".gnupg",
    ".kube",
    ".docker",
    ".config/gh",
    ".config/gcloud",
];

/// Home-relative paths a child may write: package/toolchain caches plus agent
/// self-state. Kept narrow; users extend via `[sandbox] extra_write_paths`.
pub const WRITABLE_HOME_DIRS: &[&str] = &[
    ".cargo",
    ".rustup",
    ".cache",
    "Library/Caches",
    ".npm",
    ".pnpm-store",
    ".local/share/pnpm",
    "go/pkg",
    ".claude",
    ".codex",
];

/// Extra writable files (not dirs) allowed under seatbelt/bwrap.
pub const WRITABLE_HOME_FILES: &[&str] = &[".claude.json"];

fn bin_on_path(name: &str) -> Option<PathBuf> {
    let path = std::env::var("PATH").ok()?;
    for dir in path.split(':') {
        let cand = Path::new(dir).join(name);
        if cand.is_file() {
            return Some(cand);
        }
    }
    None
}

pub fn detect_backend(mode: &str) -> SandboxBackend {
    match mode.trim() {
        "off" | "none" => SandboxBackend::None,
        "seatbelt" => SandboxBackend::Seatbelt,
        "bwrap" => SandboxBackend::Bwrap,
        _ => {
            if cfg!(target_os = "macos") && bin_on_path("sandbox-exec").is_some() {
                SandboxBackend::Seatbelt
            } else if cfg!(target_os = "linux") && bin_on_path("bwrap").is_some() {
                SandboxBackend::Bwrap
            } else {
                SandboxBackend::None
            }
        }
    }
}

fn dedup(paths: Vec<PathBuf>) -> Vec<PathBuf> {
    let mut seen = std::collections::BTreeSet::new();
    paths
        .into_iter()
        .filter(|p| seen.insert(p.clone()))
        .collect()
}

/// Resolve confinement for one spawn. `writable_required` are caller-supplied
/// roots that MUST be writable (worktree, `<repo>/.git`, run dir, scratch dir).
/// `extra_writes` come from `[sandbox] extra_write_paths`.
/// When the resolved backend is Seatbelt, `profile_path` is required and the
/// profile is written there.
pub fn resolve(
    mode: &str,
    writable_required: Vec<PathBuf>,
    extra_writes: &[String],
    profile_path: Option<PathBuf>,
    net: NetConfine,
) -> Result<SandboxSpec, WanaxError> {
    let backend = detect_backend(mode);
    if backend == SandboxBackend::None {
        return Ok(SandboxSpec::none());
    }
    let home = home_dir();
    let mut writable = writable_required;
    for rel in WRITABLE_HOME_DIRS {
        writable.push(home.join(rel));
    }
    for rel in WRITABLE_HOME_FILES {
        writable.push(home.join(rel));
    }
    for e in extra_writes {
        if !e.trim().is_empty() {
            writable.push(PathBuf::from(e));
        }
    }
    if let Some(tmp) = std::env::var_os("TMPDIR") {
        let tmp = PathBuf::from(tmp);
        // Also cover the per-user temp root (/var/folders/<u>/<h>/ on macOS):
        // clang/rustc write caches into the sibling `C/` directory.
        if let Some(parent) = tmp.parent() {
            writable.push(parent.to_path_buf());
        }
        writable.push(tmp);
    }
    writable.push(PathBuf::from("/tmp"));
    writable.push(PathBuf::from("/private/tmp"));
    let deny_read: Vec<PathBuf> = SECRET_HOME_DIRS
        .iter()
        .map(|d| home.join(d))
        .filter(|p| p.exists())
        .map(|p| fs::canonicalize(&p).unwrap_or(p))
        .collect();
    // Seatbelt matches on canonical paths: `/var` is a symlink to
    // `/private/var` on macOS, so raw paths silently miss the rules.
    let writable = dedup(writable)
        .into_iter()
        .map(|p| fs::canonicalize(&p).unwrap_or(p))
        .collect();
    let spec = SandboxSpec {
        backend,
        writable,
        deny_read,
        net,
        seatbelt_profile: profile_path.clone(),
    };
    if backend == SandboxBackend::Seatbelt {
        let Some(path) = profile_path else {
            return Err(WanaxError::new(
                ErrorCode::Db,
                "seatbelt sandbox requires a profile path",
            ));
        };
        write_seatbelt_profile(&spec, &path)?;
    }
    Ok(spec)
}

fn seatbelt_escape(path: &Path) -> String {
    path.display().to_string().replace('\\', "\\\\").replace('"', "\\\"")
}

pub fn seatbelt_profile_text(spec: &SandboxSpec) -> String {
    let mut s = String::from("(version 1)\n(allow default)\n");
    if !spec.deny_read.is_empty() {
        s.push_str("(deny file-read*\n");
        for p in &spec.deny_read {
            s.push_str(&format!("    (subpath \"{}\")\n", seatbelt_escape(p)));
        }
        s.push_str(")\n");
    }
    s.push_str("(deny file-write*)\n");
    s.push_str("(allow file-write*\n");
    s.push_str("    (literal \"/dev/null\")\n    (literal \"/dev/tty\")\n");
    for p in &spec.writable {
        // A `subpath` rule covers a directory's contents but not a file at
        // the path itself (e.g. `~/.claude.json`); files need `literal`.
        let rule = if p.is_file() { "literal" } else { "subpath" };
        s.push_str(&format!("    ({rule} \"{}\")\n", seatbelt_escape(p)));
    }
    s.push_str(")\n");
    match spec.net {
        NetConfine::Open => {}
        NetConfine::ProxyOnly(port) => {
            // Only the local allowlist proxy may be reached — outbound DNS,
            // HTTPS to arbitrary hosts, everything else is dead.
            s.push_str("(deny network-outbound)\n");
            s.push_str(&format!(
                "(allow network-outbound (remote tcp \"localhost:{port}\"))\n"
            ));
        }
        NetConfine::Isolated => {
            s.push_str("(deny network*)\n");
        }
    }
    s
}

fn write_seatbelt_profile(spec: &SandboxSpec, path: &Path) -> Result<(), WanaxError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| WanaxError::with_detail(ErrorCode::Db, e))?;
    }
    fs::write(path, seatbelt_profile_text(spec))
        .map_err(|e| WanaxError::with_detail(ErrorCode::Db, e))
}

/// Rewrite `program args` into a sandboxed invocation. `SandboxBackend::None`
/// returns the inputs unchanged.
pub fn wrap(
    spec: &SandboxSpec,
    program: &Path,
    args: &[String],
) -> Result<(PathBuf, Vec<String>), WanaxError> {
    match spec.backend {
        SandboxBackend::None => Ok((program.to_path_buf(), args.to_vec())),
        SandboxBackend::Seatbelt => {
            let bin = bin_on_path("sandbox-exec").ok_or_else(|| {
                WanaxError::new(ErrorCode::AdapterMissing, "sandbox-exec not found")
            })?;
            let profile = spec.seatbelt_profile.clone().ok_or_else(|| {
                WanaxError::new(ErrorCode::Db, "seatbelt profile missing")
            })?;
            let mut argv = vec![
                "-f".to_string(),
                profile.display().to_string(),
                program.display().to_string(),
            ];
            argv.extend(args.iter().cloned());
            Ok((bin, argv))
        }
        SandboxBackend::Bwrap => {
            let bin = bin_on_path("bwrap").ok_or_else(|| {
                WanaxError::new(ErrorCode::AdapterMissing, "bwrap not found")
            })?;
            let mut argv: Vec<String> = vec![
                "--die-with-parent".into(),
                "--new-session".into(),
                "--proc".into(),
                "/proc".into(),
                "--dev".into(),
                "/dev".into(),
                "--ro-bind".into(),
                "/".into(),
                "/".into(),
            ];
            if spec.net == NetConfine::Isolated {
                argv.push("--unshare-net".into());
            }
            for p in &spec.writable {
                if p.exists() {
                    argv.push("--bind".into());
                    argv.push(p.display().to_string());
                    argv.push(p.display().to_string());
                }
            }
            for p in &spec.deny_read {
                argv.push("--tmpfs".into());
                argv.push(p.display().to_string());
            }
            argv.push("--".into());
            argv.push(program.display().to_string());
            argv.extend(args.iter().cloned());
            Ok((bin, argv))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn off_mode_resolves_to_none() {
        let spec = resolve("off", vec![PathBuf::from("/x")], &[], None, NetConfine::Open).unwrap();
        assert_eq!(spec.backend, SandboxBackend::None);
        let (prog, argv) = wrap(&spec, Path::new("/bin/echo"), &["hi".into()]).unwrap();
        assert_eq!(prog, PathBuf::from("/bin/echo"));
        assert_eq!(argv, vec!["hi"]);
    }

    #[test]
    fn seatbelt_profile_denies_writes_then_allows_worktree() {
        let spec = SandboxSpec {
            backend: SandboxBackend::Seatbelt,
            writable: vec![PathBuf::from("/repo/.wt"), PathBuf::from("/repo/.git")],
            deny_read: vec![PathBuf::from("/u/.ssh")],
            net: NetConfine::Open,
            seatbelt_profile: Some(PathBuf::from("/tmp/p.sb")),
        };
        let text = seatbelt_profile_text(&spec);
        let deny_pos = text.find("(deny file-write*)").unwrap();
        let allow_pos = text.find("(allow file-write*").unwrap();
        assert!(deny_pos < allow_pos, "deny must precede allow");
        assert!(text.contains("(subpath \"/repo/.wt\")"));
        assert!(text.contains("(subpath \"/repo/.git\")"));
        assert!(text.contains("(subpath \"/u/.ssh\")"));
    }

    #[test]
    fn seatbelt_uses_literal_for_file_paths() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("state.json");
        std::fs::write(&file, "{}").unwrap();
        let spec = SandboxSpec {
            backend: SandboxBackend::Seatbelt,
            writable: vec![file.clone()],
            deny_read: vec![],
            net: NetConfine::Open,
            seatbelt_profile: Some(dir.path().join("p.sb")),
        };
        let text = seatbelt_profile_text(&spec);
        assert!(
            text.contains(&format!("(literal \"{}\")", file.display())),
            "{text}"
        );
    }

    #[test]
    fn seatbelt_net_rules_proxy_only_and_isolated() {
        let base = |net| SandboxSpec {
            backend: SandboxBackend::Seatbelt,
            writable: vec![],
            deny_read: vec![],
            net,
            seatbelt_profile: Some(PathBuf::from("/tmp/p.sb")),
        };
        let proxied = seatbelt_profile_text(&base(NetConfine::ProxyOnly(4321)));
        let deny = proxied.find("(deny network-outbound)").unwrap();
        let allow = proxied
            .find("(allow network-outbound (remote tcp \"localhost:4321\"))")
            .unwrap();
        assert!(deny < allow, "deny must precede the localhost allow");

        let isolated = seatbelt_profile_text(&base(NetConfine::Isolated));
        assert!(isolated.contains("(deny network*)"));
        let open = seatbelt_profile_text(&base(NetConfine::Open));
        assert!(!open.contains("network"));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn seatbelt_wrap_prefixes_argv() {
        let spec = SandboxSpec {
            backend: SandboxBackend::Seatbelt,
            writable: vec![],
            deny_read: vec![],
            net: NetConfine::Open,
            seatbelt_profile: Some(PathBuf::from("/tmp/p.sb")),
        };
        let (prog, argv) = wrap(&spec, Path::new("/usr/bin/claude"), &["-p".into()]).unwrap();
        assert_eq!(prog, bin_on_path("sandbox-exec").unwrap());
        assert_eq!(argv[0], "-f");
        assert!(argv[2].ends_with("claude"));
        assert_eq!(argv[3], "-p");
    }
}
