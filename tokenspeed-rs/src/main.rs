use std::io::{IsTerminal, Read};
use std::path::PathBuf;
use std::process::exit;

use tokenspeed::collectors::{Accuracy, Agent, TurnMeasurement};
use tokenspeed::config::{normalize_project, Config};
use tokenspeed::monitor::{
    detect_source, run_engine, scan_once, EngineEvent, EngineOptions, FollowerReport, Selector,
};
use tokenspeed::ui;

fn usage_error(message: &str) -> ! {
    eprintln!("{message}");
    exit(2);
}

/// The desktop HUD must not stay attached to the launching console: closing a
/// console window on Windows terminates every process attached to it, which
/// killed the HUD as soon as users dismissed the console that double-click
/// opened. CLI subcommands keep their console and stdout untouched.
#[cfg(windows)]
fn detach_console() {
    // Failure is fine: the process may have no console to detach from.
    unsafe {
        windows_sys::Win32::System::Console::FreeConsole();
    }
}

#[cfg(not(windows))]
fn detach_console() {}

fn run_desktop() {
    detach_console();
    if let Err(error) = ui::run() {
        eprintln!("desktop UI error: {error}");
        exit(2);
    }
}

#[cfg(test)]
#[test]
fn human_speed_includes_accuracy_label() {
    assert_eq!(
        format_speed(12.3456, Accuracy::Exact),
        "12.346 tok/s (exact)"
    );
    assert_eq!(
        format_speed(0.125, Accuracy::Estimated),
        "0.125 tok/s (estimated)"
    );
}

#[cfg(test)]
#[test]
fn missing_model_speed_is_an_em_dash() {
    assert_eq!(format_model_speed(None, Accuracy::Unavailable), "—");
}

#[cfg(test)]
#[test]
fn legacy_secs_truncates_minutes_without_rolling_over() {
    assert_eq!(legacy_secs(0), "0.0s");
    assert_eq!(legacy_secs(59_400), "59.4s");
    // 119.96s must truncate to 1m59s, never round into "1m60s".
    assert_eq!(legacy_secs(119_960), "1m59s");
    assert_eq!(legacy_secs(90_500), "1m30s");
}

#[cfg(test)]
#[test]
fn legacy_thousands_groups_by_three() {
    assert_eq!(legacy_thousands(0), "0");
    assert_eq!(legacy_thousands(999), "999");
    assert_eq!(legacy_thousands(1_000), "1,000");
    assert_eq!(legacy_thousands(40_284), "40,284");
    assert_eq!(legacy_thousands(-1_234_567), "-1,234,567");
}

#[cfg(test)]
#[test]
fn legacy_speed_prefers_model_speed_and_marks_estimates() {
    use tokenspeed::collectors::SessionRef;
    let turn = |model_speed| TurnMeasurement {
        turn_id: "t".into(),
        session: SessionRef {
            id: "s".into(),
            project: None,
        },
        output_tokens: 100,
        input_tokens: 0,
        started_at: 0,
        completed_at: 1_000,
        effective_speed: 50.0,
        accuracy: Accuracy::Estimated,
        model: None,
        model_speed,
        model_accuracy: Accuracy::Unavailable,
    };
    assert_eq!(legacy_speed(&turn(Some(80.0))), (80.0, false));
    assert_eq!(legacy_speed(&turn(None)), (50.0, true));
}

fn parse_agent(value: &str) -> Agent {
    match value.to_ascii_lowercase().as_str() {
        "zcode" | "zc" => Agent::ZCode,
        "codex" | "cx" => Agent::Codex,
        "opencode" | "oc" => Agent::OpenCode,
        "claude" | "claude-code" | "cc" => Agent::ClaudeCode,
        "pi" => Agent::Pi,
        _ => usage_error("unknown agent"),
    }
}

fn accuracy_label(accuracy: Accuracy) -> &'static str {
    match accuracy {
        Accuracy::Exact => "exact",
        Accuracy::Estimated => "estimated",
        Accuracy::Unavailable => "unavailable",
    }
}

fn format_speed(value: f64, accuracy: Accuracy) -> String {
    format!("{value:.3} tok/s ({})", accuracy_label(accuracy))
}

fn format_model_speed(value: Option<f64>, accuracy: Accuracy) -> String {
    value.map_or_else(|| "—".into(), |speed| format_speed(speed, accuracy))
}

