use super::collectors::Agent;
use super::collectors::Snapshot;
use super::monitor::{
    aggregate_totals, detect_all_installed, is_relevant_change, path_matches, rebuild_session_totals,
    run_engine, scan_once, snapshots_for, AgentTotals, EngineEvent, EngineOptions, Selector,
    SourceKind, SourceLocation,
};
use std::fs;
use std::path::Path;
use std::sync::{mpsc, Mutex, OnceLock};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

static ENV_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

fn fixture(name: &str) -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

#[test]
fn selector_matches_project_and_session_exactly() {
    let selector = Selector {
        agent: Agent::Codex,
        project: Some("/work/project".into()),
        session: Some("session-1".into()),
    };
    assert!(selector.matches(Some("/work/project"), "session-1"));
    assert!(!selector.matches(Some("/work/project-other"), "session-1"));
    assert!(!selector.matches(Some("/work/project"), "session-10"));
}

#[test]
fn windows_path_matching_strips_prefix_and_ascii_case() {
    assert!(path_matches(
        Path::new(r"\\?\C:\Work\Project"),
        Path::new(r"c:\work\project")
    ));
}

#[test]
fn unix_paths_with_colons_remain_case_sensitive() {
    assert!(!path_matches(
        Path::new("/work/Case:One"),
        Path::new("/work/case:one")
    ));
}

#[test]
fn unc_path_matching_normalizes_extended_unc_prefix() {
    assert!(path_matches(
        std::path::Path::new(r"\\?\UNC\Server\Share\Project"),
        std::path::Path::new(r"\\server\share\project")
    ));
}

#[test]
fn source_kind_is_stable_for_sqlite_and_jsonl_sources() {
    assert_eq!(SourceKind::ZCodeDb.as_str(), "zcode");
    assert_eq!(SourceKind::ClaudeProjects.as_str(), "claude-code");
    assert_eq!(SourceKind::PiSessions.as_str(), "pi");
    assert_eq!(
        serde_json::to_string(&SourceKind::CodexSessions).unwrap(),
        "\"codex\""
    );
    assert_eq!(
        serde_json::to_string(&SourceKind::PiSessions).unwrap(),
        "\"pi\""
    );
}

#[test]
fn sqlite_watcher_accepts_database_wal_and_shm_only() {
    let db = std::path::Path::new("/tmp/cache/opencode.db");
    assert!(is_relevant_change(SourceKind::OpenCodeDb, db, db));
    assert!(is_relevant_change(
        SourceKind::OpenCodeDb,
        &db.with_file_name("opencode.db-wal"),
        db
    ));
    assert!(is_relevant_change(
        SourceKind::OpenCodeDb,
        &db.with_file_name("opencode.db-shm"),
        db
    ));
    assert!(!is_relevant_change(
        SourceKind::OpenCodeDb,
        &db.with_file_name("other.db-wal"),
        db
    ));
}

