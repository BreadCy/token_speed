use crate::collectors::{
    collect_claude_files_with_running, collect_claude_with_running, collect_codex_with_running,
    collect_opencode_with_running, collect_pi_files_with_running, collect_pi_with_running,
    collect_zcode_with_running, Accuracy, Agent, Snapshot, SourceError, TurnMeasurement,
};
use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use rusqlite::{Connection, OpenFlags};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fmt;
use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError, TryRecvError};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum SourceKind {
    #[serde(rename = "zcode")]
    ZCodeDb,
    #[serde(rename = "codex")]
    CodexSessions,
    #[serde(rename = "opencode")]
    OpenCodeDb,
    #[serde(rename = "claude-code")]
    ClaudeProjects,
    #[serde(rename = "pi")]
    PiSessions,
}

impl SourceKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ZCodeDb => "zcode",
            Self::CodexSessions => "codex",
            Self::OpenCodeDb => "opencode",
            Self::ClaudeProjects => "claude-code",
            Self::PiSessions => "pi",
        }
    }

    pub fn agent(self) -> Agent {
        match self {
            Self::ZCodeDb => Agent::ZCode,
            Self::CodexSessions => Agent::Codex,
            Self::OpenCodeDb => Agent::OpenCode,
            Self::ClaudeProjects => Agent::ClaudeCode,
            Self::PiSessions => Agent::Pi,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceLocation {
    pub kind: SourceKind,
    pub path: PathBuf,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Selector {
    pub agent: Agent,
    pub project: Option<String>,
    pub session: Option<String>,
}

impl Selector {
    pub fn matches(&self, project: Option<&str>, session: &str) -> bool {
        self.session
            .as_deref()
            .is_none_or(|wanted| wanted == session)
            && self.project.as_deref().is_none_or(|wanted| {
                project.is_some_and(|value| path_matches(Path::new(wanted), Path::new(value)))
            })
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FollowerReport {
    pub agent: Agent,
    pub source: SourceLocation,
    pub project: Option<String>,
    pub session: String,
    pub running: bool,
    pub activity_at: i64,
    pub turns: Vec<TurnMeasurement>,
    pub session_total_tokens: u64,
    #[serde(default)]
    pub session_total_input_tokens: u64,
    pub session_total_elapsed_ms: i64,
    pub session_accuracy: crate::collectors::Accuracy,
}

#[derive(Clone, Debug)]
pub enum MonitorError {
    Io(String),
    Source(String),
    Notify(String),
}

impl fmt::Display for MonitorError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(message) => write!(f, "monitor I/O error: {message}"),
            Self::Source(message) => write!(f, "monitor source error: {message}"),
            Self::Notify(message) => write!(f, "monitor watcher error: {message}"),
        }
    }
}

impl std::error::Error for MonitorError {}

fn home_path(parts: &[&str]) -> Option<PathBuf> {
    // Windows 用 USERPROFILE，macOS/Linux 用 HOME
    let home = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME"))?;
    Some(
        parts
            .iter()
            .fold(PathBuf::from(home), |path, part| path.join(part)),
    )
}

fn explicit_source_candidates(agent: Agent) -> Vec<(String, SourceLocation)> {
    let mut candidates = Vec::new();
    match agent {
        Agent::ZCode => {
            if let Some(root) = std::env::var_os("ZCODE_HOME") {
                candidates.push((
                    "ZCODE_HOME".into(),
                    SourceLocation {
                        kind: SourceKind::ZCodeDb,
                        path: PathBuf::from(root).join("cli/db/db.sqlite"),
                    },
                ));
            }
        }
        Agent::Codex => {
            if let Some(root) = std::env::var_os("CODEX_HOME") {
                candidates.push((
                    "CODEX_HOME".into(),
                    SourceLocation {
                        kind: SourceKind::CodexSessions,
                        path: PathBuf::from(root).join("sessions"),
                    },
                ));
            }
        }
        Agent::OpenCode => {
            if let Some(root) = std::env::var_os("XDG_DATA_HOME") {
                candidates.push((
                    "XDG_DATA_HOME".into(),
                    SourceLocation {
                        kind: SourceKind::OpenCodeDb,
                        path: PathBuf::from(root).join("opencode/opencode.db"),
                    },
                ));
            }
            #[cfg(windows)]
            if let Some(root) = std::env::var_os("LOCALAPPDATA") {
                candidates.push((
                    "LOCALAPPDATA".into(),
                    SourceLocation {
                        kind: SourceKind::OpenCodeDb,
                        path: PathBuf::from(root).join("opencode/opencode.db"),
                    },
                ));
            }
        }
        Agent::ClaudeCode => {
            if let Some(root) = std::env::var_os("CLAUDE_CONFIG_DIR") {
                candidates.push((
                    "CLAUDE_CONFIG_DIR".into(),
                    SourceLocation {
                        kind: SourceKind::ClaudeProjects,
                        path: PathBuf::from(root).join("projects"),
                    },
                ));
            }
        }
        Agent::Pi => {
            // pi resolves its session dir as PI_CODING_AGENT_SESSION_DIR, else
            // PI_CODING_AGENT_DIR/sessions, else ~/.pi/agent/sessions (config.js).
            if let Some(root) = std::env::var_os("PI_CODING_AGENT_SESSION_DIR") {
                candidates.push((
                    "PI_CODING_AGENT_SESSION_DIR".into(),
                    SourceLocation {
                        kind: SourceKind::PiSessions,
                        path: PathBuf::from(root),
                    },
                ));
            }
            if let Some(root) = std::env::var_os("PI_CODING_AGENT_DIR") {
                candidates.push((
                    "PI_CODING_AGENT_DIR".into(),
                    SourceLocation {
                        kind: SourceKind::PiSessions,
                        path: PathBuf::from(root).join("sessions"),
                    },
                ));
            }
        }
    }
    candidates
}

fn default_source_candidates(agent: Agent) -> Vec<SourceLocation> {
    let mut candidates = Vec::new();
    let path = match agent {
        Agent::ZCode => home_path(&[".zcode", "cli", "db", "db.sqlite"]),
        Agent::Codex => home_path(&[".codex", "sessions"]),
        Agent::OpenCode => home_path(&[".local", "share", "opencode", "opencode.db"]),
        Agent::ClaudeCode => home_path(&[".claude", "projects"]),
        Agent::Pi => home_path(&[".pi", "agent", "sessions"]),
    };
    if let Some(path) = path {
        candidates.push(SourceLocation {
            kind: match agent {
                Agent::ZCode => SourceKind::ZCodeDb,
                Agent::Codex => SourceKind::CodexSessions,
                Agent::OpenCode => SourceKind::OpenCodeDb,
                Agent::ClaudeCode => SourceKind::ClaudeProjects,
                Agent::Pi => SourceKind::PiSessions,
            },
            path,
        });
    }
    candidates
}

pub fn source_candidates(agent: Agent) -> Vec<SourceLocation> {
    explicit_source_candidates(agent)
        .into_iter()
        .map(|(_, source)| source)
        .chain(default_source_candidates(agent))
        .collect()
}

fn sqlite_header(path: &Path) -> Result<bool, MonitorError> {
    let mut file = fs::File::open(path).map_err(|e| MonitorError::Io(e.to_string()))?;
    let mut header = [0_u8; 16];
    file.read_exact(&mut header)
        .map_err(|e| MonitorError::Io(e.to_string()))?;
    Ok(&header == b"SQLite format 3\0")
}

pub fn detect_source(agent: Agent) -> Result<Option<SourceLocation>, MonitorError> {
    if let Some((env_name, candidate)) = explicit_source_candidates(agent).into_iter().next() {
        match candidate.kind {
            SourceKind::ZCodeDb | SourceKind::OpenCodeDb => {
                if !candidate.path.is_file() {
                    return Err(MonitorError::Source(format!(
                        "{env_name} points to missing or unreadable SQLite source: {}",
                        candidate.path.display()
                    )));
                }
                if !sqlite_header(&candidate.path).map_err(|error| {
                    MonitorError::Source(format!(
                        "{env_name} SQLite source is not readable: {} ({error})",
                        candidate.path.display()
                    ))
                })? {
                    return Err(MonitorError::Source(format!(
                        "{env_name} source is not a SQLite database: {}",
                        candidate.path.display()
                    )));
                }
            }
            SourceKind::CodexSessions | SourceKind::ClaudeProjects | SourceKind::PiSessions => {
                if !candidate.path.is_dir() {
                    return Err(MonitorError::Source(format!(
                        "{env_name} points to missing or unreadable source directory: {}",
                        candidate.path.display()
                    )));
                }
                fs::read_dir(&candidate.path).map_err(|error| {
                    MonitorError::Source(format!(
                        "{env_name} source directory is not readable: {} ({error})",
                        candidate.path.display()
                    ))
                })?;
            }
        }
        return Ok(Some(candidate));
    }
    for candidate in default_source_candidates(agent) {
        let valid = match candidate.kind {
            SourceKind::ZCodeDb | SourceKind::OpenCodeDb => {
                candidate.path.is_file() && sqlite_header(&candidate.path).unwrap_or(false)
            }
            SourceKind::CodexSessions | SourceKind::ClaudeProjects | SourceKind::PiSessions => {
                candidate.path.is_dir()
            }
        };
        if valid {
            return Ok(Some(candidate));
        }
    }
    Ok(None)
}

/// Batch-detect every installed agent source; one agent's broken env override or
/// missing install never affects the others (the CLI keeps strict `detect_source`).
/// Same no-fallback rule as `detect_source`: an env override is used exclusively —
/// a broken one simply means "not installed" instead of silently probing defaults.
pub fn detect_all_installed() -> Vec<SourceLocation> {
    let mut found = Vec::new();
    for agent in [
        Agent::ZCode,
        Agent::Codex,
        Agent::OpenCode,
        Agent::ClaudeCode,
        Agent::Pi,
    ] {
        let explicit = explicit_source_candidates(agent);
        let candidate = match explicit.into_iter().next() {
            Some((_, source)) => source,
            None => match default_source_candidates(agent).into_iter().next() {
                Some(source) => source,
                None => continue,
            },
        };
        let valid = match candidate.kind {
            SourceKind::ZCodeDb | SourceKind::OpenCodeDb => {
                candidate.path.is_file() && sqlite_header(&candidate.path).unwrap_or(false)
            }
            SourceKind::CodexSessions | SourceKind::ClaudeProjects | SourceKind::PiSessions => {
                candidate.path.is_dir()
            }
        };
        if valid {
            found.push(candidate);
        }
    }
    found
}

pub fn path_matches(left: &Path, right: &Path) -> bool {
    fn normalized(path: &Path) -> Option<String> {
        let value = path.to_str()?.replace('\\', "/");
        let value = if value
            .get(..8)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case("//?/unc/"))
        {
            &value[8..]
        } else {
            value.strip_prefix("//?/").unwrap_or(&value)
        };
        let mut out = Vec::new();
        for component in value.split('/') {
            if component.is_empty() || component == "." {
                continue;
            }
            if component == ".." {
                let _ = out.pop();
            } else {
                out.push(component);
            }
        }
        Some(out.join("/"))
    }
    let Some(a) = normalized(left) else {
        return false;
    };
    let Some(b) = normalized(right) else {
        return false;
    };
    let is_windows_path = |path: &Path| {
        let value = path.to_string_lossy();
        value.starts_with("\\\\")
            || value.as_bytes().get(1).is_some_and(|byte| *byte == b':')
                && value.as_bytes()[0].is_ascii_alphabetic()
    };
    if cfg!(windows) || is_windows_path(left) || is_windows_path(right) {
        a.eq_ignore_ascii_case(&b)
    } else {
        a == b
    }
}

fn open_sqlite(path: &Path) -> Result<Connection, MonitorError> {
    Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|e| MonitorError::Source(e.to_string()))
}

