//! tokenspeed — AI 编码工具生成速度统计 (tokens/s)。
//!
//! 支持多个工具的本地会话数据:
//!   ZCode   ~/.zcode/cli/db/db.sqlite (model_usage 表, SQLite 只读)
//!           精确值: tokens/s = output_tokens / ((completed_at - first_token_at)/1000)
//!   Codex   ~/.codex/sessions/**/rollout-*.jsonl (token_count 事件)
//!           估算值: 相邻 token_count 的时间差 ≈ 本轮时长 (含工具执行, 标 ~)
//!   OpenCode ~/.local/share/opencode/opencode.db (message.data JSON)
//!           估算值: created→completed 覆盖整轮 (含工具执行, 标 ~)
//!   Claude  ~/.claude/projects/**/*.jsonl (assistant 消息 usage)
//!           估算值: 相邻 assistant 时间差 (标 ~)
//!
//! SQLite 一律只读打开，WAL 模式下与运行中的客户端互不阻塞、零写入。
//! 四个数据路径可在悬浮条托盘菜单"设置…"中修改（存 HKCU\Software\tokenspeed），
//! `--db` 参数仍可临时覆盖 ZCode 数据库路径。
//!
//! 用法:
//!   tokenspeed                 无参数 = 启动置顶悬浮条（双击场景）; --report 输出人读统计
//!   tokenspeed --report        人读统计: 各工具最近一次 + ZCode 会话汇总 + 最近 10 次明细
//!   tokenspeed --limit 20      显示最近 20 次
//!   tokenspeed --tool codex    只看某个工具 (zc/cx/oc/cc 或全名)
//!   tokenspeed --session sid   只统计指定 ZCode 会话
//!   tokenspeed --hook          ZCode Stop hook 模式: 输出单行 {"additionalContext": "..."}
//!   tokenspeed --autostart=on  设置开机自启（登录后自动启动悬浮条）
//!   tokenspeed --bench         分项耗时输出到 stderr

use std::io::{self, IsTerminal, Read, Write};
use std::path::{Path, PathBuf};
use std::process::exit;
use std::time::{Duration, Instant};

use chrono::Local;
use rusqlite::{Connection, OpenFlags};

const FALSY: [&str; 5] = ["false", "0", "off", "no", "none"];
const MAX_SESSION_ROWS: usize = 2000; // 单会话聚合上限，防止异常膨胀
const TAIL_BYTES: u64 = 1 << 20; // jsonl 尾部读取量
const MAX_EST_GEN_MS: i64 = 1_800_000; // 估算时长超过 30 分钟视为跨轮间隔，丢弃

const ROW_SQL: &str = "\
SELECT model_id, variant, agent, session_id, started_at, first_token_at,
       completed_at, time_to_first_token_ms, output_tokens,
       CASE WHEN first_token_at IS NOT NULL THEN completed_at - first_token_at
            ELSE completed_at - started_at END,
       CASE WHEN first_token_at IS NULL THEN 1 ELSE 0 END
FROM model_usage
WHERE status = 'completed' AND completed_at IS NOT NULL AND output_tokens > 0";

#[derive(Clone, Copy, PartialEq, Eq)]
enum Src {
    Zcode,
    Codex,
    Opencode,
    Claude,
}


impl Src {
    fn tag(self) -> &'static str {
        match self {
            Src::Zcode => "ZC",
            Src::Codex => "CX",
            Src::Opencode => "OC",
            Src::Claude => "CC",
        }
    }
    fn name(self) -> &'static str {
        match self {
            Src::Zcode => "ZCode",
            Src::Codex => "Codex",
            Src::Opencode => "OpenCode",
            Src::Claude => "Claude Code",
        }
    }
    fn parse(s: &str) -> Option<Src> {
        match s.to_ascii_lowercase().as_str() {
            "zc" | "zcode" => Some(Src::Zcode),
            "cx" | "codex" => Some(Src::Codex),
            "oc" | "opencode" => Some(Src::Opencode),
            "cc" | "claude" | "claude-code" | "claudecode" => Some(Src::Claude),
            _ => None,
        }
    }
}