fn print_help() {
    println!(
        "tokenspeed {}\n\nUsage:\n  tokenspeed                 Open the desktop monitor\n  tokenspeed report [--agent AGENT] [--project PATH] [--session ID] [--json]\n  tokenspeed config show|set-agent AGENT|pin-project PATH|off\n  tokenspeed watch",
        env!("CARGO_PKG_VERSION")
    );
}

fn next_value(args: &mut impl Iterator<Item = String>, option: &str) -> String {
    args.next()
        .unwrap_or_else(|| usage_error(&format!("missing value for {option}")))
}

fn report(selector: Selector, json: bool) -> i32 {
    let source = match detect_source(selector.agent) {
        Ok(Some(source)) => source,
        Ok(None) => {
            if json {
                println!("null");
            } else {
                eprintln!("no source found for {}", selector.agent.as_str());
            }
            return 1;
        }
        Err(error) => {
            eprintln!("{error}");
            return 2;
        }
    };
    match scan_once(&selector) {
        Ok(Some(value)) => {
            if json {
                println!(
                    "{}",
                    serde_json::to_string(&value).unwrap_or_else(|_| "{}".into())
                );
            } else {
                println!(
                    "{} {} session={} running={} activity_at={}",
                    value.agent.as_str(),
                    value.project.as_deref().unwrap_or("-"),
                    value.session,
                    value.running,
                    value.activity_at
                );
                if value.session_total_elapsed_ms > 0 {
                    let weighted = value.session_total_tokens as f64 * 1000.0
                        / value.session_total_elapsed_ms as f64;
                    println!(
                        "session weighted average={weighted:.3} tok/s ({})",
                        accuracy_label(value.session_accuracy)
                    );
                } else {
                    println!(
                        "session weighted average=— ({})",
                        accuracy_label(value.session_accuracy)
                    );
                }
                for turn in value.turns {
                    println!(
                        "  {} tokens={} effective={} model_speed={}",
                        turn.turn_id,
                        turn.output_tokens,
                        format_speed(turn.effective_speed, turn.accuracy),
                        format_model_speed(turn.model_speed, turn.model_accuracy)
                    );
                }
            }
            0
        }
        Ok(None) => {
            if json {
                println!("null");
            } else {
                eprintln!("no matching session in source {}", source.path.display());
            }
            1
        }
        Err(error) => {
            eprintln!("{error}");
            2
        }
    }
}

fn config_command(mut args: impl Iterator<Item = String>) -> i32 {
    let action = next_value(&mut args, "config action");
    let mut config = Config::load().unwrap_or_else(|error| usage_error(&error.to_string()));
    match action.as_str() {
        "show" if args.next().is_none() => {
            println!("{}", serde_json::to_string_pretty(&config).unwrap());
            0
        }
        "set-agent" => {
            let value = next_value(&mut args, "set-agent");
            if args.next().is_some() {
                usage_error("too many arguments for set-agent");
            }
            config.selected_agent = parse_agent(&value);
            config
                .save()
                .unwrap_or_else(|error| usage_error(&error.to_string()));
            0
        }
        "pin-project" => {
            let value = next_value(&mut args, "pin-project");
            if args.next().is_some() {
                usage_error("too many arguments for pin-project");
            }
            config.pinned_project = if value == "off" {
                None
            } else {
                Some(
                    normalize_project(&PathBuf::from(value))
                        .unwrap_or_else(|error| usage_error(&error.to_string())),
                )
            };
            config
                .save()
                .unwrap_or_else(|error| usage_error(&error.to_string()));
            0
        }
        _ => usage_error("usage: tokenspeed config show|set-agent AGENT|pin-project PATH|off"),
    }
}

