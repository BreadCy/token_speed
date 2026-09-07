use super::collectors::{
    collect_claude, collect_codex, collect_codex_with_running, collect_opencode, collect_zcode,
    collect_zcode_with_running, Accuracy, Agent, SourceError,
};
use rusqlite::{params, Connection};
use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(name)
}

#[test]
fn zcode_joins_project_and_aggregates_completed_turn() {
    let con = Connection::open_in_memory().unwrap();
    con.execute_batch(&fs::read_to_string(fixture("zcode.sql")).unwrap())
        .unwrap();
    let snapshots = collect_zcode(&con).unwrap();
    assert_eq!(snapshots.len(), 2);
    assert!(snapshots.iter().all(|snapshot| snapshot
        .turns
        .iter()
        .all(|turn| turn.session.id == snapshot.session.id)));
    let snapshot = snapshots
        .iter()
        .find(|snapshot| snapshot.session.id == "sess-z-1")
        .unwrap();
    assert_eq!(snapshot.agent, Agent::ZCode);
    assert_eq!(snapshot.session.id, "sess-z-1");
    assert_eq!(
        snapshot.session.project.as_deref(),
        Some("/redacted/project")
    );
    assert_eq!(snapshot.turns.len(), 5);
    let turn = snapshot
        .turns
        .iter()
        .find(|turn| turn.turn_id == "turn-z-1")
        .unwrap();
    assert_eq!(turn.turn_id, "turn-z-1");
    assert_eq!(turn.output_tokens, 100);
    assert_eq!(turn.started_at, 1_000);
    assert_eq!(turn.completed_at, 3_000);
    assert_eq!(turn.accuracy, Accuracy::Estimated);
    assert!((turn.effective_speed - 50.0).abs() < 0.01);
    assert_eq!(turn.model.as_deref(), Some("model-z"));
    assert_eq!(turn.model_accuracy, Accuracy::Exact);
    assert!((turn.model_speed.unwrap() - (100.0 * 1000.0 / 2600.0)).abs() < 0.01);
    let mixed = snapshot
        .turns
        .iter()
        .find(|turn| turn.turn_id == "turn-z-mixed")
        .unwrap();
    assert_eq!(mixed.model, None);
    assert_eq!(mixed.model_speed, None);
    assert_eq!(mixed.model_accuracy, Accuracy::Unavailable);
    let missing = snapshot
        .turns
        .iter()
        .find(|turn| turn.turn_id == "turn-z-missing")
        .unwrap();
    assert_eq!(missing.model_speed, None);
    assert_eq!(missing.model_accuracy, Accuracy::Unavailable);
    // 部分请求行缺 first_token_at：用有首 token 的行计算模型速度，精度降为估算
    let partial = snapshot
        .turns
        .iter()
        .find(|turn| turn.turn_id == "turn-z-ttfb")
        .unwrap();
    assert_eq!(partial.model.as_deref(), Some("model-z"));
    assert_eq!(partial.model_accuracy, Accuracy::Estimated);
    assert!((partial.model_speed.unwrap() - (40.0 * 1000.0 / 400.0)).abs() < 0.01);
    let missing_model = snapshot
        .turns
        .iter()
        .find(|turn| turn.turn_id == "turn-z-missing-model")
        .unwrap();
    assert_eq!(missing_model.model, None);
}

#[test]
fn codex_requires_task_boundaries_and_deduplicates_cumulative_usage() {
    let snapshots = collect_codex(&fixture("codex.jsonl")).unwrap();
    let snapshot = &snapshots[0];
    assert_eq!(snapshot.agent, Agent::Codex);
    assert_eq!(snapshot.session.id, "sess-cx-1");
    assert_eq!(
        snapshot.session.project.as_deref(),
        Some("/redacted/codex-project")
    );
    assert_eq!(snapshot.turns.len(), 1);
    let turn = &snapshot.turns[0];
    assert_eq!(turn.turn_id, "turn-cx-1");
    assert_eq!(turn.output_tokens, 25);
    assert_eq!(turn.started_at, 100_000);
    assert_eq!(turn.completed_at, 300_000);
    assert_eq!(turn.accuracy, Accuracy::Estimated);
    assert!((turn.effective_speed - 0.125).abs() < 0.01);
    assert_eq!(turn.model.as_deref(), Some("gpt-redacted"));
    assert_eq!(turn.model_accuracy, Accuracy::Unavailable);
    let incomplete = fixture("codex-incomplete.jsonl");
    assert_eq!(
        collect_codex(&incomplete),
        Err(SourceError::NoCompletedTurn)
    );
    assert_eq!(
        collect_codex(&fixture("codex-mismatched.jsonl")),
        Err(SourceError::NoCompletedTurn)
    );
    assert_eq!(
        collect_codex(&fixture("codex-malformed.jsonl")),
        Err(SourceError::NoCompletedTurn)
    );
}

