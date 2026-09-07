//! Portable, read-only metadata collectors for the four supported agents.
//!
//! Parsers intentionally return completed turns only. Message bodies are never
//! inspected beyond the metadata needed to identify a turn.

use rusqlite::Connection;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Agent {
    #[serde(rename = "zcode")]
    ZCode,
    #[serde(rename = "codex")]
    Codex,
    #[serde(rename = "opencode")]
    OpenCode,
    #[serde(rename = "claude-code")]
    ClaudeCode,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Accuracy {
    Exact,
    Estimated,
    Unavailable,
}

impl Agent {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ZCode => "zcode",
            Self::Codex => "codex",
            Self::OpenCode => "opencode",
            Self::ClaudeCode => "claude-code",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionRef {
    pub id: String,
    /// For all sources this is the user-visible project directory when present.
    pub project: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TurnMeasurement {
    pub turn_id: String,
    pub session: SessionRef,
    pub output_tokens: u64,
    pub started_at: i64,
    pub completed_at: i64,
    pub effective_speed: f64,
    pub accuracy: Accuracy,
    pub model: Option<String>,
    pub model_speed: Option<f64>,
    pub model_accuracy: Accuracy,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Snapshot {
    pub agent: Agent,
    pub session: SessionRef,
    pub turns: Vec<TurnMeasurement>,
    #[serde(skip)]
    pub(crate) all_turns: Vec<TurnMeasurement>,
    pub session_total_tokens: u64,
    pub session_total_elapsed_ms: i64,
    pub session_accuracy: Accuracy,
    pub activity_at: i64,
    pub running: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SourceError {
    Io(String),
    Sqlite(String),
    UnknownSchema(String),
    NoCompletedTurn,
}

impl fmt::Display for SourceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(s) => write!(f, "source I/O error: {s}"),
            Self::Sqlite(s) => write!(f, "source SQLite error: {s}"),
            Self::UnknownSchema(s) => write!(f, "unknown source schema: {s}"),
            Self::NoCompletedTurn => f.write_str("no completed turn"),
        }
    }
}

impl std::error::Error for SourceError {}

fn speed(tokens: u64, started: i64, completed: i64) -> f64 {
    let duration = completed.saturating_sub(started);
    if duration > 0 {
        tokens as f64 * 1000.0 / duration as f64
    } else {
        0.0
    }
}

fn session(id: impl Into<String>, project: Option<String>) -> SessionRef {
    SessionRef {
        id: id.into(),
        project,
    }
}

fn model_name(names: impl IntoIterator<Item = Option<String>>) -> Option<String> {
    let mut found: Option<String> = None;
    for name in names {
        let name = name.filter(|s| !s.is_empty())?;
        if found.as_ref().is_some_and(|old| old != &name) {
            return None;
        }
        found = Some(name);
    }
    found
}

#[derive(Deserialize)]
#[serde(untagged)]
enum Timestamp {
    Number(i64),
    Text(String),
}

fn unix_ms(v: &Timestamp) -> Option<i64> {
    if let Timestamp::Number(n) = v {
        return Some(if n.abs() < 10_000_000_000 {
            n.saturating_mul(1000)
        } else {
            *n
        });
    }
    let Timestamp::Text(s) = v else { return None };
    chrono::DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|d| d.timestamp_millis())
}

fn parse_json_stream<T: DeserializeOwned>(path: &Path) -> Result<Vec<Result<T, ()>>, SourceError> {
    let file = fs::File::open(path).map_err(|e| SourceError::Io(e.to_string()))?;
    let reader = io::BufReader::new(file);
    Ok(serde_json::Deserializer::from_reader(reader)
        .into_iter::<T>()
        .map(|value| value.map_err(|_| ()))
        .collect())
}

#[derive(Deserialize)]
struct CodexEnvelope {
    #[serde(rename = "type")]
    kind: Option<String>,
    timestamp: Option<Timestamp>,
    payload: Option<CodexPayload>,
}

#[derive(Deserialize)]
struct CodexPayload {
    #[serde(rename = "type")]
    kind: Option<String>,
    id: Option<String>,
    session_id: Option<String>,
    #[serde(rename = "sessionId")]
    session_id_camel: Option<String>,
    cwd: Option<String>,
    project: Option<String>,
    model: Option<String>,
    model_id: Option<String>,
    turn_id: Option<String>,
    #[serde(rename = "turnId")]
    turn_id_camel: Option<String>,
    started_at: Option<i64>,
    #[serde(rename = "startedAt")]
    started_at_camel: Option<i64>,
    completed_at: Option<i64>,
    #[serde(rename = "completedAt")]
    completed_at_camel: Option<i64>,
    info: Option<CodexInfo>,
    total_token_usage: Option<CodexUsage>,
}

#[derive(Deserialize)]
struct CodexInfo {
    total_token_usage: Option<CodexUsage>,
}

#[derive(Deserialize)]
struct CodexUsage {
    output_tokens: Option<i64>,
}

#[derive(Deserialize)]
struct ClaudeEvent {
    #[serde(rename = "type")]
    kind: Option<String>,
    #[serde(rename = "sessionId")]
    session_id: Option<String>,
    #[serde(rename = "session_id")]
    session_id_snake: Option<String>,
    cwd: Option<String>,
    project: Option<String>,
    timestamp: Option<Timestamp>,
    message: Option<ClaudeMessage>,
}

#[derive(Deserialize)]
struct ClaudeMessage {
    id: Option<String>,
    model: Option<String>,
    stop_reason: Option<String>,
    usage: Option<ClaudeUsage>,
    content: Option<Vec<ClaudeContentType>>,
}

#[derive(Deserialize)]
struct ClaudeUsage {
    output_tokens: Option<TokenCount>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum TokenCount {
    Valid(i64),
    Invalid(serde::de::IgnoredAny),
}

#[derive(Deserialize)]
struct ClaudeContentType {
    #[serde(rename = "type")]
    kind: Option<String>,
}

#[derive(Deserialize)]
struct OpenData {
    role: Option<String>,
    #[serde(rename = "modelID")]
    model_id: Option<String>,
    #[serde(rename = "model_id")]
    model_id_snake: Option<String>,
    model: Option<String>,
    tokens: Option<OpenTokens>,
    time: Option<OpenTime>,
    finish: Option<String>,
    parts: Option<Vec<OpenPartType>>,
}

#[derive(Deserialize)]
struct OpenTokens {
    output: Option<i64>,
}

#[derive(Deserialize)]
struct OpenTime {
    created: Option<i64>,
    completed: Option<i64>,
}

#[derive(Deserialize)]
struct OpenPartType {
    #[serde(rename = "type")]
    kind: Option<String>,
}

fn snapshots(agent: Agent, turns: Vec<TurnMeasurement>) -> Result<Vec<Snapshot>, SourceError> {
    if turns.is_empty() {
        return Err(SourceError::NoCompletedTurn);
    }
    let mut by_session: HashMap<String, (SessionRef, Vec<TurnMeasurement>)> = HashMap::new();
    for turn in turns {
        by_session
            .entry(turn.session.id.clone())
            .or_insert_with(|| (turn.session.clone(), Vec::new()))
            .1
            .push(turn);
    }
    let mut result = by_session
        .into_values()
        .map(|(session, mut all_turns)| {
            all_turns.sort_by_key(|turn| std::cmp::Reverse(turn.completed_at));
            let session_total_tokens = all_turns.iter().map(|turn| turn.output_tokens).sum();
            let session_total_elapsed_ms = all_turns
                .iter()
                .map(|turn| turn.completed_at.saturating_sub(turn.started_at).max(0))
                .sum();
            let session_accuracy = if all_turns
                .iter()
                .all(|turn| turn.accuracy == Accuracy::Exact)
            {
                Accuracy::Exact
            } else {
                Accuracy::Estimated
            };
            let turns = all_turns.iter().take(10).cloned().collect::<Vec<_>>();
            let activity_at = turns.first().map_or(0, |turn| turn.completed_at);
            Snapshot {
                agent,
                session,
                turns,
                all_turns,
                session_total_tokens,
                session_total_elapsed_ms,
                session_accuracy,
                activity_at,
                running: false,
            }
        })
        .collect::<Vec<_>>();
    result.sort_by_key(|snapshot| std::cmp::Reverse(snapshot.turns[0].completed_at));
    Ok(result)
}

fn sql_error(e: rusqlite::Error) -> SourceError {
    match e {
        rusqlite::Error::SqliteFailure(_, msg) => {
            let message = msg.map_or_else(|| "sqlite failure".into(), |m| m.to_string());
            if message.contains("no such table") || message.contains("no such column") {
                SourceError::UnknownSchema(message)
            } else {
                SourceError::Sqlite(message)
            }
        }
        rusqlite::Error::SqlInputError { msg, .. } => {
            if msg.contains("no such table") || msg.contains("no such column") {
                SourceError::UnknownSchema(msg)
            } else {
                SourceError::Sqlite(msg)
            }
        }
        rusqlite::Error::InvalidColumnName(name) => SourceError::UnknownSchema(name),
        other => SourceError::Sqlite(other.to_string()),
    }
}

struct ZTurn {
    session: SessionRef,
    turn_id: String,
    started_at: i64,
    completed_at: i64,
    output: u64,
    generation_ms: i64,
    /// 有 first_token_at 的完成行累计的生成输出（模型速度分子）
    generation_output: u64,
    /// 完成行总数 / 其中有 first_token_at 的行数
    completed_rows: u32,
    ttfb_rows: u32,
    has_incomplete_sibling: bool,
    models: Vec<Option<String>>,
}

/// Read completed ZCode requests, grouped by their persisted `turn_id`.
pub fn collect_zcode(con: &Connection) -> Result<Vec<Snapshot>, SourceError> {
    collect_zcode_impl(con, false)
}

pub fn collect_zcode_with_running(con: &Connection) -> Result<Vec<Snapshot>, SourceError> {
    collect_zcode_impl(con, true)
}

fn collect_zcode_impl(
    con: &Connection,
    include_running: bool,
) -> Result<Vec<Snapshot>, SourceError> {
    let mut stmt = con
        .prepare(
            "SELECT mu.session_id, mu.turn_id, s.directory, mu.model_id, mu.status,
                    mu.started_at, mu.first_token_at, mu.completed_at, mu.output_tokens
             FROM model_usage mu JOIN session s ON s.id = mu.session_id
             WHERE mu.turn_id IS NOT NULL AND mu.started_at IS NOT NULL
             ORDER BY mu.completed_at ASC",
        )
        .map_err(sql_error)?;
    let mut grouped: HashMap<(String, String), ZTurn> = HashMap::new();
    let rows = stmt
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, Option<String>>(2)?,
                r.get::<_, Option<String>>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, i64>(5)?,
                r.get::<_, Option<i64>>(6)?,
                r.get::<_, Option<i64>>(7)?,
                r.get::<_, Option<i64>>(8)?,
            ))
        })
        .map_err(sql_error)?;
    for row in rows {
        let (sid, tid, directory, model, status, started, first, completed, output) =
            row.map_err(sql_error)?;
        let entry = grouped
            .entry((sid.clone(), tid.clone()))
            .or_insert_with(|| ZTurn {
                session: session(sid.clone(), directory.clone()),
                turn_id: tid,
                started_at: started,
                completed_at: completed.unwrap_or(started),
                output: 0,
                generation_ms: 0,
                generation_output: 0,
                completed_rows: 0,
                ttfb_rows: 0,
                has_incomplete_sibling: false,
                models: Vec::new(),
            });
        if status != "completed" {
            entry.has_incomplete_sibling = true;
            continue;
        }
        let (Some(completed), Some(output)) = (completed, output) else {
            entry.has_incomplete_sibling = true;
            continue;
        };
        if output <= 0 || completed <= started {
            entry.has_incomplete_sibling = true;
            continue;
        }
        entry.started_at = entry.started_at.min(started);
        entry.completed_at = entry.completed_at.max(completed);
        entry.output = entry.output.saturating_add(output as u64);
        entry.completed_rows += 1;
        entry.models.push(model);
        // 缺 first_token_at 的行无法拆出纯生成时长，不参与模型速度，
        // 但不影响该行的其余指标；同 turn 同模型，分段速度可代表整体
        match first {
            Some(first) if completed >= first => {
                entry.generation_ms = entry.generation_ms.saturating_add(completed - first);
                entry.generation_output = entry.generation_output.saturating_add(output as u64);
                entry.ttfb_rows += 1;
            }
            _ => {}
        }
    }
    let mut states = HashMap::new();
    for turn in grouped.values() {
        let state = states
            .entry(turn.session.id.clone())
            .or_insert_with(|| (turn.session.clone(), turn.completed_at, false));
        state.1 = state.1.max(turn.completed_at);
        state.2 |= turn.has_incomplete_sibling;
    }
    let turns = grouped
        .into_values()
        .filter(|t| !t.has_incomplete_sibling)
        .map(|t| {
            let accuracy = Accuracy::Estimated;
            let model = model_name(t.models);
            let model_speed = (model.is_some() && t.ttfb_rows > 0 && t.generation_ms > 0)
                .then(|| speed(t.generation_output, 0, t.generation_ms));
            TurnMeasurement {
                turn_id: t.turn_id,
                session: t.session,
                output_tokens: t.output,
                started_at: t.started_at,
                completed_at: t.completed_at,
                effective_speed: speed(t.output, t.started_at, t.completed_at),
                accuracy,
                model,
                model_speed,
                model_accuracy: if model_speed.is_some() {
                    if t.ttfb_rows == t.completed_rows {
                        Accuracy::Exact
                    } else {
                        Accuracy::Estimated
                    }
                } else {
                    Accuracy::Unavailable
                },
            }
        })
        .collect();
    let mut result = snapshots(Agent::ZCode, turns).unwrap_or_default();
    for snapshot in &mut result {
        if let Some((_, activity_at, running)) = states.get(&snapshot.session.id) {
            snapshot.activity_at = snapshot.activity_at.max(*activity_at);
            snapshot.running = *running;
        }
    }
    if include_running {
        for (id, (session, activity_at, running)) in states {
            if running && !result.iter().any(|snapshot| snapshot.session.id == id) {
                result.push(Snapshot {
                    agent: Agent::ZCode,
                    session,
                    turns: Vec::new(),
                    all_turns: Vec::new(),
                    session_total_tokens: 0,
                    session_total_elapsed_ms: 0,
                    session_accuracy: Accuracy::Unavailable,
                    activity_at,
                    running: true,
                });
            }
        }
        result.sort_by_key(|snapshot| std::cmp::Reverse(snapshot.activity_at));
    }
    if result.is_empty() {
        Err(SourceError::NoCompletedTurn)
    } else {
        Ok(result)
    }
}