fn jsonl_files(root: &Path) -> io::Result<Vec<PathBuf>> {
    if root.is_file() {
        return Ok(vec![root.to_path_buf()]);
    }
    let mut files = Vec::new();
    for entry in fs::read_dir(root)? {
        let path = entry?.path();
        if path.is_dir() {
            files.extend(jsonl_files(&path)?);
        } else if path.extension().and_then(|e| e.to_str()) == Some("jsonl") {
            files.push(path);
        }
    }
    Ok(files)
}

#[derive(Deserialize)]
struct JsonHeader {
    #[serde(rename = "sessionId")]
    session_id: Option<String>,
    session_id_snake: Option<String>,
    cwd: Option<String>,
    project: Option<String>,
    payload: Option<JsonHeaderPayload>,
}

#[derive(Deserialize)]
struct JsonHeaderPayload {
    id: Option<String>,
    #[serde(rename = "sessionId")]
    session_id: Option<String>,
    session_id_snake: Option<String>,
    cwd: Option<String>,
    project: Option<String>,
}

fn jsonl_head(path: &Path) -> io::Result<Vec<JsonHeader>> {
    let file = fs::File::open(path)?;
    Ok(
        serde_json::Deserializer::from_reader(io::BufReader::new(file))
            .into_iter::<JsonHeader>()
            .take(16)
            .filter_map(Result::ok)
            .collect(),
    )
}