struct Args {
    limit: usize,
    session: Option<String>,
    db: Option<PathBuf>, // --db 覆盖 ZCode 数据库；None = 设置值/默认位置
    hook: bool,
    auto_report: String,
    bench: bool,
    watch: Option<u64>, // 刷新间隔秒数
    float: bool,        // 置顶悬浮条模式
    tool: Option<Src>,  // 只看某个工具
    report: bool,       // 强制人读报告（无参数时默认启动悬浮条）
}

/// ZCode 数据库：--db 优先，其次设置中的自定义路径，最后默认位置
fn zcode_db(args: &Args) -> PathBuf {
    args.db.clone().unwrap_or_else(|| settings::get().zcode_db)
}

fn parse_args() -> Args {
    let mut a = Args {
        limit: 10,
        session: None,
        db: None,
        hook: false,
        auto_report: "true".into(),
        bench: false,
        watch: None,
        float: false,
        tool: None,
        report: false,
    };
    let mut it = std::env::args().skip(1).peekable();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--hook" => a.hook = true,
            "--bench" => a.bench = true,
            "--float" => a.float = true,
            "--report" => a.report = true,
            "--watch" => {
                // 可选紧跟刷新秒数：--watch 1；若是下一个 flag（-- 开头）则用默认 2s
                let secs = it
                    .next_if(|s| !s.starts_with('-') && s.parse::<u64>().is_ok())
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(2);
                a.watch = Some(secs);
            }
            "--tool" => a.tool = it.next().and_then(|v| Src::parse(&v)),
            "-h" | "--help" => {
                println!("tokenspeed — AI 编码工具生成速度统计 (tokens/s)\n\n\
                          支持: ZCode(zc) / Codex(cx) / OpenCode(oc) / Claude Code(cc)\n\
                          ZCode 为精确值；其余工具按本地会话记录估算，输出以 ~ 标注。\n\n\
                          双击运行（无参数）= 启动置顶悬浮条 + 托盘图标。\n\n\
                          用法: tokenspeed [--report] [--limit N] [--tool zc|cx|oc|cc]\n\
                          \
                          [--session sess_xxx] [--db PATH] [--hook] [--auto-report=true|false]\n\
                          \
                          [--bench] [--watch [秒]] [--float] [--autostart=on|off]\n\n\
                          \
                          --report           输出人读统计（终端里想看报表时用）\n\
                          \
                          --autostart=on|off 设置/取消开机自启（登录后启动悬浮条）\n\
                          \
                          --watch [秒]       实时刷新模式，默认 2s，Ctrl+C 退出\n\
                          \
                          --float            置顶悬浮条（托盘菜单: 切工具/自启/设置/退出）");
                exit(0);
            }
            "--limit" => a.limit = it.next().and_then(|v| v.parse().ok()).unwrap_or(10),
            "--session" => a.session = it.next(),
            "--db" => {
                if let Some(v) = it.next() {
                    a.db = Some(PathBuf::from(v));
                }
            }
            "--auto-report" => a.auto_report = it.next().unwrap_or_default(),
            _ if let Some(v) = arg.strip_prefix("--auto-report=") => a.auto_report = v.to_string(),
            _ if let Some(v) = arg.strip_prefix("--limit=") => a.limit = v.parse().unwrap_or(10),
            _ if let Some(v) = arg.strip_prefix("--watch=") => a.watch = v.parse().ok(),
            _ if let Some(v) = arg.strip_prefix("--tool=") => a.tool = Src::parse(v),
            _ if let Some(v) = arg.strip_prefix("--autostart=") => {
                // 命令行直接设置开机自启后退出（写/删 HKCU Run 键）
                if !tray::handle_cli_autostart(v) {
                    eprintln!("--autostart 只接受 on/off");
                    exit(1);
                }
                exit(0);
            }
            _ => {}
        }
    }
    a
}

struct Row {
    src: Src,
    model: String,
    variant: Option<String>,
    agent: Option<String>,
    session_id: String,
    started_at: i64,    // epoch ms
    completed_at: i64,  // epoch ms
    ttft_ms: Option<i64>,
    output_tokens: i64,
    gen_ms: i64,
    estimated: bool,
}