#[test]
fn codex_rollout_schema_errors_are_not_silently_empty() {
    let root = std::env::temp_dir().join(format!(
        "tokenspeed-monitor-{}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir_all(&root).unwrap();
    fs::write(root.join("rollout-bad.jsonl"), "{\"type\":\"unknown\"}\n").unwrap();
    let result = snapshots_for(&SourceLocation {
        kind: SourceKind::CodexSessions,
        path: root.clone(),
    });
    assert!(matches!(
        result,
        Err(super::monitor::MonitorError::Source(_))
    ));
    fs::remove_dir_all(root).unwrap();
}

fn engine_options(max_runtime: Duration) -> EngineOptions {
    EngineOptions {
        project: None,
        debounce: Duration::from_millis(2),
        reconcile: Duration::from_millis(20),
        max_runtime: Some(max_runtime),
    }
}

/// Pin every agent env var to a missing location so only the fixture under test is
/// detected — otherwise the engine would pick up this machine's real agent data.
fn isolate_agent_env(root: &Path) -> Vec<(&'static str, Option<std::ffi::OsString>)> {
    let saved = [
        "ZCODE_HOME",
        "XDG_DATA_HOME",
        "CLAUDE_CONFIG_DIR",
        "PI_CODING_AGENT_DIR",
        "PI_CODING_AGENT_SESSION_DIR",
    ]
    .iter()
    .map(|name| (*name, std::env::var_os(name)))
    .collect();
    std::env::set_var("ZCODE_HOME", root.join("none/zcode"));
    std::env::set_var("XDG_DATA_HOME", root.join("none/xdg"));
    std::env::set_var("CLAUDE_CONFIG_DIR", root.join("none/claude"));
    std::env::set_var("PI_CODING_AGENT_DIR", root.join("none/pi"));
    std::env::remove_var("PI_CODING_AGENT_SESSION_DIR");
    saved
}

fn restore_agent_env(saved: Vec<(&'static str, Option<std::ffi::OsString>)>) {
    for (name, value) in saved {
        match value {
            Some(value) => std::env::set_var(name, value),
            None => std::env::remove_var(name),
        }
    }
}

fn codex_status(event: &EngineEvent) -> Option<&super::monitor::AgentStatus> {
    match event {
        EngineEvent::Statuses(statuses) => {
            statuses.iter().find(|status| status.agent == Agent::Codex)
        }
    }
}

#[test]
fn engine_reconcile_emits_statuses_repeatedly() {
    let _guard = ENV_LOCK.get_or_init(|| Mutex::new(())).lock().unwrap();
    let root = std::env::temp_dir().join(format!(
        "tokenspeed-watch-{}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let sessions = root.join("sessions");
    fs::create_dir_all(&sessions).unwrap();
    fs::copy(
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/codex.jsonl"),
        sessions.join("rollout.jsonl"),
    )
    .unwrap();
    let old = std::env::var_os("CODEX_HOME");
    let saved = isolate_agent_env(&root);
    std::env::set_var("CODEX_HOME", &root);
    let (tx, rx) = mpsc::channel();
    run_engine(
        engine_options(Duration::from_millis(300)),
        None,
        None,
        |event| {
            if matches!(event, EngineEvent::Statuses(_)) {
                tx.send(()).unwrap();
            }
        },
    )
    .unwrap();
    assert!(rx.try_iter().count() >= 2);
    match old {
        Some(value) => std::env::set_var("CODEX_HOME", value),
        None => std::env::remove_var("CODEX_HOME"),
    }
    restore_agent_env(saved);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn engine_reports_scan_error_and_recovers_on_a_later_event() {
    let _guard = ENV_LOCK.get_or_init(|| Mutex::new(())).lock().unwrap();
    let root = std::env::temp_dir().join(format!(
        "tokenspeed-watch-recover-{}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let sessions = root.join("sessions");
    fs::create_dir_all(&sessions).unwrap();
    fs::write(
        sessions.join("rollout-bad.jsonl"),
        "{\"type\":\"unknown\"}\n",
    )
    .unwrap();
    let old = std::env::var_os("CODEX_HOME");
    let saved = isolate_agent_env(&root);
    std::env::set_var("CODEX_HOME", &root);
    let fixture =
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/codex.jsonl");
    let (tx, rx) = mpsc::channel::<(bool, bool)>();
    let _ = run_engine(
        engine_options(Duration::from_millis(1000)),
        None,
        None,
        |event| {
            if let Some(status) = codex_status(&event) {
                let has_error = status.error.is_some();
                let has_report = status.report.is_some();
                tx.send((has_error, has_report)).unwrap();
                if has_error && !has_report {
                    fs::remove_file(sessions.join("rollout-bad.jsonl")).unwrap();
                    fs::copy(&fixture, sessions.join("rollout-good.jsonl")).unwrap();
                }
            }
        },
    );
    let updates = rx.try_iter().collect::<Vec<_>>();
    assert!(updates.iter().any(|(error, _)| *error));
    assert!(updates.iter().any(|(_, report)| *report));
    match old {
        Some(value) => std::env::set_var("CODEX_HOME", value),
        None => std::env::remove_var("CODEX_HOME"),
    }
    restore_agent_env(saved);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn engine_detects_an_agent_that_installs_while_running() {
    let _guard = ENV_LOCK.get_or_init(|| Mutex::new(())).lock().unwrap();
    let root = std::env::temp_dir().join(format!(
        "tokenspeed-watch-source-recover-{}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir_all(&root).unwrap();
    let old = std::env::var_os("CODEX_HOME");
    let saved = isolate_agent_env(&root);
    std::env::set_var("CODEX_HOME", &root);
    let fixture = fixture("codex.jsonl");
    let delayed_root = root.clone();
    let writer = thread::spawn(move || {
        thread::sleep(Duration::from_millis(40));
        let sessions = delayed_root.join("sessions");
        fs::create_dir_all(&sessions).unwrap();
        fs::copy(fixture, sessions.join("rollout.jsonl")).unwrap();
    });
    let (tx, rx) = mpsc::channel::<(bool, bool)>();
    let _ = run_engine(
        engine_options(Duration::from_millis(500)),
        None,
        None,
        |event| {
            if let Some(status) = codex_status(&event) {
                tx.send((status.installed, status.report.is_some()))
                    .unwrap();
            }
        },
    );
    writer.join().unwrap();
    let updates = rx.try_iter().collect::<Vec<_>>();
    assert!(updates.iter().any(|(installed, _)| !*installed));
    assert!(updates
        .iter()
        .any(|(installed, report)| *installed && *report));
    match old {
        Some(value) => std::env::set_var("CODEX_HOME", value),
        None => std::env::remove_var("CODEX_HOME"),
    }
    restore_agent_env(saved);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn detect_all_installed_reports_every_agent_independently() {
    let _guard = ENV_LOCK.get_or_init(|| Mutex::new(())).lock().unwrap();
    let root = std::env::temp_dir().join(format!(
        "tokenspeed-detect-all-{}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    // ZCode：有效 SQLite 文件
    let zcode_home = root.join("zcode/cli/db");
    fs::create_dir_all(&zcode_home).unwrap();
    {
        let con = rusqlite::Connection::open(zcode_home.join("db.sqlite")).unwrap();
        con.execute_batch("CREATE TABLE t(x); INSERT INTO t VALUES (1);")
            .unwrap();
    }
    // Codex：sessions 目录
    fs::create_dir_all(root.join("codex/sessions")).unwrap();
    // OpenCode：有效 SQLite 文件
    let opencode_home = root.join("opencode");
    fs::create_dir_all(&opencode_home).unwrap();
    {
        let con = rusqlite::Connection::open(opencode_home.join("opencode.db")).unwrap();
        con.execute_batch("CREATE TABLE t(x); INSERT INTO t VALUES (1);")
            .unwrap();
    }
    // Claude：projects 目录
    fs::create_dir_all(root.join("claude/projects")).unwrap();
    // Pi：agent/sessions 目录（PI_CODING_AGENT_DIR 语义，与 pi 的 config.js 一致）
    fs::create_dir_all(root.join("pi/agent/sessions")).unwrap();

    let old_zcode = std::env::var_os("ZCODE_HOME");
    let old_codex = std::env::var_os("CODEX_HOME");
    let old_xdg = std::env::var_os("XDG_DATA_HOME");
    let old_claude = std::env::var_os("CLAUDE_CONFIG_DIR");
    let old_pi_dir = std::env::var_os("PI_CODING_AGENT_DIR");
    let old_pi_sessions = std::env::var_os("PI_CODING_AGENT_SESSION_DIR");
    std::env::set_var("ZCODE_HOME", root.join("zcode"));
    std::env::set_var("CODEX_HOME", root.join("codex"));
    std::env::set_var("XDG_DATA_HOME", root.join("xdg"));
    std::env::set_var("CLAUDE_CONFIG_DIR", root.join("claude"));
    std::env::set_var("PI_CODING_AGENT_DIR", root.join("pi/agent"));
    std::env::remove_var("PI_CODING_AGENT_SESSION_DIR");
    // XDG 指向的 opencode 数据库不存在：该 agent 保持未安装，也不回退污染结果
    let found = detect_all_installed();
    let kinds: Vec<SourceKind> = found.iter().map(|source| source.kind).collect();
    assert_eq!(
        kinds,
        vec![
            SourceKind::ZCodeDb,
            SourceKind::CodexSessions,
            SourceKind::ClaudeProjects,
            SourceKind::PiSessions
        ]
    );

    // 一个 agent 的环境变量指错位置不影响其它 agent 的检测结果
    std::env::set_var("CODEX_HOME", root.join("missing"));
    let found = detect_all_installed();
    let kinds: Vec<SourceKind> = found.iter().map(|source| source.kind).collect();
    assert!(!kinds.contains(&SourceKind::CodexSessions));
    assert!(kinds.contains(&SourceKind::ZCodeDb));

    match old_zcode {
        Some(value) => std::env::set_var("ZCODE_HOME", value),
        None => std::env::remove_var("ZCODE_HOME"),
    }
    match old_codex {
        Some(value) => std::env::set_var("CODEX_HOME", value),
        None => std::env::remove_var("CODEX_HOME"),
    }
    match old_xdg {
        Some(value) => std::env::set_var("XDG_DATA_HOME", value),
        None => std::env::remove_var("XDG_DATA_HOME"),
    }
    match old_claude {
        Some(value) => std::env::set_var("CLAUDE_CONFIG_DIR", value),
        None => std::env::remove_var("CLAUDE_CONFIG_DIR"),
    }
    match old_pi_dir {
        Some(value) => std::env::set_var("PI_CODING_AGENT_DIR", value),
        None => std::env::remove_var("PI_CODING_AGENT_DIR"),
    }
    match old_pi_sessions {
        Some(value) => std::env::set_var("PI_CODING_AGENT_SESSION_DIR", value),
        None => std::env::remove_var("PI_CODING_AGENT_SESSION_DIR"),
    }
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn pi_sessions_source_is_detected_and_scanned() {
    let _guard = ENV_LOCK.get_or_init(|| Mutex::new(())).lock().unwrap();
    let root = std::env::temp_dir().join(format!(
        "tokenspeed-pi-scan-{}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let sessions = root.join("agent/sessions");
    fs::create_dir_all(&sessions).unwrap();
    fs::write(
        sessions.join("2025-10-09T08-00-00-000Z_sess-pi-1.jsonl"),
        fs::read_to_string(fixture("pi.jsonl")).unwrap(),
    )
    .unwrap();
    let old_dir = std::env::var_os("PI_CODING_AGENT_DIR");
    let old_sessions = std::env::var_os("PI_CODING_AGENT_SESSION_DIR");
    std::env::set_var("PI_CODING_AGENT_DIR", root.join("agent"));
    std::env::remove_var("PI_CODING_AGENT_SESSION_DIR");
    let report = scan_once(&Selector {
        agent: Agent::Pi,
        project: None,
        session: None,
    })
    .unwrap()
    .unwrap();
    assert_eq!(report.agent, Agent::Pi);
    assert_eq!(report.session, "sess-pi-1");
    assert_eq!(report.project.as_deref(), Some("/work/pi-project"));
    assert_eq!(report.turns.len(), 1);
    assert_eq!(report.turns[0].output_tokens, 100);
    // Fixture timestamps are stale relative to the real clock: never running.
    assert!(!report.running);
    match old_dir {
        Some(value) => std::env::set_var("PI_CODING_AGENT_DIR", value),
        None => std::env::remove_var("PI_CODING_AGENT_DIR"),
    }
    match old_sessions {
        Some(value) => std::env::set_var("PI_CODING_AGENT_SESSION_DIR", value),
        None => std::env::remove_var("PI_CODING_AGENT_SESSION_DIR"),
    }
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn default_codex_scan_uses_newest_rollout_file() {
    let _guard = ENV_LOCK.get_or_init(|| Mutex::new(())).lock().unwrap();
    let root = std::env::temp_dir().join(format!(
        "tokenspeed-scan-newest-{}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let sessions = root.join("sessions");
    fs::create_dir_all(&sessions).unwrap();
    fs::write(
        sessions.join("rollout-old.jsonl"),
        fs::read_to_string(fixture("codex.jsonl"))
            .unwrap()
            .replace("sess-cx-1", "sess-old"),
    )
    .unwrap();
    std::thread::sleep(Duration::from_millis(10));
    fs::write(
        sessions.join("rollout-new.jsonl"),
        fs::read_to_string(fixture("codex.jsonl"))
            .unwrap()
            .replace("sess-cx-1", "sess-new"),
    )
    .unwrap();
    let old = std::env::var_os("CODEX_HOME");
    std::env::set_var("CODEX_HOME", &root);
    let report = super::monitor::scan_once(&Selector {
        agent: Agent::Codex,
        project: None,
        session: None,
    })
    .unwrap()
    .unwrap();
    assert_eq!(report.session, "sess-new");
    match old {
        Some(value) => std::env::set_var("CODEX_HOME", value),
        None => std::env::remove_var("CODEX_HOME"),
    }
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn default_codex_scan_merges_previous_completed_rollout_for_latest_running_session() {
    let _guard = ENV_LOCK.get_or_init(|| Mutex::new(())).lock().unwrap();
    let root = std::env::temp_dir().join(format!(
        "tokenspeed-scan-same-session-{}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let sessions = root.join("sessions");
    fs::create_dir_all(&sessions).unwrap();
    let completed = fs::read_to_string(fixture("codex.jsonl")).unwrap();
    fs::write(
        sessions.join("rollout-completed.jsonl"),
        completed.replace("sess-cx-1", "sess-same"),
    )
    .unwrap();
    std::thread::sleep(Duration::from_millis(10));
    fs::write(
        sessions.join("rollout-running.jsonl"),
        fs::read_to_string(fixture("codex-incomplete.jsonl"))
            .unwrap()
            .replace("sess-cx-incomplete", "sess-same"),
    )
    .unwrap();
    let old = std::env::var_os("CODEX_HOME");
    std::env::set_var("CODEX_HOME", &root);
    let report = super::monitor::scan_once(&Selector {
        agent: Agent::Codex,
        project: None,
        session: None,
    })
    .unwrap()
    .unwrap();
    assert_eq!(report.session, "sess-same");
    assert!(report.running);
    assert_eq!(report.turns.len(), 1);
    assert_eq!(report.turns[0].output_tokens, 25);
    match old {
        Some(value) => std::env::set_var("CODEX_HOME", value),
        None => std::env::remove_var("CODEX_HOME"),
    }
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn default_codex_merge_deduplicates_duplicate_turns() {
    let _guard = ENV_LOCK.get_or_init(|| Mutex::new(())).lock().unwrap();
    let root = std::env::temp_dir().join(format!(
        "tokenspeed-scan-duplicate-turn-{}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let sessions = root.join("sessions");
    fs::create_dir_all(&sessions).unwrap();
    let completed = fs::read_to_string(fixture("codex.jsonl")).unwrap();
    let completed = completed.replace("sess-cx-1", "sess-duplicate");
    fs::write(sessions.join("rollout-old.jsonl"), &completed).unwrap();
    std::thread::sleep(Duration::from_millis(10));
    fs::write(sessions.join("rollout-new.jsonl"), &completed).unwrap();
    let old = std::env::var_os("CODEX_HOME");
    std::env::set_var("CODEX_HOME", &root);
    let report = super::monitor::scan_once(&Selector {
        agent: Agent::Codex,
        project: None,
        session: None,
    })
    .unwrap()
    .unwrap();
    assert_eq!(report.session, "sess-duplicate");
    assert_eq!(report.turns.len(), 1);
    assert_eq!(report.session_total_tokens, 25);
    match old {
        Some(value) => std::env::set_var("CODEX_HOME", value),
        None => std::env::remove_var("CODEX_HOME"),
    }
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn codex_multi_rollout_totals_include_turns_outside_each_file_recent_ten() {
    let _guard = ENV_LOCK.get_or_init(|| Mutex::new(())).lock().unwrap();
    let root = std::env::temp_dir().join(format!(
        "tokenspeed-scan-many-turns-{}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let sessions = root.join("sessions");
    fs::create_dir_all(&sessions).unwrap();
    let rollout = |first: u32| {
        let mut jsonl =
            "{\"type\":\"session_meta\",\"payload\":{\"id\":\"sess-many\",\"cwd\":\"/project\"}}\n"
                .to_string();
        for index in first..first + 12 {
            let started = i64::from(index) * 100;
            let completed = started + 50;
            let total = i64::from(index - first + 1) * 25;
            jsonl.push_str(&format!(
                concat!(
                    "{{\"type\":\"event_msg\",\"payload\":{{\"type\":\"task_started\",\"turn_id\":\"turn-{index}\",\"started_at\":{started}}}}}\n",
                    "{{\"type\":\"event_msg\",\"payload\":{{\"type\":\"token_count\",\"info\":{{\"total_token_usage\":{{\"output_tokens\":{total}}}}}}}}}\n",
                    "{{\"type\":\"event_msg\",\"payload\":{{\"type\":\"task_complete\",\"turn_id\":\"turn-{index}\",\"completed_at\":{completed}}}}}\n"
                ),
                index = index,
                started = started,
                completed = completed,
                total = total,
            ));
        }
        jsonl
    };
    fs::write(sessions.join("rollout-a.jsonl"), rollout(1)).unwrap();
    std::thread::sleep(Duration::from_millis(10));
    fs::write(sessions.join("rollout-b.jsonl"), rollout(13)).unwrap();
    let old = std::env::var_os("CODEX_HOME");
    std::env::set_var("CODEX_HOME", &root);
    let report = super::monitor::scan_once(&Selector {
        agent: Agent::Codex,
        project: None,
        session: None,
    })
    .unwrap()
    .unwrap();
    assert_eq!(report.session_total_tokens, 600);
    assert_eq!(report.turns.len(), 10);
    match old {
        Some(value) => std::env::set_var("CODEX_HOME", value),
        None => std::env::remove_var("CODEX_HOME"),
    }
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn invalid_explicit_source_does_not_fallback_to_default() {
    let _guard = ENV_LOCK.get_or_init(|| Mutex::new(())).lock().unwrap();
    let root = std::env::temp_dir().join(format!(
        "tokenspeed-invalid-source-{}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir_all(&root).unwrap();
    let old_zcode = std::env::var_os("ZCODE_HOME");
    std::env::set_var("ZCODE_HOME", &root);
    assert!(matches!(
        super::monitor::detect_source(Agent::ZCode),
        Err(super::monitor::MonitorError::Source(_))
    ));
    match old_zcode {
        Some(value) => std::env::set_var("ZCODE_HOME", value),
        None => std::env::remove_var("ZCODE_HOME"),
    }
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn scan_once_honors_project_and_session_pins_without_fallback_and_is_read_only() {
    let _guard = ENV_LOCK.get_or_init(|| Mutex::new(())).lock().unwrap();
    let root = std::env::temp_dir().join(format!(
        "tokenspeed-scan-{}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let db_dir = root.join("cli/db");
    fs::create_dir_all(&db_dir).unwrap();
    let db = db_dir.join("db.sqlite");
    let con = rusqlite::Connection::open(&db).unwrap();
    con.execute_batch(
        &fs::read_to_string(
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/zcode.sql"),
        )
        .unwrap(),
    )
    .unwrap();
    drop(con);
    let before = fs::metadata(&db).unwrap().modified().unwrap();
    let old = std::env::var_os("ZCODE_HOME");
    std::env::set_var("ZCODE_HOME", &root);

    let selected = super::monitor::scan_once(&Selector {
        agent: Agent::ZCode,
        project: Some("/redacted/project".into()),
        session: Some("sess-z-1".into()),
    })
    .unwrap()
    .unwrap();
    assert_eq!(selected.session, "sess-z-1");
    assert_eq!(selected.project.as_deref(), Some("/redacted/project"));
    assert!(super::monitor::scan_once(&Selector {
        agent: Agent::ZCode,
        project: Some("/redacted/project".into()),
        session: Some("sess-z-2".into()),
    })
    .unwrap()
    .is_none());
    assert!(super::monitor::scan_once(&Selector {
        agent: Agent::ZCode,
        project: Some("/redacted/missing".into()),
        session: None,
    })
    .unwrap()
    .is_none());
    assert_eq!(fs::metadata(&db).unwrap().modified().unwrap(), before);

    match old {
        Some(value) => std::env::set_var("ZCODE_HOME", value),
        None => std::env::remove_var("ZCODE_HOME"),
    }
    fs::remove_dir_all(root).unwrap();
}

fn totals_snapshot(id: &str, project: Option<&str>, output: u64, input: u64) -> Snapshot {
    Snapshot {
        agent: Agent::Codex,
        session: crate::collectors::SessionRef {
            id: id.into(),
            project: project.map(str::to_owned),
        },
        turns: Vec::new(),
        all_turns: Vec::new(),
        session_total_tokens: output,
        session_total_input_tokens: input,
        session_total_elapsed_ms: 0,
        session_accuracy: crate::collectors::Accuracy::Estimated,
        activity_at: 0,
        running: false,
    }
}

#[test]
fn totals_aggregate_by_project_sorted_descending_with_unknown_bucket() {
    use std::collections::HashMap;

    let snapshots = vec![
        totals_snapshot("s1", Some("/work/alpha"), 100, 900),
        totals_snapshot("s2", Some("/work/beta"), 50, 50),
        totals_snapshot("s3", Some("/work/alpha"), 10, 0),
        totals_snapshot("s4", None, 7, 3),
    ];
    let mut slot = HashMap::new();
    rebuild_session_totals(&mut slot, &snapshots);
    assert_eq!(slot.len(), 4);
    let totals = aggregate_totals(&slot);
    // 总数 = 明细之和（无项目组也计入）
    assert_eq!(totals.total_tokens, 1120);
    assert_eq!(totals.projects.len(), 3);
    // 按 tokens 降序
    assert_eq!(totals.projects[0].project.as_deref(), Some("/work/alpha"));
    assert_eq!(totals.projects[0].tokens, 1010);
    assert_eq!(totals.projects[1].project.as_deref(), Some("/work/beta"));
    assert_eq!(totals.projects[1].tokens, 100);
    // 无项目会话单独归组，参与总数
    assert_eq!(totals.projects[2].project, None);
    assert_eq!(totals.projects[2].tokens, 10);
}

#[test]
fn totals_rebuild_replaces_stale_entries_and_aggregate_of_empty_is_zero() {
    use std::collections::HashMap;

    let mut slot = HashMap::new();
    slot.insert(
        "deleted-session".to_string(),
        (Some("/work/ghost".to_string()), 999),
    );
    let snapshots = vec![totals_snapshot("s1", Some("/work/alpha"), 10, 20)];
    rebuild_session_totals(&mut slot, &snapshots);
    // 全量重建清掉已消失的会话，总数不残留幽灵数据
    assert_eq!(slot.len(), 1);
    let totals = aggregate_totals(&slot);
    assert_eq!(totals.total_tokens, 30);
    assert_eq!(
        aggregate_totals(&HashMap::new()),
        AgentTotals::default()
    );
}