fn header_session_id(value: &JsonHeader, source_kind: SourceKind) -> Option<String> {
    let payload = value.payload.as_ref();
    if source_kind == SourceKind::CodexSessions {
        return payload.and_then(|payload| {
            payload
                .session_id
                .clone()
                .or_else(|| payload.session_id_snake.clone())
                .or_else(|| payload.id.clone())
        });
    }
    value
        .session_id
        .clone()
        .or_else(|| value.session_id_snake.clone())
}

fn header_project(value: &JsonHeader, source_kind: SourceKind) -> Option<&str> {
    if source_kind == SourceKind::CodexSessions {
        value
            .payload
            .as_ref()
            .and_then(|payload| payload.cwd.as_deref().or(payload.project.as_deref()))
    } else {
        value.cwd.as_deref().or(value.project.as_deref())
    }
}

fn file_matches_selector(path: &Path, source_kind: SourceKind, selector: &Selector) -> bool {
    let Ok(values) = jsonl_head(path) else {
        return false;
    };
    let session_match = selector.session.as_deref().is_none_or(|wanted| {
        path.file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.contains(wanted))
            || values
                .iter()
                .any(|value| header_session_id(value, source_kind).as_deref() == Some(wanted))
    });
    let project_match = selector.project.as_deref().is_none_or(|wanted| {
        values.iter().any(|value| {
            header_project(value, source_kind)
                .is_some_and(|project| path_matches(Path::new(wanted), Path::new(project)))
        })
    });
    session_match && project_match
}

fn codex_session_id(path: &Path) -> Option<String> {
    jsonl_head(path)
        .ok()?
        .into_iter()
        .find_map(|value| header_session_id(&value, SourceKind::CodexSessions))
}

