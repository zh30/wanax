use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;
use tempfile::TempDir;

fn wanax() -> Command {
    Command::new(env!("CARGO_BIN_EXE_wanax"))
}

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .expect("git");
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn setup_git(dir: &Path) {
    git(dir, &["init", "-b", "main"]);
    git(dir, &["config", "user.email", "wanax@test"]);
    git(dir, &["config", "user.name", "wanax-test"]);
    git(dir, &["config", "commit.gpgsign", "false"]);
}

fn write_fixture_crate(dir: &Path) {
    fs::write(
        dir.join("Cargo.toml"),
        "[package]\nname = \"fixture\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .unwrap();
    fs::write(dir.join(".gitignore"), "/target\nCargo.lock\n").unwrap();
    fs::create_dir_all(dir.join("src")).unwrap();
    fs::write(
        dir.join("src/lib.rs"),
        "pub fn add(a: i32, b: i32) -> i32 { unimplemented!() }\n\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn test_add() { assert_eq!(super::add(2, 3), 5); }\n}\n",
    )
    .unwrap();
}

fn contract(allowed: &str) -> String {
    format!(
        r#"---
spec: wanax.contract
version: 1
name: "add-fn"
test_command: "cargo test"
test_timeout_secs: 120
allowed_globs:
  - "{allowed}"
forbidden_globs:
  - "**/.env"
---

## Intent

Add the missing add function so the unit test passes.

## Decisions

- Implement add in src/lib.rs only

## Boundaries

- Allowed: {allowed}

## Completion Criteria

- CC-01: cargo test exits 0
"#
    )
}

const GOOD_LIB: &str = "pub fn add(a: i32, b: i32) -> i32 { a + b }\n\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn test_add() { assert_eq!(super::add(2, 3), 5); }\n}\n";

fn fake_toml_good() -> String {
    format!(
        "turns = 1\nrun_tests = false\n\n[[writes]]\npath = \"src/lib.rs\"\ncontent = '''{GOOD_LIB}'''\n"
    )
}

const BAD_LIB: &str = "pub fn add(a: i32, b: i32) -> i32 { a - b }\n\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn test_add() { assert_eq!(super::add(2, 3), 5); }\n}\n";

fn fake_toml_bad() -> String {
    format!(
        "turns = 1\nrun_tests = false\nclaimed_pass = true\n\n[[writes]]\npath = \"src/lib.rs\"\ncontent = '''{BAD_LIB}'''\n"
    )
}

fn fake_toml_boundary() -> String {
    format!(
        "turns = 1\nrun_tests = false\n\n[[writes]]\npath = \"src/lib.rs\"\ncontent = '''{GOOD_LIB}'''\n\n[[writes]]\npath = \"Cargo.toml\"\ncontent = '''[package]\nname = \"fixture\"\nversion = \"0.2.0\"\nedition = \"2021\"\n'''\n"
    )
}

struct Harness {
    repo: TempDir,
    data: TempDir,
}

impl Harness {
    fn new() -> Self {
        let repo = TempDir::new().unwrap();
        setup_git(repo.path());
        write_fixture_crate(repo.path());
        git(
            repo.path(),
            &["add", "Cargo.toml", ".gitignore", "src/lib.rs"],
        );
        git(repo.path(), &["commit", "-m", "init crate"]);
        let h = Self {
            repo,
            data: TempDir::new().unwrap(),
        };
        h.run(&["init"], 0);
        git(h.path(), &["add", "specs"]);
        git(h.path(), &["commit", "-m", "wanax init"]);
        h
    }

    fn path(&self) -> &Path {
        self.repo.path()
    }

    fn run(&self, args: &[&str], expect: i32) -> Output {
        let mut cmd = wanax();
        cmd.args(args)
            .arg("--data-dir")
            .arg(self.data.path())
            .current_dir(self.path())
            .env("WANAX_DATA_DIR", self.data.path())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let out = cmd.output().expect("wanax");
        let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
        let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
        let code = out.status.code().unwrap_or(1);
        assert_eq!(
            code, expect,
            "wanax {args:?} exit {code} want {expect}\nstdout:\n{stdout}\nstderr:\n{stderr}"
        );
        Output {
            stdout,
            stderr,
            code,
        }
    }

    fn write_contract(&self, body: &str) {
        fs::write(self.path().join("specs/add.contract.md"), body).unwrap();
        git(self.path(), &["add", "specs/add.contract.md"]);
        git(self.path(), &["commit", "-m", "contract"]);
    }

    fn write_fake(&self, body: &str) {
        fs::write(self.path().join(".wanax/fake.toml"), body).unwrap();
    }

    fn set_adapter_fake(&self) {
        let p = self.path().join(".wanax/config.toml");
        let mut t = fs::read_to_string(&p).unwrap();
        t = t.replace("adapter = \"octoscode\"", "adapter = \"fake\"");
        fs::write(&p, t).unwrap();
    }

    fn set_repo_exclusive(&self, exclusive: bool) {
        let p = self.path().join(".wanax/config.toml");
        let mut t = fs::read_to_string(&p).unwrap();
        t = t.replace("repo_exclusive = true", "repo_exclusive = false");
        if exclusive {
            t = t.replace("repo_exclusive = false", "repo_exclusive = true");
        }
        fs::write(&p, t).unwrap();
    }

    fn set_adapter_claude(&self, bin: &Path) {
        let p = self.path().join(".wanax/config.toml");
        let mut t = fs::read_to_string(&p).unwrap();
        t = t.replace("adapter = \"octoscode\"", "adapter = \"claude\"");
        t = t.replace("adapter = \"fake\"", "adapter = \"claude\"");
        t = t.replace(
            "claude_bin = \"claude\"",
            &format!("claude_bin = \"{}\"", bin.display()),
        );
        fs::write(&p, t).unwrap();
    }

    fn set_adapter_cmd(&self, cmd: &Path) {
        let p = self.path().join(".wanax/config.toml");
        let mut t = fs::read_to_string(&p).unwrap();
        t = t.replace("adapter = \"octoscode\"", "adapter = \"cmd\"");
        t = t.replace(
            "octoscode_bin = \"octoscode\"",
            &format!(
                "octoscode_bin = \"octoscode\"\ncmd = \"{}\"\ncmd_args = []",
                cmd.display()
            ),
        );
        fs::write(&p, t).unwrap();
    }

    fn envelope(&self) -> Value {
        let runs = self.path().join(".wanax/runs");
        let run_dir = fs::read_dir(&runs)
            .unwrap()
            .filter_map(|e| e.ok())
            .find(|e| e.path().join("envelope.json").is_file())
            .expect("run dir")
            .path();
        let raw = fs::read_to_string(run_dir.join("envelope.json")).unwrap();
        serde_json::from_str(&raw).unwrap()
    }
}

struct Output {
    stdout: String,
    stderr: String,
    #[allow(dead_code)]
    code: i32,
}

#[test]
fn init_on_git_repo_creates_files() {
    let h = Harness::new();
    assert!(h.path().join(".wanax/config.toml").is_file());
    assert!(h.path().join(".wanax/.gitignore").is_file());
    assert!(h.path().join("specs/example.contract.md").is_file());
    let example = fs::read_to_string(h.path().join("specs/example.contract.md")).unwrap();
    assert!(
        example.contains("- \"src/**\""),
        "example must allow implementation paths"
    );
    assert!(
        !example.contains("- \"tests/**\""),
        "example must not allow rewriting tests: {example}"
    );
    let gi = fs::read_to_string(h.path().join(".wanax/.gitignore")).unwrap();
    assert!(gi.contains("worktrees/"));
    assert!(gi.contains("LOCK"));
    assert!(gi.contains("locks/"));
    assert!(gi.contains("LOCKSET"));
}

#[test]
fn init_on_non_git_fails_without_writes() {
    let tmp = TempDir::new().unwrap();
    let data = TempDir::new().unwrap();
    let out = wanax()
        .args(["init", "--data-dir"])
        .arg(data.path())
        .current_dir(tmp.path())
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("E_NOT_GIT"), "{stderr}");
    assert!(!tmp.path().join(".wanax").exists());
}

#[test]
fn invalid_contract_does_not_lock() {
    let h = Harness::new();
    h.set_adapter_fake();
    fs::write(
        h.path().join("specs/bad.contract.md"),
        "---\nspec: wanax.contract\nversion: 1\ntest_command: \"cargo test\"\nallowed_globs: []\n---\n\n## Intent\n\nx\n\n## Decisions\n\n- d\n\n## Completion Criteria\n\n- CC-01: x\n",
    )
    .unwrap();
    git(h.path(), &["add", "specs/bad.contract.md"]);
    git(h.path(), &["commit", "-m", "bad"]);
    let out = h.run(
        &[
            "start",
            "--contract",
            "specs/bad.contract.md",
            "--adapter",
            "fake",
        ],
        4,
    );
    assert!(out.stderr.contains("E_CONTRACT_INVALID"), "{}", out.stderr);
    assert!(!h.path().join(".wanax/LOCK").exists());
}

#[test]
fn dirty_worktree_refuses_start() {
    let h = Harness::new();
    h.set_adapter_fake();
    h.write_contract(&contract("src/**"));
    fs::write(h.path().join("dirty.txt"), "nope").unwrap();
    let out = h.run(
        &[
            "start",
            "--contract",
            "specs/add.contract.md",
            "--adapter",
            "fake",
        ],
        5,
    );
    assert!(out.stderr.contains("E_DIRTY_WORKTREE"), "{}", out.stderr);
    assert!(!h.path().join(".wanax/LOCK").exists());
}

#[test]
fn turns_budget_exhausts() {
    let h = Harness::new();
    h.set_adapter_fake();
    let p = h.path().join(".wanax/config.toml");
    let mut t = fs::read_to_string(&p).unwrap();
    t = t.replace("max_inner_turns = 40", "max_inner_turns = 1");
    t = t.replace("adapter = \"octoscode\"", "adapter = \"fake\"");
    fs::write(&p, t).unwrap();
    h.write_contract(&contract("src/**"));
    h.write_fake("turns = 2\nrun_tests = true\n\n[[writes]]\npath = \"src/lib.rs\"\ncontent = '''pub fn add(a: i32, b: i32) -> i32 { a + b }\n'''\n");
    let out = h.run(
        &[
            "start",
            "--contract",
            "specs/add.contract.md",
            "--adapter",
            "fake",
        ],
        1,
    );
    assert!(
        out.stdout.contains("budget_exhausted") || out.stderr.contains("E_BUDGET"),
        "stdout={} stderr={}",
        out.stdout,
        out.stderr
    );
}

#[test]
fn dangerous_test_command_rejected() {
    let h = Harness::new();
    h.set_adapter_fake();
    let body = contract("src/**").replace("cargo test", "cargo test && rm -rf /");
    fs::write(h.path().join("specs/add.contract.md"), body).unwrap();
    git(h.path(), &["add", "specs/add.contract.md"]);
    git(h.path(), &["commit", "-m", "danger"]);
    let out = h.run(
        &[
            "start",
            "--contract",
            "specs/add.contract.md",
            "--adapter",
            "fake",
        ],
        4,
    );
    assert!(
        out.stderr.contains("E_TEST_COMMAND_FORBIDDEN"),
        "{}",
        out.stderr
    );
}

#[test]
fn happy_path_fake_accepts() {
    let h = Harness::new();
    h.set_adapter_fake();
    h.write_contract(&contract("src/**"));
    h.write_fake(&fake_toml_good());
    let out = h.run(
        &[
            "start",
            "--contract",
            "specs/add.contract.md",
            "--adapter",
            "fake",
        ],
        0,
    );
    assert!(out.stdout.contains("state=accepted"), "{}", out.stdout);
    assert!(out.stdout.contains("/inner"), "{}", out.stdout);
    let env = h.envelope();
    let run_id = env["run_id"].as_str().unwrap();
    let branches = String::from_utf8(
        Command::new("git")
            .args(["branch", "--list"])
            .current_dir(h.path())
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap();
    assert!(
        branches.contains(&format!("wanax/{run_id}/inner")),
        "{branches}"
    );
    assert!(
        branches.contains(&format!("wanax/{run_id}/outer")),
        "{branches}"
    );
    assert_eq!(env["contract_sha256"].as_str().unwrap().len(), 64);
    assert_eq!(env["current_state"], "accepted");
    assert_eq!(env["schema_version"], "1.0.0");
    let events = env["events"].as_array().unwrap();
    assert!(events.iter().any(|e| e["kind"] == "run_started"));
    assert!(events.iter().any(|e| e["kind"] == "unit_dispatched"));
    assert!(events.iter().any(|e| e["kind"] == "receipt_submitted"));
    assert!(events.iter().any(|e| e["kind"] == "outer_test_started"));
    assert!(events.iter().any(|e| e["kind"] == "verdict"));
    let outer = events
        .iter()
        .find(|e| e["kind"] == "outer_test_started")
        .unwrap();
    let cwd = outer["payload"]["cwd"].as_str().unwrap();
    let inner = outer["payload"]["inner_cwd"].as_str().unwrap();
    assert_ne!(cwd, inner);
    assert!(cwd.contains("-outer-"));
    assert!(h
        .path()
        .join(".wanax/runs")
        .join(run_id)
        .join("RESULT.md")
        .is_file());
    assert!(!h.path().join(".wanax/LOCK").exists());
    let reviews: Vec<&Value> = events
        .iter()
        .filter(|e| e["kind"] == "state_changed" && e["payload"]["phase"] == "self_review")
        .collect();
    assert!(!reviews.is_empty(), "{events:?}");
    assert!(reviews
        .iter()
        .all(|e| e["payload"]["self_review"] == "degraded"));
    assert!(events
        .iter()
        .any(|e| e["kind"] == "budget_tick" && e["payload"]["cost_estimated"] == true));
}

#[test]
fn second_start_locked() {
    let h = Harness::new();
    h.set_adapter_fake();
    h.write_contract(&contract("src/**"));
    h.write_fake("turns = 1\nsleep_ms = 20000\nrun_tests = false\n");
    let mut child = wanax()
        .args([
            "start",
            "--contract",
            "specs/add.contract.md",
            "--adapter",
            "fake",
            "--data-dir",
        ])
        .arg(h.data.path())
        .current_dir(h.path())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut locked = false;
    for _ in 0..50 {
        std::thread::sleep(Duration::from_millis(100));
        if h.path().join(".wanax/LOCK").exists() {
            locked = true;
            break;
        }
    }
    assert!(locked, "first start did not write LOCK");
    let out = h.run(
        &[
            "start",
            "--contract",
            "specs/add.contract.md",
            "--adapter",
            "fake",
        ],
        6,
    );
    assert!(out.stderr.contains("E_REPO_LOCKED"), "{}", out.stderr);
    let _ = child.kill();
    let _ = child.wait();
}

#[test]
fn red_tests_not_accepted_then_escalate() {
    let h = Harness::new();
    h.set_adapter_fake();
    h.write_contract(&contract("src/**"));
    h.write_fake(&fake_toml_bad());
    let out = h.run(
        &[
            "start",
            "--contract",
            "specs/add.contract.md",
            "--adapter",
            "fake",
        ],
        1,
    );
    assert!(
        out.stdout.contains("state=escalate") || out.stderr.contains("E_REWORK_LIMIT"),
        "stdout={} stderr={}",
        out.stdout,
        out.stderr
    );
    assert!(!out.stdout.contains("state=accepted"));
}

#[test]
fn boundary_violation_rejects() {
    let h = Harness::new();
    h.set_adapter_fake();
    h.write_contract(&contract("src/**"));
    h.write_fake(&fake_toml_boundary());
    let out = h.run(
        &[
            "start",
            "--contract",
            "specs/add.contract.md",
            "--adapter",
            "fake",
        ],
        1,
    );
    assert!(
        out.stderr.contains("E_BOUNDARY") || out.stdout.contains("state=rejected"),
        "stdout={} stderr={}",
        out.stdout,
        out.stderr
    );
    assert!(!out.stdout.contains("state=accepted"));
}

#[test]
fn claimed_pass_true_outer_red_not_accept() {
    let h = Harness::new();
    h.set_adapter_fake();
    h.write_contract(&contract("src/**"));
    h.write_fake(&fake_toml_bad());
    let out = h.run(
        &[
            "start",
            "--contract",
            "specs/add.contract.md",
            "--adapter",
            "fake",
        ],
        1,
    );
    assert!(!out.stdout.contains("state=accepted"));
    let env = h.envelope();
    let receipt = env["events"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["kind"] == "receipt_submitted")
        .unwrap();
    assert_eq!(receipt["payload"]["claimed_pass"], true);
}

#[test]
fn accept_override_when_commander_lies() {
    let h = Harness::new();
    h.set_adapter_fake();
    h.write_contract(&contract("src/**"));
    h.write_fake(&fake_toml_bad());
    let script = h.data.path().join("commander.json");
    fs::write(
        &script,
        r#"{
          "dispatch": {"title": "add-fn", "instruction": "implement add in src/lib.rs. test_command: cargo test. allowed: src/**. CC-01: cargo test exits 0"},
          "verdicts": [
            {"decision": "accept", "reason": "i insist it is fine", "files_reviewed": ["src/lib.rs"]}
          ]
        }"#,
    )
    .unwrap();
    let out = wanax()
        .args([
            "start",
            "--contract",
            "specs/add.contract.md",
            "--adapter",
            "fake",
            "--data-dir",
        ])
        .arg(h.data.path())
        .current_dir(h.path())
        .env("WANAX_COMMANDER_SCRIPT", &script)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_ne!(out.status.code(), Some(0));
    assert!(!stdout.contains("state=accepted"));
    let env = h.envelope();
    let has_override = env["events"]
        .as_array()
        .unwrap()
        .iter()
        .any(|e| e["kind"] == "error" && e["payload"]["code"] == "E_ACCEPT_OVERRIDE");
    assert!(
        has_override || stdout.contains("rework") || stderr.contains("E_REWORK"),
        "stdout={stdout} stderr={stderr} env={env}"
    );
}