fn row_from(r: &rusqlite::Row) -> rusqlite::Result<Row> {
    Ok(Row {
        src: Src::Zcode,
        model: r.get(0)?,
        variant: r.get(1)?,
        agent: r.get(2)?,
        session_id: r.get(3)?,
        started_at: r.get(4)?,
        completed_at: r.get(6)?,
        ttft_ms: r.get(7)?,
        output_tokens: r.get(8)?,
        gen_ms: r.get(9)?,
        estimated: r.get::<_, i64>(10)? != 0,
    })
}

fn open_ro(db: &std::path::Path) -> rusqlite::Result<Connection> {
    Connection::open_with_flags(db, OpenFlags::SQLITE_OPEN_READ_ONLY)
}

fn speed(r: &Row) -> f64 {
    if r.gen_ms > 0 {
        r.output_tokens as f64 * 1000.0 / r.gen_ms as f64
    } else {
        0.0
    }
}

fn fmt_secs(ms: i64) -> String {
    let s = ms as f64 / 1000.0;
    if s < 60.0 {
        format!("{:.1}s", s)
    } else {
        format!("{}m{}s", (s / 60.0) as i64, (s % 60.0) as i64)
    }
}

fn fmt_ts(epoch_ms: i64) -> String {
    match chrono::DateTime::from_timestamp_millis(epoch_ms) {
        Some(utc) => utc.with_timezone(&Local).format("%m-%d %H:%M:%S").to_string(),
        None => "-".into(),
    }
}