fn selected_jsonl_files(
    source: &SourceLocation,
    selector: &Selector,
) -> Result<Vec<PathBuf>, MonitorError> {
    let mut files = jsonl_files(&source.path).map_err(|e| MonitorError::Io(e.to_string()))?;
    if selector.project.is_none() && selector.session.is_none() {
        files.sort_by_key(|path| {
            fs::metadata(path)
                .and_then(|metadata| metadata.modified())
                .ok()
        });
        let latest = files.pop();
        if source.kind == SourceKind::CodexSessions {
            if let Some(latest_path) = latest {
                if let Some(session_id) = codex_session_id(&latest_path) {
                    files.push(latest_path);
                    files.retain(|path| codex_session_id(path).as_deref() == Some(&session_id));
                    return Ok(files);
                }
                return Ok(vec![latest_path]);
            }
            return Ok(Vec::new());
        }
        return Ok(latest.into_iter().collect());
    }
    // ponytail: pinned searches scan file heads linearly; default selection pays only for newest.
    files.retain(|path| file_matches_selector(path, source.kind, selector));
    Ok(files)
}

fn snapshots_for_codex_files(files: Vec<PathBuf>) -> Result<Vec<Snapshot>, MonitorError> {
    let mut snapshots = Vec::new();
    let mut first_error = None;
    let mut schema_error = None;
    for path in files {
        match collect_codex_with_running(&path) {
            Ok(mut found) => snapshots.append(&mut found),
            Err(error @ crate::collectors::SourceError::UnknownSchema(_)) => {
                schema_error.get_or_insert((path, error));
            }
            Err(error) => {
                first_error.get_or_insert((path, error));
            }
        }
    }
    if !snapshots.is_empty() {
        let mut merged: std::collections::HashMap<String, Snapshot> =
            std::collections::HashMap::new();
        for snapshot in snapshots {
            let entry = merged
                .entry(snapshot.session.id.clone())
                .or_insert_with(|| Snapshot {
                    agent: snapshot.agent,
                    session: snapshot.session.clone(),
                    turns: Vec::new(),
                    all_turns: Vec::new(),
                    session_total_tokens: 0,
                    session_total_input_tokens: 0,
                    session_total_elapsed_ms: 0,
                    session_accuracy: Accuracy::Unavailable,
                    activity_at: 0,
                    running: false,
                });
            entry.all_turns.extend(snapshot.all_turns);
            entry.activity_at = entry.activity_at.max(snapshot.activity_at);
            entry.running |= snapshot.running;
        }
        for snapshot in merged.values_mut() {
            let mut unique = std::collections::HashMap::new();
            for turn in snapshot.all_turns.drain(..) {
                unique.entry(turn.turn_id.clone()).or_insert(turn);
            }
            snapshot.all_turns = unique.into_values().collect();
            snapshot
                .all_turns
                .sort_by_key(|turn| std::cmp::Reverse(turn.completed_at));
            snapshot.session_total_tokens = snapshot
                .all_turns
                .iter()
                .map(|turn| turn.output_tokens)
                .sum();
            snapshot.session_total_input_tokens = snapshot
                .all_turns
                .iter()
                .map(|turn| turn.input_tokens)
                .sum();
            snapshot.session_total_elapsed_ms = snapshot
                .all_turns
                .iter()
                .map(|turn| turn.completed_at.saturating_sub(turn.started_at).max(0))
                .sum();
            snapshot.session_accuracy = if snapshot.all_turns.is_empty() {
                Accuracy::Unavailable
            } else if snapshot
                .all_turns
                .iter()
                .all(|turn| turn.accuracy == Accuracy::Exact)
            {
                Accuracy::Exact
            } else {
                Accuracy::Estimated
            };
            snapshot.turns = snapshot.all_turns.iter().take(10).cloned().collect();
        }
        let mut snapshots = merged.into_values().collect::<Vec<_>>();
        snapshots.sort_by_key(|snapshot| std::cmp::Reverse(snapshot.activity_at));
        return Ok(snapshots);
    }
    let error = schema_error.or(first_error).map_or_else(
        || "no valid Codex rollout records".to_string(),
        |(path, error)| format!("{}: {error}", path.display()),
    );
    Err(MonitorError::Source(error))
}

pub(crate) fn snapshots_for(source: &SourceLocation) -> Result<Vec<Snapshot>, MonitorError> {
    match source.kind {
        SourceKind::ZCodeDb => {
            let con = open_sqlite(&source.path)?;
            collect_zcode_with_running(&con).map_err(|e| MonitorError::Source(e.to_string()))
        }
        SourceKind::OpenCodeDb => {
            let con = open_sqlite(&source.path)?;
            collect_opencode_with_running(&con).map_err(|e| MonitorError::Source(e.to_string()))
        }
        SourceKind::CodexSessions => snapshots_for_codex_files(
            jsonl_files(&source.path).map_err(|e| MonitorError::Io(e.to_string()))?,
        ),
        SourceKind::ClaudeProjects => collect_claude_with_running(&source.path)
            .map_err(|e| MonitorError::Source(e.to_string())),
        SourceKind::PiSessions => collect_pi_with_running(&source.path)
            .map_err(|e| MonitorError::Source(e.to_string())),
    }
}