#[test]
fn budget_usd_zero_exhausts() {
    let h = Harness::new();
    h.set_adapter_fake();
    let p = h.path().join(".wanax/config.toml");
    let mut t = fs::read_to_string(&p).unwrap();
    t = t.replace("max_usd = \"5.00\"", "max_usd = \"0.00\"");
    t = t.replace("adapter = \"octoscode\"", "adapter = \"fake\"");
    fs::write(&p, t).unwrap();
    h.write_contract(&contract("src/**"));
    h.write_fake(&fake_toml_good());
    let out = h.run(
        &[
            "start",
            "--contract",
            "specs/add.contract.md",
            "--adapter",
            "fake",
        ],
        1,
    );
    assert!(
        out.stdout.contains("budget_exhausted") || out.stderr.contains("E_BUDGET"),
        "stdout={} stderr={}",
        out.stdout,
        out.stderr
    );
}

#[test]
fn cancel_releases_lock() {
    let h = Harness::new();
    h.set_adapter_fake();
    h.write_contract(&contract("src/**"));
    h.write_fake("turns = 1\nsleep_ms = 30000\nrun_tests = false\n");
    let mut child = wanax()
        .args([
            "start",
            "--contract",
            "specs/add.contract.md",
            "--adapter",
            "fake",
            "--data-dir",
        ])
        .arg(h.data.path())
        .current_dir(h.path())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut run_id = String::new();
    for _ in 0..80 {
        std::thread::sleep(Duration::from_millis(100));
        if let Ok(rd) = fs::read_dir(h.path().join(".wanax/runs")) {
            if let Some(e) = rd.filter_map(|x| x.ok()).next() {
                run_id = e.file_name().to_string_lossy().into_owned();
                break;
            }
        }
    }
    assert!(!run_id.is_empty(), "no run id");
    h.run(&["cancel", &run_id], 0);
    let _ = child.wait();
    assert!(!h.path().join(".wanax/LOCK").exists());
}

#[test]
fn inner_env_has_no_github_token() {
    let h = Harness::new();
    h.set_adapter_fake();
    h.write_contract(&contract("src/**"));
    h.write_fake(&fake_toml_good());
    let out = wanax()
        .args([
            "start",
            "--contract",
            "specs/add.contract.md",
            "--adapter",
            "fake",
            "--data-dir",
        ])
        .arg(h.data.path())
        .current_dir(h.path())
        .env("GH_TOKEN", "ghp_shouldneverleak")
        .env("GIT_ASKPASS", "/bin/false")
        .env("SSH_AUTH_SOCK", "/tmp/fake.sock")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let log = find_file(h.path(), "wanax.log").expect("log");
    let text = fs::read_to_string(log).unwrap_or_default();
    assert!(!text.contains("ghp_shouldneverleak"), "{text}");
    assert!(!text.contains("ghp_"), "{text}");
}

#[test]
fn inner_git_push_is_denied() {
    let h = Harness::new();
    h.set_adapter_fake();
    h.write_contract(&contract("src/**"));
    git(
        h.path(),
        &["remote", "add", "origin", "/nonexistent/wanax-remote.git"],
    );
    h.write_fake(&format!(
        "turns = 1\nrun_tests = false\nattempt_push = true\n\n[[writes]]\npath = \"src/lib.rs\"\ncontent = '''{GOOD_LIB}'''\n"
    ));
    let out = h.run(
        &[
            "start",
            "--contract",
            "specs/add.contract.md",
            "--adapter",
            "fake",
        ],
        0,
    );
    assert!(out.stdout.contains("state=accepted"), "{}", out.stdout);
    assert!(
        !out.stderr.contains("inner git push succeeded"),
        "{}",
        out.stderr
    );
}

#[test]
fn tombstone_restores_events() {
    let h = Harness::new();
    h.set_adapter_fake();
    h.write_contract(&contract("src/**"));
    h.write_fake(&fake_toml_good());
    h.run(
        &[
            "start",
            "--contract",
            "specs/add.contract.md",
            "--adapter",
            "fake",
        ],
        0,
    );
    let env = h.envelope();
    for ev in env["events"].as_array().unwrap() {
        assert!(ev["id"].as_str().unwrap().starts_with("wx_"));
        assert!(ev["payload_sha256"].as_str().unwrap().len() == 64);
        assert!(ev["at"].as_str().unwrap().ends_with('Z'));
    }
}

#[test]
fn status_empty_and_unknown() {
    let h = Harness::new();
    let out = h.run(&["status"], 0);
    assert!(out.stdout.contains("No runs."), "{}", out.stdout);
    let missing = h.run(&["status", "wx_01AAAAAAAAAAAAAAAAAAAAAAAA"], 7);
    assert!(
        missing.stderr.contains("E_RUN_NOT_FOUND"),
        "{}",
        missing.stderr
    );
}

#[test]
fn writable_tests_warns_but_still_accepts() {
    let h = Harness::new();
    h.set_adapter_fake();
    let body = contract("src/**").replace(
        "allowed_globs:\n  - \"src/**\"",
        "allowed_globs:\n  - \"src/**\"\n  - \"tests/**\"",
    );
    h.write_contract(&body);
    h.write_fake(&fake_toml_good());
    let out = h.run(
        &[
            "start",
            "--contract",
            "specs/add.contract.md",
            "--adapter",
            "fake",
        ],
        0,
    );
    assert!(out.stdout.contains("state=accepted"), "{}", out.stdout);
    assert!(
        out.stderr.contains("E_CONTRACT_TESTS_WRITABLE"),
        "{}",
        out.stderr
    );
    let env = h.envelope();
    let events = env["events"].as_array().unwrap();
    assert!(events
        .iter()
        .any(|e| { e["kind"] == "error" && e["payload"]["code"] == "E_CONTRACT_TESTS_WRITABLE" }));
}

#[test]
fn doctor_strict_writable_tests() {
    let h = Harness::new();
    let body = contract("src/**").replace(
        "allowed_globs:\n  - \"src/**\"",
        "allowed_globs:\n  - \"src/**\"\n  - \"tests/**\"",
    );
    fs::write(h.path().join("specs/add.contract.md"), body).unwrap();
    let out = wanax()
        .args(["doctor", "--strict", "--data-dir"])
        .arg(h.data.path())
        .current_dir(h.path())
        .env("WANAX_COMMANDER_API_KEY", "test-not-a-real-key")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(4));
    let stderr = String::from_utf8_lossy(&out.stderr);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stderr.contains("E_CONTRACT_TESTS_WRITABLE")
            || stdout.contains("E_CONTRACT_TESTS_WRITABLE"),
        "stdout={stdout} stderr={stderr}"
    );
}

#[test]
fn doctor_strict_missing_key() {
    let h = Harness::new();
    let out = wanax()
        .args(["doctor", "--strict", "--data-dir"])
        .arg(h.data.path())
        .current_dir(h.path())
        .env_remove("WANAX_COMMANDER_API_KEY")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(8));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("E_MISSING_API_KEY"), "{stderr}");
}

#[test]
fn doctor_strict_cli_commander_skips_api_key() {
    let h = Harness::new();
    let script = install_cmd_script(h.data.path(), "claude-commander", "#!/bin/sh\nexit 0\n");
    let cfg = h.path().join(".wanax/config.toml");
    let mut t = fs::read_to_string(&cfg).unwrap();
    t = t.replacen(
        "[commander]\nprovider = \"openai_compat\"\nmodel = \"commander\"",
        &format!(
            "[commander]\nprovider = \"claude_cli\"\nmodel = \"commander\"\ncli_bin = \"{}\"",
            script.display()
        ),
        1,
    );
    fs::write(&cfg, t).unwrap();
    let out = wanax()
        .args(["doctor", "--strict", "--data-dir"])
        .arg(h.data.path())
        .current_dir(h.path())
        .env_remove("WANAX_COMMANDER_API_KEY")
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(
        out.status.code(),
        Some(0),
        "stdout={stdout} stderr={stderr}"
    );
    assert!(!stderr.contains("E_MISSING_API_KEY"), "{stderr}");
}

#[test]
fn no_web_tui_telemetry_in_sources() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut hits = Vec::new();
    walk(&root.join("crates"), &mut hits);
    assert!(hits.is_empty(), "forbidden sources: {hits:?}");
}

fn walk(dir: &Path, hits: &mut Vec<String>) {
    let Ok(rd) = fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            if p.file_name().and_then(|s| s.to_str()) == Some("target") {
                continue;
            }
            walk(&p, hits);
            continue;
        }
        if p.file_name().and_then(|s| s.to_str()) != Some("Cargo.toml") {
            continue;
        }
        let Ok(text) = fs::read_to_string(&p) else {
            continue;
        };
        for needle in [
            "ratatui",
            "crossterm",
            "axum",
            "actix-web",
            "sentry",
            "posthog",
        ] {
            if text.contains(needle) {
                hits.push(format!("{}: {needle}", p.display()));
            }
        }
    }
}

fn write_openai_cassette(dir: &Path, dispatch_instruction: &str) {
    let dispatch = serde_json::json!({
        "title": "add-fn",
        "instruction": dispatch_instruction,
    })
    .to_string();
    let verdict = serde_json::json!({
        "decision": "accept",
        "reason": "outer gates look fine",
        "files_reviewed": ["src/lib.rs"]
    })
    .to_string();
    let cassette = serde_json::json!({
        "provider": "openai",
        "calls": [
            {
                "body": {
                    "choices": [{"message": {"content": dispatch}}],
                    "usage": {"prompt_tokens": 21, "completion_tokens": 14}
                }
            },
            {
                "body": {
                    "choices": [{"message": {"content": verdict}}],
                    "usage": {"prompt_tokens": 18, "completion_tokens": 9}
                }
            }
        ]
    });
    fs::write(dir.join("cassette.json"), cassette.to_string()).unwrap();
}

#[test]
fn llm_fixture_accepts_and_records_usage() {
    let h = Harness::new();
    h.set_adapter_fake();
    let p = h.path().join(".wanax/config.toml");
    let mut t = fs::read_to_string(&p).unwrap();
    t = t.replace(
        "# empty model degrades self-review (Phase 2)\n",
        "model = \"inner\"\n",
    );
    fs::write(&p, t).unwrap();
    h.write_contract(&contract("src/**"));
    h.write_fake(&fake_toml_good());
    let cassette_dir = h.data.path().join("llm-cassette");
    fs::create_dir_all(&cassette_dir).unwrap();
    write_openai_cassette(
        &cassette_dir,
        "Implement add in src/lib.rs. Allowed: src/**. Forbidden: **/.env. test_command: cargo test. CC-01: cargo test exits 0",
    );
    let out = wanax()
        .args([
            "start",
            "--contract",
            "specs/add.contract.md",
            "--adapter",
            "fake",
            "--data-dir",
        ])
        .arg(h.data.path())
        .current_dir(h.path())
        .env("WANAX_LLM_FIXTURE_DIR", &cassette_dir)
        .env_remove("WANAX_COMMANDER_SCRIPT")
        .env_remove("WANAX_COMMANDER_API_KEY")
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "stdout={stdout} stderr={stderr}");
    assert!(stdout.contains("state=accepted"), "{stdout}");
    let env = h.envelope();
    assert_eq!(env["current_state"], "accepted");
    // Audit: the model's input prompt is persisted as a run artifact and
    // hash-linked into the budget_tick event that charged it.
    let ticks: Vec<&Value> = env["events"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["kind"] == "budget_tick" && e["payload"]["prompt_sha256"].is_string())
        .collect();
    assert!(!ticks.is_empty(), "no prompt-bearing budget_tick in {env}");
    let run_dir = h.path().join(format!(".wanax/runs/{}", h.envelope()["run_id"].as_str().unwrap()));
    let rel = ticks[0]["payload"]["prompt_path"].as_str().unwrap();
    let prompt = fs::read_to_string(h.path().join(rel)).unwrap();
    assert!(prompt.contains("== user =="), "prompt missing user section");
    assert!(prompt.contains("Boundaries"), "prompt missing contract: {prompt}");
    let sha = {
        use wanax_core::hashutil::sha256_hex;
        sha256_hex(prompt.as_bytes())
    };
    assert_eq!(ticks[0]["payload"]["prompt_sha256"], sha, "sha mismatch");
    let _ = run_dir;
    let events = env["events"].as_array().unwrap();
    let reviews: Vec<&Value> = events
        .iter()
        .filter(|e| e["kind"] == "state_changed" && e["payload"]["phase"] == "self_review")
        .collect();
    assert!(!reviews.is_empty(), "{events:?}");
    assert!(reviews
        .iter()
        .all(|e| e["payload"]["self_review"] == "degraded"));
    let ticks: Vec<&Value> = events
        .iter()
        .filter(|e| e["kind"] == "budget_tick")
        .collect();
    assert!(ticks.iter().any(|e| e["payload"]["cost_estimated"] == false
        && e["payload"]["prompt_tokens"] == 21
        && e["payload"]["completion_tokens"] == 14));
    assert!(ticks.iter().any(|e| e["payload"]["cost_estimated"] == false
        && e["payload"]["prompt_tokens"] == 18
        && e["payload"]["completion_tokens"] == 9));
}

fn install_cmd_script(dest_dir: &Path, name: &str, body: &str) -> PathBuf {
    let dest = dest_dir.join(name);
    fs::write(&dest, body).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&dest, fs::Permissions::from_mode(0o755)).unwrap();
    }
    dest
}

fn install_cmd_fixture(dest_dir: &Path) -> PathBuf {
    let src = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/write_add.sh");
    let dest = dest_dir.join("write_add.sh");
    fs::copy(&src, &dest).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&dest, fs::Permissions::from_mode(0o755)).unwrap();
    }
    dest
}

