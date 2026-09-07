# TokenSpeed

AI coding agent 的本地 token 速度悬浮窗（Rust），支持 ZCode / Codex / OpenCode / Claude Code / Pi。

## 命令

```bash
cd tokenspeed-rs
cargo test              # 全部单测
cargo build --release   # 发布构建
./target/release/tokenspeed   # 运行 macOS 桌面 HUD（或 cargo run）
```

## 结构

- `tokenspeed-rs/src/ui.rs` — eframe(egui) 无边框 HUD 窗口，macOS/Windows 同一套代码；视觉基准是 tokenspeed.html 的 `.win` 组件
- `tokenspeed-rs/src/monitor.rs` — 聚合监控引擎（`spawn_engine`/`run_engine`）：批量检测全部已装 agent（`detect_all_installed`），每源一个文件 watcher + 30s reconcile，输出 `AgentStatus` 列表驱动 UI
- `tokenspeed-rs/src/collectors.rs` — 五个 agent 的本地数据解析（只读本地，无网络）
- `tokenspeed-rs/src/config.rs` — 跨平台 config.json（`selected_agent` + `follow_mode`）
- `tokenspeed/` — ZCode 插件（skills / commands）
- `packaging/` `dist/` — 打包脚本与产物

## 约定

- 界面文案中文，代码、注释、commit message 英文。
- 改 UI 后必须实际运行并截图验证布局（方法见 MEMORY.md）。
