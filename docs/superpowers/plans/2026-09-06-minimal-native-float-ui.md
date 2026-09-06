# Minimal Native Float UI Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use executing-plans to implement this plan task-by-task with review checkpoints.

**Goal:** 将 TokenSpeed 的 egui 桌面悬浮窗改为纸面仪表盘风格，并修复 macOS/Windows 中文字体回退。

**Architecture:** 保持现有 `TokenSpeedApp`、监视线程和 `FollowerReport` 不变；在 `ui.rs` 内增加集中式视觉配置与小型详情布局，继续使用现有格式化函数。字体配置只负责追加可用的本地 CJK fallback，不覆盖 egui 默认字体。

**Tech Stack:** Rust 2021, eframe/egui 0.32, cargo test。

---

### Task 1: 固化字体与视觉配置行为

**Files:**
- Modify: `tokenspeed-rs/src/ui_tests.rs`
- Modify: `tokenspeed-rs/src/ui.rs`

- [ ] **Step 1: Write the failing tests**

添加两个行为测试：macOS 的 CJK 候选必须优先包含 Hiragino/PingFang，且准确性标签继续返回“精确/估算/—”。

```rust
#[cfg(target_os = "macos")]
#[test]
fn macos_ui_prefers_a_modern_cjk_font() {
    let path = system_cjk_font_path().expect("a local CJK fallback is required");
    assert!(path.contains("Hiragino") || path.contains("PingFang"));
}
```

- [ ] **Step 2: Run tests and verify the new test fails**

Run: `cargo test --manifest-path tokenspeed-rs/Cargo.toml macos_ui_prefers_a_modern_cjk_font -- --exact`

Expected: FAIL because the current candidate returns `STHeiti Light.ttc`.

- [ ] **Step 3: Implement the minimal font/style helpers**

在 `ui.rs` 中将 macOS 候选顺序改为 `Hiragino Sans GB.ttc`、`PingFang.ttc`、`STHeiti Light.ttc`，保留 Windows 候选；字体加入 fallback 链而不是替换默认字体。新增 `configure_style`，集中设置暖白背景、深色文字、细边框、10px 窗口圆角和 4px 控件圆角，并在 `run` 的创建回调中调用。

- [ ] **Step 4: Run the focused tests**

Run: `cargo test --manifest-path tokenspeed-rs/Cargo.toml ui_uses_an_em_dash_when_model_speed_is_unavailable macos_ui_prefers_a_modern_cjk_font`

Expected: PASS。

### Task 2: 重排主悬浮窗信息层级

**Files:**
- Modify: `tokenspeed-rs/src/ui.rs:180-320`
- Modify: `tokenspeed-rs/src/ui_tests.rs`

- [ ] **Step 1: Write the failing test**

先为会话平均值格式化增加纯文本行为测试，避免视觉重排时重复拼接或误删准确性标签。

```rust
#[test]
fn ui_formats_session_average_with_accuracy() {
    assert_eq!(
        format_session_average(11_950, 1_000, crate::collectors::Accuracy::Exact),
        "11.950 tok/s · 精确"
    );
}
```

- [ ] **Step 2: Run the test and verify it fails or exposes the required shape**

Run: `cargo test --manifest-path tokenspeed-rs/Cargo.toml ui_formats_session_average_with_accuracy -- --exact`

Expected: FAIL because `format_session_average` does not exist yet.

- [ ] **Step 3: Implement the minimal layout**

新增并使用以下最小 helper，统一会话平均值格式：

```rust
pub(crate) fn format_session_average(tokens: u64, elapsed_ms: i64, accuracy: Accuracy) -> String {
    if elapsed_ms > 0 {
        format!(
            "{:.3} tok/s · {}",
            tokens as f64 * 1000.0 / elapsed_ms as f64,
            TokenSpeedApp::accuracy_label(accuracy)
        )
    } else {
        "—".into()
    }
}
```

将 `CentralPanel` 内容改为：顶栏状态点 + Agent + 设置；大号等宽速度；“精确/估算”轻量标签；三项详情；默认收起的“最近 10 轮”；底部状态。保留原有按钮、配置保存、监视重启和生成中提示，只调整 egui 布局与颜色。使用 `RichText` 的固定字号和 `FontFamily::Monospace` 显示数字，避免 emoji/图标字符。

- [ ] **Step 4: Run all tests**

Run: `cargo test --manifest-path tokenspeed-rs/Cargo.toml`

Expected: PASS with no new warnings。

### Task 3: 视觉验证与收尾

**Files:**
- Modify: `tokenspeed-rs/src/ui.rs` only if verification finds truncation or unreadable contrast.

- [ ] **Step 1: Build the release binary**

Run: `cargo build --manifest-path tokenspeed-rs/Cargo.toml --release`

Expected: `Finished release profile`。

- [ ] **Step 2: Run the desktop UI on macOS and inspect the real window**

Launch `tokenspeed-rs/target/release/tokenspeed`, verify Chinese glyphs, speed hierarchy, settings entry, and collapsed recent turns. No tofu squares, clipped text, gradients, or heavy shadows.

- [ ] **Step 3: Run formatting and diff checks**

Run: `cargo fmt --manifest-path tokenspeed-rs/Cargo.toml -- --check` and `git diff --check`.

Expected: both commands exit 0。

- [ ] **Step 4: Commit the implementation**

```bash
git add tokenspeed-rs/src/ui.rs tokenspeed-rs/src/ui_tests.rs
git commit -m "feat: redesign native monitor window"
```