/// 全量扫描的容错版，专供总量聚合使用：历史累计不该因单个损坏文件清零。
/// JSONL 源按文件逐个解析——坏文件跳过并记住首个真实错误；`NoCompletedTurn`
/// 只是没有已完成轮次，视为空文件。全部文件都失败时才整体报错。
/// db 源是单文件，维持原样（失败即报错）。会话与文件一一对应（Codex 在
/// `snapshots_for_codex_files` 内部自行合并），因此逐文件结果直接拼接。
fn tolerant_snapshots(source: &SourceLocation) -> Result<Vec<Snapshot>, MonitorError> {
    if !matches!(
        source.kind,
        SourceKind::ClaudeProjects | SourceKind::PiSessions
    ) {
        return snapshots_for(source);
    }
    let files = jsonl_files(&source.path).map_err(|e| MonitorError::Io(e.to_string()))?;
    let mut all: Vec<Snapshot> = Vec::new();
    let mut first_error = None;
    for file in files {
        let result = match source.kind {
            SourceKind::ClaudeProjects => {
                collect_claude_files_with_running(std::slice::from_ref(&file))
            }
            _ => collect_pi_files_with_running(std::slice::from_ref(&file)),
        };
        match result {
            Ok(mut found) => all.append(&mut found),
            Err(SourceError::NoCompletedTurn) => {}
            Err(error) => {
                first_error.get_or_insert((file, error));
            }
        }
    }
    if all.is_empty() {
        let error = first_error.map_or_else(
            || MonitorError::Source(SourceError::NoCompletedTurn.to_string()),
            |(path, error)| MonitorError::Source(format!("{}: {error}", path.display())),
        );
        return Err(error);
    }
    all.sort_by_key(|snapshot| std::cmp::Reverse(snapshot.activity_at));
    Ok(all)
}

fn snapshots_for_selector(
    source: &SourceLocation,
    selector: &Selector,
) -> Result<Vec<Snapshot>, MonitorError> {
    match source.kind {
        SourceKind::CodexSessions => {
            let files = selected_jsonl_files(source, selector)?;
            if files.is_empty() && (selector.project.is_some() || selector.session.is_some()) {
                return Ok(Vec::new());
            }
            snapshots_for_codex_files(files)
        }
        SourceKind::ClaudeProjects => {
            let files = selected_jsonl_files(source, selector)?;
            if files.is_empty() {
                return Ok(Vec::new());
            }
            crate::collectors::collect_claude_files_with_running(&files)
                .map_err(|e| MonitorError::Source(e.to_string()))
        }
        SourceKind::PiSessions => {
            let files = selected_jsonl_files(source, selector)?;
            if files.is_empty() {
                return Ok(Vec::new());
            }
            crate::collectors::collect_pi_files_with_running(&files)
                .map_err(|e| MonitorError::Source(e.to_string()))
        }
        _ => snapshots_for(source),
    }
}

pub(crate) fn is_relevant_change(
    source_kind: SourceKind,
    changed: &Path,
    source_db: &Path,
) -> bool {
    match source_kind {
        SourceKind::ZCodeDb | SourceKind::OpenCodeDb => {
            changed == source_db
                || changed
                    == source_db.with_file_name(format!(
                        "{}-wal",
                        source_db
                            .file_name()
                            .and_then(|name| name.to_str())
                            .unwrap_or("db")
                    ))
                || changed
                    == source_db.with_file_name(format!(
                        "{}-shm",
                        source_db
                            .file_name()
                            .and_then(|name| name.to_str())
                            .unwrap_or("db")
                    ))
        }
        SourceKind::CodexSessions | SourceKind::ClaudeProjects | SourceKind::PiSessions => {
            changed.is_dir() || changed.extension().and_then(|ext| ext.to_str()) == Some("jsonl")
        }
    }
}

/// One project's cumulative token usage (input + output, all completed turns,
/// full local history). `project: None` buckets sessions without a project dir.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectTotals {
    pub project: Option<String>,
    pub tokens: u64,
}

/// One agent's usage totals across all projects. `projects` is sorted by tokens
/// descending and `total_tokens` equals the sum of its entries.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentTotals {
    pub total_tokens: u64,
    pub projects: Vec<ProjectTotals>,
}

/// Per-agent cache of session-level usage keyed by session id. The event path
/// upserts only the active session (cheap latest-session scan); startup, explicit
/// rescan, and every reconcile tick rebuild the whole entry from a full scan, so
/// dormant history can never drift. Never do the full rebuild on file events —
/// that is the "event-driven full rescan" regression the engine was redesigned
/// to avoid.
type SessionTotalsCache = HashMap<Agent, HashMap<String, (Option<String>, u64)>>;

pub(crate) fn rebuild_session_totals(
    slot: &mut HashMap<String, (Option<String>, u64)>,
    snapshots: &[Snapshot],
) {
    slot.clear();
    for snapshot in snapshots {
        let tokens = snapshot
            .session_total_tokens
            .saturating_add(snapshot.session_total_input_tokens);
        slot.insert(
            snapshot.session.id.clone(),
            (snapshot.session.project.clone(), tokens),
        );
    }
}