fn fmt_thousands(n: i64) -> String {
    let s = n.abs().to_string();
    let bytes = s.as_bytes();
    let mut out = String::new();
    for (i, &b) in bytes.iter().enumerate() {
        if i > 0 && (bytes.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(b as char);
    }
    if n < 0 {
        format!("-{out}")
    } else {
        out
    }
}

fn fmt_model(model: &str, variant: &Option<String>) -> String {
    match variant {
        Some(v) if !v.is_empty() => format!("{model} ({v})"),
        _ => model.to_string(),
    }
}

fn fmt_agent(agent: &Option<String>) -> String {
    match agent {
        None => "-".into(),
        Some(a) if a == "zcode-agent" => "main".into(),
        Some(a) => a.strip_prefix("zcode-").unwrap_or(a).to_string(),
    }
}

fn parse_iso_ms(s: &str) -> i64 {
    chrono::DateTime::parse_from_rfc3339(s)
        .map(|d| d.timestamp_millis())
        .unwrap_or(0)
}

fn latest_line(r: &Row) -> String {
    let est = if r.estimated { "~" } else { "" };
    let mut parts = vec![
        format!("[{}] {}{:.1} tok/s", r.src.tag(), est, speed(r)),
        fmt_model(&r.model, &r.variant),
        format!("{} tokens / {}{} 生成", r.output_tokens, est, fmt_secs(r.gen_ms)),
    ];
    if let Some(ttft) = r.ttft_ms {
        parts.push(format!("TTFT {:.1}s", ttft as f64 / 1000.0));
    }
    parts.push(fmt_ts(r.started_at));
    if r.src == Src::Zcode {
        parts.push(fmt_agent(&r.agent));
    }
    parts.join(" | ")
}

// ---------- 数据采集 ----------

/// 读取 jsonl 尾部（最多 max_bytes），返回完整的行；文件开头可能截断的半行被丢弃。
fn tail_lines(path: &Path, max_bytes: u64) -> Option<Vec<String>> {
    let mut f = std::fs::File::open(path).ok()?;
    let len = f.metadata().ok()?.len();
    let start = len.saturating_sub(max_bytes);
    use std::io::Seek;
    f.seek(io::SeekFrom::Start(start)).ok()?;
    let mut bytes = Vec::new();
    f.read_to_end(&mut bytes).ok()?;
    let buf = String::from_utf8_lossy(&bytes);
    let mut lines: Vec<String> = buf.lines().map(String::from).collect();
    if start > 0 && !lines.is_empty() {
        lines.remove(0); // 起点很可能落在某行中间
    }
    Some(lines)
}

fn read_dir_ok(dir: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(dir)
        .map(|rd| rd.filter_map(|e| e.ok()).map(|e| e.path()).collect())
        .unwrap_or_default()
}

/// 最新的 Codex rollout 文件（文件名含日期时间，字典序即时间序）
fn newest_codex_rollout(root: &Path) -> Option<PathBuf> {
    let mut best: Option<(String, PathBuf)> = None;
    for y in read_dir_ok(root) {
        for m in read_dir_ok(&y) {
            for d in read_dir_ok(&m) {
                for f in read_dir_ok(&d) {
                    let name = f.file_name();
                    if let Some(name) = name.and_then(|n| n.to_str()) {
                        if name.starts_with("rollout-")
                            && name.ends_with(".jsonl")
                            && best.as_ref().map_or(true, |(bn, _)| name > bn.as_str())
                        {
                            best = Some((name.to_string(), f));
                        }
                    }
                }
            }
        }
    }
    best.map(|(_, p)| p)
}

/// Codex: 解析最新 rollout 尾部的 token_count 事件。
/// last_token_usage.output_tokens 是单次 API 请求的输出；
/// 与上一条 token_count 的时间差近似本轮时长（含中间工具执行，估算）。
fn collect_codex(root: &Path, limit: usize) -> Vec<Row> {
    let Some(path) = newest_codex_rollout(root) else { return Vec::new() };
    let Some(lines) = tail_lines(&path, TAIL_BYTES) else { return Vec::new() };
    let session_id = path
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();
    let mut model = String::from("codex");
    let mut stamps: Vec<(i64, i64)> = Vec::new(); // (completed_ts_ms, output_tokens)
    for l in &lines {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(l) else { continue };
        let Some(p) = v.get("payload") else { continue };
        match p.get("type").and_then(|t| t.as_str()) {
            Some("thread_settings_applied") => {
                if let Some(m) = p.pointer("/thread_settings/model").and_then(|m| m.as_str()) {
                    model = m.to_string();
                }
            }
            Some("token_count") => {
                let out = p
                    .pointer("/info/last_token_usage/output_tokens")
                    .and_then(|x| x.as_i64())
                    .unwrap_or(0);
                let ts = parse_iso_ms(v.get("timestamp").and_then(|t| t.as_str()).unwrap_or(""));
                if out > 0 && ts > 0 {
                    stamps.push((ts, out));
                }
            }
            _ => {}
        }
    }
    let mut rows = Vec::new();
    for i in 1..stamps.len() {
        let (ts, out) = stamps[i];
        let gen = ts - stamps[i - 1].0;
        if gen < 200 || gen > MAX_EST_GEN_MS {
            continue; // 过短是噪声，过长跨了空闲间隔，速度都没有意义
        }
        rows.push(Row {
            src: Src::Codex,
            model: model.clone(),
            variant: None,
            agent: None,
            session_id: session_id.clone(),
            started_at: ts - gen,
            completed_at: ts,
            ttft_ms: None,
            output_tokens: out,
            gen_ms: gen,
            estimated: true,
        });
    }
    rows.reverse();
    rows.truncate(limit);
    rows
}

/// 最新的 Claude Code 会话文件 (<claude 目录>/projects/<proj>/<uuid>.jsonl)
fn newest_claude_jsonl(root: &Path) -> Option<PathBuf> {
    let mut best: Option<(std::time::SystemTime, PathBuf)> = None;
    for proj in read_dir_ok(root) {
        if !proj.is_dir() {
            continue;
        }
        for f in read_dir_ok(&proj) {
            if f.extension().map_or(true, |e| e != "jsonl") {
                continue;
            }
            if let Ok(mt) = f.metadata().and_then(|m| m.modified()) {
                if best.as_ref().map_or(true, |(bm, _)| mt > *bm) {
                    best = Some((mt, f));
                }
            }
        }
    }
    best.map(|(_, p)| p)
}

/// Claude Code: 解析最新会话文件尾部的 assistant 消息 usage。
/// 相邻 assistant 消息的时间差近似本次请求时长（估算）。
fn collect_claude(root: &Path, limit: usize) -> Vec<Row> {
    let Some(path) = newest_claude_jsonl(root) else { return Vec::new() };
    let Some(lines) = tail_lines(&path, TAIL_BYTES) else { return Vec::new() };
    let mut rows = Vec::new();
    let mut prev_ts: Option<i64> = None;
    for l in &lines {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(l) else { continue };
        if v.get("type").and_then(|t| t.as_str()) != Some("assistant") {
            continue;
        }
        let ts = parse_iso_ms(v.get("timestamp").and_then(|t| t.as_str()).unwrap_or(""));
        let out = v
            .pointer("/message/usage/output_tokens")
            .and_then(|x| x.as_i64())
            .unwrap_or(0);
        let model = v
            .pointer("/message/model")
            .and_then(|m| m.as_str())
            .unwrap_or("claude")
            .to_string();
        let sid = v
            .get("sessionId")
            .and_then(|s| s.as_str())
            .unwrap_or("")
            .to_string();
        if let Some(p) = prev_ts {
            let gen = ts - p;
            if out > 0 && ts > 0 && gen >= 200 && gen <= MAX_EST_GEN_MS {
                rows.push(Row {
                    src: Src::Claude,
                    model,
                    variant: None,
                    agent: None,
                    session_id: sid,
                    started_at: ts - gen,
                    completed_at: ts,
                    ttft_ms: None,
                    output_tokens: out,
                    gen_ms: gen,
                    estimated: true,
                });
            }
        }
        prev_ts = Some(ts);
    }
    rows.reverse();
    rows.truncate(limit);
    rows
}

/// OpenCode: opencode.db 的 message 表，data 列为 JSON。
/// tokens.output + time.created/completed；间隔覆盖整轮（含工具执行，估算）。
fn collect_opencode(db: &Path, limit: usize) -> Vec<Row> {
    if !db.exists() {
        return Vec::new();
    }
    let Ok(con) = open_ro(&db) else { return Vec::new() };
    let Ok(mut stmt) =
        con.prepare("SELECT data FROM message ORDER BY rowid DESC LIMIT 8000")
    else {
        return Vec::new();
    };
    let Ok(it) = stmt.query_map([], |r| r.get::<_, String>(0)) else {
        return Vec::new();
    };
    let mut rows = Vec::new();
    for data in it.filter_map(|d| d.ok()) {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&data) else { continue };
        if v.get("role").and_then(|r| r.as_str()) != Some("assistant") {
            continue;
        }
        let out = v.pointer("/tokens/output").and_then(|x| x.as_i64()).unwrap_or(0);
        let created = v.pointer("/time/created").and_then(|x| x.as_i64()).unwrap_or(0);
        let completed = v.pointer("/time/completed").and_then(|x| x.as_i64()).unwrap_or(0);
        let gen = completed - created;
        if out <= 0 || gen < 200 || gen > MAX_EST_GEN_MS {
            continue;
        }
        rows.push(Row {
            src: Src::Opencode,
            model: v
                .get("modelID")
                .and_then(|m| m.as_str())
                .unwrap_or("opencode")
                .to_string(),
            variant: v
                .get("providerID")
                .and_then(|m| m.as_str())
                .map(String::from),
            agent: None,
            session_id: v
                .get("session_id")
                .and_then(|s| s.as_str())
                .unwrap_or("")
                .to_string(),
            started_at: created,
            completed_at: completed,
            ttft_ms: None,
            output_tokens: out,
            gen_ms: gen,
            estimated: true,
        });
        if rows.len() >= limit {
            break;
        }
    }
    rows.sort_by(|a, b| b.completed_at.cmp(&a.completed_at));
    rows.truncate(limit);
    rows
}