#[test]
fn codex_session_totals_include_turns_between_large_file_windows() {
    let path = std::env::temp_dir().join(format!(
        "tokenspeed-codex-full-session-{}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let turn = |id: &str, started_at: i64, completed_at: i64, output_tokens: i64| {
        format!(
            concat!(
                "{{\"type\":\"event_msg\",\"payload\":{{\"type\":\"task_started\",\"turn_id\":\"{id}\",\"started_at\":{started_at}}}}}\n",
                "{{\"type\":\"event_msg\",\"payload\":{{\"type\":\"token_count\",\"info\":{{\"total_token_usage\":{{\"output_tokens\":{output_tokens}}}}}}}}}\n",
                "{{\"type\":\"event_msg\",\"payload\":{{\"type\":\"task_complete\",\"turn_id\":\"{id}\",\"completed_at\":{completed_at}}}}}\n"
            ),
            id = id,
            started_at = started_at,
            completed_at = completed_at,
            output_tokens = output_tokens,
        )
    };
    let mut jsonl =
        "{\"type\":\"session_meta\",\"payload\":{\"id\":\"large-session\",\"cwd\":\"/project\"}}\n"
            .to_string();
    jsonl.push_str(&turn("turn-1", 100, 200, 25));
    jsonl.push_str(&format!("{{\"ignored\":\"{}\"}}\n", "x".repeat(70 * 1024)));
    jsonl.push_str(&turn("turn-2", 300, 400, 50));
    jsonl.push_str(&format!(
        "{{\"ignored\":\"{}\"}}\n",
        "y".repeat(1100 * 1024)
    ));
    jsonl.push_str(&turn("turn-3", 500, 600, 75));
    fs::write(&path, jsonl).unwrap();

    let snapshots = collect_codex(&path).unwrap();
    assert_eq!(snapshots[0].session_total_tokens, 75);
    assert_eq!(snapshots[0].turns.len(), 3);

    fs::remove_file(path).unwrap();
}

#[test]
fn opencode_uses_session_id_and_closes_at_stop() {
    let con = Connection::open_in_memory().unwrap();
    con.execute_batch(&fs::read_to_string(fixture("opencode.sql")).unwrap())
        .unwrap();
    let snapshots = collect_opencode(&con).unwrap();
    let snapshot = &snapshots[0];
    assert_eq!(snapshot.agent, Agent::OpenCode);
    assert_eq!(snapshot.session.id, "sess-oc-1");
    assert_eq!(
        snapshot.session.project.as_deref(),
        Some("/redacted/opencode-project")
    );
    assert_eq!(snapshot.turns.len(), 1);
    let turn = &snapshot.turns[0];
    assert_eq!(turn.output_tokens, 50);
    assert_eq!(turn.started_at, 500);
    assert_eq!(turn.completed_at, 3_000);
    assert_eq!(turn.accuracy, Accuracy::Estimated);
    assert_eq!(turn.model.as_deref(), Some("model-oc"));
}

#[test]
fn opencode_drops_turns_after_malformed_or_incomplete_assistant_metadata() {
    let con = Connection::open_in_memory().unwrap();
    con.execute_batch(
        "CREATE TABLE session (id TEXT PRIMARY KEY, directory TEXT);
         CREATE TABLE message (id TEXT PRIMARY KEY, session_id TEXT, time_created INTEGER, time_updated INTEGER, data TEXT);",
    )
    .unwrap();
    for (sid, bad_data) in [
        (
            "sess-oc-malformed",
            r#"{"role":"assistant","tokens":{"output":"bad"},"time":{"created":200,"completed":300},"finish":"tool-calls"}"#,
        ),
        (
            "sess-oc-missing-output",
            r#"{"role":"assistant","time":{"created":200,"completed":300},"finish":"tool-calls"}"#,
        ),
        (
            "sess-oc-missing-completed",
            r#"{"role":"assistant","tokens":{"output":20},"time":{"created":200},"finish":"tool-calls"}"#,
        ),
        (
            "sess-oc-invalid-time",
            r#"{"role":"assistant","tokens":{"output":20},"time":{"created":300,"completed":200},"finish":"tool-calls"}"#,
        ),
        (
            "sess-oc-negative-output",
            r#"{"role":"assistant","tokens":{"output":-1},"time":{"created":200,"completed":300},"finish":"tool-calls"}"#,
        ),
    ] {
        con.execute(
            "INSERT INTO session VALUES (?1, ?2)",
            params![sid, "/redacted/opencode-project"],
        )
        .unwrap();
        con.execute(
            "INSERT INTO message VALUES (?1, ?2, 100, 100, ?3)",
            params![format!("{sid}-user"), sid, r#"{"role":"user","parts":[]}"#],
        )
        .unwrap();
        con.execute(
            "INSERT INTO message VALUES (?1, ?2, 150, 180, ?3)",
            params![format!("{sid}-seed"), sid, r#"{"role":"assistant","modelID":"model-oc","tokens":{"output":10},"time":{"created":150,"completed":180},"finish":"tool-calls"}"#],
        )
        .unwrap();
        con.execute(
            "INSERT INTO message VALUES (?1, ?2, 200, 300, ?3)",
            params![format!("{sid}-bad"), sid, bad_data],
        )
        .unwrap();
        con.execute(
            "INSERT INTO message VALUES (?1, ?2, 300, 400, ?3)",
            params![
                format!("{sid}-stop"),
                sid,
                r#"{"role":"assistant","modelID":"model-oc","tokens":{"output":30},"time":{"created":300,"completed":400},"finish":"stop"}"#
            ],
        )
        .unwrap();
    }

    assert_eq!(collect_opencode(&con), Err(SourceError::NoCompletedTurn));
}

#[test]
fn opencode_human_user_replaces_active_and_orphan_assistant_is_ignored() {
    let con = Connection::open_in_memory().unwrap();
    con.execute_batch(
        "CREATE TABLE session (id TEXT PRIMARY KEY, directory TEXT);
         CREATE TABLE message (id TEXT PRIMARY KEY, session_id TEXT, time_created INTEGER, time_updated INTEGER, data TEXT);
         INSERT INTO session VALUES ('sess-oc-replaced', '/project');
         INSERT INTO session VALUES ('sess-oc-orphan', '/project');",
    )
    .unwrap();
    let rows = [
        (
            "replaced-user-1",
            "sess-oc-replaced",
            100,
            100,
            r#"{"role":"user","parts":[]}"#,
        ),
        (
            "replaced-assistant-1",
            "sess-oc-replaced",
            150,
            200,
            r#"{"role":"assistant","modelID":"model","tokens":{"output":10},"time":{"created":150,"completed":200},"finish":"tool-calls"}"#,
        ),
        (
            "replaced-user-2",
            "sess-oc-replaced",
            300,
            300,
            r#"{"role":"user","parts":[]}"#,
        ),
        (
            "replaced-assistant-2",
            "sess-oc-replaced",
            350,
            400,
            r#"{"role":"assistant","modelID":"model","tokens":{"output":20},"time":{"created":350,"completed":400},"finish":"stop"}"#,
        ),
        (
            "orphan-assistant",
            "sess-oc-orphan",
            100,
            200,
            r#"{"role":"assistant","modelID":"model","tokens":{"output":20},"time":{"created":100,"completed":200},"finish":"stop"}"#,
        ),
    ];
    for (id, sid, created, updated, data) in rows {
        con.execute(
            "INSERT INTO message VALUES (?1, ?2, ?3, ?4, ?5)",
            params![id, sid, created, updated, data],
        )
        .unwrap();
    }

    let snapshots = collect_opencode(&con).unwrap();
    assert_eq!(snapshots.len(), 1);
    let turn = &snapshots[0].turns[0];
    assert_eq!(turn.session.id, "sess-oc-replaced");
    assert_eq!(turn.output_tokens, 20);
    assert_eq!(turn.started_at, 300);
}

#[test]
fn sqlite_schema_errors_are_unknown_schema() {
    let empty = Connection::open_in_memory().unwrap();
    assert!(matches!(
        collect_zcode(&empty),
        Err(SourceError::UnknownSchema(_))
    ));
    assert!(matches!(
        collect_opencode(&empty),
        Err(SourceError::UnknownSchema(_))
    ));

    let missing_column = Connection::open_in_memory().unwrap();
    missing_column
        .execute_batch(
            "CREATE TABLE session (id TEXT PRIMARY KEY);
             CREATE TABLE model_usage (session_id TEXT, turn_id TEXT, started_at INTEGER);",
        )
        .unwrap();
    assert!(matches!(
        collect_zcode(&missing_column),
        Err(SourceError::UnknownSchema(_))
    ));
}

#[test]
fn snapshots_keep_only_the_latest_ten_turns_in_descending_completion_order() {
    let con = Connection::open_in_memory().unwrap();
    con.execute_batch(
        "CREATE TABLE session (id TEXT PRIMARY KEY, directory TEXT);
         CREATE TABLE model_usage (
             session_id TEXT, turn_id TEXT, model_id TEXT, status TEXT,
             started_at INTEGER, first_token_at INTEGER, completed_at INTEGER,
             output_tokens INTEGER
         );
         INSERT INTO session VALUES ('session', '/project');",
    )
    .unwrap();
    for n in 1..=12 {
        con.execute(
            "INSERT INTO model_usage VALUES (?1, ?2, 'model', 'completed', ?3, ?4, ?5, 1)",
            params![
                "session",
                format!("turn-{n}"),
                n * 100,
                n * 100 + 10,
                n * 100 + 50
            ],
        )
        .unwrap();
    }
    let snapshots = collect_zcode(&con).unwrap();
    let ids = snapshots[0]
        .turns
        .iter()
        .map(|turn| turn.turn_id.clone())
        .collect::<Vec<_>>();
    assert_eq!(
        ids,
        (3..=12)
            .rev()
            .map(|n| format!("turn-{n}"))
            .collect::<Vec<_>>()
    );
    assert_eq!(snapshots[0].session_total_tokens, 12);
    assert_eq!(snapshots[0].session_total_elapsed_ms, 600);
    assert_eq!(snapshots[0].session_accuracy, Accuracy::Estimated);
}

#[test]
fn opencode_human_user_without_assistant_is_a_running_pending_session() {
    let con = Connection::open_in_memory().unwrap();
    con.execute_batch(
        "CREATE TABLE session (id TEXT PRIMARY KEY, directory TEXT);
         CREATE TABLE message (id TEXT PRIMARY KEY, session_id TEXT, time_created INTEGER, time_updated INTEGER, data TEXT);
         INSERT INTO session VALUES ('sess-pending', '/pending');
         INSERT INTO message VALUES ('user-1', 'sess-pending', 100, 100, '{\"role\":\"user\",\"parts\":[]}');",
    )
    .unwrap();
    let snapshots = super::collectors::collect_opencode_with_running(&con).unwrap();
    assert_eq!(snapshots.len(), 1);
    assert_eq!(snapshots[0].session.id, "sess-pending");
    assert!(snapshots[0].running);
    assert!(snapshots[0].turns.is_empty());
    assert_eq!(snapshots[0].session_total_tokens, 0);
}

#[test]
fn claude_closes_from_real_user_and_deduplicates_assistant_ids() {
    let snapshots = collect_claude(&fixture("claude.jsonl")).unwrap();
    let snapshot = &snapshots[0];
    assert_eq!(snapshot.agent, Agent::ClaudeCode);
    assert_eq!(snapshot.session.id, "sess-cc-1");
    assert_eq!(
        snapshot.session.project.as_deref(),
        Some("/redacted/claude-project")
    );
    assert_eq!(snapshot.turns.len(), 1);
    let turn = &snapshot.turns[0];
    assert_eq!(turn.output_tokens, 30);
    assert_eq!(turn.started_at, 1_767_225_600_000);
    assert_eq!(turn.completed_at, 1_767_225_602_500);
    assert_eq!(turn.accuracy, Accuracy::Estimated);
    assert_eq!(turn.model, None);
}

#[test]
fn incomplete_codex_and_zcode_sessions_are_reported_as_running() {
    let codex = collect_codex_with_running(&fixture("codex-incomplete.jsonl")).unwrap();
    assert_eq!(codex.len(), 1);
    assert!(codex[0].running);
    assert!(codex[0].turns.is_empty());
    assert!(codex[0].activity_at > 0);

    let con = Connection::open_in_memory().unwrap();
    con.execute_batch(&fs::read_to_string(fixture("zcode.sql")).unwrap())
        .unwrap();
    let zcode = collect_zcode_with_running(&con).unwrap();
    let active = zcode
        .iter()
        .find(|snapshot| snapshot.session.id == "sess-z-1")
        .unwrap();
    assert!(active.running);
    assert_eq!(active.activity_at, 13_000);
}

#[test]
fn claude_sorts_files_and_deduplicates_per_session_after_active_confirmation() {
    let root = std::env::temp_dir().join(format!(
        "tokenspeed-claude-{}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir_all(&root).unwrap();
    fs::write(
        root.join("a.jsonl"),
        "{\"type\":\"assistant\",\"sessionId\":\"sess-a\",\"timestamp\":50,\"message\":{\"id\":\"shared\",\"model\":\"model\",\"usage\":{\"output_tokens\":9},\"stop_reason\":\"end_turn\"}}\n",
    )
    .unwrap();
    fs::write(
        root.join("b.jsonl"),
        concat!(
            "{\"type\":\"assistant\",\"sessionId\":\"sess-a\",\"timestamp\":200,\"message\":{\"id\":\"shared\",\"model\":\"model\",\"usage\":{\"output_tokens\":10},\"stop_reason\":\"end_turn\"}}\n",
            "{\"type\":\"user\",\"sessionId\":\"sess-a\",\"cwd\":\"/a\",\"timestamp\":100,\"message\":{\"role\":\"user\",\"content\":[{\"type\":\"text\"}]}}\n",
            "{\"type\":\"user\",\"sessionId\":\"sess-c\",\"cwd\":\"/c\",\"timestamp\":100,\"message\":{\"role\":\"user\",\"content\":[{\"type\":\"text\"}]}}\n",
            "{\"type\":\"assistant\",\"sessionId\":\"sess-c\",\"timestamp\":200,\"message\":{\"id\":\"shared\",\"model\":\"model\",\"usage\":{\"output_tokens\":20},\"stop_reason\":\"end_turn\"}}\n",
            "{\"type\":\"user\",\"sessionId\":\"sess-bad\",\"cwd\":\"/bad\",\"timestamp\":100,\"message\":{\"role\":\"user\",\"content\":[{\"type\":\"text\"}]}}\n",
            "{\"type\":\"assistant\",\"sessionId\":\"sess-bad\",\"timestamp\":200,\"message\":{\"id\":\"bad\",\"model\":\"model\",\"usage\":{\"output_tokens\":\"bad\"},\"stop_reason\":\"tool_use\"}}\n",
            "{\"type\":\"assistant\",\"sessionId\":\"sess-bad\",\"timestamp\":300,\"message\":{\"id\":\"bad-2\",\"model\":\"model\",\"usage\":{\"output_tokens\":30},\"stop_reason\":\"end_turn\"}}\n",
            "{\"type\":\"user\",\"sessionId\":\"sess-no-ts\",\"cwd\":\"/no-ts\",\"timestamp\":100,\"message\":{\"role\":\"user\",\"content\":[{\"type\":\"text\"}]}}\n",
            "{\"type\":\"assistant\",\"sessionId\":\"sess-no-ts\",\"message\":{\"id\":\"no-ts-bad\",\"model\":\"model\",\"usage\":{\"output_tokens\":\"bad\"},\"stop_reason\":\"tool_use\"}}\n",
            "{\"type\":\"assistant\",\"sessionId\":\"sess-no-ts\",\"timestamp\":300,\"message\":{\"id\":\"no-ts-stop\",\"model\":\"model\",\"usage\":{\"output_tokens\":30},\"stop_reason\":\"end_turn\"}}\n",
            "{\"type\":\"user\",\"sessionId\":\"sess-no-ts-valid\",\"cwd\":\"/no-ts-valid\",\"timestamp\":100,\"message\":{\"role\":\"user\",\"content\":[{\"type\":\"text\"}]}}\n",
            "{\"type\":\"assistant\",\"sessionId\":\"sess-no-ts-valid\",\"message\":{\"id\":\"no-ts-valid\",\"model\":\"model\",\"usage\":{\"output_tokens\":20},\"stop_reason\":\"tool_use\"}}\n",
            "{\"type\":\"assistant\",\"sessionId\":\"sess-no-ts-valid\",\"timestamp\":300,\"message\":{\"id\":\"no-ts-valid-stop\",\"model\":\"model\",\"usage\":{\"output_tokens\":30},\"stop_reason\":\"end_turn\"}}\n",
        ),
    )
    .unwrap();

    let snapshots = collect_claude(&root).unwrap();
    let mut by_session = snapshots
        .into_iter()
        .map(|snapshot| (snapshot.session.id.clone(), snapshot))
        .collect::<std::collections::HashMap<_, _>>();
    assert_eq!(by_session.len(), 2);
    assert_eq!(
        by_session.remove("sess-a").unwrap().turns[0].output_tokens,
        10
    );
    assert_eq!(
        by_session.remove("sess-c").unwrap().turns[0].output_tokens,
        20
    );
    assert!(!by_session.contains_key("sess-no-ts"));
    assert!(!root.exists() || fs::remove_dir_all(&root).is_ok());
}