pub(crate) fn aggregate_totals(cache: &HashMap<String, (Option<String>, u64)>) -> AgentTotals {
    let mut by_project: HashMap<Option<String>, u64> = HashMap::new();
    for (project, tokens) in cache.values() {
        *by_project.entry(project.clone()).or_insert(0) += tokens;
    }
    let mut projects: Vec<ProjectTotals> = by_project
        .into_iter()
        .map(|(project, tokens)| ProjectTotals { project, tokens })
        .collect();
    projects.sort_by(|a, b| b.tokens.cmp(&a.tokens).then_with(|| a.project.cmp(&b.project)));
    let total_tokens = projects.iter().map(|project| project.tokens).sum();
    AgentTotals {
        total_tokens,
        projects,
    }
}

fn report_for(
    selector: &Selector,
    source: SourceLocation,
) -> Result<Option<FollowerReport>, MonitorError> {
    let snapshots = snapshots_for_selector(&source, selector)?;
    Ok(select_report(snapshots, selector, source))
}

fn select_report(
    snapshots: Vec<Snapshot>,
    selector: &Selector,
    source: SourceLocation,
) -> Option<FollowerReport> {
    let selected = snapshots.into_iter().filter(|snapshot| {
        selector.matches(snapshot.session.project.as_deref(), &snapshot.session.id)
    });
    let snapshot = selected.max_by_key(|snapshot| {
        snapshot
            .activity_at
            .max(snapshot.turns.first().map_or(0, |turn| turn.completed_at))
    });
    snapshot.map(|snapshot| FollowerReport {
        agent: snapshot.agent,
        source,
        project: snapshot.session.project,
        session: snapshot.session.id,
        running: snapshot.running,
        activity_at: snapshot
            .activity_at
            .max(snapshot.turns.first().map_or(0, |turn| turn.completed_at)),
        turns: snapshot.turns.into_iter().take(10).collect(),
        session_total_tokens: snapshot.session_total_tokens,
        session_total_input_tokens: snapshot.session_total_input_tokens,
        session_total_elapsed_ms: snapshot.session_total_elapsed_ms,
        session_accuracy: snapshot.session_accuracy,
    })
}

pub fn scan_once(selector: &Selector) -> Result<Option<FollowerReport>, MonitorError> {
    let Some(source) = detect_source(selector.agent)? else {
        return Ok(None);
    };
    report_for(selector, source)
}

/// One agent's view as produced by the engine: whether it is installed, whether a
/// turn is running, and the report for the currently selected session (if any).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AgentStatus {
    pub agent: Agent,
    pub installed: bool,
    pub running: bool,
    pub activity_at: i64,
    pub report: Option<FollowerReport>,
    #[serde(default)]
    pub totals: AgentTotals,
    pub error: Option<String>,
}

#[derive(Clone, Debug)]
pub enum EngineEvent {
    Statuses(Vec<AgentStatus>),
}

/// Handle to a spawned engine: stop it, or ask for an immediate re-detect + rescan
/// (so a freshly installed agent shows up without waiting for the reconcile tick).
#[derive(Clone)]
pub struct EngineHandle {
    stop: Arc<AtomicBool>,
    rescan: Arc<AtomicBool>,
}

impl EngineHandle {
    pub fn stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
    }

    pub fn request_rescan(&self) {
        self.rescan.store(true, Ordering::Relaxed);
    }
}

pub struct EngineOptions {
    pub project: Option<String>,
    pub debounce: Duration,
    pub reconcile: Duration,
    pub max_runtime: Option<Duration>,
}

impl Default for EngineOptions {
    fn default() -> Self {
        Self {
            project: None,
            debounce: Duration::from_millis(250),
            reconcile: Duration::from_secs(30),
            max_runtime: None,
        }
    }
}

/// Spawn the aggregate engine thread: it watches every installed agent source at
/// once and pushes full `Statuses` snapshots through the channel. The UI never
/// restarts it when the user switches agent — switching is a pure UI selection.
pub fn spawn_engine(project: Option<String>) -> (EngineHandle, mpsc::Receiver<EngineEvent>) {
    let (tx, rx) = mpsc::channel();
    let handle = EngineHandle {
        stop: Arc::new(AtomicBool::new(false)),
        rescan: Arc::new(AtomicBool::new(false)),
    };
    let stopped = handle.stop.clone();
    let rescan = handle.rescan.clone();
    thread::spawn(move || {
        let _ = run_engine(
            EngineOptions {
                project,
                ..EngineOptions::default()
            },
            Some(&stopped),
            Some(&rescan),
            |event| {
                let _ = tx.send(event);
            },
        );
    });
    (handle, rx)
}