#[test]
fn start_creates_missing_data_dir() {
    let h = Harness::new();
    h.set_adapter_fake();
    h.write_contract(&contract("src/**"));
    h.write_fake(&fake_toml_good());
    let data = h.data.path().join("nested").join("store");
    assert!(!data.exists());
    let out = wanax()
        .args([
            "start",
            "--contract",
            "specs/add.contract.md",
            "--adapter",
            "fake",
            "--data-dir",
        ])
        .arg(&data)
        .current_dir(h.path())
        .env("WANAX_DATA_DIR", &data)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "stdout={stdout} stderr={stderr}");
    assert!(data.join("wanax.db").is_file());
}

#[test]
fn cmd_adapter_accepts() {
    let h = Harness::new();
    let script = install_cmd_fixture(h.data.path());
    h.set_adapter_cmd(&script);
    h.write_contract(&contract("src/**"));
    let out = h.run(
        &[
            "start",
            "--contract",
            "specs/add.contract.md",
            "--adapter",
            "cmd",
        ],
        0,
    );
    assert!(out.stdout.contains("state=accepted"), "{}", out.stdout);
    let env = h.envelope();
    assert_eq!(env["current_state"], "accepted");
}

#[test]
fn cmd_adapter_boundary_rejects() {
    let h = Harness::new();
    let script = install_cmd_script(
        h.data.path(),
        "write_boundary.sh",
        &format!(
            "#!/bin/sh\nset -eu\ncat > src/lib.rs << 'EOF'\n{GOOD_LIB}EOF\ncat > Cargo.toml << 'EOF'\n[package]\nname = \"fixture\"\nversion = \"0.2.0\"\nedition = \"2021\"\nEOF\n"
        ),
    );
    h.set_adapter_cmd(&script);
    h.write_contract(&contract("src/**"));
    let out = h.run(
        &[
            "start",
            "--contract",
            "specs/add.contract.md",
            "--adapter",
            "cmd",
        ],
        1,
    );
    assert!(
        out.stderr.contains("E_BOUNDARY") || out.stdout.contains("state=rejected"),
        "stdout={} stderr={}",
        out.stdout,
        out.stderr
    );
    assert!(!out.stdout.contains("state=accepted"));
}

#[test]
fn cmd_rewritten_tests_rejected_when_outside_globs() {
    let h = Harness::new();
    fs::write(
        h.path().join("src/lib.rs"),
        "pub fn add(a: i32, b: i32) -> i32 { unimplemented!() }\n",
    )
    .unwrap();
    fs::create_dir_all(h.path().join("tests")).unwrap();
    fs::write(
        h.path().join("tests/add.rs"),
        "#[test]\nfn test_add() { assert_eq!(fixture::add(2, 3), 5); }\n",
    )
    .unwrap();
    git(h.path(), &["add", "src/lib.rs", "tests/add.rs"]);
    git(h.path(), &["commit", "-m", "move tests out of src"]);
    let script = install_cmd_script(
        h.data.path(),
        "rewrite_tests.sh",
        "#!/bin/sh\nset -eu\ncat > src/lib.rs << 'EOF'\npub fn add(a: i32, b: i32) -> i32 { a + b }\nEOF\ncat > tests/add.rs << 'EOF'\n#[test]\nfn test_add() { assert!(true); }\nEOF\n",
    );
    h.set_adapter_cmd(&script);
    h.write_contract(&contract("src/**"));
    let out = h.run(
        &[
            "start",
            "--contract",
            "specs/add.contract.md",
            "--adapter",
            "cmd",
        ],
        1,
    );
    assert!(
        out.stderr.contains("E_BOUNDARY") || out.stdout.contains("state=rejected"),
        "stdout={} stderr={}",
        out.stdout,
        out.stderr
    );
    assert!(!out.stdout.contains("state=accepted"));
}

#[test]
fn cmd_adapter_missing_binary() {
    let h = Harness::new();
    h.set_adapter_cmd(Path::new("wanax-missing-cmd-binary-9f3a"));
    h.write_contract(&contract("src/**"));
    let out = h.run(
        &[
            "start",
            "--contract",
            "specs/add.contract.md",
            "--adapter",
            "cmd",
        ],
        8,
    );
    assert!(out.stderr.contains("E_ADAPTER_MISSING"), "{}", out.stderr);
}

#[test]
fn goal_stops_on_no_progress() {
    let h = Harness::new();
    h.set_adapter_fake();
    h.write_contract(&contract("src/**"));
    h.write_fake("turns = 1\nrun_tests = false\nclaimed_pass = true\n");
    let out = h.run(
        &[
            "start",
            "--contract",
            "specs/add.contract.md",
            "--adapter",
            "fake",
        ],
        1,
    );
    assert!(
        out.stdout.contains("state=escalate") || out.stderr.contains("E_REWORK_LIMIT"),
        "stdout={} stderr={}",
        out.stdout,
        out.stderr
    );
    let env = h.envelope();
    let events = env["events"].as_array().unwrap();
    let first_outer = events
        .iter()
        .position(|e| e["kind"] == "outer_test_started")
        .expect("outer test");
    let reviews_before_outer = events[..first_outer]
        .iter()
        .filter(|e| e["kind"] == "state_changed" && e["payload"]["phase"] == "self_review")
        .count();
    assert_eq!(reviews_before_outer, 1, "{events:?}");
}

fn setup_peer_crate(dir: &Path) {
    fs::write(
        dir.join("src/lib.rs"),
        "pub mod a;\npub mod b;\n\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn both() {\n        assert_eq!(super::a::x(), 1);\n        assert_eq!(super::b::y(), 2);\n    }\n}\n",
    )
    .unwrap();
    fs::write(dir.join("src/a.rs"), "pub fn x() -> i32 { unimplemented!() }\n").unwrap();
    fs::write(dir.join("src/b.rs"), "pub fn y() -> i32 { unimplemented!() }\n").unwrap();
}

fn peer_dispatch_script(overlap: bool) -> String {
    if overlap {
        r#"{
          "dispatch": {"peers": [
            {"title": "peer-a", "instruction": "implement a::x in src/a.rs", "allowed_globs": ["src/**"]},
            {"title": "peer-b", "instruction": "implement b::y in src/b.rs", "allowed_globs": ["src/a/**"]}
          ]},
          "verdicts": [{"decision": "accept", "reason": "ok", "files_reviewed": []}]
        }"#
        .into()
    } else {
        r#"{
          "dispatch": {"peers": [
            {"title": "peer-a", "instruction": "implement a::x in src/a.rs", "allowed_globs": ["src/a.rs"]},
            {"title": "peer-b", "instruction": "implement b::y in src/b.rs", "allowed_globs": ["src/b.rs"]}
          ]},
          "verdicts": [{"decision": "accept", "reason": "both modules pass", "files_reviewed": ["src/a.rs", "src/b.rs"]}]
        }"#
        .into()
    }
}

fn write_peer_fakes(h: &Harness) {
    fs::write(
        h.path().join(".wanax/fake-peer-1.toml"),
        "turns = 1\nrun_tests = false\n\n[[writes]]\npath = \"src/a.rs\"\ncontent = '''pub fn x() -> i32 { 1 }\n'''\n",
    )
    .unwrap();
    fs::write(
        h.path().join(".wanax/fake-peer-2.toml"),
        "turns = 1\nrun_tests = false\n\n[[writes]]\npath = \"src/b.rs\"\ncontent = '''pub fn y() -> i32 { 2 }\n'''\n",
    )
    .unwrap();
}

fn write_fake_gh(bin_root: &Path) -> PathBuf {
    let bin = bin_root.join("bin");
    fs::create_dir_all(&bin).unwrap();
    let gh = bin.join("gh");
    fs::write(
        &gh,
        "#!/bin/sh\necho \"https://github.com/example/wanax/pull/42\"\n",
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(&gh).unwrap().permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&gh, perms).unwrap();
    }
    bin
}

#[test]
fn peer_overlap_rejected() {
    let h = Harness::new();
    h.set_adapter_fake();
    setup_peer_crate(h.path());
    git(h.path(), &["add", "src"]);
    git(h.path(), &["commit", "-m", "peer crate"]);
    h.write_contract(&contract("src/**"));
    write_peer_fakes(&h);
    let script = h.data.path().join("peer-overlap.json");
    fs::write(&script, peer_dispatch_script(true)).unwrap();
    let out = wanax()
        .args([
            "start",
            "--contract",
            "specs/add.contract.md",
            "--adapter",
            "fake",
            "--data-dir",
        ])
        .arg(h.data.path())
        .current_dir(h.path())
        .env("WANAX_COMMANDER_SCRIPT", &script)
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_ne!(out.status.code(), Some(0));
    assert!(
        stderr.contains("E_PEER_OVERLAP"),
        "stderr={stderr} stdout={}",
        String::from_utf8_lossy(&out.stdout)
    );
}

#[test]
fn peer_disjoint_recovery_accepted() {
    let h = Harness::new();
    h.set_adapter_fake();
    setup_peer_crate(h.path());
    git(h.path(), &["add", "src"]);
    git(h.path(), &["commit", "-m", "peer crate"]);
    h.write_contract(&contract("src/**"));
    write_peer_fakes(&h);
    let script = h.data.path().join("peer-ok.json");
    fs::write(&script, peer_dispatch_script(false)).unwrap();
    let out = wanax()
        .args([
            "start",
            "--contract",
            "specs/add.contract.md",
            "--adapter",
            "fake",
            "--data-dir",
        ])
        .arg(h.data.path())
        .current_dir(h.path())
        .env("WANAX_COMMANDER_SCRIPT", &script)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(out.status.code(), Some(0), "stderr={}", String::from_utf8_lossy(&out.stderr));
    assert!(stdout.contains("state=accepted"), "{stdout}");
    let env = h.envelope();
    let peer_receipts = env["events"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["actor"] == "peer" && e["kind"] == "receipt_submitted")
        .count();
    assert_eq!(peer_receipts, 2);
}

#[test]
fn gh_pr_after_accept_outer_only() {
    let h = Harness::new();
    h.set_adapter_fake();
    h.write_contract(&contract("src/**"));
    h.write_fake(&fake_toml_good());
    let cfg = h.path().join(".wanax/config.toml");
    let mut t = fs::read_to_string(&cfg).unwrap();
    t.push_str("\n[github]\ncreate_pr = true\n");
    fs::write(&cfg, t).unwrap();
    let gh_bin = write_fake_gh(h.data.path());
    let path = std::env::var("PATH").unwrap_or_default();
    let out = wanax()
        .args([
            "start",
            "--contract",
            "specs/add.contract.md",
            "--adapter",
            "fake",
            "--data-dir",
        ])
        .arg(h.data.path())
        .current_dir(h.path())
        .env("PATH", format!("{}:{path}", gh_bin.display()))
        .env("GH_TOKEN", "test-token-outer-only")
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(out.status.code(), Some(0), "stderr={}", String::from_utf8_lossy(&out.stderr));
    assert!(stdout.contains("state=accepted"), "{stdout}");
    assert!(stdout.contains("pr=https://github.com/example/wanax/pull/42"), "{stdout}");
    let env = h.envelope();
    let pr_event = env["events"]
        .as_array()
        .unwrap()
        .iter()
        .any(|e| e["kind"] == "state_changed" && e["payload"]["github_pr"].is_string());
    assert!(pr_event);
}

#[test]
fn lang_zh_status_empty() {
    let h = Harness::new();
    let out = wanax()
        .args(["--lang", "zh", "status", "--data-dir"])
        .arg(h.data.path())
        .current_dir(h.path())
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(out.status.code(), Some(0), "{stdout}");
    assert!(stdout.contains("没有运行记录。"), "{stdout}");
    assert!(!stdout.contains("No runs."));
}

#[test]
fn resume_rejects_terminal_run() {
    let h = Harness::new();
    h.set_adapter_fake();
    h.write_contract(&contract("src/**"));
    h.write_fake(&fake_toml_good());
    let out = h.run(
        &[
            "start",
            "--contract",
            "specs/add.contract.md",
            "--adapter",
            "fake",
        ],
        0,
    );
    let run_id = out
        .stdout
        .lines()
        .next()
        .expect("run id")
        .trim()
        .to_string();
    let resume = h.run(&["resume", &run_id], 1);
    assert!(
        resume.stderr.contains("E_RESUME"),
        "stderr={}",
        resume.stderr
    );
}

#[test]
fn resume_after_kill_accepts() {
    let h = Harness::new();
    h.set_adapter_fake();
    h.write_contract(&contract("src/**"));
    h.write_fake(&format!(
        "turns = 1\nrun_tests = false\nsleep_ms = 60000\n\n[[writes]]\npath = \"src/lib.rs\"\ncontent = '''{GOOD_LIB}'''\n"
    ));
    let mut child = wanax()
        .args([
            "start",
            "--contract",
            "specs/add.contract.md",
            "--adapter",
            "fake",
            "--data-dir",
        ])
        .arg(h.data.path())
        .current_dir(h.path())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut run_id = String::new();
    for _ in 0..100 {
        std::thread::sleep(Duration::from_millis(100));
        if h.path().join(".wanax/LOCK").is_file() {
            if let Ok(rd) = fs::read_dir(h.path().join(".wanax/runs")) {
                if let Some(e) = rd.filter_map(|x| x.ok()).next() {
                    run_id = e.file_name().to_string_lossy().into_owned();
                    break;
                }
            }
        }
    }
    assert!(!run_id.is_empty(), "no run started");
    std::thread::sleep(Duration::from_millis(300));
    let _ = child.kill();
    let _ = child.wait();
    h.write_fake(&fake_toml_good());
    let out = h.run(&["resume", &run_id, "--adapter", "fake"], 0);
    assert!(
        out.stdout.contains("state=accepted"),
        "stdout={} stderr={}",
        out.stdout,
        out.stderr
    );
}

fn dag_dispatch_script() -> String {
    r#"{
      "dispatch": {"units": [
        {"id": "u1", "title": "add", "instruction": "implement add", "depends_on": [], "test_command": "cargo test test_add -- --exact"},
        {"id": "u2", "title": "mul", "instruction": "implement mul", "depends_on": ["u1"]}
      ]},
      "verdicts": [{"decision": "accept", "reason": "ok", "files_reviewed": ["src/lib.rs"]}]
    }"#
    .into()
}

