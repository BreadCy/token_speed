use crate::collectors::Agent;
use crate::monitor::{AgentStatus, AgentTotals};
#[cfg(target_os = "macos")]
use crate::ui::system_cjk_font_path;
use crate::ui::{
    compact_tokens, main_window_height, model_speed_text, pick_auto_agent, recents_list_height,
    relative_time, totals_list_height, weighted_average_speed,
};

fn status(agent: Agent, installed: bool, running: bool, activity_at: i64) -> AgentStatus {
    AgentStatus {
        agent,
        installed,
        running,
        activity_at,
        report: None,
        totals: AgentTotals::default(),
        error: None,
    }
}

#[test]
fn auto_follow_keeps_current_until_it_goes_idle() {
    // 当前对象仍在生成：即使另一个 agent 更晚活跃也不切换
    let statuses = [
        status(Agent::Codex, true, true, 100),
        status(Agent::ZCode, true, true, 900),
    ];
    assert_eq!(
        pick_auto_agent(&statuses, Some(Agent::Codex)),
        Some(Agent::Codex)
    );
}

#[test]
fn auto_follow_takes_the_latest_running_agent_when_current_is_idle() {
    let statuses = [
        status(Agent::Codex, true, true, 100),
        status(Agent::ZCode, true, true, 900),
    ];
    assert_eq!(
        pick_auto_agent(&statuses, Some(Agent::ClaudeCode)),
        Some(Agent::ZCode)
    );
}

#[test]
fn auto_follow_stays_on_the_most_recently_active_when_none_runs() {
    let statuses = [
        status(Agent::Codex, true, false, 100),
        status(Agent::ZCode, true, false, 900),
    ];
    assert_eq!(
        pick_auto_agent(&statuses, Some(Agent::Codex)),
        Some(Agent::ZCode)
    );
    // 已是最近活跃时保持不动（空闲期不来回跳）
    assert_eq!(
        pick_auto_agent(&statuses, Some(Agent::ZCode)),
        Some(Agent::ZCode)
    );
}

#[test]
fn auto_follow_ignores_uninstalled_agents_and_empty_results() {
    let statuses = [
        status(Agent::Codex, false, true, 900),
        status(Agent::ZCode, true, false, 100),
    ];
    assert_eq!(
        pick_auto_agent(&statuses, Some(Agent::Codex)),
        Some(Agent::ZCode)
    );
    assert_eq!(pick_auto_agent(&statuses, None), Some(Agent::ZCode));
    let none = [status(Agent::Codex, false, true, 900)];
    assert_eq!(pick_auto_agent(&none, None), None);
}

#[test]
fn ui_uses_an_em_dash_when_model_speed_is_unavailable() {
    assert_eq!(
        model_speed_text(None, crate::collectors::Accuracy::Unavailable),
        "—"
    );
}

#[test]
fn ui_weighted_average_needs_elapsed_time() {
    assert_eq!(weighted_average_speed(11_950, 10_000), Some(1_195.0));
    assert_eq!(weighted_average_speed(11_950, 0), None);
}

#[test]
fn ui_relative_time_uses_human_units() {
    assert_eq!(relative_time(0, 5_000), "5 秒前");
    assert_eq!(relative_time(0, 120_000), "2 分钟前");
    assert_eq!(relative_time(0, 7_200_000), "2 小时前");
    // 时钟回拨时不得出现负数描述
    assert_eq!(relative_time(10_000, 0), "0 秒前");
}

#[test]
fn main_window_height_tracks_the_recents_content() {
    // 收起态 = 两节表头（最近 10 轮 + 全项目累计）
    assert_eq!(main_window_height(false, 0, false, 0), 345.0);
    // 满列表（7 行整封顶滚动，不留半行）；展开列表额外带 6px 表头→列表间距
    assert_eq!(main_window_height(true, 10, false, 0), 345.0 + 182.0 + 6.0);
    // 行数少时窗口随列表收缩，不留空白死区
    assert_eq!(main_window_height(true, 5, false, 0), 345.0 + 130.0 + 6.0);
    assert_eq!(main_window_height(true, 0, false, 0), 345.0 + 30.0 + 6.0);
}

#[test]
fn totals_list_expands_independently_from_recents() {
    // 明细单独展开：最高 7 行封顶（与轮次列表同预算）
    assert_eq!(main_window_height(false, 0, true, 12), 345.0 + 182.0 + 6.0);
    // 明细行数少时贴合内容
    assert_eq!(main_window_height(false, 0, true, 3), 345.0 + 78.0 + 6.0);
    // 两个列表同时展开：高度直接相加，互不挤占
    assert_eq!(
        main_window_height(true, 10, true, 12),
        345.0 + (182.0 + 6.0) * 2.0
    );
    assert_eq!(
        main_window_height(true, 5, true, 3),
        345.0 + (130.0 + 6.0) + (78.0 + 6.0)
    );
}

#[test]
fn totals_list_caps_at_the_scroll_viewport_like_recents() {
    assert_eq!(totals_list_height(0), 26.0);
    assert_eq!(totals_list_height(3), 78.0);
    // 封顶 = 行高的整数倍（与轮次列表同预算 182）
    assert_eq!(totals_list_height(12), 182.0);
}

#[test]
fn compact_tokens_uses_human_units() {
    assert_eq!(compact_tokens(890), "890");
    assert_eq!(compact_tokens(45_200), "45.2K");
    assert_eq!(compact_tokens(1_234_567), "1.2M");
    assert_eq!(compact_tokens(0), "0");
}

#[test]
fn recents_list_caps_at_the_scroll_viewport() {
    assert_eq!(recents_list_height(0), 30.0);
    assert_eq!(recents_list_height(5), 130.0);
    // 封顶 = 行高的整数倍，避免最后一行被截断
    assert_eq!(recents_list_height(12), 182.0);
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