pub(crate) fn collect_statuses(
    project: Option<&str>,
    sources: &[SourceLocation],
    totals_cache: &mut SessionTotalsCache,
    full: bool,
) -> Vec<AgentStatus> {
    let installed: Vec<Agent> = sources.iter().map(|source| source.kind.agent()).collect();
    // 全量重建时清掉已卸载 agent 的缓存条目，避免幽灵项目留在明细里
    if full {
        totals_cache.retain(|agent, _| installed.contains(agent));
    }
    [
        Agent::ZCode,
        Agent::Codex,
        Agent::OpenCode,
        Agent::ClaudeCode,
        Agent::Pi,
    ]
    .into_iter()
    .map(
        |agent| match sources.iter().find(|source| source.kind.agent() == agent) {
            Some(source) => scan_agent_status(project, source, totals_cache, full),
            None => AgentStatus {
                agent,
                installed: false,
                running: false,
                activity_at: 0,
                report: None,
                totals: AgentTotals::default(),
                error: None,
            },
        },
    )
    .collect()
}

/// Full rescan of one agent's source. Event bursts only re-scan the agents whose
/// files actually changed (see `run_engine`); this keeps a single busy agent from
/// making every emit re-parse the other sources.
///
/// `full` drives the totals cache: startup, explicit rescan, and reconcile ticks
/// rebuild it from a full source scan; file events only upsert the latest
/// session's entry (the event path must stay cheap — no full rescans).
pub(crate) fn scan_agent_status(
    project: Option<&str>,
    source: &SourceLocation,
    totals_cache: &mut SessionTotalsCache,
    full: bool,
) -> AgentStatus {
    let agent = source.kind.agent();
    let selector = Selector {
        agent,
        project: project.map(str::to_owned),
        session: None,
    };
    let report = if full {
        match tolerant_snapshots(source) {
            // 全量扫描：先抽出会话级累计，再从同一份 snapshots 选出报告会话
            Ok(snapshots) => {
                rebuild_session_totals(totals_cache.entry(agent).or_default(), &snapshots);
                select_report(snapshots, &selector, source.clone())
            }
            Err(error) => return errored_status(agent, error),
        }
    } else {
        match report_for(&selector, source.clone()) {
            Ok(Some(report)) => {
                totals_cache.entry(agent).or_default().insert(
                    report.session.clone(),
                    (
                        report.project.clone(),
                        report
                            .session_total_tokens
                            .saturating_add(report.session_total_input_tokens),
                    ),
                );
                Some(report)
            }
            // 无匹配会话（如项目固定后无活动）：缓存保持原样
            Ok(None) => None,
            Err(error) => return errored_status(agent, error),
        }
    };
    let totals = aggregate_totals(totals_cache.entry(agent).or_default());
    match report {
        Some(report) => AgentStatus {
            agent,
            installed: true,
            running: report.running,
            activity_at: report.activity_at,
            report: Some(report),
            totals,
            error: None,
        },
        None => AgentStatus {
            agent,
            installed: true,
            running: false,
            activity_at: 0,
            report: None,
            totals,
            error: None,
        },
    }
}

fn errored_status(agent: Agent, error: MonitorError) -> AgentStatus {
    AgentStatus {
        agent,
        installed: true,
        running: false,
        activity_at: 0,
        report: None,
        totals: AgentTotals::default(),
        error: Some(error.to_string()),
    }
}

fn watch_source(
    source: &SourceLocation,
    tx: mpsc::Sender<Agent>,
) -> Result<RecommendedWatcher, MonitorError> {
    let watch_path = if matches!(source.kind, SourceKind::ZCodeDb | SourceKind::OpenCodeDb) {
        source.path.parent().unwrap_or(&source.path).to_path_buf()
    } else {
        source.path.clone()
    };
    let mode = if watch_path == source.path {
        RecursiveMode::Recursive
    } else {
        RecursiveMode::NonRecursive
    };
    let kind = source.kind;
    let source_db = source.path.clone();
    let mut watcher = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
        let relevant = match &event {
            Ok(value) => value
                .paths
                .iter()
                .any(|changed| is_relevant_change(kind, changed, &source_db)),
            Err(_) => true,
        };
        if relevant {
            let _ = tx.send(kind.agent());
        }
    })
    .map_err(|e| MonitorError::Notify(e.to_string()))?;
    watcher
        .watch(&watch_path, mode)
        .map_err(|e| MonitorError::Notify(e.to_string()))?;
    Ok(watcher)
}

/// (Re)create watchers for the currently installed sources; called on startup, on
/// explicit rescan, and on every reconcile tick so installs/uninstalls appear live.
fn create_watchers(
    sources: &[SourceLocation],
    tx: &mpsc::Sender<Agent>,
) -> Result<Vec<(SourceLocation, RecommendedWatcher)>, MonitorError> {
    sources
        .iter()
        .map(|source| watch_source(source, tx.clone()).map(|watcher| (source.clone(), watcher)))
        .collect()
}

fn drain_pending(rx: &mpsc::Receiver<Agent>) -> Result<(), MonitorError> {
    loop {
        match rx.try_recv() {
            Ok(_) => {}
            Err(TryRecvError::Empty) => return Ok(()),
            Err(TryRecvError::Disconnected) => {
                return Err(MonitorError::Notify("watch channel closed".into()))
            }
        }
    }
}