fn collect_zcode(db: &Path, limit: usize) -> Vec<Row> {
    if !db.exists() {
        return Vec::new();
    }
    let Ok(con) = open_ro(db) else { return Vec::new() };
    let sql = ROW_SQL.to_owned() + " ORDER BY started_at DESC LIMIT ?1";
    let Ok(mut stmt) = con.prepare(&sql) else { return Vec::new() };
    let Ok(rows) = stmt.query_map([limit as i64], row_from) else {
        return Vec::new();
    };
    rows.filter_map(|r| r.ok()).collect()
}

struct Collect {
    rows: Vec<Row>, // 全部工具合并，按完成时间倒序
    running: Option<(i64, Option<i64>, i64)>, // ZCode 生成中的请求
    times: Vec<(&'static str, Duration)>,     // bench 用
}

impl Collect {
    fn latest(&self) -> Option<&Row> {
        self.rows.first()
    }
}

fn collect(args: &Args, limit: usize) -> Collect {
    let mut times = Vec::new();
    let mut rows = Vec::new();
    let cfg = settings::get();

    // 工具与顺序来自设置里的列表（未配置过 = 本机探测结果）；--tool 仍单独过滤
    let tools: Vec<Src> = match args.tool {
        Some(t) => vec![t],
        None => settings::order(),
    };
    if tools.contains(&Src::Zcode) {
        let t = Instant::now();
        rows.extend(collect_zcode(&zcode_db(args), limit));
        times.push(("zcode", t.elapsed()));
    }
    if tools.contains(&Src::Codex) {
        let t = Instant::now();
        rows.extend(collect_codex(&cfg.codex_dir, limit));
        times.push(("codex", t.elapsed()));
    }
    if tools.contains(&Src::Opencode) {
        let t = Instant::now();
        rows.extend(collect_opencode(&cfg.opencode_db, limit));
        times.push(("opencode", t.elapsed()));
    }
    if tools.contains(&Src::Claude) {
        let t = Instant::now();
        rows.extend(collect_claude(&cfg.claude_dir, limit));
        times.push(("claude", t.elapsed()));
    }
    rows.sort_by(|a, b| b.completed_at.cmp(&a.completed_at));

    let zdb = zcode_db(args);
    let running = if tools.contains(&Src::Zcode) && zdb.exists() {
        open_ro(&zdb)
            .ok()
            .and_then(|con| {
                con.query_row(
                    "SELECT started_at, first_token_at, output_tokens FROM model_usage \
                     WHERE status = 'running' ORDER BY started_at DESC LIMIT 1",
                    [],
                    |r| {
                        Ok((
                            r.get::<_, i64>(0)?,
                            r.get::<_, Option<i64>>(1)?,
                            r.get::<_, i64>(2)?,
                        ))
                    },
                )
                .ok()
            })
    } else {
        None
    };

    Collect { rows, running, times }
}

fn fetch_session_rows(con: &Connection, sid: &str) -> Vec<Row> {
    let sql = ROW_SQL.to_owned() + " AND session_id = ?1 ORDER BY started_at DESC";
    let Ok(mut stmt) = con.prepare(&sql) else {
        return Vec::new();
    };
    let Ok(rows) = stmt.query_map([sid], row_from) else {
        return Vec::new();
    };
    rows.filter_map(|r| r.ok()).take(MAX_SESSION_ROWS).collect()
}

// ---------- 输出 ----------

fn report_human(args: &Args) -> i32 {
    let t_open_query = Instant::now();
    let col = collect(args, args.limit.max(1));

    if col.rows.is_empty() {
        println!("没有找到已完成的模型请求数据。");
        return 0;
    }

    // 标题: 最近一次（跨工具取最新；--tool 过滤后即该工具）
    let head = col.latest().unwrap();
    println!("⚡ 最近一次: {}", latest_line(head));

    let tools = args.tool.map(|t| vec![t]).unwrap_or_else(|| settings::order());
    if tools.len() > 1 {
        println!("🧰 各工具最近一次:");
        for src in &tools {
            match col.rows.iter().find(|r| r.src == *src) {
                Some(r) => println!("  {}", latest_line(r)),
                None => println!("  [{}] （无本地会话数据）", src.tag()),
            }
        }
        println!("   （~ 前缀 = 按本地记录估算，含工具执行时间；ZCode 为精确值）");
    }

    // ZCode 会话汇总
    if tools.contains(&Src::Zcode) {
        let Some(zc) = col.rows.iter().find(|r| r.src == Src::Zcode) else {
            return 0;
        };
        let sid = args.session.clone().unwrap_or_else(|| zc.session_id.clone());
        let rows = if let Ok(con) = open_ro(&zcode_db(args)) {
            fetch_session_rows(&con, &sid)
        } else {
            Vec::new()
        };
        if !rows.is_empty() {
            let n = rows.len();
            let total_out: i64 = rows.iter().map(|r| r.output_tokens).sum();
            let total_gen: i64 = rows.iter().map(|r| r.gen_ms).sum();
            let est_cnt = rows.iter().filter(|r| r.estimated).count();
            let avg = if total_gen > 0 {
                total_out as f64 * 1000.0 / total_gen as f64
            } else {
                0.0
            };
            let sid_short = if sid.chars().count() > 22 {
                format!("{}…", sid.chars().take(19).collect::<String>())
            } else {
                sid.clone()
            };
            println!(
                "📊 ZCode 会话 {sid_short} 汇总: {n} 次请求 | 加权平均 {avg:.1} tok/s | 共输出 {} tokens / 生成 {}",
                fmt_thousands(total_out),
                fmt_secs(total_gen)
            );
            if est_cnt > 0 {
                println!("   （其中 {est_cnt} 次缺首 token 时间，速度按总时长估算）");
            }
        }
    }

    // 合并明细表
    let limit = args.limit.max(1).min(col.rows.len());
    let label = if tools.len() > 1 { "全部工具" } else { head.src.name() };
    println!("📋 最近 {limit} 次 ({label}):");
    println!(
        "  {:<4}{:<14} {:<20} {:>6} {:>7} {:>7} {:>6} {:<10}",
        "SRC", "TIME", "MODEL", "TOK/S", "OUT", "GEN", "TTFT", "AGENT"
    );
    for r in col.rows.iter().take(limit) {
        let gen = format!("{}{}", if r.estimated { "~" } else { " " }, fmt_secs(r.gen_ms));
        let ttft = r
            .ttft_ms
            .map(|m| format!("{:.1}s", m as f64 / 1000.0))
            .unwrap_or_else(|| "-".into());
        println!(
            "  {:<4}{:<14} {:<20} {:>6.1} {:>7} {:>7} {:>6} {:<10}",
            r.src.tag(),
            fmt_ts(r.started_at),
            fmt_model(&r.model, &r.variant),
            speed(r),
            r.output_tokens,
            gen,
            ttft,
            fmt_agent(&r.agent)
        );
    }
    if args.bench {
        eprintln!("[bench] (采集+查询 {:?})", t_open_query.elapsed());
        for (name, d) in &col.times {
            eprintln!("[bench]   {:>9}: {:7.1} ms", name, d.as_secs_f64() * 1000.0);
        }
        eprintln!("[bench]    total: {:7.1} ms", t_open_query.elapsed().as_secs_f64() * 1000.0);
    }
    0
}

fn report_hook(args: &Args) {
    if FALSY.iter().any(|f| f.eq_ignore_ascii_case(args.auto_report.trim())) {
        return;
    }
    let mut event = String::new();
    if !io::stdin().is_terminal() {
        let _ = io::stdin().read_to_string(&mut event);
    }
    let session_id: Option<String> = serde_json::from_str::<serde_json::Value>(&event)
        .ok()
        .and_then(|v| v.get("session_id").and_then(|x| x.as_str()).map(String::from));

    // hook 模式任何失败都静默退出（无输出、exit 0），绝不阻塞对话。
    // 只看 ZCode 自己的数据（hook 事件属于 ZCode 会话）。
    let row = (|| -> Option<Row> {
        let con = open_ro(&zcode_db(args)).ok()?;
        if let Some(sid) = session_id {
            if let Some(r) = fetch_session_rows(&con, &sid).into_iter().next() {
                return Some(r);
            }
        }
        let sql = ROW_SQL.to_owned() + " ORDER BY started_at DESC LIMIT 1";
        let mut stmt = con.prepare(&sql).ok()?;
        stmt.query_row([], row_from).ok()
    })();
    let Some(row) = row else { return };

    let est = if row.estimated { "~" } else { "" };
    let mut text = format!(
        "⚡ {}{:.1} tok/s | {} | {} tokens / {}{}",
        est,
        speed(&row),
        fmt_model(&row.model, &row.variant),
        row.output_tokens,
        est,
        fmt_secs(row.gen_ms)
    );
    if let Some(ttft) = row.ttft_ms {
        text += &format!(" | TTFT {:.1}s", ttft as f64 / 1000.0);
    }
    println!("{}", serde_json::json!({ "additionalContext": text }));
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// watch 模式的单行状态：跨工具最近一次速度 + ZCode 会话均值 + 生成中指示。
/// 单行 `\r` 原地刷新，兼容所有终端（不依赖 ANSI）。
fn watch_line(col: &Collect, args: &Args) -> String {
    let mut line = match col.latest() {
        None => "⚡ 等待第一条已完成的请求…".to_string(),
        Some(r) => {
            let est = if r.estimated { "~" } else { "" };
            let mut s = format!(
                "⚡ {}{:.1} tok/s | [{}] {} | {} tok/{}",
                est,
                speed(r),
                r.src.tag(),
                r.model,
                r.output_tokens,
                fmt_secs(r.gen_ms)
            );
            if r.src == Src::Zcode {
                if let Ok(con) = open_ro(&zcode_db(args)) {
                    let rows = fetch_session_rows(&con, &r.session_id);
                    let total_out: i64 = rows.iter().map(|x| x.output_tokens).sum();
                    let total_gen: i64 = rows.iter().map(|x| x.gen_ms).sum();
                    if total_gen > 0 {
                        s += &format!(
                            " | 会话均值 {:.1} ({}次)",
                            total_out as f64 * 1000.0 / total_gen as f64,
                            rows.len()
                        );
                    }
                }
            }
            s
        }
    };

    // ZCode 正在生成中的请求（若有）
    if let Some((started, first_tok, out)) = col.running {
        let elapsed = now_ms() - first_tok.unwrap_or(started);
        if (0..=180_000).contains(&elapsed) {
            if out > 0 {
                let secs = elapsed as f64 / 1000.0;
                line += &format!(" | 🔴 生成中 {:.1} tok/s ({out} tok/{secs:.1}s)", out as f64 * 1000.0 / secs as f64);
            } else {
                line += &format!(" | 🔴 生成中… {}s", fmt_secs(elapsed.max(0)));
            }
        }
    }
    line
}

fn run_watch(args: &Args) {
    let interval = args.watch.unwrap_or(2).max(1);
    println!("tokenspeed watch（每 {interval}s 刷新，Ctrl+C 退出；ZC=ZCode CX=Codex OC=OpenCode CC=Claude）");
    let mut last_chars = 0usize;
    loop {
        let col = collect(args, 1);
        let line = watch_line(&col, args);
        // 用空格抹掉上一帧残留，再回到行首
        let pad = last_chars.saturating_sub(line.chars().count());
        print!("\r{}{}", line, " ".repeat(pad));
        let _ = io::stdout().flush();
        last_chars = line.chars().count();
        std::thread::sleep(std::time::Duration::from_secs(interval));
    }
}

fn main() {
    // 控制台输出统一 UTF-8，避免 GBK 控制台下中文乱码
    #[cfg(windows)]
    unsafe {
        windows_sys::Win32::System::Console::SetConsoleOutputCP(65001);
        // 设置窗口的文件/目录选择对话框（comdlg32/Shell）要求 STA COM，
        // 未初始化时 GetOpenFileNameW 会死锁或静默失败
        windows_sys::Win32::System::Com::CoInitializeEx(
            std::ptr::null(),
            (windows_sys::Win32::System::Com::COINIT_APARTMENTTHREADED
                | windows_sys::Win32::System::Com::COINIT_DISABLE_OLE1DDE) as u32,
        );
    }
    let args = parse_args();
    if args.hook {
        report_hook(&args);
        return;
    }
    // 无参数（双击启动）时默认进入悬浮条模式
    let any_arg = std::env::args().skip(1).next().is_some();
    if args.float || (!any_arg && !args.report) {
        float::run_float(&args);
        return;
    }
    if args.watch.is_some() {
        run_watch(&args);
        return;
    }
    exit(report_human(&args));
}

mod float;
mod settings;
mod tray;