fn main() {
    let mut args = std::env::args().skip(1);
    let first = args.next();
    if matches!(first.as_deref(), Some("-h" | "--help")) {
        print_help();
        return;
    }
    if matches!(first.as_deref(), Some("-V" | "--version")) {
        println!("{}", env!("CARGO_PKG_VERSION"));
        return;
    }
    if first.is_none() {
        run_desktop();
        return;
    }
    // Legacy plugin CLI surface (v0.5.9): the ZCode plugin's skill and Stop
    // hook invoke `tokenspeed --limit N [--tool zc|cx|oc|cc]` and
    // `tokenspeed --hook --auto-report=...`. Route those flags before the
    // subcommand/`report` parsing claims them.
    if let Some(value) = first.as_deref() {
        let name = value.split('=').next().unwrap_or_default();
        if matches!(
            name,
            "--hook"
                | "--limit"
                | "--tool"
                | "--report"
                | "--bench"
                | "--db"
                | "--auto-report"
                | "--autostart"
        ) {
            exit(legacy_cli(std::iter::once(value.to_string()).chain(args)));
        }
    }
    let report_alias = first
        .as_deref()
        .is_some_and(|value| value.starts_with("--"));
    let command = if report_alias {
        "report".to_string()
    } else {
        first.as_deref().unwrap_or("").to_string()
    };
    let mut args: Box<dyn Iterator<Item = String>> = if report_alias {
        Box::new(std::iter::once(first.unwrap()).chain(args))
    } else {
        Box::new(args)
    };
    match command.as_str() {
        "ui" => {
            if args.next().is_some() {
                usage_error("ui does not accept arguments");
            }
            run_desktop();
        }
        "config" => exit(config_command(args)),
        "watch" => {
            if args.next().is_some() {
                usage_error("watch does not accept arguments");
            }
            let config = Config::load().unwrap_or_else(|error| usage_error(&error.to_string()));
            let agent = config.selected_agent;
            if let Err(error) = run_engine(
                EngineOptions {
                    project: config.pinned_project.clone(),
                    ..EngineOptions::default()
                },
                None,
                None,
                |event| match event {
                    EngineEvent::Statuses(statuses) => match statuses
                        .into_iter()
                        .find(|status| status.agent == agent)
                        .filter(|status| status.installed)
                        .and_then(|status| status.report)
                    {
                        Some(report) => println!(
                            "{}",
                            serde_json::to_string(&report).unwrap_or_else(|_| "{}".into())
                        ),
                        None => eprintln!("no matching session"),
                    },
                },
            ) {
                eprintln!("{error}");
                exit(2);
            }
        }
        "report" => {
            let mut agent = None;
            let mut project = None;
            let mut session = None;
            let mut json = false;
            while let Some(option) = args.next() {
                match option.as_str() {
                    "-h" | "--help" => {
                        print_help();
                        exit(0);
                    }
                    "-V" | "--version" => {
                        println!("{}", env!("CARGO_PKG_VERSION"));
                        exit(0);
                    }
                    "--agent" | "--tool" => {
                        agent = Some(parse_agent(&next_value(&mut args, &option)))
                    }
                    "--project" => {
                        project = Some(
                            normalize_project(&PathBuf::from(next_value(&mut args, &option)))
                                .unwrap_or_else(|error| usage_error(&error.to_string())),
                        )
                    }
                    "--session" => session = Some(next_value(&mut args, "--session")),
                    "--json" => json = true,
                    _ if option.starts_with("--agent=") || option.starts_with("--tool=") => {
                        agent = Some(parse_agent(option.split_once('=').unwrap().1));
                    }
                    _ if option.starts_with("--project=") => {
                        project = Some(
                            normalize_project(
                                PathBuf::from(option.split_once('=').unwrap().1).as_path(),
                            )
                            .unwrap_or_else(|error| usage_error(&error.to_string())),
                        );
                    }
                    _ if option.starts_with("--session=") => {
                        session = Some(option.split_once('=').unwrap().1.to_string());
                    }
                    _ => usage_error("unknown report option"),
                }
            }
            let config = Config::load().unwrap_or_else(|error| usage_error(&error.to_string()));
            exit(report(
                Selector {
                    agent: agent.unwrap_or(config.selected_agent),
                    project: project.or(config.pinned_project),
                    session,
                },
                json,
            ));
        }
        _ => usage_error("usage: tokenspeed ui|report|config|watch"),
    }
}

#[allow(dead_code)]
fn _report_type_is_serializable(_: &FollowerReport) {}

// ---------- Legacy plugin CLI (v0.5.9 compatible) ----------
// The ZCode plugin's skill runs `tokenspeed --limit N [--tool zc|cx|oc|cc]`
// and its Stop hook runs `tokenspeed --hook --auto-report=...`. These flags
// predate the subcommand CLI and must keep working across installs.

const LEGACY_AGENTS: [Agent; 5] = [
    Agent::ZCode,
    Agent::Codex,
    Agent::OpenCode,
    Agent::ClaudeCode,
    Agent::Pi,
];
const LEGACY_FALSY: [&str; 3] = ["0", "false", "off"];