/// 逐源全量重建总量缓存，每完成一个源就推送一次（渐进出现在 UI）。
/// `dbs_only` 用于 reconcile 节拍：JSONL 源历史静态、事件路径已增量维护，
/// 只有 db 源需要周期性全量自愈；完整重建留给启动与显式重扫。
fn progressive_full_rescan(
    project: Option<&str>,
    sources: &[SourceLocation],
    totals_cache: &mut SessionTotalsCache,
    cached: &mut [AgentStatus],
    dbs_only: bool,
    mut emit: impl FnMut(&[AgentStatus]),
) {
    for source in sources {
        let full = !dbs_only || matches!(source.kind, SourceKind::ZCodeDb | SourceKind::OpenCodeDb);
        if let Some(slot) = cached
            .iter_mut()
            .find(|status| status.agent == source.kind.agent())
        {
            *slot = scan_agent_status(project, source, totals_cache, full);
        }
        emit(cached);
    }
}

/// One engine pass shared by the spawned thread and tests: watches all installed
/// sources, debounces file events, and re-detects on every reconcile tick.
pub fn run_engine(
    options: EngineOptions,
    stopped: Option<&AtomicBool>,
    rescan_requested: Option<&AtomicBool>,
    mut on_event: impl FnMut(EngineEvent),
) -> Result<(), MonitorError> {
    let EngineOptions {
        project,
        debounce,
        reconcile,
        max_runtime,
    } = options;
    let started = Instant::now();
    let (tx, rx) = mpsc::channel();
    let mut sources = detect_all_installed();
    // Watchers are never read after creation — holding them in a variable IS the
    // contract: notify stops watching when the value is dropped, so reassigning on
    // rebuild is what retires the previous source set.
    let mut watched = create_watchers(&sources, &tx)?;
    let mut totals_cache: SessionTotalsCache = HashMap::new();
    // 先用廉价扫描出首屏（与旧引擎同成本），总量随后逐源全量重建渐进补齐——
    // Codex 全量解析可达数秒，不能挡在首轮 Statuses 之前
    let mut cached = collect_statuses(project.as_deref(), &sources, &mut totals_cache, false);
    let mut emit = |cached: &[AgentStatus]| {
        on_event(EngineEvent::Statuses(cached.to_vec()));
    };
    emit(&cached);
    progressive_full_rescan(
        project.as_deref(),
        &sources,
        &mut totals_cache,
        &mut cached,
        false,
        |cached| emit(cached),
    );
    let mut next_reconcile = Instant::now() + reconcile;
    let mut dirty: HashSet<Agent> = HashSet::new();
    loop {
        if stopped.is_some_and(|stop| stop.load(Ordering::Relaxed))
            || max_runtime.is_some_and(|limit| started.elapsed() >= limit)
        {
            return Ok(());
        }
        if rescan_requested.is_some_and(|flag| flag.swap(false, Ordering::Relaxed)) {
            sources = detect_all_installed();
            watched = create_watchers(&sources, &tx)?;
            cached = collect_statuses(project.as_deref(), &sources, &mut totals_cache, false);
            emit(&cached);
            progressive_full_rescan(
                project.as_deref(),
                &sources,
                &mut totals_cache,
                &mut cached,
                false,
                |cached| emit(cached),
            );
            next_reconcile = Instant::now() + reconcile;
        }
        let remaining = next_reconcile.saturating_duration_since(Instant::now());
        let timeout = remaining.min(debounce).min(if stopped.is_some() {
            Duration::from_millis(250)
        } else {
            Duration::MAX
        });
        match rx.recv_timeout(timeout) {
            Ok(agent) => {
                // debounce: swallow further events inside the window, then re-scan
                // only the agents whose sources actually fired
                dirty.insert(agent);
                loop {
                    match rx.recv_timeout(debounce) {
                        Ok(more) => {
                            dirty.insert(more);
                        }
                        Err(RecvTimeoutError::Timeout) => break,
                        Err(RecvTimeoutError::Disconnected) => {
                            return Err(MonitorError::Notify("watch channel closed".into()));
                        }
                    }
                }
                for agent in dirty.drain() {
                    if let Some(source) = sources.iter().find(|source| source.kind.agent() == agent)
                    {
                        if let Some(slot) = cached.iter_mut().find(|status| status.agent == agent) {
                            // 事件路径只增量更新活跃会话；历史由 reconcile 全量重建兜底
                            *slot =
                                scan_agent_status(project.as_deref(), source, &mut totals_cache, false);
                        }
                    }
                }
                emit(&cached);
            }
            Err(RecvTimeoutError::Timeout) => {
                if Instant::now() >= next_reconcile {
                    drain_pending(&rx)?;
                    sources = detect_all_installed();
                    watched = create_watchers(&sources, &tx)?;
                    // JSONL 源历史静态、事件已增量维护；只全量自愈便宜的 db 源
                    progressive_full_rescan(
                        project.as_deref(),
                        &sources,
                        &mut totals_cache,
                        &mut cached,
                        true,
                        |cached| emit(cached),
                    );
                    next_reconcile = Instant::now() + reconcile;
                }
            }
            Err(RecvTimeoutError::Disconnected) => {
                return Err(MonitorError::Notify("watch channel closed".into()))
            }
        }
        let _ = &watched;
    }
}
