use crate::collectors::Agent;
use crate::config::Config;
use crate::ui::selector_for_config;
#[cfg(target_os = "macos")]
use crate::ui::system_cjk_font_path;
use crate::ui::{format_session_average, model_speed_text};

#[test]
fn ui_selector_uses_exactly_the_configured_agent_and_pin() {
    let config = Config {
        selected_agent: Agent::ClaudeCode,
        pinned_project: Some("/project".into()),
        ..Config::default()
    };
    let selector = selector_for_config(&config);
    assert_eq!(selector.agent, Agent::ClaudeCode);
    assert_eq!(selector.project.as_deref(), Some("/project"));
    assert_eq!(selector.session, None);
}

#[test]
fn ui_uses_an_em_dash_when_model_speed_is_unavailable() {
    assert_eq!(
        model_speed_text(None, crate::collectors::Accuracy::Unavailable),
        "—"
    );
}

#[test]
fn ui_formats_session_average_with_accuracy() {
    assert_eq!(
        format_session_average(11_950, 1_000_000, crate::collectors::Accuracy::Exact),
        "11.950 tok/s · 精确"
    );
}

#[cfg(target_os = "macos")]
#[test]
fn macos_ui_has_a_local_cjk_font_fallback() {
    assert!(system_cjk_font_path().is_some());
}

#[cfg(target_os = "macos")]
#[test]
fn macos_ui_prefers_a_modern_cjk_font() {
    let path = system_cjk_font_path().expect("a local CJK fallback is required");
    assert!(path.contains("Hiragino") || path.contains("PingFang"));
}
