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
    let gi = fs::read_to_string(h.path().join(".wanax/.gitignore")).unwrap();
    assert!(gi.contains("worktrees/"));
    assert!(gi.contains("LOCK"));
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