struct CodexTurn {
    session: SessionRef,
    id: String,
    started_at: i64,
    max_total: u64,
    baseline: u64,
    model: Option<String>,
    malformed: bool,
}

/// Read Codex's explicit task boundaries; adjacent token events never form a turn.
pub fn collect_codex(path: &Path) -> Result<Vec<Snapshot>, SourceError> {
    collect_codex_impl(path, false)
}

pub fn collect_codex_with_running(path: &Path) -> Result<Vec<Snapshot>, SourceError> {
    collect_codex_impl(path, true)
}

fn collect_codex_impl(path: &Path, include_running: bool) -> Result<Vec<Snapshot>, SourceError> {
    let events = parse_json_stream::<CodexEnvelope>(path)?;
    if events.is_empty() {
        return Err(SourceError::UnknownSchema("empty JSONL".into()));
    }
    let mut sid = String::new();
    let mut project = None;
    let mut model = None;
    let mut turn_context = None;
    let mut total = 0_u64;
    let mut active: Option<CodexTurn> = None;
    let mut turns = Vec::new();
    let mut last_activity = 0_i64;
    for parsed in events {
        let Ok(event) = parsed else {
            if let Some(turn) = active.as_mut() {
                turn.malformed = true;
            }
            continue;
        };
        let outer = event.kind.as_deref();
        let kind = match outer {
            Some("session_meta") | Some("turn_context") => outer,
            Some("event_msg") => event
                .payload
                .as_ref()
                .and_then(|payload| payload.kind.as_deref())
                .filter(|kind| matches!(*kind, "task_started" | "task_complete" | "token_count")),
            _ => None,
        };
        let stamp = event.timestamp.as_ref().and_then(unix_ms);
        if let Some(stamp) = stamp {
            last_activity = last_activity.max(stamp);
        }
        let p = event.payload.as_ref();
        match kind {
            Some("session_meta") => {
                if let Some(p) = p {
                    sid =
                        p.id.clone()
                            .or_else(|| p.session_id.clone())
                            .or_else(|| p.session_id_camel.clone())
                            .unwrap_or(sid);
                    project = p.cwd.clone().or_else(|| p.project.clone());
                    model = p.model.clone().or_else(|| p.model_id.clone()).or(model);
                }
            }
            Some("turn_context") => {
                if let Some(p) = p {
                    turn_context = p
                        .turn_id
                        .clone()
                        .or_else(|| p.turn_id_camel.clone())
                        .or_else(|| p.id.clone());
                    model = p.model.clone().or_else(|| p.model_id.clone()).or(model);
                }
            }
            Some("token_count") => {
                let output = p
                    .and_then(|payload| {
                        payload
                            .info
                            .as_ref()
                            .and_then(|info| info.total_token_usage.as_ref())
                            .or(payload.total_token_usage.as_ref())
                    })
                    .and_then(|usage| usage.output_tokens)
                    .filter(|n| *n >= 0);
                if let Some(n) = output {
                    total = total.max(n as u64);
                    if let Some(turn) = active.as_mut() {
                        turn.max_total = turn.max_total.max(total);
                    }
                } else if let Some(turn) = active.as_mut() {
                    turn.malformed = true;
                }
            }
            Some("task_started") => {
                if active.is_some() {
                    continue;
                }
                let id = p
                    .and_then(|p| {
                        p.turn_id
                            .clone()
                            .or_else(|| p.turn_id_camel.clone())
                            .or_else(|| p.id.clone())
                    })
                    .or_else(|| turn_context.clone());
                let Some(id) = id else { continue };
                let started_at = p
                    .and_then(|p| p.started_at.or(p.started_at_camel))
                    .map(|s| {
                        if s.abs() < 10_000_000_000 {
                            s.saturating_mul(1000)
                        } else {
                            s
                        }
                    })
                    .or(stamp);
                let Some(started_at) = started_at else {
                    continue;
                };
                active = Some(CodexTurn {
                    session: session(sid.clone(), project.clone()),
                    id,
                    started_at,
                    max_total: total,
                    baseline: total,
                    model: model.clone(),
                    malformed: false,
                });
            }
            Some("task_complete") => {
                let Some(turn_id) =
                    p.and_then(|p| p.turn_id.clone().or_else(|| p.turn_id_camel.clone()))
                else {
                    continue;
                };
                if active.as_ref().is_some_and(|turn| turn.id != turn_id) {
                    continue;
                }
                let Some(turn) = active.take() else { continue };
                let completed_at = p
                    .and_then(|p| p.completed_at.or(p.completed_at_camel))
                    .map(|s| {
                        if s.abs() < 10_000_000_000 {
                            s.saturating_mul(1000)
                        } else {
                            s
                        }
                    })
                    .or(stamp);
                let Some(completed_at) = completed_at else {
                    continue;
                };
                let output = turn.max_total.saturating_sub(turn.baseline);
                if turn.malformed || output == 0 || completed_at <= turn.started_at {
                    continue;
                }
                turns.push(TurnMeasurement {
                    turn_id: turn.id,
                    session: turn.session,
                    output_tokens: output,
                    started_at: turn.started_at,
                    completed_at,
                    effective_speed: speed(output, turn.started_at, completed_at),
                    accuracy: Accuracy::Estimated,
                    model: turn.model,
                    model_speed: None,
                    model_accuracy: Accuracy::Unavailable,
                });
            }
            _ => {}
        }
    }
    if sid.is_empty() && turns.is_empty() {
        return Err(SourceError::UnknownSchema("missing session_meta".into()));
    }
    let active_state = active.map(|turn| (turn.session, turn.started_at, turn.id));
    let mut result = snapshots(Agent::Codex, turns).unwrap_or_default();
    if include_running {
        if let Some((session, started_at, _)) = active_state {
            if let Some(snapshot) = result
                .iter_mut()
                .find(|snapshot| snapshot.session.id == session.id)
            {
                snapshot.running = true;
                snapshot.activity_at = snapshot.activity_at.max(last_activity.max(started_at));
            } else {
                result.push(Snapshot {
                    agent: Agent::Codex,
                    session,
                    turns: Vec::new(),
                    all_turns: Vec::new(),
                    session_total_tokens: 0,
                    session_total_elapsed_ms: 0,
                    session_accuracy: Accuracy::Unavailable,
                    activity_at: last_activity.max(started_at),
                    running: true,
                });
            }
        }
    }
    if result.is_empty() {
        Err(SourceError::NoCompletedTurn)
    } else {
        Ok(result)
    }
}