#[test]
fn dag_units_run_in_dependency_order() {
    let h = Harness::new();
    h.set_adapter_fake();
    fs::write(
        h.path().join("src/lib.rs"),
        "pub fn add(a: i32, b: i32) -> i32 { unimplemented!() }\npub fn mul(a: i32, b: i32) -> i32 { unimplemented!() }\n\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn test_add() { assert_eq!(super::add(2, 3), 5); }\n    #[test]\n    fn test_mul() { assert_eq!(super::mul(2, 3), 6); }\n}\n",
    )
    .unwrap();
    git(h.path(), &["add", "src/lib.rs"]);
    git(h.path(), &["commit", "-m", "dag crate"]);
    h.write_contract(&contract("src/**"));
    fs::write(
        h.path().join(".wanax/fake-unit-1.toml"),
        "turns = 1\nrun_tests = false\n\n[[writes]]\npath = \"src/lib.rs\"\ncontent = '''pub fn add(a: i32, b: i32) -> i32 { a + b }\npub fn mul(a: i32, b: i32) -> i32 { unimplemented!() }\n\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn test_add() { assert_eq!(super::add(2, 3), 5); }\n    #[test]\n    fn test_mul() { assert_eq!(super::mul(2, 3), 6); }\n}\n'''\n",
    )
    .unwrap();
    fs::write(
        h.path().join(".wanax/fake-unit-2.toml"),
        "turns = 1\nrun_tests = false\n\n[[writes]]\npath = \"src/lib.rs\"\ncontent = '''pub fn add(a: i32, b: i32) -> i32 { a + b }\npub fn mul(a: i32, b: i32) -> i32 { a * b }\n\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn test_add() { assert_eq!(super::add(2, 3), 5); }\n    #[test]\n    fn test_mul() { assert_eq!(super::mul(2, 3), 6); }\n}\n'''\n",
    )
    .unwrap();
    let script = h.data.path().join("dag.json");
    fs::write(&script, dag_dispatch_script()).unwrap();
    let out = wanax()
        .args([
            "start",
            "--contract",
            "specs/add.contract.md",
            "--adapter",
            "fake",
            "--data-dir",
        ])
        .arg(h.data.path())
        .current_dir(h.path())
        .env("WANAX_COMMANDER_SCRIPT", &script)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(
        out.status.code(),
        Some(0),
        "stderr={} stdout={stdout}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(stdout.contains("state=accepted"), "{stdout}");
    let env = h.envelope();
    let dispatched = env["events"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["kind"] == "unit_dispatched")
        .count();
    assert!(dispatched >= 2, "{env}");
}

/// Event-sourced resume: wipe every work_units row for a mid-flight dag run
/// — the tombstone envelope still has unit_seeded/dispatched/receipt/verdict
/// evidence, so resume rebuilds the unit set and finishes the run.
#[test]
fn resume_heals_work_units_from_envelope() {
    let h = Harness::new();
    h.set_adapter_fake();
    fs::write(
        h.path().join("src/lib.rs"),
        "pub fn add(a: i32, b: i32) -> i32 { unimplemented!() }\npub fn mul(a: i32, b: i32) -> i32 { unimplemented!() }\n\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn test_add() { assert_eq!(super::add(2, 3), 5); }\n    #[test]\n    fn test_mul() { assert_eq!(super::mul(2, 3), 6); }\n}\n",
    )
    .unwrap();
    git(h.path(), &["add", "src/lib.rs"]);
    git(h.path(), &["commit", "-m", "dag crate"]);
    h.write_contract(&contract("src/**"));
    fs::write(
        h.path().join(".wanax/fake-unit-1.toml"),
        "turns = 1\nrun_tests = false\nsleep_ms = 60000\n\n[[writes]]\npath = \"src/lib.rs\"\ncontent = '''pub fn add(a: i32, b: i32) -> i32 { a + b }\npub fn mul(a: i32, b: i32) -> i32 { unimplemented!() }\n\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn test_add() { assert_eq!(super::add(2, 3), 5); }\n    #[test]\n    fn test_mul() { assert_eq!(super::mul(2, 3), 6); }\n}\n'''\n",
    )
    .unwrap();
    fs::write(
        h.path().join(".wanax/fake-unit-2.toml"),
        "turns = 1\nrun_tests = false\n\n[[writes]]\npath = \"src/lib.rs\"\ncontent = '''pub fn add(a: i32, b: i32) -> i32 { a + b }\npub fn mul(a: i32, b: i32) -> i32 { a * b }\n\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn test_add() { assert_eq!(super::add(2, 3), 5); }\n    #[test]\n    fn test_mul() { assert_eq!(super::mul(2, 3), 6); }\n}\n'''\n",
    )
    .unwrap();
    let script = h.data.path().join("dag.json");
    fs::write(&script, dag_dispatch_script()).unwrap();

    let mut child = wanax()
        .args([
            "start",
            "--contract",
            "specs/add.contract.md",
            "--adapter",
            "fake",
            "--data-dir",
        ])
        .arg(h.data.path())
        .current_dir(h.path())
        .env("WANAX_COMMANDER_SCRIPT", &script)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    // Wait until the envelope holds dag evidence (two seeds + a dispatch).
    let mut run_id = String::new();
    for _ in 0..300 {
        std::thread::sleep(Duration::from_millis(100));
        if let Ok(rd) = fs::read_dir(h.path().join(".wanax/runs")) {
            if let Some(e) = rd.filter_map(|x| x.ok()).next() {
                run_id = e.file_name().to_string_lossy().into_owned();
            }
        }
        if !run_id.is_empty() {
            let env_path =
                h.path().join(format!(".wanax/runs/{run_id}/envelope.json"));
            if let Ok(text) = fs::read_to_string(&env_path) {
                let seeds = text.matches("\"unit_seeded\"").count();
                if seeds >= 2 && text.contains("\"unit_dispatched\"") {
                    break;
                }
            }
        }
    }
    assert!(!run_id.is_empty(), "no run started");
    let _ = child.kill();
    let _ = child.wait();

    // The cache is gone — the envelope is all that remains.
    let db = h.data.path().join("wanax.db");
    let wiped = Command::new("sqlite3")
        .arg(&db)
        .arg("DELETE FROM work_units")
        .status()
        .expect("sqlite3");
    assert!(wiped.success());

    // Make the inner steps fast for the resumed run.
    for n in ["fake-unit-1.toml", "fake-unit-2.toml"] {
        let p = h.path().join(".wanax").join(n);
        let t = fs::read_to_string(&p).unwrap().replace("sleep_ms = 60000\n", "");
        fs::write(&p, t).unwrap();
    }
    let out = h.run(&["resume", &run_id, "--adapter", "fake"], 0);
    assert!(
        out.stdout.contains("state=accepted"),
        "stdout={} stderr={}",
        out.stdout,
        out.stderr
    );
    // Replay healed the cache: both dag units exist again in the DB.
    let healed = Command::new("sqlite3")
        .arg(&db)
        .arg("SELECT COUNT(*) FROM work_units")
        .output()
        .expect("sqlite3 count");
    assert_eq!(String::from_utf8_lossy(&healed.stdout).trim(), "2");
    // …and the divergence was logged to the tombstone.
    let env = h.envelope();
    assert!(
        env["events"].as_array().unwrap().iter().any(|e| {
            e["kind"] == "error" && e["payload"]["detail"].as_str()
                .unwrap_or("").contains("rebuilt from envelope")
        }),
        "{env}"
    );
}

/// DAG wave parallelism: independent ready units with disjoint allowed_globs
/// are dispatched together as one peer batch — one outer review accepts the
/// whole wave — then dependents run after.
#[test]
fn dag_ready_wave_runs_in_parallel() {
    let h = Harness::new();
    h.set_adapter_fake();
    h.write_contract(&contract("src/**"));
    // u1 + u2 independent & disjoint → one wave; u3 depends on both → after.
    // u1 also delivers the working lib.rs so the merged cargo test is green.
    let script = h.data.path().join("dagwave.json");
    fs::write(
        &script,
        r#"{
          "dispatch": {"units": [
            {"id": "u1", "title": "fix-lib", "instruction": "implement add/mul", "depends_on": [], "allowed_globs": ["src/lib.rs"]},
            {"id": "u2", "title": "add-b", "instruction": "write b.rs", "depends_on": [], "allowed_globs": ["src/b.rs"]},
            {"id": "u3", "title": "add-c", "instruction": "write c.rs", "depends_on": ["u1", "u2"], "allowed_globs": ["src/c.rs"]}
          ]},
          "verdicts": [
            {"decision": "accept", "reason": "wave ok", "files_reviewed": ["src/lib.rs", "src/b.rs"]},
            {"decision": "accept", "reason": "u3 ok", "files_reviewed": ["src/c.rs"]}
          ]
        }"#,
    )
    .unwrap();
    fs::write(
        h.path().join(".wanax/fake-peer-1.toml"),
        format!(
            "turns = 1\nrun_tests = false\n\n[[writes]]\npath = \"src/lib.rs\"\ncontent = '''{GOOD_LIB}'''\n"
        ),
    )
    .unwrap();
    fs::write(
        h.path().join(".wanax/fake-peer-2.toml"),
        "turns = 1\nrun_tests = false\n\n[[writes]]\npath = \"src/b.rs\"\ncontent = '''pub fn b() -> i32 { 2 }\n'''\n",
    )
    .unwrap();
    fs::write(
        h.path().join(".wanax/fake-unit-3.toml"),
        "turns = 1\nrun_tests = false\n\n[[writes]]\npath = \"src/c.rs\"\ncontent = '''pub fn c() -> i32 { 3 }\n'''\n",
    )
    .unwrap();
    let out = wanax()
        .args([
            "start",
            "--contract",
            "specs/add.contract.md",
            "--adapter",
            "fake",
            "--data-dir",
        ])
        .arg(h.data.path())
        .current_dir(h.path())
        .env("WANAX_COMMANDER_SCRIPT", &script)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(
        out.status.code(),
        Some(0),
        "stderr={} stdout={stdout}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(stdout.contains("state=accepted"), "{stdout}");

    let env = h.envelope();
    let events = env["events"].as_array().unwrap();
    // One peer_batch dispatch covering u1+u2.
    assert!(
        events.iter().any(|e| e["kind"] == "unit_dispatched"
            && e["payload"]["mode"] == "peer_batch"
            && e["payload"]["peer_count"] == 2),
        "{env}"
    );
    // u3's single dispatch happens after the peer receipts.
    let last_peer_receipt = events
        .iter()
        .rposition(|e| e["kind"] == "receipt_submitted" && e["actor"] == "peer")
        .expect("peer receipts");
    let u3_dispatch = events
        .iter()
        .position(|e| {
            e["kind"] == "unit_dispatched" && e["payload"]["title"] == "add-c"
        })
        .expect("u3 dispatch");
    assert!(u3_dispatch > last_peer_receipt, "{env}");
    // All three changes landed on the inner branch: wave lib.rs+b.rs, then c.rs.
    let run_id = env["run_id"].as_str().unwrap();
    let tree = Command::new("git")
        .args(["ls-tree", "-r", "--name-only", &format!("wanax/{run_id}/inner"), "src/"])
        .current_dir(h.path())
        .output()
        .unwrap();
    let files = String::from_utf8_lossy(&tree.stdout);
    for f in ["src/b.rs", "src/c.rs"] {
        assert!(files.contains(f), "{files}");
    }
    let lib = Command::new("git")
        .args(["show", &format!("wanax/{run_id}/inner:src/lib.rs")])
        .current_dir(h.path())
        .output()
        .unwrap();
    assert!(
        String::from_utf8_lossy(&lib.stdout).contains("a + b"),
        "wave never delivered the lib fix"
    );
}

/// A reworked wave must re-create peer branches/worktrees without colliding
/// with leftovers from the first attempt.
#[test]
fn dag_wave_rework_is_idempotent() {
    let h = Harness::new();
    h.set_adapter_fake();
    h.write_contract(&contract("src/**"));
    let script = h.data.path().join("dagwave2.json");
    fs::write(
        &script,
        r#"{
          "dispatch": {"units": [
            {"id": "u1", "title": "fix-lib", "instruction": "implement add", "depends_on": [], "allowed_globs": ["src/lib.rs"]},
            {"id": "u2", "title": "add-b", "instruction": "write b.rs", "depends_on": [], "allowed_globs": ["src/b.rs"]}
          ]},
          "verdicts": [
            {"decision": "rework", "reason": "not quite", "files_reviewed": ["src/lib.rs"]},
            {"decision": "accept", "reason": "ok", "files_reviewed": ["src/lib.rs", "src/b.rs"]}
          ]
        }"#,
    )
    .unwrap();
    fs::write(
        h.path().join(".wanax/fake-peer-1.toml"),
        format!("turns = 1\nrun_tests = false\n\n[[writes]]\npath = \"src/lib.rs\"\ncontent = '''{GOOD_LIB}'''\n"),
    )
    .unwrap();
    fs::write(
        h.path().join(".wanax/fake-peer-2.toml"),
        "turns = 1\nrun_tests = false\n\n[[writes]]\npath = \"src/b.rs\"\ncontent = '''pub fn b() -> i32 { 2 }\n'''\n",
    )
    .unwrap();
    let out = wanax()
        .args([
            "start",
            "--contract",
            "specs/add.contract.md",
            "--adapter",
            "fake",
            "--data-dir",
        ])
        .arg(h.data.path())
        .current_dir(h.path())
        .env("WANAX_COMMANDER_SCRIPT", &script)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(
        out.status.code(),
        Some(0),
        "stderr={} stdout={stdout}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(stdout.contains("state=accepted"), "{stdout}");
    assert!(stdout.contains("state=rework"), "{stdout}");
}

/// bound_test criteria are mechanical: the named test must exist in
/// `cargo test -- --list` (no vacuous 0-match pass) and exit 0 — otherwise
/// an accept verdict gets enforced down to rework.
#[test]
fn bound_test_blocks_vacuous_accept() {
    let h = Harness::new();
    h.set_adapter_fake();
    let mut body = contract("src/**");
    body = body.replace(
        "- CC-01: cargo test exits 0",
        "- CC-01: cargo test exits 0\n- CC-02: the named test must exist (bound_test: `test_new_thing`)",
    );
    h.write_contract(&body);
    // GOOD_LIB passes `cargo test` (it has test_add) but lacks test_new_thing.
    h.write_fake(&fake_toml_good());
    let out = h.run(
        &[
            "start",
            "--contract",
            "specs/add.contract.md",
            "--adapter",
            "fake",
        ],
        1,
    );
    assert!(out.stdout.contains("state=escalate"), "{}", out.stdout);
    let env = h.envelope();
    let bt = env["events"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["kind"] == "bound_test")
        .expect("bound_test event");
    assert_eq!(bt["payload"]["ok"], false, "{env}");
    assert_eq!(bt["payload"]["listed"], false, "{env}");
    // Outer cargo test was green yet the verdict was rework — the bound gate.
    assert!(
        env["events"].as_array().unwrap().iter().any(|e| e["kind"]
            == "verdict"
            && e["payload"]["decision"] == "rework"
            && e["payload"]["outer_test_exit_code"] == 0),
        "{env}"
    );
}

/// wanax.log is a real run activity log: lifecycle lines land there
/// (run started, worker spawn, gates, verdict, terminal state).
#[test]
fn run_log_records_lifecycle() {
    let h = Harness::new();
    h.set_adapter_fake();
    h.write_contract(&contract("src/**"));
    h.write_fake(&fake_toml_good());
    let out = h.run(
        &["start", "--contract", "specs/add.contract.md", "--adapter", "fake"],
        0,
    );
    let run_id = out.stdout.lines().next().unwrap().to_string();
    let log = find_file(h.path(), "wanax.log").expect("wanax.log");
    let text = fs::read_to_string(log).unwrap_or_default();
    for needle in [
        "started contract=",
        "worker spawned",
        "verdict proposed=accept",
        "accepted spent=",
    ] {
        assert!(text.contains(needle), "wanax.log missing {needle}:\n{text}");
    }
    let _ = run_id;
}

/// `wanax watch` polls to terminal state: exit 0 on accepted, exit 1
/// on a non-accept terminal, timeout path is covered by a short bound.
#[test]
fn watch_polls_to_terminal() {
    let h = Harness::new();
    h.set_adapter_fake();
    h.write_contract(&contract("src/**"));
    h.write_fake(&fake_toml_good());
    let out = h.run(
        &["start", "--contract", "specs/add.contract.md", "--adapter", "fake"],
        0,
    );
    let run_id = out.stdout.lines().next().unwrap().to_string();
    // Already-terminal run exits immediately with success.
    let out = h.run(&["watch", &run_id, "--interval-secs", "1"], 0);
    assert!(out.stdout.contains("accepted"), "{}", out.stdout);
    // A nonexistent run id errors through get_run.
    let out = h.run(&["watch", "wx_doesnotexist", "--interval-secs", "1"], 7);
    assert!(out.stderr.contains("E_RUN_NOT_FOUND"), "{}", out.stderr);
}

/// `wanax diff` shows the inner branch diff — stat by default, --patch
/// for the full unified diff.
#[test]
fn diff_shows_inner_changes() {
    let h = Harness::new();
    h.set_adapter_fake();
    h.write_contract(&contract("src/**"));
    h.write_fake(&fake_toml_good());
    let out = h.run(
        &["start", "--contract", "specs/add.contract.md", "--adapter", "fake"],
        0,
    );
    let run_id = out.stdout.lines().next().unwrap().to_string();
    let out = h.run(&["diff", &run_id], 0);
    assert!(out.stdout.contains("src/lib.rs"), "{}", out.stdout);
    let out = h.run(&["diff", &run_id, "--patch"], 0);
    assert!(out.stdout.contains("diff --git"), "{}", out.stdout);
    assert!(out.stdout.contains("pub fn add"), "{}", out.stdout);
}

/// [worker] pass_env copies allowed keys into the worker child — agent
/// CLIs can carry their own credentials — while FORBIDDEN_ENV names
/// (WANAX_*_API_KEY etc.) still lose unconditionally.
#[test]
fn worker_pass_env_allows_keys_and_still_blocks_forbidden() {
    let h = Harness::new();
    h.set_adapter_fake();
    let p = h.path().join(".wanax/config.toml");
    let mut t = fs::read_to_string(&p).unwrap();
    t = t.replace(
        "adapter = \"fake\"",
        "adapter = \"cmd\"\ncmd = \"sh\"\ncmd_args = [\"-c\", \"printf %s $MY_AGENT_KEY > src/env.proof; printf %s ${WANAX_INNER_API_KEY:-EMPTY} >> src/env.proof; printf 'pub fn z() -> i32 { 9 }\\n' > src/lib.rs\"]\npass_env = [\"MY_AGENT_KEY\", \"WANAX_INNER_API_KEY\"]",
    );
    fs::write(&p, t).unwrap();
    h.write_contract(&contract("src/**"));
    // The passthrough key lives in the parent env; WANAX_INNER_API_KEY too,
    // to prove forbidden always wins over pass_env.
    std::env::set_var("MY_AGENT_KEY", "sekrit");
    std::env::set_var("WANAX_INNER_API_KEY", "must-not-leak");
    let out = h.run(
        &[
            "start",
            "--contract",
            "specs/add.contract.md",
            "--adapter",
            "cmd",
        ],
        0,
    );
    std::env::remove_var("MY_AGENT_KEY");
    std::env::remove_var("WANAX_INNER_API_KEY");
    assert!(out.stdout.contains("state=accepted"), "{}", out.stdout);
    // The proof lands inside the worktree (src/ is allowed) and ends up on
    // the inner branch like any other artifact.
    let run_id = out.stdout.lines().next().unwrap().to_string();
    let proof = Command::new("git")
        .args(["-C", &h.path().display().to_string(), "show",
               &format!("wanax/{run_id}/inner:src/env.proof")])
        .output()
        .expect("git show");
    let proof = String::from_utf8_lossy(&proof.stdout).into_owned();
    assert!(proof.starts_with("sekrit"), "proof={proof}");
    assert!(proof.ends_with("EMPTY"), "proof={proof}");
}

/// `status --json` emits machine-readable run detail; `doctor` resolves
/// the acp adapter binary instead of falling through to octoscode.
#[test]
fn status_json_and_doctor_acp() {
    let h = Harness::new();
    h.set_adapter_fake();
    h.write_contract(&contract("src/**"));
    h.write_fake(&fake_toml_good());
    let out = h.run(
        &["start", "--contract", "specs/add.contract.md", "--adapter", "fake"],
        0,
    );
    let run_id = out.stdout.lines().next().unwrap().to_string();
    let out = h.run(&["status", &run_id, "--json"], 0);
    let v: Value = serde_json::from_str(out.stdout.trim()).unwrap();
    assert_eq!(v["run_id"], run_id);
    assert_eq!(v["state"], "accepted");
    assert_eq!(v["units"][0]["state"], "accepted");
    let out = h.run(&["status", "--json"], 0);
    let v: Value = serde_json::from_str(out.stdout.trim()).unwrap();
    assert!(
        v["runs"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["run_id"] == run_id),
        "{}",
        out.stdout
    );
    // doctor: adapter=acp must check acp_bin, not octoscode.
    let p = h.path().join(".wanax/config.toml");
    let mut t = fs::read_to_string(&p).unwrap();
    t = t.replace(
        "adapter = \"fake\"",
        "adapter = \"acp\"\nacp_bin = \"definitely-not-installed-acp-bin\"",
    );
    fs::write(&p, t).unwrap();
    let out = h.run(&["doctor"], 0);
    assert!(
        out.stdout.contains("adapter definitely-not-installed-acp-bin"),
        "doctor should check acp_bin: {}",
        out.stdout
    );
}

/// `wanax replay` reconstructs unit state from the envelope and
/// reconciles it against the DB cache — human view into resume's truth.
#[test]
fn replay_reconstructs_and_reconciles() {
    let h = Harness::new();
    h.set_adapter_fake();
    h.write_contract(&contract("src/**"));
    h.write_fake(&fake_toml_good());
    let out = h.run(
        &["start", "--contract", "specs/add.contract.md", "--adapter", "fake"],
        0,
    );
    let run_id = out.stdout.lines().next().unwrap().to_string();
    let out = h.run(&["replay", &run_id], 0);
    assert!(out.stdout.contains("unit(s) reconstructed"), "{}", out.stdout);
    assert!(out.stdout.contains("divergence: none"), "{}", out.stdout);
    let out = h.run(&["replay", &run_id, "--json"], 0);
    let v: Value = serde_json::from_str(out.stdout.trim()).unwrap();
    assert_eq!(v["envelope_units"].as_u64().unwrap(), 1);
    assert_eq!(v["diverged"], false);
    assert_eq!(v["units"][0]["state"], "accepted");
    // Wipe the cache — replay still reconstructs and reports divergence.
    let wiped = Command::new("sqlite3")
        .arg(h.data.path().join("wanax.db"))
        .arg(format!("DELETE FROM work_units WHERE run_id = '{run_id}'"))
        .output()
        .expect("sqlite3");
    assert!(wiped.status.success(), "{}", String::from_utf8_lossy(&wiped.stderr));
    let out = h.run(&["replay", &run_id], 0);
    assert!(out.stdout.contains("missing in DB"), "{}", out.stdout);
    assert!(out.stdout.contains("divergence: 1"), "{}", out.stdout);
}

/// must_have criteria are mechanical: "add module X" that never creates
/// X is enforced down to rework even when tests are green.
#[test]
fn must_have_blocks_incomplete_accept() {
    let h = Harness::new();
    h.set_adapter_fake();
    let mut body = contract("src/**");
    body = body.replace(
        "- CC-01: cargo test exits 0",
        "- CC-01: cargo test exits 0\n- CC-02: new module exists (must_have: `src/new.rs`)",
    );
    h.write_contract(&body);
    // The worker fixes lib.rs but never creates src/new.rs.
    h.write_fake(&fake_toml_good());
    let out = h.run(
        &[
            "start",
            "--contract",
            "specs/add.contract.md",
            "--adapter",
            "fake",
        ],
        1,
    );
    assert!(out.stdout.contains("state=escalate"), "{}", out.stdout);
    let env = h.envelope();
    let mh = env["events"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["kind"] == "must_have")
        .expect("must_have event");
    assert!(
        mh["payload"]["missing"]
            .as_array()
            .unwrap()
            .iter()
            .any(|m| m.as_str().unwrap_or("").contains("src/new.rs")),
        "{env}"
    );
    assert!(
        env["events"].as_array().unwrap().iter().any(|e| e["kind"]
            == "verdict"
            && e["payload"]["decision"] == "rework"),
        "{env}"
    );
}

/// And the positive: a worker that creates the must_have file accepts.
#[test]
fn must_have_accepts_when_file_created() {
    let h = Harness::new();
    h.set_adapter_fake();
    let mut body = contract("src/**");
    body = body.replace(
        "- CC-01: cargo test exits 0",
        "- CC-01: cargo test exits 0 (must_have: `src/new.rs`)",
    );
    h.write_contract(&body);
    h.write_fake(&format!(
        "turns = 1\nrun_tests = false\n\n[[writes]]\npath = \"src/lib.rs\"\ncontent = '''{GOOD_LIB}'''\n\n[[writes]]\npath = \"src/new.rs\"\ncontent = '''pub fn new_mod() -> i32 {{ 7 }}\n'''\n"
    ));
    let out = h.run(
        &[
            "start",
            "--contract",
            "specs/add.contract.md",
            "--adapter",
            "fake",
        ],
        0,
    );
    assert!(out.stdout.contains("state=accepted"), "{}", out.stdout);
    // No must_have failure event when the criterion is satisfied.
    let env = h.envelope();
    assert!(
        !env["events"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["kind"] == "must_have"),
        "{env}"
    );
}

/// `wanax check` fails a must_have the worker can't write (outside
/// allowed_globs = impossible contract), and warns on glob forms.
#[test]
fn check_fails_must_have_outside_allowed() {
    let h = Harness::new();
    h.set_adapter_fake();
    let mut body = contract("src/**");
    body = body.replace(
        "- CC-01: cargo test exits 0",
        "- CC-01: cargo test exits 0\n- CC-02: docs exist (must_have: `docs/spec.md`)",
    );
    h.write_contract(&body);
    let out = h.run(&["check", "--contract", "specs/add.contract.md"], 4);
    assert!(out.stdout.contains("must"), "{}", out.stdout);
    assert!(out.stdout.contains("cannot create"), "{}", out.stdout);
}

/// And the positive: a bound test that exists and passes accepts cleanly.
#[test]
fn bound_test_accepts_when_test_exists() {
    let h = Harness::new();
    h.set_adapter_fake();
    let mut body = contract("src/**");
    body = body.replace(
        "- CC-01: cargo test exits 0",
        "- CC-01: cargo test exits 0 (bound_test: `test_add`)",
    );
    h.write_contract(&body);
    h.write_fake(&fake_toml_good());
    let out = h.run(
        &[
            "start",
            "--contract",
            "specs/add.contract.md",
            "--adapter",
            "fake",
        ],
        0,
    );
    assert!(out.stdout.contains("state=accepted"), "{}", out.stdout);
    let env = h.envelope();
    let bt = env["events"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["kind"] == "bound_test")
        .expect("bound_test event");
    assert_eq!(bt["payload"]["ok"], true, "{env}");
    assert_eq!(bt["payload"]["listed"], true, "{env}");
}

/// egress = "allowlist": the run starts an allowlist proxy, confines the
/// inner sandbox to localhost-only outbound, injects proxy env into the
/// worker, and records the mode in run_started.
#[test]
fn egress_allowlist_confines_worker_network() {
    let h = Harness::new();
    h.set_adapter_fake();
    let p = h.path().join(".wanax/config.toml");
    let mut t = fs::read_to_string(&p).unwrap();
    t = t.replace(
        "adapter = \"fake\"",
        "adapter = \"fake\"\negress = \"allowlist\"\negress_allow = [\"api.anthropic.com\"]",
    );
    fs::write(&p, t).unwrap();
    h.write_contract(&contract("src/**"));
    h.write_fake(&fake_toml_good());
    let out = h.run(
        &["start", "--contract", "specs/add.contract.md", "--adapter", "fake"],
        0,
    );
    assert!(out.stdout.contains("state=accepted"), "{}", out.stdout);

    let env = h.envelope();
    let started = env["events"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["kind"] == "run_started")
        .expect("run_started");
    let egress = started["payload"]["egress"].as_str().unwrap_or("");
    assert!(egress.starts_with("allowlist:"), "{env}");

    // The inner sandbox profile denies outbound except the local proxy —
    // the `.sb` artifact is Seatbelt-only (bwrap confinement is argv).
    #[cfg(target_os = "macos")]
    {
        let runs = h.path().join(".wanax/runs");
        let profile = fs::read_dir(&runs)
            .unwrap()
            .filter_map(|e| e.ok())
            .find_map(|e| {
                let p = e.path().join("sandbox-inner.sb");
                p.is_file().then(|| fs::read_to_string(p).unwrap())
            })
            .expect("inner sb profile");
        assert!(
            profile.contains("(deny network-outbound)")
                && profile.contains("(remote tcp \"localhost:"),
            "{profile}"
        );
    }
}

/// egress = "isolated": no proxy, no network at all.
#[test]
fn egress_isolated_denies_all_network() {
    let h = Harness::new();
    h.set_adapter_fake();
    let p = h.path().join(".wanax/config.toml");
    let mut t = fs::read_to_string(&p).unwrap();
    t = t.replace(
        "adapter = \"fake\"",
        "adapter = \"fake\"\negress = \"isolated\"",
    );
    fs::write(&p, t).unwrap();
    h.write_contract(&contract("src/**"));
    h.write_fake(&fake_toml_good());
    let out = h.run(
        &["start", "--contract", "specs/add.contract.md", "--adapter", "fake"],
        0,
    );
    assert!(out.stdout.contains("state=accepted"), "{}", out.stdout);
    #[cfg(target_os = "macos")]
    {
        let runs = h.path().join(".wanax/runs");
        let profile = fs::read_dir(&runs)
            .unwrap()
            .filter_map(|e| e.ok())
            .find_map(|e| {
                let p = e.path().join("sandbox-inner.sb");
                p.is_file().then(|| fs::read_to_string(p).unwrap())
            })
            .expect("inner sb profile");
        assert!(profile.contains("(deny network*)"), "{profile}");
    }
}

/// `[run] notify_cmd` fires once on terminal state with WANAX_* env —
/// and never fires on a cancelled lock-holder mid-flight.
#[test]
fn notify_cmd_fires_on_terminal_state() {
    let h = Harness::new();
    h.set_adapter_fake();
    h.write_contract(&contract("src/**"));
    h.write_fake(&fake_toml_good());
    let p = h.path().join(".wanax/config.toml");
    let mut t = fs::read_to_string(&p).unwrap();
    t.push_str("\n[run]\nnotify_cmd = \"printf %s \\\"$WANAX_STATE $WANAX_RUN_ID\\\" > notify.out\"\n");
    fs::write(&p, t).unwrap();
    git(h.path(), &["add", ".wanax/config.toml"]);
    git(h.path(), &["commit", "-m", "notify config"]);
    let out = h.run(
        &["start", "--contract", "specs/add.contract.md", "--adapter", "fake"],
        0,
    );
    let run_id = out.stdout.lines().next().unwrap().to_string();
    // The hook is fire-and-forget — give it a beat to land.
    let f = h.path().join("notify.out");
    for _ in 0..20 {
        if f.is_file() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(150));
    }
    let got = fs::read_to_string(&f).expect("notify hook never ran");
    assert_eq!(got, format!("accepted {run_id}"), "{got}");
}

