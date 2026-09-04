use crate::error::{ErrorCode, WanaxError};
use crate::state::transition;
use crate::timeutil::now_rfc3339;
use crate::types::{
    AssigneeRole, Contract, FactoryRun, Receipt, RunState, Verdict, VerdictDecision, WorkUnit,
    WorkUnitState, WorkerAdapterKind,
};
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};
use sqlx::{Row, SqlitePool};
use std::path::Path;

#[derive(Clone)]
pub struct Store {
    pool: SqlitePool,
}

impl Store {
    pub async fn open(db_path: &Path) -> Result<Self, WanaxError> {
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| WanaxError::with_detail(ErrorCode::Db, e))?;
        }
        let opts = SqliteConnectOptions::new()
            .filename(db_path)
            .create_if_missing(true)
            .journal_mode(SqliteJournalMode::Wal)
            .foreign_keys(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(5)
            .connect_with(opts)
            .await?;
        let store = Self { pool };
        store.migrate().await?;
        Ok(store)
    }

    async fn migrate(&self) -> Result<(), WanaxError> {
        for stmt in [
            r#"CREATE TABLE IF NOT EXISTS contracts (
                id TEXT PRIMARY KEY,
                path TEXT NOT NULL,
                content_sha256 TEXT NOT NULL,
                intent TEXT NOT NULL,
                decisions TEXT NOT NULL,
                allowed_globs TEXT NOT NULL,
                forbidden_globs TEXT NOT NULL,
                forbidden_rules TEXT NOT NULL,
                completion_criteria TEXT NOT NULL,
                test_command TEXT NOT NULL,
                test_timeout_secs INTEGER NOT NULL,
                name TEXT
            )"#,
            r#"CREATE TABLE IF NOT EXISTS factory_runs (
                id TEXT PRIMARY KEY,
                repo_root TEXT NOT NULL,
                contract_id TEXT NOT NULL,
                contract_sha256 TEXT NOT NULL,
                state TEXT NOT NULL,
                base_sha TEXT NOT NULL,
                inner_branch TEXT NOT NULL,
                outer_branch TEXT NOT NULL,
                commander_model TEXT NOT NULL,
                inner_model TEXT NOT NULL,
                reviewer_model TEXT,
                max_usd_micros INTEGER NOT NULL,
                max_inner_turns INTEGER NOT NULL,
                spent_usd_micros INTEGER NOT NULL,
                spent_inner_turns INTEGER NOT NULL,
                worker_adapter TEXT NOT NULL,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                finished_at TEXT,
                last_error TEXT,
                worker_pid INTEGER,
                start_pid INTEGER
            )"#,
            r#"CREATE TABLE IF NOT EXISTS work_units (
                id TEXT PRIMARY KEY,
                run_id TEXT NOT NULL,
                seq INTEGER NOT NULL,
                title TEXT NOT NULL,
                instruction TEXT NOT NULL,
                state TEXT NOT NULL,
                assignee_role TEXT NOT NULL,
                parent_id TEXT,
                allowed_globs TEXT,
                rework_count INTEGER NOT NULL,
                inner_commit_sha TEXT,
                receipt_id TEXT,
                verdict_id TEXT
            )"#,
            r#"CREATE TABLE IF NOT EXISTS receipts (
                id TEXT PRIMARY KEY,
                work_unit_id TEXT NOT NULL,
                changed_files TEXT NOT NULL,
                diffstat TEXT NOT NULL,
                commit_sha TEXT NOT NULL,
                test_command TEXT NOT NULL,
                test_exit_code INTEGER NOT NULL,
                test_excerpt TEXT NOT NULL,
                claimed_pass INTEGER NOT NULL,
                duration_ms INTEGER NOT NULL,
                adapter TEXT NOT NULL,
                raw_artifact_path TEXT
            )"#,
            r#"CREATE TABLE IF NOT EXISTS verdicts (
                id TEXT PRIMARY KEY,
                work_unit_id TEXT NOT NULL,
                decision TEXT NOT NULL,
                reason TEXT NOT NULL,
                outer_test_exit_code INTEGER NOT NULL,
                outer_test_excerpt TEXT NOT NULL,
                boundary_ok INTEGER NOT NULL,
                files_reviewed TEXT NOT NULL,
                commander_model TEXT NOT NULL,
                created_at TEXT NOT NULL
            )"#,
            "CREATE INDEX IF NOT EXISTS idx_runs_repo ON factory_runs(repo_root)",
            "CREATE INDEX IF NOT EXISTS idx_units_run ON work_units(run_id)",
        ] {
            sqlx::query(stmt).execute(&self.pool).await?;
        }
        let _ = sqlx::query("ALTER TABLE work_units ADD COLUMN allowed_globs TEXT")
            .execute(&self.pool)
            .await;
        let _ = sqlx::query("ALTER TABLE work_units ADD COLUMN depends_on TEXT")
            .execute(&self.pool)
            .await;
        let _ = sqlx::query("ALTER TABLE work_units ADD COLUMN test_command TEXT")
            .execute(&self.pool)
            .await;
        let _ = sqlx::query("ALTER TABLE work_units ADD COLUMN local_key TEXT")
            .execute(&self.pool)
            .await;
        let _ = sqlx::query("ALTER TABLE contracts ADD COLUMN agent_spec TEXT")
            .execute(&self.pool)
            .await;
        Ok(())
    }

    pub async fn insert_contract(&self, c: &Contract) -> Result<(), WanaxError> {
        sqlx::query(
            r#"INSERT INTO contracts
            (id, path, content_sha256, intent, decisions, allowed_globs, forbidden_globs,
             forbidden_rules, completion_criteria, test_command, test_timeout_secs, name,
             agent_spec)
            VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)"#,
        )
        .bind(&c.id)
        .bind(&c.path)
        .bind(&c.content_sha256)
        .bind(&c.intent)
        .bind(to_json(&c.decisions)?)
        .bind(to_json(&c.allowed_globs)?)
        .bind(to_json(&c.forbidden_globs)?)
        .bind(to_json(&c.forbidden_rules)?)
        .bind(to_json(&c.completion_criteria)?)
        .bind(&c.test_command)
        .bind(c.test_timeout_secs as i64)
        .bind(&c.name)
        .bind(&c.agent_spec)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn get_contract(&self, id: &str) -> Result<Contract, WanaxError> {
        let row = sqlx::query("SELECT * FROM contracts WHERE id = ?")
            .bind(id)
            .fetch_optional(&self.pool)
            .await?
            .ok_or_else(|| WanaxError::from_code(ErrorCode::RunNotFound))?;
        Ok(Contract {
            id: row.try_get("id")?,
            path: row.try_get("path")?,
            content_sha256: row.try_get("content_sha256")?,
            intent: row.try_get("intent")?,
            decisions: from_json(row.try_get("decisions")?)?,
            allowed_globs: from_json(row.try_get("allowed_globs")?)?,
            forbidden_globs: from_json(row.try_get("forbidden_globs")?)?,
            forbidden_rules: from_json(row.try_get("forbidden_rules")?)?,
            completion_criteria: from_json(row.try_get("completion_criteria")?)?,
            test_command: row.try_get("test_command")?,
            test_timeout_secs: row.try_get::<i64, _>("test_timeout_secs")? as u32,
            name: row.try_get("name")?,
            agent_spec: row.try_get("agent_spec").ok().flatten(),
        })
    }

    pub async fn insert_run(&self, r: &FactoryRun) -> Result<(), WanaxError> {
        sqlx::query(
            r#"INSERT INTO factory_runs
            (id, repo_root, contract_id, contract_sha256, state, base_sha, inner_branch,
             outer_branch, commander_model, inner_model, reviewer_model, max_usd_micros,
             max_inner_turns, spent_usd_micros, spent_inner_turns, worker_adapter,
             created_at, updated_at, finished_at, last_error, worker_pid, start_pid)
            VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)"#,
        )
        .bind(&r.id)
        .bind(&r.repo_root)
        .bind(&r.contract_id)
        .bind(&r.contract_sha256)
        .bind(r.state.as_str())
        .bind(&r.base_sha)
        .bind(&r.inner_branch)
        .bind(&r.outer_branch)
        .bind(&r.commander_model)
        .bind(&r.inner_model)
        .bind(&r.reviewer_model)
        .bind(r.max_usd_micros)
        .bind(r.max_inner_turns as i64)
        .bind(r.spent_usd_micros)
        .bind(r.spent_inner_turns as i64)
        .bind(r.worker_adapter.as_str())
        .bind(&r.created_at)
        .bind(&r.updated_at)
        .bind(&r.finished_at)
        .bind(&r.last_error)
        .bind(r.worker_pid)
        .bind(r.start_pid)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn get_run(&self, id: &str) -> Result<FactoryRun, WanaxError> {
        let row = sqlx::query("SELECT * FROM factory_runs WHERE id = ?")
            .bind(id)
            .fetch_optional(&self.pool)
            .await?
            .ok_or_else(|| WanaxError::from_code(ErrorCode::RunNotFound))?;
        row_to_run(row)
    }

    pub async fn list_runs(&self) -> Result<Vec<FactoryRun>, WanaxError> {
        let rows = sqlx::query("SELECT * FROM factory_runs ORDER BY created_at DESC")
            .fetch_all(&self.pool)
            .await?;
        rows.into_iter().map(row_to_run).collect()
    }

    pub async fn set_state(
        &self,
        run: &mut FactoryRun,
        to: RunState,
        last_error: Option<String>,
    ) -> Result<(), WanaxError> {
        let next = transition(run.state, to)?;
        run.state = next;
        run.updated_at = now_rfc3339();
        run.last_error = last_error.clone();
        if next.is_terminal() {
            run.finished_at = Some(run.updated_at.clone());
        }
        sqlx::query(
            r#"UPDATE factory_runs SET state=?, updated_at=?, finished_at=?, last_error=?,
               spent_usd_micros=?, spent_inner_turns=?, worker_pid=?
               WHERE id=?"#,
        )
        .bind(run.state.as_str())
        .bind(&run.updated_at)
        .bind(&run.finished_at)
        .bind(&run.last_error)
        .bind(run.spent_usd_micros)
        .bind(run.spent_inner_turns as i64)
        .bind(run.worker_pid)
        .bind(&run.id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn save_run_progress(&self, run: &FactoryRun) -> Result<(), WanaxError> {
        sqlx::query(
            r#"UPDATE factory_runs SET updated_at=?, spent_usd_micros=?, spent_inner_turns=?,
               worker_pid=?, start_pid=?, last_error=? WHERE id=?"#,
        )
        .bind(now_rfc3339())
        .bind(run.spent_usd_micros)
        .bind(run.spent_inner_turns as i64)
        .bind(run.worker_pid)
        .bind(run.start_pid)
        .bind(&run.last_error)
        .bind(&run.id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn insert_work_unit(&self, u: &WorkUnit) -> Result<(), WanaxError> {
        sqlx::query(
            r#"INSERT INTO work_units
            (id, run_id, seq, title, instruction, state, assignee_role, parent_id,
             allowed_globs, depends_on, test_command, local_key, rework_count,
             inner_commit_sha, receipt_id, verdict_id)
            VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)"#,
        )
        .bind(&u.id)
        .bind(&u.run_id)
        .bind(u.seq as i64)
        .bind(&u.title)
        .bind(&u.instruction)
        .bind(u.state.as_str())
        .bind(u.assignee_role.as_str())
        .bind(&u.parent_id)
        .bind(optional_json(&u.allowed_globs)?)
        .bind(to_json(&u.depends_on)?)
        .bind(&u.test_command)
        .bind(&u.local_key)
        .bind(u.rework_count as i64)
        .bind(&u.inner_commit_sha)
        .bind(&u.receipt_id)
        .bind(&u.verdict_id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn update_work_unit(&self, u: &WorkUnit) -> Result<(), WanaxError> {
        sqlx::query(
            r#"UPDATE work_units SET title=?, instruction=?, state=?, assignee_role=?,
               rework_count=?, inner_commit_sha=?, receipt_id=?, verdict_id=?,
               depends_on=?, test_command=?, local_key=? WHERE id=?"#,
        )
        .bind(&u.title)
        .bind(&u.instruction)
        .bind(u.state.as_str())
        .bind(u.assignee_role.as_str())
        .bind(u.rework_count as i64)
        .bind(&u.inner_commit_sha)
        .bind(&u.receipt_id)
        .bind(&u.verdict_id)
        .bind(to_json(&u.depends_on)?)
        .bind(&u.test_command)
        .bind(&u.local_key)
        .bind(&u.id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn latest_active_run(&self, repo_root: &str) -> Result<FactoryRun, WanaxError> {
        let rows = sqlx::query(
            "SELECT * FROM factory_runs WHERE repo_root = ? ORDER BY created_at DESC",
        )
        .bind(repo_root)
        .fetch_all(&self.pool)
        .await?;
        for row in rows {
            let run = row_to_run(row)?;
            if !run.state.is_terminal() {
                return Ok(run);
            }
        }
        Err(WanaxError::from_code(ErrorCode::Resume))
    }

    pub async fn work_units_for_run(&self, run_id: &str) -> Result<Vec<WorkUnit>, WanaxError> {
        let rows = sqlx::query("SELECT * FROM work_units WHERE run_id = ? ORDER BY seq")
            .bind(run_id)
            .fetch_all(&self.pool)
            .await?;
        rows.into_iter().map(row_to_unit).collect()
    }

    pub async fn insert_receipt(&self, r: &Receipt) -> Result<(), WanaxError> {
        sqlx::query(
            r#"INSERT INTO receipts
            (id, work_unit_id, changed_files, diffstat, commit_sha, test_command,
             test_exit_code, test_excerpt, claimed_pass, duration_ms, adapter, raw_artifact_path)
            VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)"#,
        )
        .bind(&r.id)
        .bind(&r.work_unit_id)
        .bind(to_json(&r.changed_files)?)
        .bind(&r.diffstat)
        .bind(&r.commit_sha)
        .bind(&r.test_command)
        .bind(r.test_exit_code)
        .bind(&r.test_excerpt)
        .bind(i64::from(r.claimed_pass))
        .bind(r.duration_ms as i64)
        .bind(&r.adapter)
        .bind(&r.raw_artifact_path)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn get_receipt(&self, id: &str) -> Result<Receipt, WanaxError> {
        let row = sqlx::query("SELECT * FROM receipts WHERE id = ?")
            .bind(id)
            .fetch_optional(&self.pool)
            .await?
            .ok_or_else(|| WanaxError::from_code(ErrorCode::RunNotFound))?;
        row_to_receipt(row)
    }

    pub async fn insert_verdict(&self, v: &Verdict) -> Result<(), WanaxError> {
        sqlx::query(
            r#"INSERT INTO verdicts
            (id, work_unit_id, decision, reason, outer_test_exit_code, outer_test_excerpt,
             boundary_ok, files_reviewed, commander_model, created_at)
            VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)"#,
        )
        .bind(&v.id)
        .bind(&v.work_unit_id)
        .bind(v.decision.as_str())
        .bind(&v.reason)
        .bind(v.outer_test_exit_code)
        .bind(&v.outer_test_excerpt)
        .bind(i64::from(v.boundary_ok))
        .bind(to_json(&v.files_reviewed)?)
        .bind(&v.commander_model)
        .bind(&v.created_at)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn latest_verdict(&self, work_unit_id: &str) -> Result<Option<Verdict>, WanaxError> {
        let row = sqlx::query(
            "SELECT * FROM verdicts WHERE work_unit_id = ? ORDER BY created_at DESC LIMIT 1",
        )
        .bind(work_unit_id)
        .fetch_optional(&self.pool)
        .await?;
        match row {
            Some(r) => Ok(Some(row_to_verdict(r)?)),
            None => Ok(None),
        }
    }
}

fn to_json<T: serde::Serialize>(v: &T) -> Result<String, WanaxError> {
    serde_json::to_string(v).map_err(|e| WanaxError::with_detail(ErrorCode::Db, e))
}

fn optional_json<T: serde::Serialize>(v: &Option<T>) -> Result<Option<String>, WanaxError> {
    match v {
        Some(x) => Ok(Some(to_json(x)?)),
        None => Ok(None),
    }
}

fn from_json<T: serde::de::DeserializeOwned>(s: String) -> Result<T, WanaxError> {
    serde_json::from_str(&s).map_err(|e| WanaxError::with_detail(ErrorCode::Db, e))
}

fn row_to_run(row: sqlx::sqlite::SqliteRow) -> Result<FactoryRun, WanaxError> {
    let state_s: String = row.try_get("state")?;
    let adapter_s: String = row.try_get("worker_adapter")?;
    Ok(FactoryRun {
        id: row.try_get("id")?,
        repo_root: row.try_get("repo_root")?,
        contract_id: row.try_get("contract_id")?,
        contract_sha256: row.try_get("contract_sha256")?,
        state: RunState::parse(&state_s)
            .ok_or_else(|| WanaxError::with_detail(ErrorCode::Db, "bad state"))?,
        base_sha: row.try_get("base_sha")?,
        inner_branch: row.try_get("inner_branch")?,
        outer_branch: row.try_get("outer_branch")?,
        commander_model: row.try_get("commander_model")?,
        inner_model: row.try_get("inner_model")?,
        reviewer_model: row.try_get("reviewer_model")?,
        max_usd_micros: row.try_get("max_usd_micros")?,
        max_inner_turns: row.try_get::<i64, _>("max_inner_turns")? as u32,
        spent_usd_micros: row.try_get("spent_usd_micros")?,
        spent_inner_turns: row.try_get::<i64, _>("spent_inner_turns")? as u32,
        worker_adapter: WorkerAdapterKind::parse(&adapter_s)
            .ok_or_else(|| WanaxError::with_detail(ErrorCode::Db, "bad adapter"))?,
        created_at: row.try_get("created_at")?,
        updated_at: row.try_get("updated_at")?,
        finished_at: row.try_get("finished_at")?,
        last_error: row.try_get("last_error")?,
        worker_pid: row.try_get("worker_pid")?,
        start_pid: row.try_get("start_pid")?,
    })
}

fn row_to_unit(row: sqlx::sqlite::SqliteRow) -> Result<WorkUnit, WanaxError> {
    let state_s: String = row.try_get("state")?;
    let role_s: String = row.try_get("assignee_role")?;
    Ok(WorkUnit {
        id: row.try_get("id")?,
        run_id: row.try_get("run_id")?,
        seq: row.try_get::<i64, _>("seq")? as u32,
        title: row.try_get("title")?,
        instruction: row.try_get("instruction")?,
        state: WorkUnitState::parse(&state_s)
            .ok_or_else(|| WanaxError::with_detail(ErrorCode::Db, "bad unit state"))?,
        assignee_role: AssigneeRole::parse(&role_s)
            .ok_or_else(|| WanaxError::with_detail(ErrorCode::Db, "bad role"))?,
        parent_id: row.try_get("parent_id")?,
        allowed_globs: row
            .try_get::<Option<String>, _>("allowed_globs")?
            .map(from_json)
            .transpose()?,
        depends_on: row
            .try_get::<Option<String>, _>("depends_on")
            .ok()
            .flatten()
            .map(from_json)
            .transpose()?
            .unwrap_or_default(),
        test_command: row.try_get("test_command").ok().flatten(),
        local_key: row.try_get("local_key").ok().flatten(),
        rework_count: row.try_get::<i64, _>("rework_count")? as u32,
        inner_commit_sha: row.try_get("inner_commit_sha")?,
        receipt_id: row.try_get("receipt_id")?,
        verdict_id: row.try_get("verdict_id")?,
    })
}

fn row_to_receipt(row: sqlx::sqlite::SqliteRow) -> Result<Receipt, WanaxError> {
    Ok(Receipt {
        id: row.try_get("id")?,
        work_unit_id: row.try_get("work_unit_id")?,
        changed_files: from_json(row.try_get("changed_files")?)?,
        diffstat: row.try_get("diffstat")?,
        commit_sha: row.try_get("commit_sha")?,
        test_command: row.try_get("test_command")?,
        test_exit_code: row.try_get("test_exit_code")?,
        test_excerpt: row.try_get("test_excerpt")?,
        claimed_pass: row.try_get::<i64, _>("claimed_pass")? != 0,
        duration_ms: row.try_get::<i64, _>("duration_ms")? as u64,
        adapter: row.try_get("adapter")?,
        raw_artifact_path: row.try_get("raw_artifact_path")?,
    })
}

fn row_to_verdict(row: sqlx::sqlite::SqliteRow) -> Result<Verdict, WanaxError> {
    let d: String = row.try_get("decision")?;
    Ok(Verdict {
        id: row.try_get("id")?,
        work_unit_id: row.try_get("work_unit_id")?,
        decision: VerdictDecision::parse(&d)
            .ok_or_else(|| WanaxError::with_detail(ErrorCode::Db, "bad decision"))?,
        reason: row.try_get("reason")?,
        outer_test_exit_code: row.try_get("outer_test_exit_code")?,
        outer_test_excerpt: row.try_get("outer_test_excerpt")?,
        boundary_ok: row.try_get::<i64, _>("boundary_ok")? != 0,
        files_reviewed: from_json(row.try_get("files_reviewed")?)?,
        commander_model: row.try_get("commander_model")?,
        created_at: row.try_get("created_at")?,
    })
}