struct OpenTurn {
    session: SessionRef,
    id: String,
    started_at: i64,
    completed_at: i64,
    output: u64,
    models: Vec<Option<String>>,
    malformed: bool,
}

struct ClaudeTurn {
    session: SessionRef,
    id: String,
    started_at: i64,
    completed_at: i64,
    output: u64,
    models: Vec<Option<String>>,
    malformed: bool,
}

/// Aggregate OpenCode assistant messages until the explicit `finish=stop` record.
pub fn collect_opencode(con: &Connection) -> Result<Vec<Snapshot>, SourceError> {
    collect_opencode_impl(con, false)
}

pub fn collect_opencode_with_running(con: &Connection) -> Result<Vec<Snapshot>, SourceError> {
    collect_opencode_impl(con, true)
}

fn collect_opencode_impl(
    con: &Connection,
    include_running: bool,
) -> Result<Vec<Snapshot>, SourceError> {
    let mut stmt = con
        .prepare(
            "SELECT m.id, m.session_id, s.directory, m.time_created, m.time_updated, m.data
             FROM message m JOIN session s ON s.id = m.session_id
             ORDER BY m.time_created ASC, m.id ASC",
        )
        .map_err(sql_error)?;
    let rows = stmt
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, Option<String>>(2)?,
                r.get::<_, i64>(3)?,
                r.get::<_, i64>(4)?,
                r.get::<_, String>(5)?,
            ))
        })
        .map_err(sql_error)?;
    let mut user_started: HashMap<String, i64> = HashMap::new();
    let mut pending: HashMap<String, (SessionRef, i64)> = HashMap::new();
    let mut active: HashMap<String, OpenTurn> = HashMap::new();
    let mut malformed_sessions = HashSet::new();
    let mut turns = Vec::new();
    for row in rows {
        let (message_id, sid, directory, db_created, db_updated, data) = row.map_err(sql_error)?;
        let Ok(v) = serde_json::from_str::<OpenData>(&data) else {
            if let Some(turn) = active.get_mut(&sid) {
                turn.malformed = true;
            } else {
                malformed_sessions.insert(sid);
            }
            continue;
        };
        if v.role.as_deref() == Some("user") {
            let tool_result = v.parts.as_ref().is_some_and(|parts| {
                parts
                    .iter()
                    .any(|part| part.kind.as_deref() == Some("tool_result"))
            });
            if !tool_result {
                user_started.insert(sid.clone(), db_created);
                active.remove(&sid);
                pending.insert(
                    sid.clone(),
                    (session(sid.clone(), directory.clone()), db_created),
                );
                malformed_sessions.remove(&sid);
            }
            continue;
        }
        if v.role.as_deref() != Some("assistant") {
            continue;
        }
        let created = v
            .time
            .as_ref()
            .and_then(|time| time.created)
            .unwrap_or(db_created);
        let completed = v
            .time
            .as_ref()
            .and_then(|time| time.completed)
            .unwrap_or(db_updated);
        let output = v.tokens.as_ref().and_then(|tokens| tokens.output);
        let complete_metadata = output.is_some_and(|value| value >= 0)
            && v.time.as_ref().and_then(|time| time.completed).is_some();
        if !complete_metadata {
            malformed_sessions.insert(sid.clone());
            if let Some(turn) = active.get_mut(&sid) {
                turn.malformed = true;
            }
        }
        if created <= 0 || completed < created {
            malformed_sessions.insert(sid.clone());
            if let Some(turn) = active.get_mut(&sid) {
                turn.malformed = true;
            }
            continue;
        }
        let Some(started_at) = user_started.get(&sid).copied() else {
            continue;
        };
        pending.remove(&sid);
        let entry = active.entry(sid.clone()).or_insert_with(|| OpenTurn {
            session: session(sid.clone(), directory.clone()),
            id: format!("{sid}:{message_id}"),
            started_at,
            completed_at: completed,
            output: 0,
            models: Vec::new(),
            malformed: malformed_sessions.contains(&sid),
        });
        entry.started_at = entry.started_at.min(created);
        entry.completed_at = entry.completed_at.max(completed);
        entry.output = entry
            .output
            .saturating_add(output.unwrap_or(0).max(0) as u64);
        entry.models.push(
            v.model_id
                .clone()
                .or_else(|| v.model_id_snake.clone())
                .or_else(|| v.model.clone()),
        );
        if v.finish.as_deref() == Some("stop") {
            if let Some(done) = active.remove(&sid) {
                user_started.remove(&sid);
                pending.remove(&sid);
                malformed_sessions.remove(&sid);
                if !done.malformed && done.output > 0 && done.completed_at > done.started_at {
                    turns.push(TurnMeasurement {
                        turn_id: done.id,
                        session: done.session,
                        output_tokens: done.output,
                        started_at: done.started_at,
                        completed_at: done.completed_at,
                        effective_speed: speed(done.output, done.started_at, done.completed_at),
                        accuracy: Accuracy::Estimated,
                        model: model_name(done.models),
                        model_speed: None,
                        model_accuracy: Accuracy::Unavailable,
                    });
                }
            }
        }
    }
    let mut active_state = active
        .into_values()
        .map(|turn| (turn.session, turn.completed_at))
        .collect::<Vec<_>>();
    active_state.extend(pending.into_values());
    let mut result = snapshots(Agent::OpenCode, turns).unwrap_or_default();
    if include_running {
        for (session, activity_at) in active_state {
            if let Some(snapshot) = result
                .iter_mut()
                .find(|snapshot| snapshot.session.id == session.id)
            {
                snapshot.running = true;
                snapshot.activity_at = snapshot.activity_at.max(activity_at);
            } else {
                result.push(Snapshot {
                    agent: Agent::OpenCode,
                    session,
                    turns: Vec::new(),
                    all_turns: Vec::new(),
                    session_total_tokens: 0,
                    session_total_elapsed_ms: 0,
                    session_accuracy: Accuracy::Unavailable,
                    activity_at,
                    running: true,
                });
            }
        }
    }
    if result.is_empty() {
        Err(SourceError::NoCompletedTurn)
    } else {
        Ok(result)
    }
}