/// `wanax export` bundles the run's audit evidence into one directory.
#[test]
fn export_bundles_tombstone_diff_and_manifest() {
    let h = Harness::new();
    h.set_adapter_fake();
    h.write_contract(&contract("src/**"));
    h.write_fake(&fake_toml_good());
    let out = h.run(
        &["start", "--contract", "specs/add.contract.md", "--adapter", "fake"],
        0,
    );
    let run_id = out.stdout.lines().next().unwrap().to_string();
    let out = h.run(&["export", &run_id], 0);
    assert!(out.stdout.contains("exported"), "{}", out.stdout);
    let dir = h.path().join(format!(".wanax/exports/{run_id}"));
    for f in [
        "envelope.json",
        "TOMBSTONE.md",
        "contract.md",
        "replay.json",
        "inner.diff",
        "manifest.json",
    ] {
        assert!(dir.join(f).is_file(), "missing {f}");
    }
    let diff = fs::read_to_string(dir.join("inner.diff")).unwrap();
    assert!(diff.contains("pub fn add"), "{diff}");
    let manifest: Value =
        serde_json::from_str(&fs::read_to_string(dir.join("manifest.json")).unwrap()).unwrap();
    assert_eq!(manifest["state"], "accepted");
    assert_eq!(manifest["verdicts"], serde_json::json!(["accept"]));
    assert!(manifest["inner_diff"].as_bool().unwrap());
}