fn legacy_tag(agent: Agent) -> &'static str {
    match agent {
        Agent::ZCode => "ZC",
        Agent::Codex => "CX",
        Agent::OpenCode => "OC",
        Agent::ClaudeCode => "CC",
        Agent::Pi => "PI",
    }
}

fn legacy_name(agent: Agent) -> &'static str {
    match agent {
        Agent::ZCode => "ZCode",
        Agent::Codex => "Codex",
        Agent::OpenCode => "OpenCode",
        Agent::ClaudeCode => "Claude Code",
        Agent::Pi => "Pi",
    }
}

fn legacy_secs(ms: i64) -> String {
    let s = ms as f64 / 1000.0;
    if s < 60.0 {
        format!("{s:.1}s")
    } else {
        // Truncate, never round: rounding would print "1m60s" at s = 119.96.
        format!("{}m{}s", (s / 60.0) as i64, (s % 60.0) as i64)
    }
}

fn legacy_ts(epoch_ms: i64) -> String {
    chrono::DateTime::from_timestamp_millis(epoch_ms)
        .map(|utc| {
            utc.with_timezone(&chrono::Local)
                .format("%m-%d %H:%M:%S")
                .to_string()
        })
        .unwrap_or_else(|| "-".into())
}

fn legacy_thousands(n: i64) -> String {
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

/// Best available speed for a legacy row: pure-generation model speed when
/// reliable, otherwise the whole-turn estimate (marked with `~`).
fn legacy_speed(turn: &TurnMeasurement) -> (f64, bool) {
    turn.model_speed
        .map(|speed| (speed, false))
        .unwrap_or((turn.effective_speed, true))
}

fn legacy_latest_line(agent: Agent, turn: &TurnMeasurement) -> String {
    let (speed, estimated) = legacy_speed(turn);
    let model = turn.model.as_deref().unwrap_or("-");
    format!(
        "[{}] {} {} {}{speed:.1} tok/s | {} tokens / {}",
        legacy_tag(agent),
        legacy_ts(turn.completed_at),
        model,
        if estimated { "~" } else { "" },
        legacy_thousands(turn.output_tokens as i64),
        legacy_secs(turn.completed_at.saturating_sub(turn.started_at)),
    )
}

fn legacy_report_for(agent: Agent, session: Option<String>) -> Option<FollowerReport> {
    detect_source(agent).ok().flatten()?;
    scan_once(&Selector {
        agent,
        project: None,
        session,
    })
    .ok()
    .flatten()
}

fn legacy_cli(mut args: impl Iterator<Item = String>) -> i32 {
    let mut limit = 10usize;
    let mut tool = None;
    let mut session = None;
    let mut hook = false;
    let mut auto_report = "true".to_string();
    while let Some(arg) = args.next() {
        let (name, inline) = match arg.split_once('=') {
            Some((name, value)) => (name.to_string(), Some(value.to_string())),
            None => (arg.clone(), None),
        };
        let mut value = || inline.clone().or_else(|| args.next());
        match name.as_str() {
            "--hook" => hook = true,
            "--report" | "--bench" => {}
            "--limit" => limit = value().and_then(|v| v.parse().ok()).unwrap_or(10),
            "--tool" => tool = value(),
            "--session" => session = value(),
            "--auto-report" => auto_report = value().unwrap_or_default(),
            // Accepted for compatibility; the new build derives paths itself.
            "--db" | "--autostart" => {
                let _ = value();
            }
            _ => {}
        }
    }
    if hook {
        legacy_hook(&auto_report, session);
        return 0;
    }
    let agents: Vec<Agent> = match tool.as_deref() {
        Some(value) => vec![parse_agent(value)],
        None => LEGACY_AGENTS.to_vec(),
    };
    legacy_report(&agents, limit.max(1))
}

/// Stop-hook mode: print one `{"additionalContext": ...}` line for the freshest
/// ZCode turn. Every failure path returns silently — a hook must never block
/// the conversation.
fn legacy_hook(auto_report: &str, session: Option<String>) {
    if LEGACY_FALSY
        .iter()
        .any(|f| f.eq_ignore_ascii_case(auto_report.trim()))
    {
        return;
    }
    let mut event = String::new();
    if !std::io::stdin().is_terminal() {
        let _ = std::io::stdin().read_to_string(&mut event);
    }
    let event_session = serde_json::from_str::<serde_json::Value>(&event)
        .ok()
        .and_then(|v| {
            v.get("session_id")
                .and_then(|x| x.as_str())
                .map(str::to_owned)
        });
    let report = legacy_report_for(
        Agent::ZCode,
        session.or_else(|| event_session.filter(|s| !s.is_empty())),
    )
    .or_else(|| legacy_report_for(Agent::ZCode, None));
    let Some(report) = report else { return };
    let Some(turn) = report.turns.first() else {
        return;
    };
    let (speed, estimated) = legacy_speed(turn);
    let mark = if estimated { "~" } else { "" };
    let model = turn.model.as_deref().unwrap_or("-");
    let text = format!(
        "⚡ {mark}{speed:.1} tok/s | {model} | {} tokens / {mark}{}",
        legacy_thousands(turn.output_tokens as i64),
        legacy_secs(turn.completed_at.saturating_sub(turn.started_at)),
    );
    println!("{}", serde_json::json!({ "additionalContext": text }));
}

fn legacy_report(agents: &[Agent], limit: usize) -> i32 {
    let mut reports: Vec<(Agent, Option<FollowerReport>)> = Vec::new();
    for &agent in agents {
        let report = legacy_report_for(agent, None);
        reports.push((agent, report));
    }
    let mut all_rows: Vec<(Agent, &TurnMeasurement)> = reports
        .iter()
        .filter_map(|(agent, report)| report.as_ref().map(|r| (*agent, r)))
        .flat_map(|(agent, report)| report.turns.iter().map(move |turn| (agent, turn)))
        .collect();
    all_rows.sort_by(|a, b| b.1.completed_at.cmp(&a.1.completed_at));
    let Some((head_agent, head)) = all_rows.first().copied() else {
        println!("没有找到已完成的模型请求数据。");
        return 0;
    };
    println!("⚡ 最近一次: {}", legacy_latest_line(head_agent, head));
    if agents.len() > 1 {
        println!("🧰 各工具最近一次:");
        for &(agent, ref report) in &reports {
            let line = report
                .as_ref()
                .and_then(|r| r.turns.first())
                .map(|turn| legacy_latest_line(agent, turn));
            match line {
                Some(line) => println!("  {line}"),
                None => println!("  [{}] （无本地会话数据）", legacy_tag(agent)),
            }
        }
        println!("   （~ 前缀 = 按本地会话记录估算，含工具执行时间）");
    }
    if agents.contains(&Agent::ZCode) {
        if let Some(report) = reports
            .iter()
            .find(|(agent, _)| *agent == Agent::ZCode)
            .and_then(|(_, report)| report.as_ref())
        {
            if report.session_total_elapsed_ms > 0 {
                let avg = report.session_total_tokens as f64 * 1000.0
                    / report.session_total_elapsed_ms as f64;
                let sid = &report.session;
                let sid_short = if sid.chars().count() > 22 {
                    format!("{}…", sid.chars().take(19).collect::<String>())
                } else {
                    sid.clone()
                };
                println!(
                    "📊 ZCode 会话 {sid_short} 汇总: 最近 {} 轮 | 加权平均 {avg:.1} tok/s | 共输出 {} tokens / 生成 {}",
                    report.turns.len(),
                    legacy_thousands(report.session_total_tokens as i64),
                    legacy_secs(report.session_total_elapsed_ms as i64),
                );
            }
        }
    }
    let label = if agents.len() > 1 {
        "全部工具"
    } else {
        legacy_name(agents[0])
    };
    let rows: Vec<(Agent, &TurnMeasurement)> = all_rows.into_iter().take(limit).collect();
    println!("📋 最近 {} 次 ({label}):", rows.len());
    println!(
        "  {:<4}{:<14} {:<22} {:>9} {:>9} {:>8}",
        "SRC", "TIME", "MODEL", "TOK/S", "OUT", "GEN"
    );
    for (agent, turn) in rows {
        let (speed, estimated) = legacy_speed(turn);
        let gen = format!(
            "{}{}",
            if estimated { "~" } else { " " },
            legacy_secs(turn.completed_at.saturating_sub(turn.started_at))
        );
        println!(
            "  {:<4}{:<14} {:<22} {:>8.1} {:>9} {:>8}",
            legacy_tag(agent),
            legacy_ts(turn.completed_at),
            turn.model.as_deref().unwrap_or("-"),
            speed,
            legacy_thousands(turn.output_tokens as i64),
            gen,
        );
    }
    0
}