fn claude_files(path: &Path) -> Result<Vec<PathBuf>, SourceError> {
    if path.is_file() {
        return Ok(vec![path.to_path_buf()]);
    }
    let mut files = Vec::new();
    let entries = fs::read_dir(path).map_err(|e| SourceError::Io(e.to_string()))?;
    for entry in entries.flatten() {
        let p = entry.path();
        if p.is_dir() {
            files.extend(claude_files(&p)?);
        } else if p.extension().and_then(|e| e.to_str()) == Some("jsonl") {
            files.push(p);
        }
    }
    Ok(files)
}

/// Close Claude turns only on a real human user followed by `end_turn`.
pub fn collect_claude(path: &Path) -> Result<Vec<Snapshot>, SourceError> {
    collect_claude_impl(path, false)
}

pub fn collect_claude_with_running(path: &Path) -> Result<Vec<Snapshot>, SourceError> {
    collect_claude_impl(path, true)
}

fn collect_claude_impl(path: &Path, include_running: bool) -> Result<Vec<Snapshot>, SourceError> {
    let files = claude_files(path)?;
    collect_claude_paths(files, include_running)
}

pub(crate) fn collect_claude_files_with_running(
    files: &[PathBuf],
) -> Result<Vec<Snapshot>, SourceError> {
    collect_claude_paths(files.to_vec(), true)
}

