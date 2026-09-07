use super::config::{normalize_project, Agent, Config, ConfigError, FollowMode, WindowPosition};
use std::fs;
use std::time::{SystemTime, UNIX_EPOCH};

fn temp_path(name: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!(
        "tokenspeed-config-{name}-{}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ))
}

#[test]
fn default_config_is_one_agent_and_manual_follow() {
    let config = Config::default();
    assert_eq!(config.schema_version, 1);
    assert_eq!(config.selected_agent, Agent::Codex);
    assert_eq!(config.follow_mode, FollowMode::Manual);
    assert!(config.pinned_project.is_none());
    assert!(config.window_position.is_none());
    assert!(config.skill_hashes.is_empty());
}

#[test]
fn config_without_follow_mode_field_loads_as_manual() {
    let path = temp_path("legacy-no-follow.json");
    // 旧版本配置没有 follow_mode 字段，也没有已删除的 autostart：都必须能加载
    fs::write(
        &path,
        r#"{
            "schema_version": 1,
            "selected_agent": "codex",
            "autostart": true,
            "pinned_project": null,
            "window_position": null,
            "skill_hashes": {}
        }"#,
    )
    .unwrap();
    let loaded = Config::load_from(&path).unwrap();
    assert_eq!(loaded.follow_mode, FollowMode::Manual);
    assert_eq!(loaded.selected_agent, Agent::Codex);
    fs::remove_file(path).unwrap();
}

#[test]
fn config_round_trip_persists_agent_pin_and_position() {
    let path = temp_path("round-trip.json");
    let config = Config {
        selected_agent: Agent::ClaudeCode,
        pinned_project: Some("/tmp/project".into()),
        window_position: Some(WindowPosition { x: 3, y: -4 }),
        ..Config::default()
    };
    config.save_to(&path).unwrap();
    let loaded = Config::load_from(&path).unwrap();
    assert_eq!(loaded.selected_agent, Agent::ClaudeCode);
    assert_eq!(loaded.pinned_project.as_deref(), Some("/tmp/project"));
    assert_eq!(loaded.window_position, Some(WindowPosition { x: 3, y: -4 }));
    fs::remove_file(path).unwrap();
}

#[test]
fn corrupt_config_is_an_error_and_is_not_overwritten() {
    let path = temp_path("corrupt.json");
    fs::write(&path, b"{not json").unwrap();
    assert!(matches!(
        Config::load_from(&path),
        Err(ConfigError::InvalidJson(_))
    ));
    assert_eq!(fs::read(&path).unwrap(), b"{not json");
    fs::remove_file(path).unwrap();
}

#[test]
fn normalize_project_makes_relative_paths_absolute_without_shell_expansion() {
    let value = normalize_project(std::path::Path::new("~/literal/$PROJECT")).unwrap();
    assert!(value.ends_with("~/literal/$PROJECT"));
}

#[test]
fn normalize_project_lexically_collapses_missing_path_components() {
    let value = normalize_project(std::path::Path::new("missing/./nested/../project")).unwrap();
    assert!(value.ends_with("missing/project"));
    assert!(!value.contains("/./"));
    assert!(!value.contains("/../"));
}

#[test]
fn config_save_replaces_existing_file() {
    let path = temp_path("replace.json");
    fs::write(&path, b"old").unwrap();
    Config {
        selected_agent: Agent::ZCode,
        ..Config::default()
    }
    .save_to(&path)
    .unwrap();
    assert_eq!(
        Config::load_from(&path).unwrap().selected_agent,
        Agent::ZCode
    );
    fs::remove_file(path).unwrap();
}
