use std::path::PathBuf;
use std::process::exit;

use tokenspeed::collectors::{Accuracy, Agent};
use tokenspeed::config::{normalize_project, Config};
use tokenspeed::monitor::{
    detect_source, run_engine, scan_once, EngineEvent, EngineOptions, FollowerReport, Selector,
};
use tokenspeed::ui;

fn usage_error(message: &str) -> ! {
    eprintln!("{message}");
    exit(2);
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

fn parse_agent(value: &str) -> Agent {
    match value.to_ascii_lowercase().as_str() {
        "zcode" | "zc" => Agent::ZCode,
        "codex" | "cx" => Agent::Codex,
        "opencode" | "oc" => Agent::OpenCode,
        "claude" | "claude-code" | "cc" => Agent::ClaudeCode,
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
        if let Err(error) = ui::run() {
            eprintln!("desktop UI error: {error}");
            exit(2);
        }
        return;
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
            if let Err(error) = ui::run() {
                eprintln!("desktop UI error: {error}");
                exit(2);
            }
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