fn collect_claude_paths(
    files: Vec<PathBuf>,
    include_running: bool,
) -> Result<Vec<Snapshot>, SourceError> {
    if files.is_empty() {
        return Err(SourceError::UnknownSchema("no JSONL files".into()));
    }
    let mut active: HashMap<String, ClaudeTurn> = HashMap::new();
    let mut seen_assistant: HashSet<(String, String)> = HashSet::new();
    let mut malformed_sessions = HashSet::new();
    let mut malformed_without_session = false;
    let mut turns = Vec::new();
    let mut events = Vec::new();
    let mut order = 0usize;
    for file in files {
        for parsed in parse_json_stream::<ClaudeEvent>(&file)? {
            let timestamp = parsed
                .as_ref()
                .ok()
                .and_then(|event| event.timestamp.as_ref())
                .and_then(unix_ms);
            let session_id = parsed.as_ref().ok().and_then(|event| {
                event
                    .session_id
                    .as_ref()
                    .or(event.session_id_snake.as_ref())
                    .cloned()
            });
            events.push((timestamp, order, session_id, parsed));
            order += 1;
        }
    }
    events.sort_by_key(|(timestamp, order, _, _)| (timestamp.unwrap_or(i64::MAX), *order));
    for (_, _, malformed_session_id, parsed) in events {
        let Ok(v) = parsed else {
            if let Some(sid) = malformed_session_id {
                malformed_sessions.insert(sid.clone());
                if let Some(turn) = active.get_mut(&sid) {
                    turn.malformed = true;
                }
            } else {
                malformed_without_session = true;
                for turn in active.values_mut() {
                    turn.malformed = true;
                }
            }
            continue;
        };
        let kind = v.kind.as_deref();
        let sid = v
            .session_id
            .clone()
            .or(v.session_id_snake.clone())
            .unwrap_or_default();
        if sid.is_empty() {
            continue;
        }
        let cwd = v.cwd.clone().or(v.project.clone());
        let Some(ts) = v.timestamp.as_ref().and_then(unix_ms) else {
            if kind == Some("assistant") {
                malformed_sessions.insert(sid.clone());
                if let Some(turn) = active.get_mut(&sid) {
                    turn.malformed = true;
                }
            }
            continue;
        };
        if kind == Some("user") {
            let is_tool_result = v
                .message
                .as_ref()
                .and_then(|message| message.content.as_ref())
                .is_some_and(|parts| {
                    parts
                        .iter()
                        .any(|part| part.kind.as_deref() == Some("tool_result"))
                });
            if !is_tool_result {
                active.insert(
                    sid.clone(),
                    ClaudeTurn {
                        session: session(sid.clone(), cwd),
                        id: format!("{sid}:{ts}"),
                        started_at: ts,
                        completed_at: ts,
                        output: 0,
                        models: Vec::new(),
                        malformed: false,
                    },
                );
            }
            continue;
        }
        if kind != Some("assistant") {
            continue;
        }
        let Some(message) = v.message.as_ref() else {
            if let Some(turn) = active.get_mut(&sid) {
                turn.malformed = true;
            }
            continue;
        };
        let Some(entry) = active.get_mut(&sid) else {
            continue;
        };
        if let Some(id) = message.id.as_deref() {
            if !seen_assistant.insert((sid.clone(), id.to_string())) {
                continue;
            }
        }
        entry.completed_at = entry.completed_at.max(ts);
        let output = match message
            .usage
            .as_ref()
            .and_then(|usage| usage.output_tokens.as_ref())
        {
            Some(TokenCount::Valid(value)) => (*value).max(0) as u64,
            Some(TokenCount::Invalid(_)) => {
                entry.malformed = true;
                0
            }
            None => 0,
        };
        entry.output = entry.output.saturating_add(output);
        entry.models.push(message.model.clone());
        if message.stop_reason.as_deref() == Some("end_turn") {
            if let Some(done) = active.remove(&sid) {
                if !done.malformed && done.output > 0 && done.completed_at > done.started_at {
                    turns.push(TurnMeasurement {
                        turn_id: done.id,
                        session: done.session,
                        output_tokens: done.output,
                        started_at: done.started_at,
                        completed_at: done.completed_at,
                        effective_speed: speed(done.output, done.started_at, done.completed_at),
                        accuracy: Accuracy::Estimated,
                        model: model_name(done.models),
                        model_speed: None,
                        model_accuracy: Accuracy::Unavailable,
                    });
                }
            }
        }
    }
    if malformed_without_session {
        return Err(SourceError::UnknownSchema(
            "malformed Claude JSONL without session id".into(),
        ));
    }
    turns.retain(|turn| !malformed_sessions.contains(&turn.session.id));
    let active_state = active
        .into_values()
        .filter(|turn| !turn.malformed)
        .map(|turn| (turn.session, turn.completed_at))
        .collect::<Vec<_>>();
    let mut result = snapshots(Agent::ClaudeCode, turns).unwrap_or_default();
    if include_running {
        for (session, activity_at) in active_state {
            if let Some(snapshot) = result
                .iter_mut()
                .find(|snapshot| snapshot.session.id == session.id)
            {
                snapshot.running = true;
                snapshot.activity_at = snapshot.activity_at.max(activity_at);
            } else {
                result.push(Snapshot {
                    agent: Agent::ClaudeCode,
                    session,
                    turns: Vec::new(),
                    all_turns: Vec::new(),
                    session_total_tokens: 0,
                    session_total_elapsed_ms: 0,
                    session_accuracy: Accuracy::Unavailable,
                    activity_at,
                    running: true,
                });
            }
        }
    }
    if result.is_empty() {
        Err(SourceError::NoCompletedTurn)
    } else {
        Ok(result)
    }
}