/// ACP adapter: an NDJSON-RPC stub agent drives initialize → session/new →
/// session/prompt end to end — the prompt asks permission (we answer with
/// the allow option), writes the fix, streams a chunk, and reports usage.
#[test]
fn acp_adapter_drives_agent_through_rpc() {
    let h = Harness::new();
    h.write_contract(&contract("src/**"));
    // ACP stub: speaks newline-delimited JSON-RPC, waits for the
    // permission reply before writing (proves request→response works).
    let stub = r#"import json,sys
def send(o):
    sys.stdout.write(json.dumps(o)+"\n"); sys.stdout.flush()
LIB = '''@LIB@'''
for line in sys.stdin:
    line=line.strip()
    if not line: continue
    m=json.loads(line)
    mid=m.get("id"); meth=m.get("method")
    if meth=="initialize":
        send({"jsonrpc":"2.0","id":mid,"result":{"protocolVersion":1,"agentCapabilities":{}}})
    elif meth=="session/new":
        send({"jsonrpc":"2.0","id":mid,"result":{"sessionId":"s1"}})
    elif meth=="session/prompt":
        send({"jsonrpc":"2.0","id":99,"method":"session/request_permission","params":{"sessionId":"s1","options":[{"optionId":"allow","kind":"allow_always"}]}})
        for r in sys.stdin:
            m2=json.loads(r)
            if m2.get("id")==99 and "result" in m2: break
        open("src/lib.rs","w").write(LIB)
        send({"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"s1","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"wrote lib.rs"}}}})
        send({"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"s1","update":{"sessionUpdate":"usage_update","usedTokens":42,"cost":{"amount":0.001,"currency":"USD"}}}})
        send({"jsonrpc":"2.0","id":mid,"result":{"stopReason":"end_turn"}})
        break
"#
    .replace("@LIB@", GOOD_LIB);
    let stub_path = h.path().join("acp_stub.py");
    fs::write(&stub_path, &stub).unwrap();
    git(h.path(), &["add", "acp_stub.py"]);
    git(h.path(), &["commit", "-m", "acp stub"]);
    // [worker] adapter=acp, acp_bin=python3, acp_args=[stub]
    let p = h.path().join(".wanax/config.toml");
    let mut t = fs::read_to_string(&p).unwrap();
    t = t.replace(
        "adapter = \"octoscode\"",
        &format!(
            "adapter = \"acp\"\nacp_bin = \"python3\"\nacp_args = [\"{}\"]",
            stub_path.display()
        ),
    );
    fs::write(&p, t).unwrap();
    let out = h.run(
        &["start", "--contract", "specs/add.contract.md", "--adapter", "acp"],
        0,
    );
    assert!(
        out.stdout.contains("state=accepted"),
        "stdout={} stderr={}",
        out.stdout,
        out.stderr
    );
    // The stub's file made it into the inner branch.
    let env = h.envelope();
    let run_id = env["run_id"].as_str().unwrap();
    let lib = Command::new("git")
        .args(["show", &format!("wanax/{run_id}/inner:src/lib.rs")])
        .current_dir(h.path())
        .output()
        .unwrap();
    assert!(
        String::from_utf8_lossy(&lib.stdout).contains("a + b"),
        "acp worker's write never reached the inner branch"
    );
}

/// `wanax audit` verifies the tombstone: an intact envelope passes; a
/// tampered payload is caught by the per-event hash.
#[test]
fn audit_verifies_tombstone_integrity() {
    let h = Harness::new();
    h.set_adapter_fake();
    h.write_contract(&contract("src/**"));
    h.write_fake(&fake_toml_good());
    let out = h.run(
        &["start", "--contract", "specs/add.contract.md", "--adapter", "fake"],
        0,
    );
    assert!(out.stdout.contains("state=accepted"));
    let run_id = out.stdout.lines().next().unwrap().to_string();

    let out = h.run(&["audit", &run_id], 0);
    assert!(out.stdout.contains("integrity: OK"), "{}", out.stdout);

    // Tamper: flip a payload field inside envelope.json — the stored
    // payload_sha256 must catch it.
    let env_path = h
        .path()
        .join(format!(".wanax/runs/{run_id}/envelope.json"));
    let mut env: Value =
        serde_json::from_str(&fs::read_to_string(&env_path).unwrap()).unwrap();
    env["events"][0]["payload"]["injected"] = serde_json::json!("tampered");
    fs::write(&env_path, serde_json::to_string_pretty(&env).unwrap()).unwrap();
    let out = h.run(&["audit", &run_id], 1);
    assert!(
        out.stdout.contains("payload_sha256 mismatch"),
        "{}",
        out.stdout
    );
}

/// Secret content scan: a worker that writes an API key into src/lib.rs
/// passes the path boundary but the accept gate still rejects the diff —
/// path globs can't see content.
#[test]
fn secret_in_diff_rejects_accept() {
    let h = Harness::new();
    h.set_adapter_fake();
    h.write_contract(&contract("src/**"));
    h.write_fake(&format!(
        "turns = 1\nrun_tests = false\n\n[[writes]]\npath = \"src/lib.rs\"\ncontent = '''{GOOD_LIB}\nconst K: &str = \"sk-ABCDEFGHIJKLMNOPQRSTUVWXYZ12\";\n'''\n"
    ));
    let out = h.run(
        &["start", "--contract", "specs/add.contract.md", "--adapter", "fake"],
        1,
    );
    assert!(
        out.stdout.contains("state=rejected"),
        "stdout={} stderr={}",
        out.stdout,
        out.stderr
    );
    let env = h.envelope();
    let leak = env["events"].as_array().unwrap().iter().any(|e| {
        e["kind"] == "error"
            && e["payload"]["code"] == "E_SECRET_LEAK"
            && e["payload"]["findings"]
                .as_array()
                .is_some_and(|f| f.iter().any(|x| x["kind"] == "openai-key"))
    });
    assert!(leak, "{env}");
    // The secret itself never lands in the tombstone.
    assert!(!env.to_string().contains("sk-ABCDEFGHIJKLMNOPQRSTUVWXYZ12"));
}

/// `wanax check`: a sound contract passes; a contract that names a missing
/// bound_test fails with exit != 0 (it could never be accepted).
#[test]
fn check_reports_contract_health() {
    let h = Harness::new();
    h.set_adapter_fake();
    h.write_contract(&contract("src/**"));
    let out = h.run(&["check", "--contract", "specs/add.contract.md", "--adapter", "fake"], 0);
    assert!(out.stdout.contains("result: PASS"), "{}", out.stdout);
}

#[test]
fn check_warns_when_bound_test_missing() {
    let h = Harness::new();
    h.set_adapter_fake();
    let c = contract("src/**").replace(
        "cargo test exits 0",
        "cargo test exits 0\n- CC-02: never green (bound_test: `definitely_missing_test`)",
    );
    h.write_contract(&c);
    // WARN not FAIL: "the work is supposed to create the test" is a
    // legitimate pattern — a typo still fails the outer gate, not the door.
    let out = h.run(&["check", "--contract", "specs/add.contract.md", "--adapter", "fake"], 0);
    assert!(
        out.stdout.contains("definitely_missing_test")
            && out.stdout.contains("not yet listed"),
        "stdout={} stderr={}",
        out.stdout,
        out.stderr
    );
}

#[test]
fn check_warns_when_allowed_globs_cover_tests() {
    let h = Harness::new();
    h.set_adapter_fake();
    h.write_contract(&contract("tests/**"));
    let out = h.run(&["check", "--contract", "specs/add.contract.md", "--adapter", "fake"], 0);
    assert!(
        out.stdout.contains("vacuous") || out.stdout.contains("binding tests"),
        "{}",
        out.stdout
    );
}

/// LEARNINGS.md: a `## lifetime` header aggregates ALL runs — long-lived
/// stats survive the bounded tail window injected into dispatches.
#[test]
fn learnings_lifetime_block_aggregates_runs() {
    let h = Harness::new();
    h.set_adapter_fake();
    h.write_contract(&contract("src/**"));
    h.write_fake(&fake_toml_good());
    let _ = h.run(
        &["start", "--contract", "specs/add.contract.md", "--adapter", "fake"],
        0,
    );
    h.write_fake(&fake_toml_bad());
    let _ = h.run(
        &["start", "--contract", "specs/add.contract.md", "--adapter", "fake"],
        1,
    );
    let text = fs::read_to_string(h.path().join(".wanax/LEARNINGS.md")).unwrap();
    assert!(text.contains("## lifetime"), "{text}");
    assert!(text.contains("runs=2"), "{text}");
    assert!(text.contains("accepted=1"), "{text}");
    assert!(text.contains("contract add-fn: 1/2"), "{text}");
    assert!(text.contains("## entries"), "{text}");

    // The same history shows as a chronological trend in report
    // (accept → rework-limit escalate).
    let rep = h.run(&["report"], 0);
    assert!(
        rep.stdout.contains("add-fn: 1/2 accepted [AE]"),
        "{}",
        rep.stdout
    );
}

/// report --suggest: recurring failure patterns surface as rule-based hints.
#[test]
fn report_suggest_flags_boundary_runs() {
    let h = Harness::new();
    h.set_adapter_fake();
    h.write_contract(&contract("src/**"));
    // Worker writes outside allowed_globs → boundary reject.
    h.write_fake(
        "turns = 1\nrun_tests = false\n\n[[writes]]\npath = \"evil.rs\"\ncontent = '''pub fn evil() {}\n'''\n",
    );
    let _ = h.run(
        &[
            "start",
            "--contract",
            "specs/add.contract.md",
            "--adapter",
            "fake",
        ],
        1,
    );
    let out = h.run(&["report", "--suggest"], 0);
    assert!(
        out.stdout.contains("suggest") && out.stdout.contains("boundary"),
        "{}",
        out.stdout
    );
}

#[test]
fn agent_spec_plugin_runs_on_outer() {
    let h = Harness::new();
    h.set_adapter_fake();
    let mut body = contract("src/**");
    body = body.replace(
        "forbidden_globs:",
        "agent_spec: \"specs/add.spec.md\"\nforbidden_globs:",
    );
    h.write_contract(&body);
    fs::write(h.path().join("specs/add.spec.md"), "# dummy agent-spec\n").unwrap();
    git(h.path(), &["add", "specs/add.spec.md"]);
    git(h.path(), &["commit", "-m", "agent spec"]);
    h.write_fake(&fake_toml_good());
    let cfg = h.path().join(".wanax/config.toml");
    let mut t = fs::read_to_string(&cfg).unwrap();
    t = t.replace("plugins = []", "plugins = [\"agent-spec\"]");
    fs::write(&cfg, t).unwrap();
    let bin = h.data.path().join("bin");
    fs::create_dir_all(&bin).unwrap();
    let marker = h.data.path().join("plugin-ran");
    let cmds = h.data.path().join("plugin-cmds");
    let plugin = bin.join("agent-spec");
    fs::write(
        &plugin,
        format!(
            "#!/bin/sh\necho \"$PWD\" > \"{}\"\necho \"$1\" >> \"{}\"\nexit 0\n",
            marker.display(),
            cmds.display()
        ),
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(&plugin).unwrap().permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&plugin, perms).unwrap();
    }
    let path = std::env::var("PATH").unwrap_or_default();
    let out = wanax()
        .args([
            "start",
            "--contract",
            "specs/add.contract.md",
            "--adapter",
            "fake",
            "--data-dir",
        ])
        .arg(h.data.path())
        .current_dir(h.path())
        .env("PATH", format!("{}:{path}", bin.display()))
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(
        out.status.code(),
        Some(0),
        "stderr={} stdout={stdout}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(stdout.contains("state=accepted"), "{stdout}");
    assert!(marker.is_file(), "plugin did not run");
    let ran = fs::read_to_string(&cmds).unwrap();
    assert!(ran.contains("lint"), "{ran}");
    assert!(ran.contains("verify"), "{ran}");
    assert!(ran.contains("lifecycle"), "{ran}");
    let env = h.envelope();
    let plugin_ev = env["events"].as_array().unwrap().iter().any(|e| {
        e["kind"] == "state_changed" && e["payload"]["plugin"] == "agent-spec"
    });
    assert!(plugin_ev, "{env}");
}

fn split_contract(name: &str, allowed: &str, test_command: &str) -> String {
    format!(
        r#"---
spec: wanax.contract
version: 1
name: "{name}"
test_command: "{test_command}"
test_timeout_secs: 120
allowed_globs:
  - "{allowed}"
forbidden_globs:
  - "**/.env"
---

## Intent

Implement {name}.

## Decisions

- Only change {allowed}

## Boundaries

- Allowed: {allowed}

## Completion Criteria

- CC-01: {test_command} exits 0
"#
    )
}

fn write_split_crate(dir: &Path) {
    fs::write(
        dir.join("src/lib.rs"),
        "pub mod a;\npub mod b;\n\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn test_a() { assert_eq!(crate::a::value(), 1); }\n    #[test]\n    fn test_b() { assert_eq!(crate::b::value(), 2); }\n}\n",
    )
    .unwrap();
    fs::write(dir.join("src/a.rs"), "pub fn value() -> i32 { unimplemented!() }\n").unwrap();
    fs::write(dir.join("src/b.rs"), "pub fn value() -> i32 { unimplemented!() }\n").unwrap();
}

fn wait_for_path_set_lock(repo: &Path) -> bool {
    let dir = repo.join(".wanax/locks");
    let deadline = std::time::Instant::now() + Duration::from_secs(20);
    while std::time::Instant::now() < deadline {
        if dir.is_dir() {
            if let Ok(rd) = fs::read_dir(&dir) {
                if rd.filter_map(Result::ok).any(|e| e.path().is_file()) {
                    return true;
                }
            }
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    false
}

#[test]
fn claude_adapter_accepts() {
    let h = Harness::new();
    let script = install_cmd_script(
        h.data.path(),
        "claude",
        &format!(
            "#!/bin/sh\nset -eu\ncat >/dev/null\ncat > src/lib.rs << 'EOF'\n{GOOD_LIB}EOF\n"
        ),
    );
    h.set_adapter_claude(&script);
    h.write_contract(&contract("src/**"));
    let out = h.run(
        &[
            "start",
            "--contract",
            "specs/add.contract.md",
            "--adapter",
            "claude",
        ],
        0,
    );
    assert!(out.stdout.contains("state=accepted"), "{}", out.stdout);
}

#[test]
fn commander_cli_accepts() {
    let h = Harness::new();
    h.set_adapter_fake();
    h.write_contract(&contract("src/**"));
    h.write_fake(&fake_toml_good());
    let script = install_cmd_script(
        h.data.path(),
        "claude-commander",
        r#"#!/bin/sh
set -eu
body=$(cat)
if printf '%s' "$body" | grep -q '"decision"\|outer_test_exit_code'; then
  printf '%s' '{"decision":"accept","reason":"ok","files_reviewed":["src/lib.rs"]}'
else
  printf '%s' '{"title":"add-fn","instruction":"Implement add. allowed: src/**. test_command: cargo test. CC-01: pass"}'
fi
"#,
    );
    let cfg = h.path().join(".wanax/config.toml");
    let mut t = fs::read_to_string(&cfg).unwrap();
    t = t.replacen(
        "[commander]\nprovider = \"openai_compat\"\nmodel = \"commander\"",
        &format!(
            "[commander]\nprovider = \"claude_cli\"\nmodel = \"commander\"\ncli_bin = \"{}\"",
            script.display()
        ),
        1,
    );
    fs::write(&cfg, t).unwrap();
    let out = h.run(
        &[
            "start",
            "--contract",
            "specs/add.contract.md",
            "--adapter",
            "fake",
        ],
        0,
    );
    assert!(out.stdout.contains("state=accepted"), "{}", out.stdout);
}

#[test]
fn cost_lists_run_and_result_has_spent() {
    let h = Harness::new();
    h.set_adapter_fake();
    h.write_contract(&contract("src/**"));
    h.write_fake(&fake_toml_good());
    let start = h.run(
        &[
            "start",
            "--contract",
            "specs/add.contract.md",
            "--adapter",
            "fake",
        ],
        0,
    );
    assert!(start.stdout.contains("state=accepted"), "{}", start.stdout);
    let run_id = start
        .stdout
        .lines()
        .find(|l| l.starts_with("wx_"))
        .expect("run id");
    let cost = h.run(&["cost"], 0);
    assert!(cost.stdout.contains(run_id), "{}", cost.stdout);
    assert!(
        cost.stdout.contains("USD") || cost.stdout.contains("美元") || cost.stdout.contains("0.0000"),
        "{}",
        cost.stdout
    );
    let detail = h.run(&["cost", run_id], 0);
    assert!(detail.stdout.contains("spent_usd="), "{}", detail.stdout);
    let result = find_file(h.path(), "RESULT.md").expect("RESULT.md");
    let body = fs::read_to_string(result).unwrap();
    assert!(body.contains("spent_usd:"), "{body}");
}

#[test]
fn path_set_disjoint_runs_and_overlap_locked() {
    let h = Harness::new();
    h.set_adapter_fake();
    h.set_repo_exclusive(false);
    write_split_crate(h.path());
    git(h.path(), &["add", "src/lib.rs", "src/a.rs", "src/b.rs"]);
    git(h.path(), &["commit", "-m", "split crate"]);
    fs::write(
        h.path().join("specs/a.contract.md"),
        split_contract("mod-a", "src/a.rs", "cargo test test_a"),
    )
    .unwrap();
    fs::write(
        h.path().join("specs/b.contract.md"),
        split_contract("mod-b", "src/b.rs", "cargo test test_b"),
    )
    .unwrap();
    git(
        h.path(),
        &["add", "specs/a.contract.md", "specs/b.contract.md"],
    );
    git(h.path(), &["commit", "-m", "split contracts"]);

    let fake_a = h.data.path().join("fake-a.toml");
    fs::write(
        &fake_a,
        "turns = 1\nrun_tests = false\nsleep_ms = 8000\n\n[[writes]]\npath = \"src/a.rs\"\ncontent = '''pub fn value() -> i32 { 1 }\n'''\n",
    )
    .unwrap();
    let fake_b = h.data.path().join("fake-b.toml");
    fs::write(
        &fake_b,
        "turns = 1\nrun_tests = false\n\n[[writes]]\npath = \"src/b.rs\"\ncontent = '''pub fn value() -> i32 { 2 }\n'''\n",
    )
    .unwrap();
    let fake_overlap = h.data.path().join("fake-overlap.toml");
    fs::write(
        &fake_overlap,
        "turns = 1\nrun_tests = false\n\n[[writes]]\npath = \"src/a.rs\"\ncontent = '''pub fn value() -> i32 { 1 }\n'''\n",
    )
    .unwrap();

    let child_a = {
        let mut cmd = wanax();
        cmd.args([
            "start",
            "--contract",
            "specs/a.contract.md",
            "--adapter",
            "fake",
            "--data-dir",
        ])
        .arg(h.data.path())
        .current_dir(h.path())
        .env("WANAX_DATA_DIR", h.data.path())
        .env("WANAX_FAKE_SPEC", &fake_a)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
        cmd.spawn().unwrap()
    };
    assert!(
        wait_for_path_set_lock(h.path()),
        "first path-set lock never appeared"
    );

    let overlap = {
        let mut cmd = wanax();
        cmd.args([
            "start",
            "--contract",
            "specs/a.contract.md",
            "--adapter",
            "fake",
            "--data-dir",
        ])
        .arg(h.data.path())
        .current_dir(h.path())
        .env("WANAX_DATA_DIR", h.data.path())
        .env("WANAX_FAKE_SPEC", &fake_overlap)
        .output()
        .unwrap()
    };
    let overlap_err = String::from_utf8_lossy(&overlap.stderr);
    assert_eq!(overlap.status.code(), Some(6), "{overlap_err}");
    assert!(overlap_err.contains("E_REPO_LOCKED"), "{overlap_err}");

    let b = {
        let mut cmd = wanax();
        cmd.args([
            "start",
            "--contract",
            "specs/b.contract.md",
            "--adapter",
            "fake",
            "--data-dir",
        ])
        .arg(h.data.path())
        .current_dir(h.path())
        .env("WANAX_DATA_DIR", h.data.path())
        .env("WANAX_FAKE_SPEC", &fake_b)
        .output()
        .unwrap()
    };
    let b_out = String::from_utf8_lossy(&b.stdout);
    let b_err = String::from_utf8_lossy(&b.stderr);
    assert_eq!(b.status.code(), Some(0), "stdout={b_out} stderr={b_err}");
    assert!(b_out.contains("state=accepted"), "{b_out}");

    let a = child_a.wait_with_output().unwrap();
    let a_out = String::from_utf8_lossy(&a.stdout);
    let a_err = String::from_utf8_lossy(&a.stderr);
    assert_eq!(a.status.code(), Some(0), "stdout={a_out} stderr={a_err}");
    assert!(a_out.contains("state=accepted"), "{a_out}");
}

#[test]
fn resume_without_id_fails_when_two_active() {
    let h = Harness::new();
    h.set_adapter_fake();
    h.set_repo_exclusive(false);
    write_split_crate(h.path());
    git(h.path(), &["add", "src/lib.rs", "src/a.rs", "src/b.rs"]);
    git(h.path(), &["commit", "-m", "split crate"]);
    fs::write(
        h.path().join("specs/a.contract.md"),
        split_contract("mod-a", "src/a.rs", "cargo test test_a"),
    )
    .unwrap();
    fs::write(
        h.path().join("specs/b.contract.md"),
        split_contract("mod-b", "src/b.rs", "cargo test test_b"),
    )
    .unwrap();
    git(
        h.path(),
        &["add", "specs/a.contract.md", "specs/b.contract.md"],
    );
    git(h.path(), &["commit", "-m", "split contracts"]);
    let fake_a = h.data.path().join("fake-a.toml");
    fs::write(
        &fake_a,
        "turns = 1\nrun_tests = false\nsleep_ms = 15000\n\n[[writes]]\npath = \"src/a.rs\"\ncontent = '''pub fn value() -> i32 { 1 }\n'''\n",
    )
    .unwrap();
    let fake_b = h.data.path().join("fake-b.toml");
    fs::write(
        &fake_b,
        "turns = 1\nrun_tests = false\nsleep_ms = 15000\n\n[[writes]]\npath = \"src/b.rs\"\ncontent = '''pub fn value() -> i32 { 2 }\n'''\n",
    )
    .unwrap();

    let mut child_a = {
        let mut cmd = wanax();
        cmd.args([
            "start",
            "--contract",
            "specs/a.contract.md",
            "--adapter",
            "fake",
            "--data-dir",
        ])
        .arg(h.data.path())
        .current_dir(h.path())
        .env("WANAX_DATA_DIR", h.data.path())
        .env("WANAX_FAKE_SPEC", &fake_a)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
        cmd.spawn().unwrap()
    };
    assert!(wait_for_path_set_lock(h.path()));
    let mut child_b = {
        let mut cmd = wanax();
        cmd.args([
            "start",
            "--contract",
            "specs/b.contract.md",
            "--adapter",
            "fake",
            "--data-dir",
        ])
        .arg(h.data.path())
        .current_dir(h.path())
        .env("WANAX_DATA_DIR", h.data.path())
        .env("WANAX_FAKE_SPEC", &fake_b)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
        cmd.spawn().unwrap()
    };
    let deadline = std::time::Instant::now() + Duration::from_secs(15);
    let mut two = false;
    while std::time::Instant::now() < deadline {
        let locks_ready = fs::read_dir(h.path().join(".wanax/locks"))
            .map(|rd| {
                rd.filter_map(Result::ok)
                    .filter(|e| e.path().is_file())
                    .count()
                    >= 2
            })
            .unwrap_or(false);
        let runs_ready = h.run(&["status"], 0).stdout.matches("wx_").count() >= 2;
        if locks_ready && runs_ready {
            two = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(two, "expected two path-set locks and two active runs");
    let resume = h.run(&["resume"], 1);
    assert!(
        resume.stderr.contains("E_RESUME"),
        "stdout={} stderr={}",
        resume.stdout,
        resume.stderr
    );
    let _ = child_a.kill();
    let _ = child_b.kill();
    let _ = child_a.wait();
    let _ = child_b.wait();
}

fn find_file(root: &Path, name: &str) -> Option<PathBuf> {
    fn rec(dir: &Path, name: &str) -> Option<PathBuf> {
        for e in fs::read_dir(dir).ok()?.flatten() {
            let p = e.path();
            if p.is_dir() {
                if let Some(f) = rec(&p, name) {
                    return Some(f);
                }
            } else if p.file_name().and_then(|s| s.to_str()) == Some(name) {
                return Some(p);
            }
        }
        None
    }
    rec(root, name)
}

// ---------- v1.1 hardening & improvement coverage ----------

/// FR: baseline run on the untouched base is recorded so the verdict can
/// distinguish red→green from green→green.
#[test]
fn baseline_test_event_recorded() {
    let h = Harness::new();
    h.set_adapter_fake();
    h.write_contract(&contract("src/**"));
    h.write_fake(&fake_toml_good());
    let out = h.run(
        &[
            "start",
            "--contract",
            "specs/add.contract.md",
            "--adapter",
            "fake",
        ],
        0,
    );
    assert!(out.stdout.contains("state=accepted"), "{}", out.stdout);
    let env = h.envelope();
    let events = env["events"].as_array().unwrap();
    let baseline = events.iter().find(|e| e["kind"] == "baseline_test");
    let baseline = baseline.expect("baseline_test event missing");
    // The fixture's base commit fails `cargo test` (add is unimplemented).
    assert_ne!(
        baseline["payload"]["exit_code"].as_i64().unwrap_or(0),
        0,
        "baseline should be red on the fixture base: {baseline}"
    );
    let started = events.iter().find(|e| e["kind"] == "run_started").unwrap();
    assert!(
        started["payload"]["sandbox"].is_string(),
        "run_started must record sandbox backend: {started}"
    );
}

/// FR: spawned test harness never sees wanax's own secrets — a test-case
/// probe that reads WANAX_COMMANDER_API_KEY must observe an empty value.
#[test]
fn test_env_strips_wanax_keys() {
    let h = Harness::new();
    h.set_adapter_fake();
    // Extend the fixture with a probe test that records what the spawned
    // test process can see, next to the usual failing `add` test.
    fs::write(
        h.path().join("src/lib.rs"),
        concat!(
            "pub fn add(a: i32, b: i32) -> i32 { unimplemented!() }\n\n",
            "#[cfg(test)]\nmod tests {\n",
            "    #[test]\n    fn test_add() { assert_eq!(super::add(2, 3), 5); }\n",
            "    #[test]\n    fn leak_probe() {\n",
            "        let key = std::env::var(\"WANAX_COMMANDER_API_KEY\").unwrap_or_default();\n",
            "        std::fs::create_dir_all(\".wanax\").unwrap();\n",
            "        std::fs::write(\".wanax/leak.txt\", key).unwrap();\n",
            "    }\n}\n"
        ),
    )
    .unwrap();
    git(h.path(), &["add", "src/lib.rs"]);
    git(h.path(), &["commit", "-m", "probe fixture"]);
    h.write_contract(&contract("src/**"));
    h.write_fake(&fake_toml_good());
    let script = h.data.path().join("commander.json");
    fs::write(
        &script,
        r#"{
          "dispatch": {"title": "add-fn", "instruction": "implement add in src/lib.rs. test_command: cargo test. allowed: src/**. CC-01: cargo test exits 0"},
          "verdicts": [
            {"decision": "accept", "reason": "ok", "files_reviewed": ["src/lib.rs"]}
          ]
        }"#,
    )
    .unwrap();
    let out = wanax()
        .args([
            "start",
            "--contract",
            "specs/add.contract.md",
            "--adapter",
            "fake",
            "--data-dir",
        ])
        .arg(h.data.path())
        .current_dir(h.path())
        .env("WANAX_COMMANDER_API_KEY", "sk-leak-check")
        .env("WANAX_COMMANDER_SCRIPT", &script)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let run_id = stdout
        .lines()
        .find(|l| l.starts_with("wx_"))
        .expect("run id");
    let leak = h
        .path()
        .join(format!(".wanax/worktrees/{run_id}-inner/.wanax/leak.txt"));
    assert!(leak.is_file(), "leak probe file missing at {leak:?}");
    let body = fs::read_to_string(&leak).unwrap();
    assert!(
        body.is_empty(),
        "test harness saw WANAX_COMMANDER_API_KEY: {body:?}"
    );
    assert!(stdout.contains("state=accepted"), "{stdout}");
}

/// FR: a run must not mutate the main checkout's `.git/config` — process
/// isolation lives in GIT_CONFIG_GLOBAL, not `git config --local`.
#[test]
fn repo_config_not_mutated_by_run() {
    let h = Harness::new();
    h.set_adapter_fake();
    h.write_contract(&contract("src/**"));
    h.write_fake(&fake_toml_good());
    let out = h.run(
        &[
            "start",
            "--contract",
            "specs/add.contract.md",
            "--adapter",
            "fake",
        ],
        0,
    );
    assert!(out.stdout.contains("state=accepted"), "{}", out.stdout);
    let gitconfig = h.path().join(".wanax/gitconfig");
    assert!(gitconfig.is_file(), "wanax gitconfig missing");
    assert!(fs::read_to_string(&gitconfig).unwrap().contains("helper ="));
    let list = Command::new("git")
        .args(["config", "--local", "--list"])
        .current_dir(h.path())
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&list.stdout);
    assert!(
        !text.contains("credential.helper"),
        "repo config was mutated by the run:\n{text}"
    );
}

/// FR: disjoint peers actually run concurrently — receipt timestamps are ms
/// precise, so the two receipts must land within each other's sleep window.
#[test]
fn peer_receipts_run_concurrently() {
    let h = Harness::new();
    h.set_adapter_fake();
    setup_peer_crate(h.path());
    git(h.path(), &["add", "src"]);
    git(h.path(), &["commit", "-m", "peer crate"]);
    h.write_contract(&contract("src/**"));
    fs::write(
        h.path().join(".wanax/fake-peer-1.toml"),
        "turns = 1\nrun_tests = false\nsleep_ms = 4000\n\n[[writes]]\npath = \"src/a.rs\"\ncontent = '''pub fn x() -> i32 { 1 }\n'''\n",
    )
    .unwrap();
    fs::write(
        h.path().join(".wanax/fake-peer-2.toml"),
        "turns = 1\nrun_tests = false\nsleep_ms = 4000\n\n[[writes]]\npath = \"src/b.rs\"\ncontent = '''pub fn y() -> i32 { 2 }\n'''\n",
    )
    .unwrap();
    let script = h.data.path().join("peer-parallel.json");
    fs::write(&script, peer_dispatch_script(false)).unwrap();
    let out = wanax()
        .args([
            "start",
            "--contract",
            "specs/add.contract.md",
            "--adapter",
            "fake",
            "--data-dir",
        ])
        .arg(h.data.path())
        .current_dir(h.path())
        .env("WANAX_COMMANDER_SCRIPT", &script)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let env = h.envelope();
    let mut ts: Vec<_> = env["events"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["actor"] == "peer" && e["kind"] == "receipt_submitted")
        .map(|e| {
            wanax_core::timeutil::parse_rfc3339(e["at"].as_str().unwrap()).unwrap()
        })
        .collect();
    assert_eq!(ts.len(), 2, "expected two peer receipts");
    ts.sort();
    let gap_ms = (ts[1] - ts[0]).num_milliseconds();
    assert!(
        gap_ms < 3000,
        "peers must overlap (serial would be ~4000ms apart): gap={gap_ms}ms"
    );
}

/// FR: real worker metering — Claude's `total_cost_usd`/`num_turns` are
/// charged into the run budget instead of the fake turns=1/$0 placeholders.
#[test]
fn claude_metering_charges_usd_and_turns() {
    let h = Harness::new();
    let script = install_cmd_script(
        h.data.path(),
        "claude",
        &format!(
            "#!/bin/sh\nset -eu\ncat >/dev/null\ncat > src/lib.rs << 'EOF'\n{GOOD_LIB}EOF\nprintf '%s' '{{\"is_error\":false,\"num_turns\":3,\"total_cost_usd\":0.5,\"result\":\"done\"}}'\n"
        ),
    );
    h.set_adapter_claude(&script);
    h.write_contract(&contract("src/**"));
    let out = h.run(
        &[
            "start",
            "--contract",
            "specs/add.contract.md",
            "--adapter",
            "claude",
        ],
        0,
    );
    assert!(out.stdout.contains("state=accepted"), "{}", out.stdout);
    let run_id = out
        .stdout
        .lines()
        .find(|l| l.starts_with("wx_"))
        .expect("run id")
        .to_string();
    let cost = h.run(&["cost", &run_id], 0);
    assert!(cost.stdout.contains("spent_turns=3"), "{}", cost.stdout);
    // $0.50 real worker charge plus estimated commander dispatch/verdict cost.
    assert!(cost.stdout.contains("spent_usd=0.5"), "{}", cost.stdout);
    let env = h.envelope();
    let direct_charge = env["events"]
        .as_array()
        .unwrap()
        .iter()
        .any(|e| {
            e["kind"] == "budget_tick" && e["payload"]["charge_usd_micros"] == 500_000
        });
    assert!(direct_charge, "no real-usd charge_usd_micros tick: {env}");
}

// ---------- RSI loop (wanax improve) ----------

/// RSI: each improve iteration is a full independent run; a failed run hands
/// its evidence to the next via run_started.parent_run_id. The loop exits
/// non-zero when iterations exhaust without an accept.
#[test]
fn improve_links_iterations_and_stops_at_limit() {
    let h = Harness::new();
    h.set_adapter_fake();
    h.write_contract(&contract("src/**"));
    h.write_fake(&fake_toml_bad());
    let out = h.run(
        &[
            "improve",
            "--contract",
            "specs/add.contract.md",
            "--adapter",
            "fake",
            "--iterations",
            "2",
        ],
        1,
    );
    assert!(out.stdout.contains("iteration=1/2"), "{}", out.stdout);
    assert!(out.stdout.contains("iteration=2/2"), "{}", out.stdout);
    // Iteration 2's evidence is identical (same deterministic fake) so the
    // loop may exit via either the no-progress early stop or the limit.
    assert!(
        out.stderr.contains("improve: not accepted")
            || out.stderr.contains("no progress"),
        "stderr={}",
        out.stderr
    );
    let mut runs: Vec<PathBuf> = fs::read_dir(h.path().join(".wanax/runs"))
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .collect();
    assert_eq!(runs.len(), 2, "expected two iteration runs");
    runs.sort(); // run ids are ULIDs — sorted order == creation order
    let env2 = serde_json::from_str::<Value>(
        &fs::read_to_string(runs[1].join("envelope.json")).unwrap(),
    )
    .unwrap();
    let started = env2["events"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["kind"] == "run_started")
        .unwrap();
    assert_eq!(started["payload"]["iteration"], 2);
    assert!(started["payload"]["parent_run_id"].is_string());
}

/// RSI memory: each finished run appends one distilled line to
/// .wanax/LEARNINGS.md, and the next dispatch sees accumulated evidence.
#[test]
fn learnings_persist_across_runs() {
    let h = Harness::new();
    h.set_adapter_fake();
    h.write_contract(&contract("src/**"));
    h.write_fake(&fake_toml_bad());
    let r1 = h.run(
        &[
            "start",
            "--contract",
            "specs/add.contract.md",
            "--adapter",
            "fake",
        ],
        1,
    );
    let run1 = r1
        .stdout
        .lines()
        .find(|l| l.starts_with("wx_"))
        .expect("run id")
        .to_string();
    let learnings = h.path().join(".wanax/LEARNINGS.md");
    assert!(learnings.is_file(), "LEARNINGS.md not written");
    let text = fs::read_to_string(&learnings).unwrap();
    assert!(text.contains(&run1), "{text}");
    assert!(text.contains("state=escalate"), "{text}");

    // Second run: the instruction carries the accumulated learnings.
    h.write_fake(&fake_toml_good());
    let r2 = h.run(
        &[
            "start",
            "--contract",
            "specs/add.contract.md",
            "--adapter",
            "fake",
        ],
        0,
    );
    let run2 = r2
        .stdout
        .lines()
        .find(|l| l.starts_with("wx_"))
        .expect("run id")
        .to_string();
    let work_unit = fs::read_to_string(
        h.path()
            .join(format!(".wanax/worktrees/{run2}-inner/WORK_UNIT.md")),
    )
    .unwrap();
    assert!(
        work_unit.contains("Accumulated learnings across runs"),
        "{work_unit}"
    );
    assert!(work_unit.contains(&run1), "{work_unit}");
}

/// RSI stop: identical failure signatures back-to-back = no progress; the
/// loop must stop early rather than burn the remaining iterations.
#[test]
fn improve_stops_on_identical_evidence() {
    let h = Harness::new();
    h.set_adapter_fake();
    h.write_contract(&contract("src/**"));
    h.write_fake(&fake_toml_bad());
    let out = h.run(
        &[
            "improve",
            "--contract",
            "specs/add.contract.md",
            "--adapter",
            "fake",
            "--iterations",
            "3",
        ],
        1,
    );
    assert!(
        out.stderr.contains("no progress") || out.stdout.contains("no progress"),
        "stdout={} stderr={}",
        out.stdout,
        out.stderr
    );
    let runs: Vec<PathBuf> = fs::read_dir(h.path().join(".wanax/runs"))
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .collect();
    assert_eq!(runs.len(), 2, "should stop after two identical failures");
}

/// RSI --from: a later `improve` invocation can resume an evidence chain
/// seeded by an earlier run of the SAME contract.
#[test]
fn improve_from_seeds_evidence_chain() {
    let h = Harness::new();
    h.set_adapter_fake();
    h.write_contract(&contract("src/**"));
    h.write_fake(&fake_toml_bad());
    let r1 = h.run(
        &[
            "start",
            "--contract",
            "specs/add.contract.md",
            "--adapter",
            "fake",
        ],
        1,
    );
    let seed = r1
        .stdout
        .lines()
        .find(|l| l.starts_with("wx_"))
        .expect("run id")
        .to_string();

    let out = h.run(
        &[
            "improve",
            "--contract",
            "specs/add.contract.md",
            "--adapter",
            "fake",
            "--iterations",
            "1",
            "--from",
            &seed,
        ],
        1,
    );
    assert!(out.stdout.contains("iteration=1/1"), "{}", out.stdout);
    // New run's run_started links back to the seed.
    let mut runs: Vec<PathBuf> = fs::read_dir(h.path().join(".wanax/runs"))
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .collect();
    runs.sort();
    assert_eq!(runs.len(), 2);
    let env = serde_json::from_str::<Value>(
        &fs::read_to_string(runs[1].join("envelope.json")).unwrap(),
    )
    .unwrap();
    let started = env["events"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["kind"] == "run_started")
        .unwrap();
    assert_eq!(started["payload"]["parent_run_id"], seed);
    assert_eq!(started["payload"]["adapter"], "fake");

    // --from must refuse a run of a different contract.
    h.write_contract(&contract("docs/**"));
    let bad = h.run(
        &[
            "improve",
            "--contract",
            "specs/add.contract.md",
            "--adapter",
            "fake",
            "--from",
            &seed,
        ],
        4,
    );
    assert!(
        bad.stderr.contains("different contract"),
        "{}",
        bad.stderr
    );
}

/// RSI loop budget: --max-total-usd caps spend summed across iterations —
/// per-run budgets alone cannot bound a multi-iteration loop.
#[test]
fn improve_total_budget_cap() {
    let h = Harness::new();
    h.set_adapter_fake();
    h.write_contract(&contract("src/**"));
    h.write_fake(&fake_toml_bad());
    let out = h.run(
        &[
            "improve",
            "--contract",
            "specs/add.contract.md",
            "--adapter",
            "fake",
            "--iterations",
            "3",
            "--max-total-usd",
            "0.0001",
        ],
        1,
    );
    assert!(
        out.stderr.contains("loop budget exhausted"),
        "stderr={}",
        out.stderr
    );
    let runs: Vec<PathBuf> = fs::read_dir(h.path().join(".wanax/runs"))
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .collect();
    assert_eq!(runs.len(), 1, "budget stop should prevent iteration 2");
}

/// RSI adapter ladder: every adapter in the ladder is preflighted upfront.
#[test]
fn improve_adapters_ladder_preflight() {
    let h = Harness::new();
    h.set_adapter_fake();
    h.write_contract(&contract("src/**"));
    let out = h.run(
        &[
            "improve",
            "--contract",
            "specs/add.contract.md",
            "--adapters",
            "fake,cmd",
            "--iterations",
            "2",
        ],
        8,
    );
    assert!(
        out.stderr.contains("E_ADAPTER_MISSING"),
        "{}",
        out.stderr
    );
}

/// RSI: when an iteration accepts, improve stops immediately with success.
#[test]
fn improve_accepts_on_first_iteration() {
    let h = Harness::new();
    h.set_adapter_fake();
    h.write_contract(&contract("src/**"));
    h.write_fake(&fake_toml_good());
    let out = h.run(
        &[
            "improve",
            "--contract",
            "specs/add.contract.md",
            "--adapter",
            "fake",
            "--iterations",
            "3",
        ],
        0,
    );
    assert!(
        out.stdout.contains("accepted at iteration 1"),
        "{}",
        out.stdout
    );
    assert!(!out.stdout.contains("iteration=2/3"), "{}", out.stdout);
}

/// report: aggregate and per-run fitness views read tombstone + DB.
#[test]
fn report_shows_fitness() {
    let h = Harness::new();
    h.set_adapter_fake();
    h.write_contract(&contract("src/**"));
    h.write_fake(&fake_toml_good());
    let start = h.run(
        &[
            "start",
            "--contract",
            "specs/add.contract.md",
            "--adapter",
            "fake",
        ],
        0,
    );
    let run_id = start
        .stdout
        .lines()
        .find(|l| l.starts_with("wx_"))
        .expect("run id")
        .to_string();
    let agg = h.run(&["report"], 0);
    assert!(agg.stdout.contains("runs=1"), "{}", agg.stdout);
    assert!(agg.stdout.contains("accepted=1"), "{}", agg.stdout);
    let one = h.run(&["report", &run_id], 0);
    assert!(one.stdout.contains("state=accepted"), "{}", one.stdout);
    assert!(one.stdout.contains("baseline_exit="), "{}", one.stdout);
    assert!(one.stdout.contains("baseline_green="), "{}", one.stdout);
    assert!(one.stdout.contains("outer_test_exits=[0]"), "{}", one.stdout);
    assert!(one.stdout.contains("verdicts=accept"), "{}", one.stdout);

    // --json: machine-readable aggregate for external loops.
    let j = h.run(&["report", "--json"], 0);
    let v: Value = serde_json::from_str(&j.stdout).unwrap();
    assert_eq!(v["accepted"], 1, "{v}");
    assert_eq!(v["contracts"][0]["contract"], "add-fn", "{v}");
    let jd = h.run(&["report", &run_id, "--json"], 0);
    let d: Value = serde_json::from_str(&jd.stdout).unwrap();
    assert_eq!(d["state"], "accepted", "{d}");
    assert_eq!(d["verdicts"], serde_json::json!(["accept"]), "{d}");
}

/// clean: terminal non-accepted runs lose worktrees+branches; accepted runs
/// keep their output branch; --dry-run prints without touching.
#[test]
fn clean_removes_finished_run_artifacts() {
    let h = Harness::new();
    h.set_adapter_fake();
    h.write_contract(&contract("src/**"));
    h.write_fake(&fake_toml_bad());
    let start = h.run(
        &[
            "start",
            "--contract",
            "specs/add.contract.md",
            "--adapter",
            "fake",
        ],
        1,
    );
    let run_id = start
        .stdout
        .lines()
        .find(|l| l.starts_with("wx_"))
        .expect("run id")
        .to_string();
    let inner_wt = h
        .path()
        .join(format!(".wanax/worktrees/{run_id}-inner"));
    assert!(inner_wt.is_dir());
    let dry = h.run(&["clean", "--dry-run"], 0);
    assert!(dry.stdout.contains("would remove"), "{}", dry.stdout);
    assert!(inner_wt.is_dir(), "dry-run must not remove");
    let out = h.run(&["clean"], 0);
    assert!(out.stdout.contains("cleaned"), "{}", out.stdout);
    assert!(!inner_wt.exists(), "inner worktree should be removed");
    let branches = Command::new("git")
        .args(["branch", "--list", &format!("wanax/{run_id}/*")])
        .current_dir(h.path())
        .output()
        .unwrap();
    assert!(
        String::from_utf8_lossy(&branches.stdout).trim().is_empty(),
        "wanax branches for the failed run must be deleted"
    );
    // Tombstone survives by default (audit trail).
    assert!(h.path().join(format!(".wanax/runs/{run_id}")).is_dir());
    let _ = h.run(&["clean", "--runs"], 0);
    assert!(
        !h.path().join(format!(".wanax/runs/{run_id}")).exists(),
        "--runs must remove the tombstone dir"
    );
}

/// clean --include-dead: a run whose owner process died mid-flight leaves a
/// non-terminal orphan — plain `clean` skips it, --include-dead reclaims it.
#[test]
fn clean_include_dead_reclaims_orphans() {
    let h = Harness::new();
    h.set_adapter_fake();
    h.write_contract(&contract("src/**"));
    h.write_fake(&format!(
        "turns = 1\nrun_tests = false\nsleep_ms = 60000\n\n[[writes]]\npath = \"src/lib.rs\"\ncontent = '''{GOOD_LIB}'''\n"
    ));
    let mut child = wanax()
        .args([
            "start",
            "--contract",
            "specs/add.contract.md",
            "--adapter",
            "fake",
            "--data-dir",
        ])
        .arg(h.data.path())
        .current_dir(h.path())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut run_id = String::new();
    for _ in 0..150 {
        std::thread::sleep(Duration::from_millis(100));
        if h.path().join(".wanax/LOCK").is_file() {
            if let Ok(rd) = fs::read_dir(h.path().join(".wanax/runs")) {
                if let Some(e) = rd.filter_map(|x| x.ok()).next() {
                    run_id = e.file_name().to_string_lossy().into_owned();
                    break;
                }
            }
        }
    }
    assert!(!run_id.is_empty(), "no run started");
    let inner_wt = h.path().join(format!(".wanax/worktrees/{run_id}-inner"));
    for _ in 0..100 {
        if inner_wt.is_dir() {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(inner_wt.is_dir(), "inner worktree never appeared");
    std::thread::sleep(Duration::from_millis(300));
    let _ = child.kill();
    let _ = child.wait();

    assert!(inner_wt.is_dir());
    let skip = h.run(&["clean"], 0);
    assert!(skip.stdout.contains("skip"), "{}", skip.stdout);
    assert!(inner_wt.is_dir(), "orphan must survive plain clean");
    let out = h.run(&["clean", "--include-dead"], 0);
    assert!(out.stdout.contains("dead run"), "{}", out.stdout);
    assert!(!inner_wt.exists(), "dead-run worktree should be removed");
}
