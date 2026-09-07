use crate::collectors::{Accuracy, Agent};
use crate::config::{Config, FollowMode};
use crate::menubar::{Menubar, TrayCommand};
use crate::monitor::{spawn_engine, AgentStatus, EngineEvent, EngineHandle, FollowerReport};
use eframe::egui;
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::Duration;

// —— 调色板（新版低饱和设计 token）——
const BG: egui::Color32 = egui::Color32::from_rgb(29, 30, 32); // #1D1E20
const SURFACE: egui::Color32 = egui::Color32::from_rgb(36, 37, 41); // #242529
const LIST_BG: egui::Color32 = egui::Color32::from_rgb(25, 26, 29); // #191A1D
const LINE: egui::Color32 = egui::Color32::from_rgb(42, 44, 48); // #2A2C30
const HOVER: egui::Color32 = egui::Color32::from_rgb(48, 51, 57);
/// 窗口描边专用：比分隔线 LINE 亮一档，暗色背景上仍能勾出圆角轮廓
/// （四角圆弧其实始终一致，暗背景下 LINE 描边近乎隐形导致下角"看起来"是直角）
const WINDOW_EDGE: egui::Color32 = egui::Color32::from_rgb(74, 78, 86);
/// 标题栏按钮常态底色：与标题栏表面（#242529）相近但可辨的浅半档
const BTN_BG: egui::Color32 = egui::Color32::from_rgb(43, 45, 50);
const TEXT: egui::Color32 = egui::Color32::from_rgb(235, 235, 238); // #EBEBEE
const MUTED: egui::Color32 = egui::Color32::from_rgb(157, 157, 164); // #9D9DA4
const MUTED_DIM: egui::Color32 = egui::Color32::from_rgb(116, 120, 129);
const GREEN: egui::Color32 = egui::Color32::from_rgb(127, 185, 138); // #7FB98A
const AMBER: egui::Color32 = egui::Color32::from_rgb(217, 174, 90); // #D9AE5A

const HUD_RADIUS: u8 = 10;
const TITLE_H: f32 = 44.0;
const WIN_W: f32 = 400.0;
/// 列表展开视口高度：正好 7 行整（26px×7）——按 190 这类非行高倍数封顶时，
/// 最后一行会被截成半行露出框外；不足 7 行时按实际行数自适应
const RECENTS_OPEN_H: f32 = 182.0;
const RECENTS_ROW_H: f32 = 26.0;
/// 主卡收起态高度（Agent 切换与固定项目两行已移入设置抽屉）。
/// 不可低于内容自然高度：内容顶穿窗口底部会把 painted 圆角盖成直角
const MAIN_CLOSED_H: f32 = 307.0;
/// 主模式窗口恒定高度 = 最高内容态（展开 10 轮）。卡片在窗口内伸缩（收起/展开/
/// 设置），卡片下方透明区点击穿透——与球态同一架构，主模式不再 resize 窗口，
/// 从根上消灭 resize 事件丢失导致的"卡片被裁/圆角变直角"
const MAIN_MAX_H: f32 = MAIN_CLOSED_H + RECENTS_OPEN_H;
const BALL: f32 = 64.0;
/// 球体/胶囊体外圈的透明边：留给 ping 光环外扩，避免被窗口裁剪
const BODY_MARGIN: f32 = 8.0;
/// 胶囊条宽度（双击悬浮球展开的目标态）：速度 │ 加权平均 │ Agent/状态 三段
const CAPSULE_W: f32 = 260.0;
/// 设置抽屉高度（覆盖主界面内容时的窗口高度；内容超出部分内部滚动）。
/// 移除「03 / 固定项目」后内容变矮，552 → 450 避免底部死空间
const SETTINGS_H: f32 = 450.0;
/// 滚动视口 = 抽屉高 450 - 头部固定区（约 74；底栏已移除、改动即时生效）。
/// 内容 ~370px 恰好放下，无需滚动；条形滚动条隐藏，滚轮/拖拽仍可滚
const SETTINGS_SCROLL_H: f32 = 376.0;

/// 球态窗口恒定尺寸（胶囊最大体宽 + 透明边），避免展开/收起的窗口 resize
const BALL_WIN_W: f32 = 320.0;
const BALL_WIN_H: f32 = 80.0;

const NUM_FAMILY: &str = "tsnum";
const SINGLE_INSTANCE_PORT: u16 = 45_170;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Main,
    Ball,
}

#[derive(Debug, Clone, Copy)]
enum InternalEvent {
    ShowMain,
    /// 光标线程的请求：true = 光标在球体上（需可交互），false = 离开（窗口穿透）
    SetInteractive(bool),
}

/// macOS：球态窗口恒为胶囊最大尺寸，透明区靠鼠标穿透放行点击。
/// 后台线程以 8ms 轮询全局光标（CGEvent，无权限要求），与球体屏幕区域求交，
/// 交互状态变化时唤醒 UI——UI 保持 250ms 低频重绘，不受轮询影响。
#[cfg(target_os = "macos")]
mod cursor_watch {
    use core_graphics::event::CGEvent;
    use core_graphics::event_source::{CGEventSource, CGEventSourceStateID};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    pub struct Shared {
        /// 可交互屏幕区域 (min_x, min_y, max_x, max_y)；None = 全窗口可交互（线程静默）
        pub interactive_rect: Mutex<Option<[f32; 4]>>,
        /// UI 当前已应用的交互状态：true = 可交互
        pub interactive: AtomicBool,
    }

    pub fn new_shared() -> Arc<Shared> {
        Arc::new(Shared {
            interactive_rect: Mutex::new(None),
            interactive: AtomicBool::new(true),
        })
    }

    fn cursor() -> Option<(f32, f32)> {
        let source = CGEventSource::new(CGEventSourceStateID::CombinedSessionState).ok()?;
        let event = CGEvent::new(source).ok()?;
        let p = event.location();
        Some((p.x as f32, p.y as f32))
    }

    pub fn spawn(shared: Arc<Shared>, tx: std::sync::mpsc::Sender<crate::ui::InternalEvent>) {
        std::thread::spawn(move || loop {
            std::thread::sleep(Duration::from_millis(8));
            let Some(rect) = *shared.interactive_rect.lock().unwrap() else {
                continue;
            };
            let Some((cx, cy)) = cursor() else {
                continue;
            };
            let inside = cx >= rect[0] && cx <= rect[2] && cy >= rect[1] && cy <= rect[3];
            if inside != shared.interactive.load(Ordering::Relaxed)
                && tx
                    .send(crate::ui::InternalEvent::SetInteractive(inside))
                    .is_err()
            {
                return; // UI 已退出
            }
        });
    }
}

/// Auto-follow selection: keep the current agent while it is running; otherwise
/// follow the running agent with the latest activity; when none runs, stay on the
/// most recently active installed agent (stable — no flapping between idle agents).
pub(crate) fn pick_auto_agent(statuses: &[AgentStatus], current: Option<Agent>) -> Option<Agent> {
    let installed: Vec<&AgentStatus> = statuses.iter().filter(|s| s.installed).collect();
    installed.first()?;
    if let Some(current) = current {
        if installed.iter().any(|s| s.agent == current && s.running) {
            return Some(current);
        }
    }
    let latest = |list: Vec<&AgentStatus>| {
        list.into_iter()
            .max_by_key(|s| s.activity_at)
            .map(|s| s.agent)
    };
    latest(installed.iter().filter(|s| s.running).copied().collect()).or_else(|| latest(installed))
}

#[allow(dead_code)]
pub(crate) fn model_speed_text(speed: Option<f64>, accuracy: Accuracy) -> String {
    speed.map_or_else(
        || "—".into(),
        |speed| {
            format!(
                "{speed:.3} tok/s · {}",
                TokenSpeedApp::accuracy_label(accuracy)
            )
        },
    )
}

/// 会话加权平均：总输出 token 除以总会话生成时长
pub(crate) fn weighted_average_speed(tokens: u64, elapsed_ms: i64) -> Option<f64> {
    (elapsed_ms > 0).then(|| tokens as f64 * 1000.0 / elapsed_ms as f64)
}

/// 完成时刻到现在的相对时间描述
pub(crate) fn relative_time(completed_at_ms: i64, now_ms: i64) -> String {
    let seconds = (now_ms - completed_at_ms).max(0) / 1000;
    if seconds < 60 {
        format!("{seconds} 秒前")
    } else if seconds < 3600 {
        format!("{} 分钟前", seconds / 60)
    } else {
        format!("{} 小时前", seconds / 3600)
    }
}

/// 展开列表的实际高度：行数少时贴合内容不留空白，行数多时封顶改滚动
pub(crate) fn recents_list_height(turn_count: usize) -> f32 {
    if turn_count == 0 {
        30.0 // 空态提示「暂无数据」一行的高度
    } else {
        (RECENTS_ROW_H * turn_count as f32).min(RECENTS_OPEN_H)
    }
}

pub(crate) fn main_window_height(recents_open: bool, turn_count: usize) -> f32 {
    MAIN_CLOSED_H
        + if recents_open {
            recents_list_height(turn_count)
        } else {
            0.0
        }
}

/// 主显示器可用高度（点）：扣菜单栏与安全边距。
/// 窗口期望高度必须以此为上限——macOS 会把放不下的窗口钳制到屏幕内，
/// egui 若仍按期望尺寸绘制，卡片底部（连同圆角）会被切出直角
#[cfg(target_os = "macos")]
pub(crate) fn screen_available_height() -> f32 {
    let bounds = core_graphics::display::CGDisplay::main().bounds();
    (bounds.size.height as f32 - 36.0).max(320.0)
}

#[cfg(not(target_os = "macos"))]
#[allow(dead_code)]
pub(crate) fn screen_available_height() -> f32 {
    f32::INFINITY
}

#[cfg(target_os = "macos")]
pub(crate) fn system_cjk_font_path() -> Option<&'static str> {
    [
        "/System/Library/Fonts/Hiragino Sans GB.ttc",
        "/System/Library/Fonts/PingFang.ttc",
        "/System/Library/Fonts/STHeiti Light.ttc",
        "/System/Library/Fonts/Supplemental/Arial Unicode.ttf",
    ]
    .into_iter()
    .find(|path| std::path::Path::new(path).is_file())
}

#[cfg(windows)]
fn system_cjk_font_path() -> Option<&'static str> {
    [r"C:\Windows\Fonts\msyh.ttc", r"C:\Windows\Fonts\simhei.ttf"]
        .into_iter()
        .find(|path| std::path::Path::new(path).is_file())
}

#[cfg(all(not(target_os = "macos"), not(windows)))]
fn system_cjk_font_path() -> Option<&'static str> {
    ["/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc"]
        .into_iter()
        .find(|path| std::path::Path::new(path).is_file())
}

fn configure_fonts(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    let mut cjk_loaded = false;
    if let Some(path) = system_cjk_font_path() {
        if let Ok(bytes) = std::fs::read(path) {
            let mut font = egui::FontData::from_owned(bytes);
            font.index = 0; // Hiragino Sans GB W3
            fonts.font_data.insert("system-cjk".into(), font.into());
            for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
                let family_fonts = fonts.families.entry(family).or_default();
                if !family_fonts.iter().any(|name| name == "system-cjk") {
                    family_fonts.insert(0, "system-cjk".into());
                }
            }
            cjk_loaded = true;
        }
    }

    // 大号速度数字用真实 Bold 等宽字面（egui 无假粗体）；明细行数字等宽
    let mut num_stack: Vec<String> = Vec::new();
    #[cfg(target_os = "macos")]
    if let Ok(bytes) = std::fs::read("/System/Library/Fonts/Menlo.ttc") {
        for (name, index) in [("menlo", 0u32), ("menlo-bold", 1u32)] {
            let mut font = egui::FontData::from_owned(bytes.clone());
            font.index = index;
            fonts.font_data.insert(name.into(), font.into());
        }
        let mono = fonts
            .families
            .entry(egui::FontFamily::Monospace)
            .or_default();
        if !mono.iter().any(|name| name == "menlo") {
            mono.insert(0, "menlo".into());
        }
        num_stack.push("menlo-bold".into());
    }
    if cjk_loaded {
        num_stack.push("system-cjk".into());
    }
    if num_stack.is_empty() {
        num_stack = fonts
            .families
            .get(&egui::FontFamily::Monospace)
            .cloned()
            .unwrap_or_default();
    }
    fonts
        .families
        .insert(egui::FontFamily::Name(NUM_FAMILY.into()), num_stack);
    ctx.set_fonts(fonts);
}

fn configure_style(ctx: &egui::Context) {
    ctx.set_visuals(egui::Visuals::dark());
    ctx.style_mut(|style| {
        style.spacing.item_spacing = egui::vec2(8.0, 5.0);
        style.spacing.button_padding = egui::vec2(10.0, 5.0);
        let v = &mut style.visuals;
        v.override_text_color = Some(TEXT);
        v.weak_text_color = Some(MUTED);
        v.panel_fill = BG;
        v.window_fill = SURFACE;
        v.extreme_bg_color = LIST_BG; // 输入框底色
        v.window_stroke = egui::Stroke::new(1.0, LINE);
        v.window_corner_radius = egui::CornerRadius::same(HUD_RADIUS);
        v.window_shadow = egui::Shadow::NONE;
        v.hyperlink_color = GREEN;
        v.warn_fg_color = AMBER;
        v.selection.bg_fill = egui::Color32::from_rgb(46, 60, 50);
        v.selection.stroke = egui::Stroke::new(1.0, GREEN);
        v.widgets.noninteractive.bg_fill = egui::Color32::TRANSPARENT;
        v.widgets.noninteractive.bg_stroke = egui::Stroke::new(1.0, LINE);
        v.widgets.noninteractive.fg_stroke = egui::Stroke::new(1.0, MUTED);
        v.widgets.inactive.bg_fill = SURFACE;
        v.widgets.inactive.bg_stroke = egui::Stroke::new(1.0, LINE);
        v.widgets.hovered.bg_fill = HOVER;
        v.widgets.hovered.bg_stroke = egui::Stroke::new(1.0, MUTED);
        v.widgets.active.bg_fill = HOVER;
        v.widgets.active.bg_stroke = egui::Stroke::new(1.0, TEXT);
        v.widgets.open.bg_fill = HOVER;
        v.widgets.open.bg_stroke = egui::Stroke::new(1.0, TEXT);
        v.button_frame = true;
        v.collapsing_header_frame = false;
    });
}

pub fn run() -> Result<(), String> {
    let config = Config::load().map_err(|error| error.to_string())?;

    // 单实例：重复运行时唤醒已有实例的主界面，而不是开新窗口
    let (wake_tx, wake_rx) = mpsc::channel::<InternalEvent>();
    match std::net::TcpListener::bind(("127.0.0.1", SINGLE_INSTANCE_PORT)) {
        Ok(listener) => {
            let tx = wake_tx.clone();
            thread::spawn(move || {
                for stream in listener.incoming().flatten() {
                    drop(stream);
                    let _ = tx.send(InternalEvent::ShowMain);
                }
            });
        }
        Err(_) => {
            use std::io::Write as _;
            if let Ok(mut stream) =
                std::net::TcpStream::connect(("127.0.0.1", SINGLE_INSTANCE_PORT))
            {
                let _ = stream.write_all(b"show\n");
            }
            return Ok(());
        }
    }

    let mut viewport = egui::ViewportBuilder::default()
        .with_title("TokenSpeed")
        // 球态窗口恒为胶囊最大尺寸 + 透明边，展开/收起零 resize
        .with_inner_size([BALL_WIN_W, BALL_WIN_H])
        .with_always_on_top()
        .with_resizable(false)
        .with_decorations(false)
        .with_transparent(true);
    // macOS：关闭系统窗口阴影。阴影沿窗口 alpha 轮廓绘制，
    // ping 光环外扩时会沿光环拖出一圈黑边（看起来像光环变黑）
    viewport.has_shadow = Some(false);

    let options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };
    eframe::run_native(
        "TokenSpeed",
        options,
        Box::new(move |creation| {
            configure_fonts(&creation.egui_ctx);
            configure_style(&creation.egui_ctx);
            let mut app = TokenSpeedApp::new(config, wake_rx);
            #[cfg(target_os = "macos")]
            app.attach_cursor_watch(wake_tx);
            Ok(Box::new(app))
        }),
    )
    .map_err(|error| error.to_string())
}

/// 单轮明细的当帧快照，避免绘制过程中同时读写 self
struct TurnSnapshot {
    tokens: u64,
    speed: f64,
    accuracy: Accuracy,
}

struct TokenSpeedApp {
    config: Config,
    /// 引擎产出的全部 agent 状态（含未安装项），UI 的一切展示都从这里派生
    statuses: Vec<AgentStatus>,
    /// 当前展示中的 agent（手动 = selected_agent；自动 = pick_auto_agent 结果）
    display: Option<Agent>,
    /// 自动跟随档记住的当前对象，用于“多个同时生成保持当前直到空闲”
    auto_current: Option<Agent>,
    report: Option<FollowerReport>,
    status: String,
    paused: bool,
    show_settings: bool,
    /// 设置窗的暂存草稿：打开时从 config 播种，保存才回写（取消/Esc/✕ 丢弃）
    recents_open: bool,
    /// 悬浮球是否已双击展开为胶囊条
    capsule_open: bool,
    /// 球/胶囊体的当前宽度（动画中的值）
    ball_w: f32,
    /// 上一次的动画目标宽度；变化即从当前宽度起新动画
    capsule_target: f32,
    /// 进行中的展开/收起动画 (起点时刻, 起始宽, 目标宽)
    capsule_anim: Option<(std::time::Instant, f32, f32)>,
    /// 当前已应用的穿透状态：true = 窗口忽略鼠标（点击落到桌面）
    #[cfg(target_os = "macos")]
    passthrough: bool,
    /// 光标监视线程的共享状态（macOS）
    #[cfg(target_os = "macos")]
    cursor_shared: Option<std::sync::Arc<cursor_watch::Shared>>,
    mode: Mode,
    last_size: Option<(f32, f32)>,
    menubar: Option<Menubar>,
    /// 标题栏按钮热区（「暂停」「设置」；上一帧绘制时记录，双击收起时排除）
    titlebar_btn_zones: Vec<egui::Rect>,
    /// 设置抽屉打开代数：每次打开 +1，滚动区 id 随之更换（滚动位置归零）
    settings_open_generation: u32,
    /// 主模式当前卡片高度（窗口恒定 MAIN_MAX_H，卡片在其内伸缩）
    main_card_h: f32,
    wake_rx: Receiver<InternalEvent>,
    updates: Receiver<EngineEvent>,
    engine: EngineHandle,
}

impl TokenSpeedApp {
    fn new(config: Config, wake_rx: Receiver<InternalEvent>) -> Self {
        let (engine, updates) = spawn_engine(config.pinned_project.clone());
        Self {
            config,
            statuses: Vec::new(),
            display: None,
            auto_current: None,
            report: None,
            status: "正在读取本地会话…".into(),
            paused: false,
            show_settings: false,
            recents_open: false,
            capsule_open: false,
            ball_w: BALL,
            capsule_target: BALL,
            capsule_anim: None,
            #[cfg(target_os = "macos")]
            passthrough: false,
            #[cfg(target_os = "macos")]
            cursor_shared: None,
            mode: Mode::Ball,
            last_size: None,
            menubar: None,
            titlebar_btn_zones: Vec::new(),
            settings_open_generation: 0,
            main_card_h: MAIN_MAX_H,
            wake_rx,
            updates,
            engine,
        }
    }

    /// macOS：启动光标监视线程，球态下按光标位置切换窗口穿透
    #[cfg(target_os = "macos")]
    fn attach_cursor_watch(&mut self, wake_tx: std::sync::mpsc::Sender<InternalEvent>) {
        let shared = cursor_watch::new_shared();
        self.cursor_shared = Some(shared.clone());
        self.passthrough = false;
        cursor_watch::spawn(shared, wake_tx);
    }

    /// 应用交互/穿透状态（只在变化时发命令）
    #[cfg(target_os = "macos")]
    fn set_interactive(&mut self, interactive: bool, ctx: &egui::Context) {
        if self.passthrough != interactive {
            return;
        }
        self.passthrough = !interactive;
        if let Some(shared) = &self.cursor_shared {
            shared
                .interactive
                .store(interactive, std::sync::atomic::Ordering::Relaxed);
        }
        ctx.send_viewport_cmd(egui::ViewportCommand::MousePassthrough(!interactive));
    }

    fn drain_updates(&mut self) {
        let mut latest = None;
        while let Ok(EngineEvent::Statuses(statuses)) = self.updates.try_recv() {
            latest = Some(statuses);
        }
        if let Some(statuses) = latest {
            self.statuses = statuses;
        }
        if self.statuses.is_empty() {
            return; // 引擎尚未产出首轮状态
        }
        self.display = match self.config.follow_mode {
            FollowMode::Manual => Some(self.config.selected_agent),
            FollowMode::Auto => {
                if let Some(picked) = pick_auto_agent(&self.statuses, self.auto_current) {
                    self.auto_current = Some(picked);
                }
                self.auto_current
            }
        };
        let active = self
            .statuses
            .iter()
            .find(|status| Some(status.agent) == self.display);
        self.report = active.and_then(|status| status.report.clone());
        self.status = match active {
            Some(status) => match (&status.error, &status.report) {
                (Some(error), _) => format!("数据来源异常：{error}"),
                (None, Some(report)) => format!("本地会话：{}", report.session),
                (None, None) if status.installed => {
                    format!("等待 {} 会话数据…", Self::agent_label(status.agent))
                }
                (None, None) => "未检测到已安装的 Code Agent".into(),
            },
            // 自动档下没有任何已安装 agent 时才没有展示对象
            None => "未检测到已安装的 Code Agent".into(),
        };
    }

    fn save_config(&mut self) -> bool {
        if let Err(error) = self.config.save() {
            self.status = format!("无法保存设置：{error}");
            false
        } else {
            true
        }
    }

    fn agent_label(agent: Agent) -> &'static str {
        match agent {
            Agent::ZCode => "ZCode",
            Agent::Codex => "Codex",
            Agent::OpenCode => "OpenCode",
            Agent::ClaudeCode => "Claude Code",
            Agent::Pi => "Pi",
        }
    }

    fn accuracy_label(accuracy: Accuracy) -> &'static str {
        match accuracy {
            Accuracy::Exact => "精确",
            Accuracy::Estimated => "估算",
            Accuracy::Unavailable => "—",
        }
    }

    fn accuracy_color(accuracy: Accuracy) -> egui::Color32 {
        match accuracy {
            Accuracy::Exact => GREEN,
            Accuracy::Estimated => AMBER,
            Accuracy::Unavailable => MUTED,
        }
    }

    /// 极简标题栏：状态点 + Agent 名；拖动移动，双击收起为悬浮球
    fn paint_titlebar(
        &mut self,
        ui: &mut egui::Ui,
        ctx: &egui::Context,
        dot: egui::Color32,
        agent: &str,
        active: bool,
    ) {
        let width = ui.available_width();
        let (bar, _) = ui.allocate_exact_size(egui::vec2(width, TITLE_H), egui::Sense::hover());
        let painter = ui.painter();
        painter.rect_filled(
            bar,
            egui::CornerRadius {
                nw: HUD_RADIUS - 1,
                ne: HUD_RADIUS - 1,
                sw: 0,
                se: 0,
            },
            SURFACE,
        );
        painter.line_segment(
            [
                egui::pos2(bar.left(), bar.bottom() + 0.5),
                egui::pos2(bar.right(), bar.bottom() + 0.5),
            ],
            egui::Stroke::new(1.0, LINE),
        );

        let drag = ui.interact(
            bar,
            egui::Id::new("ts-titlebar"),
            egui::Sense::click_and_drag(),
        );
        let cy = bar.center().y;
        // 右上角「设置」：与标题栏同一块 interact 画布直绘，命中区独立于拖动；
        // 热区记录到 self，供「双击面板收起」排除该按钮
        let zone = egui::Rect::from_min_max(
            egui::pos2(bar.right() - 56.0, cy - 15.0),
            egui::pos2(bar.right() - 12.0, cy + 15.0),
        );
        // 「暂停/继续」紧贴「设置」左侧（原主卡底部按钮移入标题栏）；
        // 与「设置」同尺寸 44×30，两按钮等宽等高
        let pause_zone = egui::Rect::from_min_max(
            egui::pos2(bar.right() - 104.0, cy - 15.0),
            egui::pos2(bar.right() - 60.0, cy + 15.0),
        );
        self.titlebar_btn_zones = vec![pause_zone, zone];
        let pointer = drag.interact_pointer_pos();
        let in_zone = pointer.is_some_and(|pointer| zone.contains(pointer));
        let in_pause = pointer.is_some_and(|pointer| pause_zone.contains(pointer));
        if drag.dragged_by(egui::PointerButton::Primary) {
            ctx.send_viewport_cmd(egui::ViewportCommand::StartDrag);
        }
        if drag.double_clicked() && !in_zone && !in_pause {
            self.mode = Mode::Ball;
        }

        painter.rect_filled(
            bar,
            egui::CornerRadius {
                nw: HUD_RADIUS - 1,
                ne: HUD_RADIUS - 1,
                sw: 0,
                se: 0,
            },
            SURFACE,
        );
        painter.line_segment(
            [
                egui::pos2(bar.left(), bar.bottom() + 0.5),
                egui::pos2(bar.right(), bar.bottom() + 0.5),
            ],
            egui::Stroke::new(1.0, LINE),
        );
        // 生成中时状态点与 ping 光环同拍呼吸；空闲为静态点
        let time = ctx.input(|input| input.time) as f32;
        paint_status_dot(
            painter,
            egui::pos2(bar.left() + 16.0, cy),
            dot,
            active,
            time,
            1.0,
        );
        painter.text(
            egui::pos2(bar.left() + 30.0, cy),
            egui::Align2::LEFT_CENTER,
            agent,
            egui::FontId::monospace(12.0),
            TEXT,
        );

        // 暂停/继续：与「设置」同款样式
        let pause_label = if self.paused { "继续" } else { "暂停" };
        painter.rect_filled(
            pause_zone,
            egui::CornerRadius::same(4),
            if in_pause { HOVER } else { BTN_BG },
        );
        painter.rect_stroke(
            pause_zone,
            egui::CornerRadius::same(4),
            egui::Stroke::new(1.0, LINE),
            egui::StrokeKind::Inside,
        );
        painter.text(
            egui::pos2(pause_zone.center().x, pause_zone.center().y + 2.0),
            egui::Align2::CENTER_CENTER,
            pause_label,
            egui::FontId::proportional(11.0),
            if in_pause { TEXT } else { MUTED },
        );
        if drag.clicked() && in_pause {
            self.paused = !self.paused;
        }

        // 常态带相近色底（比标题栏浅半档，读得出是个按钮），悬停再提亮一档
        painter.rect_filled(
            zone,
            egui::CornerRadius::same(4),
            if in_zone { HOVER } else { BTN_BG },
        );
        painter.rect_stroke(
            zone,
            egui::CornerRadius::same(4),
            egui::Stroke::new(1.0, LINE),
            egui::StrokeKind::Inside,
        );
        painter.text(
            egui::pos2(zone.center().x, zone.center().y + 2.0),
            egui::Align2::CENTER_CENTER,
            "设置",
            egui::FontId::proportional(11.0),
            if in_zone { TEXT } else { MUTED },
        );
        if drag.clicked() && in_zone {
            if !self.show_settings {
                self.settings_open_generation += 1;
            }
            self.show_settings = true;
        }
    }

    fn agent_row_state(&self, agent: Agent) -> (bool, bool) {
        self.statuses
            .iter()
            .find(|status| status.agent == agent)
            .map_or((false, false), |status| (status.installed, status.running))
    }

    fn paint_settings(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        // 即时生效：Esc/✕ 仅关闭抽屉（无草稿、无保存按钮）
        if ctx.input(|input| input.key_pressed(egui::Key::Escape)) {
            self.show_settings = false;
            return;
        }
        // 背景由外层卡片承担，这里只画带内边距的内容
        egui::Frame::new()
            .inner_margin(egui::Margin::same(12))
            .show(ui, |ui| {
                // 头部：标题 + 关闭
                ui.horizontal(|ui| {
                    // 标题与右侧 ✕ 按钮同一行高（28px）光学居中，避免标题偏高
                    let (title_rect, _) =
                        ui.allocate_exact_size(egui::vec2(60.0, 28.0), egui::Sense::hover());
                    ui.painter().text(
                        egui::pos2(title_rect.left(), title_rect.center().y + 2.0),
                        egui::Align2::LEFT_CENTER,
                        "设置",
                        egui::FontId::proportional(15.0),
                        TEXT,
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let x = ui.add_sized(
                            [28.0, 28.0],
                            egui::Button::new(egui::RichText::new("✕").size(12.0).color(MUTED))
                                .fill(egui::Color32::TRANSPARENT)
                                .stroke(egui::Stroke::new(1.0, LINE))
                                .corner_radius(egui::CornerRadius::same(6)),
                        );
                        if x.clicked() {
                            self.show_settings = false;
                        }
                    });
                });
                ui.separator();
                ui.add_space(2.0);

                egui::ScrollArea::vertical()
                    .id_salt(("settings-scroll", self.settings_open_generation))
                    .max_height(SETTINGS_SCROLL_H)
                    .auto_shrink([false; 2])
                    .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden)
                    .show(ui, |ui| {
                        ui.add_space(4.0);
                        section_header(ui, "01 / 跟随模式");
                        let manual = self.config.follow_mode == FollowMode::Manual;
                        if settings_option_card(
                            ui,
                            "手动选择 Agent",
                            "锁定一个 Agent，只看它的速度。适合对比排查时固定视角。",
                            manual,
                            true,
                        )
                        .clicked()
                        {
                            self.config.follow_mode = FollowMode::Manual;
                            self.auto_current = None;
                            self.save_config();
                        }
                        if settings_option_card(
                            ui,
                            "自动跟随活跃 Agent",
                            "谁在输出就跟谁，主界面标签自动切换并闪动提示。",
                            !manual,
                            true,
                        )
                        .clicked()
                        {
                            self.config.follow_mode = FollowMode::Auto;
                            self.auto_current = None;
                            self.save_config();
                        }

                        ui.add_space(10.0);
                        section_header(ui, "02 / 跟随 AGENT（仅一个）");
                        ui.add_space(4.0);
                        let agents: Vec<Agent> =
                            self.statuses.iter().map(|status| status.agent).collect();
                        let mut listed = agents.clone();
                        // 手动选中但未安装的 agent 仍列出（标注未安装），避免选择凭空消失
                        if !listed.contains(&self.config.selected_agent) {
                            listed.push(self.config.selected_agent);
                        }
                        for agent in listed {
                            let (installed, running) = self.agent_row_state(agent);
                            // 行任何时候都可点：自动档下点选 = 显式切回手动并锁定
                            let selectable = installed;
                            if settings_agent_row(
                                ui,
                                Self::agent_label(agent),
                                if !installed {
                                    "未安装"
                                } else if running {
                                    "活跃中"
                                } else {
                                    "空闲"
                                },
                                self.config.selected_agent == agent,
                                selectable,
                                installed && running,
                            )
                            .clicked()
                            {
                                self.config.follow_mode = FollowMode::Manual;
                                self.config.selected_agent = agent;
                                self.auto_current = None;
                                self.save_config();
                            }
                        }
                        ui.add_space(2.0);
                        ui.label(
                            egui::RichText::new(
                                "自动跟随时点选任一 Agent 行 = 切回手动档并锁定该 Agent。",
                            )
                            .size(10.5)
                            .color(MUTED),
                        );
                        ui.add_space(6.0);
                        // 重新检测入口（原主卡空态按钮移入设置）：扫描不到 agent 时可手动重扫
                        if ghost_button(ui, "重新检测").clicked() {
                            self.engine.request_rescan();
                            self.status = "正在重新检测…".into();
                        }

                        ui.add_space(10.0);
                        ui.label(
                            egui::RichText::new("仅读取 Agent 本地数据，不会上传遥测或对话正文。")
                                .size(10.5)
                                .color(MUTED_DIM),
                        );
                        ui.add_space(4.0);
                    });
            });
    }
}

fn accuracy_cell(accuracy: Accuracy) -> (&'static str, egui::Color32) {
    (
        TokenSpeedApp::accuracy_label(accuracy),
        TokenSpeedApp::accuracy_color(accuracy),
    )
}

/// 三格指标：模型速度 / 最近一轮 / 会话加权平均（画布直绘，列宽恒定）
fn paint_tri(ui: &mut egui::Ui, cells: [(String, Option<(String, egui::Color32)>); 3]) {
    let labels = ["模型速度", "最近一轮", "会话加权平均"];
    egui::Frame::new()
        .fill(SURFACE)
        .stroke(egui::Stroke::new(1.0, LINE))
        .corner_radius(egui::CornerRadius::same(6))
        .inner_margin(egui::Margin::same(0))
        .show(ui, |ui| {
            ui.set_min_height(55.0);
            let full = ui.available_width();
            let (rect, _) = ui.allocate_exact_size(egui::vec2(full, 55.0), egui::Sense::hover());
            let painter = ui.painter();
            let col = full / 3.0;
            for (i, (value, accuracy)) in cells.into_iter().enumerate() {
                let x0 = rect.left() + col * i as f32;
                if i > 0 {
                    painter.line_segment(
                        [egui::pos2(x0, rect.top()), egui::pos2(x0, rect.bottom())],
                        egui::Stroke::new(1.0, LINE),
                    );
                }
                let tx = x0 + 12.0;
                painter.text(
                    egui::pos2(tx, rect.top() + 10.0),
                    egui::Align2::LEFT_TOP,
                    labels[i],
                    egui::FontId::proportional(10.0),
                    MUTED,
                );
                let galley = painter.layout_no_wrap(
                    value.clone(),
                    egui::FontId::new(13.0, egui::FontFamily::Monospace),
                    TEXT,
                );
                painter.galley(egui::pos2(tx, rect.top() + 26.0), galley.clone(), TEXT);
                if let Some((label, color)) = accuracy {
                    painter.text(
                        egui::pos2(tx + galley.size().x + 4.0, rect.top() + 28.0),
                        egui::Align2::LEFT_TOP,
                        format!("· {label}"),
                        egui::FontId::proportional(10.0),
                        color,
                    );
                }
            }
        });
}

/// 折叠箭头（矢量三角，不占文字列宽）：center 为三角形中心
fn paint_caret(painter: &egui::Painter, center: egui::Pos2, open: bool, color: egui::Color32) {
    // 两态取相近的小体量（约 7×5.5px），切换时大小不跳变；
    // 三角形视觉质心偏几何中心上方，整体下移 1px 与表头文字中线对齐
    let c = egui::pos2(center.x, center.y + 1.0);
    let points: Vec<egui::Pos2> = if open {
        // ▾：顶边两个角 + 底部尖点
        vec![
            egui::pos2(c.x - 3.5, c.y - 2.0),
            egui::pos2(c.x + 3.5, c.y - 2.0),
            egui::pos2(c.x, c.y + 3.5),
        ]
    } else {
        // ▸：左边两个角 + 右侧尖点
        vec![
            egui::pos2(c.x - 2.0, c.y - 3.5),
            egui::pos2(c.x - 2.0, c.y + 3.5),
            egui::pos2(c.x + 2.5, c.y),
        ]
    };
    painter.add(egui::Shape::convex_polygon(
        points,
        color,
        egui::Stroke::NONE,
    ));
}

/// 最近 10 轮：默认收起为一行按钮，点击展开细线列表。
/// 表头文字与数据行共用同一列网格（锚定 ScrollArea 的 inner_rect）：
/// 「最近 10 轮」左对齐「第 N 轮」，箭头挂在左侧 padding 内；右侧轮数对齐行尾。
fn paint_recent(ui: &mut egui::Ui, turns: &[TurnSnapshot], open: &mut bool, viewport_h: f32) {
    egui::Frame::new()
        .fill(LIST_BG)
        .stroke(egui::Stroke::new(1.0, LINE))
        .corner_radius(egui::CornerRadius::same(6))
        .inner_margin(egui::Margin::same(0))
        .show(ui, |ui| {
            let (head, head_resp) = ui
                .allocate_exact_size(egui::vec2(ui.available_width(), 36.0), egui::Sense::click());
            if head_resp.hovered() {
                // 悬停高亮跟随列表框圆角：展开时只圆顶部，收起时四角全圆
                let radius = if *open {
                    egui::CornerRadius {
                        nw: 6,
                        ne: 6,
                        sw: 0,
                        se: 0,
                    }
                } else {
                    egui::CornerRadius::same(6)
                };
                ui.painter().rect_filled(head, radius, HOVER);
            }
            if head_resp.clicked() {
                *open = !*open;
            }

            // 表头文字/箭头/分割线的锚点一律用 head（确定性矩形），
            // 不用 ScrollArea 的 inner_rect（随滚动偏移与布局状态漂移）
            if *open {
                let mut out = egui::ScrollArea::vertical()
                    .max_height(viewport_h)
                    .auto_shrink([false; 2])
                    .show(ui, |ui| {
                        ui.spacing_mut().item_spacing.y = 0.0;
                        let row_h = RECENTS_ROW_H;
                        let (block, _) = ui.allocate_exact_size(
                            egui::vec2(ui.available_width(), row_h * turns.len() as f32),
                            egui::Sense::hover(),
                        );
                        let painter = ui.painter();
                        for (i, turn) in turns.iter().enumerate() {
                            let y = block.top() + row_h * i as f32;
                            if i > 0 {
                                painter.line_segment(
                                    [
                                        egui::pos2(block.left() + 6.0, y),
                                        egui::pos2(block.right() - 6.0, y),
                                    ],
                                    egui::Stroke::new(1.0, LINE),
                                );
                            }
                            // +2 光学补偿：CJK/数字墨迹在行带内天然偏上
                            let cy = y + row_h / 2.0 + 2.0;
                            painter.text(
                                egui::pos2(block.left() + 12.0, cy),
                                egui::Align2::LEFT_CENTER,
                                format!("第 {} 轮 · {} tok", i + 1, turn.tokens),
                                egui::FontId::proportional(11.5),
                                MUTED,
                            );
                            let (label, _) = accuracy_cell(turn.accuracy);
                            painter.text(
                                egui::pos2(block.right() - 12.0, cy),
                                egui::Align2::RIGHT_CENTER,
                                format!("{:.3} tok/s · {label}", turn.speed),
                                egui::FontId::proportional(11.5),
                                TEXT,
                            );
                        }
                    });
                // 滚动偏移吸附到行高整数倍：滚轮停在任何位置都整行显示，
                // 杜绝半行残影越过框底渲染到列表框外（下一帧生效，16ms 内不可感）
                let snapped = (out.state.offset.y / RECENTS_ROW_H).round() * RECENTS_ROW_H;
                if snapped != out.state.offset.y {
                    out.state.offset.y = snapped;
                    out.state.store(ui.ctx(), out.id);
                }
            }

            // 表头最后绘制：与数据行同列起点/终点，箭头占左侧 padding 不推挤文字列
            let cy = head.center().y;
            let strong = head_resp.hovered();
            let painter = ui.painter();
            // 表头下的整条分割线（展开时）：独立于列表项之间的分割线，后者保持不变
            if *open {
                painter.line_segment(
                    [
                        egui::pos2(head.left() + 6.0, head.bottom()),
                        egui::pos2(head.right() - 6.0, head.bottom()),
                    ],
                    egui::Stroke::new(1.0, LINE),
                );
            }
            paint_caret(
                painter,
                egui::pos2(head.left() + 6.0, cy + 1.0),
                *open,
                if strong { TEXT } else { MUTED },
            );
            // 表头用 proportional：CJK 与数字同字体（Hiragino 内含拉丁字形），
            // 等宽字体的数字相对汉字明显偏小且字距发空
            painter.text(
                egui::pos2(head.left() + 15.0, cy + 2.0),
                egui::Align2::LEFT_CENTER,
                "最近 10 轮",
                egui::FontId::proportional(11.5),
                if strong { TEXT } else { MUTED },
            );
            painter.text(
                egui::pos2(head.right() - 12.0, cy + 2.0),
                egui::Align2::RIGHT_CENTER,
                format!("{} 轮", turns.len()),
                egui::FontId::proportional(11.5),
                MUTED,
            );
        });
}

fn ghost_button(ui: &mut egui::Ui, text: &str) -> egui::Response {
    // egui Button 的几何居中会让 CJK 文字看起来偏高且水平略偏，
    // 改为画布直绘：墨迹光学居中（+2px），悬停提亮
    let font = egui::FontId::proportional(11.0);
    let galley = ui
        .painter()
        .layout_no_wrap(text.to_string(), font.clone(), MUTED);
    let size = egui::vec2(galley.size().x + 20.0, 28.0);
    let (rect, resp) = ui.allocate_exact_size(size, egui::Sense::click());
    let painter = ui.painter();
    painter.rect_filled(
        rect,
        egui::CornerRadius::same(4),
        if resp.hovered() {
            HOVER
        } else {
            egui::Color32::TRANSPARENT
        },
    );
    painter.rect_stroke(
        rect,
        egui::CornerRadius::same(4),
        egui::Stroke::new(1.0, LINE),
        egui::StrokeKind::Inside,
    );
    painter.text(
        egui::pos2(rect.center().x, rect.center().y + 2.0),
        egui::Align2::CENTER_CENTER,
        text,
        font,
        if resp.hovered() { TEXT } else { MUTED },
    );
    resp
}

/// 设置窗分组标题：等宽小字（如「01 / 跟随模式」）
fn section_header(ui: &mut egui::Ui, text: &str) {
    // proportional：数字与 CJK 同字体（Hiragino 内含拉丁字形），基线/光学位置一致；
    // monospace 的 Menlo 数字相对汉字偏小且上下错位
    ui.label(egui::RichText::new(text).size(11.5).color(MUTED));
}

/// 设置窗的大卡片单选项：整卡可点，选中 = 绿描边 + 微绿底；禁用时整体降透明
fn settings_option_card(
    ui: &mut egui::Ui,
    title: &str,
    desc: &str,
    selected: bool,
    enabled: bool,
) -> egui::Response {
    let width = ui.available_width();
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(width, 58.0), egui::Sense::click());
    let alpha = if enabled { 1.0 } else { 0.4 };
    let painter = ui.painter();
    painter.rect_filled(
        rect,
        egui::CornerRadius::same(8),
        if selected {
            egui::Color32::from_rgb(46, 60, 50)
        } else {
            LIST_BG
        },
    );
    painter.rect_stroke(
        rect,
        egui::CornerRadius::same(8),
        egui::Stroke::new(1.0, if selected { GREEN } else { LINE }),
        egui::StrokeKind::Inside,
    );
    if enabled && resp.hovered() && !selected {
        painter.rect_filled(rect, egui::CornerRadius::same(8), HOVER);
    }
    let cy = rect.top() + 16.0;
    let radio_c = egui::pos2(rect.left() + 18.0, cy);
    painter.circle_stroke(
        radio_c,
        6.0,
        egui::Stroke::new(
            1.5,
            (if selected { GREEN } else { MUTED }).gamma_multiply(alpha),
        ),
    );
    if selected {
        painter.circle_filled(radio_c, 3.0, GREEN.gamma_multiply(alpha));
    }
    painter.text(
        egui::pos2(rect.left() + 32.0, cy),
        egui::Align2::LEFT_CENTER,
        title,
        egui::FontId::proportional(13.0),
        TEXT.gamma_multiply(alpha),
    );
    painter.text(
        egui::pos2(rect.left() + 32.0, rect.bottom() - 14.0),
        egui::Align2::LEFT_CENTER,
        desc,
        egui::FontId::proportional(11.0),
        MUTED.gamma_multiply(alpha),
    );
    resp
}

/// 设置窗的 Agent 行：状态点 + 名称 + 右侧状态文字；整行可点（禁用时忽略）
fn settings_agent_row(
    ui: &mut egui::Ui,
    name: &str,
    state: &str,
    selected: bool,
    enabled: bool,
    running: bool,
) -> egui::Response {
    let width = ui.available_width();
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(width, 42.0), egui::Sense::click());
    let alpha = if enabled { 1.0 } else { 0.4 };
    let painter = ui.painter();
    painter.rect_filled(rect, egui::CornerRadius::same(8), LIST_BG);
    painter.rect_stroke(
        rect,
        egui::CornerRadius::same(8),
        egui::Stroke::new(1.0, if selected { GREEN } else { LINE }),
        egui::StrokeKind::Inside,
    );
    if enabled && resp.hovered() && !selected {
        painter.rect_filled(rect, egui::CornerRadius::same(8), HOVER);
    }
    let dot_color = if !enabled && state == "未安装" {
        MUTED_DIM
    } else if running {
        GREEN
    } else {
        MUTED_DIM
    };
    painter.circle_filled(
        egui::pos2(rect.left() + 16.0, rect.center().y),
        3.5,
        dot_color.gamma_multiply(alpha),
    );
    painter.text(
        egui::pos2(rect.left() + 28.0, rect.center().y),
        egui::Align2::LEFT_CENTER,
        name,
        egui::FontId::monospace(12.5),
        TEXT.gamma_multiply(alpha),
    );
    painter.text(
        egui::pos2(rect.right() - 12.0, rect.center().y),
        egui::Align2::RIGHT_CENTER,
        state,
        egui::FontId::proportional(10.5),
        MUTED.gamma_multiply(alpha),
    );
    resp
}

/// 悬浮球/胶囊条的当帧展示快照
struct BallView<'a> {
    width: f32,
    speed_text: &'a str,
    average_text: &'a str,
    agent: &'a str,
    state: &'a str,
    active: bool,
    time: f32,
}

/// 状态点（球态右上 / 胶囊左端 / 标题栏左端共用）：活跃时与 ping 光环同拍呼吸——
/// 发射瞬间点亮到最亮最大并胀出一圈微光晕，随拍衰减到 45% / 光晕散尽；
/// 胶囊态没有光环参照，光晕是呼吸在视觉上的主要载体；空闲为静态点
fn paint_status_dot(
    painter: &egui::Painter,
    center: egui::Pos2,
    color: egui::Color32,
    active: bool,
    time: f32,
    alpha: f32,
) {
    if !active {
        painter.circle_filled(center, 4.0, color.gamma_multiply(alpha));
        return;
    }
    let t = (time % 1.6) / 1.6;
    // 微光晕：半径随拍胀出（4.5→9），亮度随拍散尽——球态最大半径 9px 仍在画布 8px 边距内
    painter.circle_filled(
        center,
        4.5 + 4.5 * t,
        color.gamma_multiply(0.22 * (1.0 - t) * alpha),
    );
    painter.circle_filled(
        center,
        4.2 - 0.8 * t,
        color.gamma_multiply((1.0 - 0.55 * t) * alpha),
    );
}

/// 悬浮球 ⇄ 胶囊条：同一个全圆角胶囊形状，宽度动效伸缩（宽 = 64 时即是圆球）。
/// 双击切换两态；球态画中心速度 + ping 光环，胶囊态画
/// 状态点 + 速度 │ 加权平均 │ Agent/状态 三段（段间细分割线）。
fn paint_ball(ui: &mut egui::Ui, ctx: &egui::Context, view: BallView, capsule_open: &mut bool) {
    let BallView {
        width,
        speed_text,
        average_text,
        agent,
        state,
        active,
        time,
    } = view;
    // 画布 = 球体/胶囊体 + 四周透明边（ping 光环的外扩空间）
    // 画布：展开/收起动画期间窗口保持最终尺寸，球体内容左锚定、宽度在此画布内动画
    let (canvas, resp) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), BALL + 2.0 * BODY_MARGIN),
        egui::Sense::click_and_drag(),
    );
    let rect = egui::Rect::from_min_max(
        canvas.min + egui::vec2(BODY_MARGIN, BODY_MARGIN),
        egui::pos2(
            canvas.min.x + BODY_MARGIN + width,
            canvas.max.y - BODY_MARGIN,
        ),
    );
    let painter = ui.painter();
    let radius = egui::CornerRadius::same(BALL as u8 / 2);
    // 球态内容：宽 > 70 起淡出，避免与胶囊内容叠加
    let ball_alpha = (1.0 - (width - 70.0) / 40.0).clamp(0.0, 1.0);
    // 手动软阴影：替代已关闭的系统投影，只出现在球态、随展开淡出；
    // 多层半透明暗圆由内向外衰减 + 下移 2px 模拟顶光，画在球体填充之下
    if ball_alpha > 0.0 {
        for (offset, alpha) in [(1.0, 38.0), (3.0, 26.0), (5.0, 15.0), (7.0, 7.0)] {
            painter.circle_filled(
                rect.center() + egui::vec2(0.0, 2.0),
                BALL / 2.0 + offset,
                egui::Color32::from_rgba_unmultiplied(0, 0, 0, (alpha * ball_alpha) as u8),
            );
        }
    }
    // 空闲时球态（宽 64）透明度 120 以区分收缩态，展开为胶囊随宽度过渡回 185；生成中实心
    let body_alpha = if active {
        255.0
    } else {
        (120.0 + (width - BALL) / (100.0 - BALL) * 65.0).clamp(120.0, 185.0)
    };
    let fill = egui::Color32::from_rgba_unmultiplied(29, 30, 32, body_alpha as u8);
    painter.rect_filled(rect, radius, fill);
    painter.rect_stroke(
        rect,
        radius,
        egui::Stroke::new(1.0, LINE),
        egui::StrokeKind::Inside,
    );

    let cy = rect.center().y;
    let ball_center = egui::pos2(rect.left() + BALL / 2.0, cy);

    if ball_alpha > 0.0 {
        if active {
            let t = (time % 1.6) / 1.6;
            let alpha = (((1.0 - t) * 170.0) * ball_alpha) as u8;
            painter.circle_stroke(
                ball_center,
                30.0 + 8.0 * t,
                egui::Stroke::new(
                    1.5,
                    egui::Color32::from_rgba_unmultiplied(127, 185, 138, alpha),
                ),
            );
        }
        painter.text(
            ball_center + egui::vec2(0.0, -6.0),
            egui::Align2::CENTER_CENTER,
            speed_text,
            egui::FontId::new(16.0, egui::FontFamily::Name(NUM_FAMILY.into())),
            TEXT.gamma_multiply(ball_alpha),
        );
        painter.text(
            ball_center + egui::vec2(0.0, 10.0),
            egui::Align2::CENTER_CENTER,
            "tok/s",
            egui::FontId::proportional(9.5),
            MUTED.gamma_multiply(ball_alpha),
        );
        paint_status_dot(
            painter,
            ball_center + egui::vec2(21.0, -22.0),
            if active { GREEN } else { MUTED_DIM },
            active,
            time,
            ball_alpha,
        );
    }

    // 胶囊态内容：宽 > 100 起淡入
    let cap_alpha = ((width - 100.0) / 50.0).clamp(0.0, 1.0);
    if cap_alpha > 0.0 {
        paint_status_dot(
            painter,
            egui::pos2(rect.left() + 18.0, cy),
            if active { GREEN } else { MUTED_DIM },
            active,
            time,
            cap_alpha,
        );
        let galley = painter.layout_no_wrap(
            speed_text.to_string(),
            egui::FontId::new(21.0, egui::FontFamily::Name(NUM_FAMILY.into())),
            TEXT.gamma_multiply(cap_alpha),
        );
        // galley 锚点是左上角：回提半个行高垂直居中；tok/s 与数字顶对齐（上标式）
        let num_top = cy - galley.size().y / 2.0;
        painter.galley(
            egui::pos2(rect.left() + 30.0, num_top),
            galley.clone(),
            TEXT,
        );
        painter.text(
            egui::pos2(rect.left() + 30.0 + galley.size().x + 4.0, num_top),
            egui::Align2::LEFT_TOP,
            "tok/s",
            egui::FontId::proportional(9.0),
            MUTED.gamma_multiply(cap_alpha),
        );

        // 分割线 + 加权平均段：label 在上、数值在下，与右侧 Agent/状态同构。
        // 分割线贴着速度尾部（+12px），钳制在 120..140 —— 短速度不留大空白，
        // 4 位数速度也不会顶进分割线；上限保证 4 位数仍放得下（5 位数与旧版同样溢出）
        let tok_galley = painter.layout_no_wrap(
            "tok/s".to_string(),
            egui::FontId::proportional(9.0),
            MUTED.gamma_multiply(cap_alpha),
        );
        let speed_end = rect.left() + 30.0 + galley.size().x + 4.0 + tok_galley.size().x;
        let div_x = (speed_end + 12.0).clamp(rect.left() + 120.0, rect.left() + 140.0);
        painter.line_segment(
            [egui::pos2(div_x, cy - 14.0), egui::pos2(div_x, cy + 14.0)],
            egui::Stroke::new(1.0, LINE.gamma_multiply(cap_alpha)),
        );
        painter.text(
            egui::pos2(div_x + 10.0, cy - 8.0),
            egui::Align2::LEFT_CENTER,
            "加权平均",
            egui::FontId::proportional(9.0),
            MUTED.gamma_multiply(cap_alpha),
        );
        painter.text(
            egui::pos2(div_x + 10.0, cy + 8.0),
            egui::Align2::LEFT_CENTER,
            average_text,
            egui::FontId::new(12.0, egui::FontFamily::Monospace),
            TEXT.gamma_multiply(cap_alpha),
        );
        // 第二条分割线：贴着右侧 Agent/状态段的左缘（按两行文字的实测宽度退让）
        let agent_w = {
            let label = painter.layout_no_wrap(
                agent.to_string(),
                egui::FontId::monospace(12.0),
                TEXT.gamma_multiply(cap_alpha),
            );
            let state = painter.layout_no_wrap(
                state.to_string(),
                egui::FontId::proportional(10.0),
                MUTED.gamma_multiply(cap_alpha),
            );
            label.size().x.max(state.size().x)
        };
        let div2_x = rect.right() - 16.0 - agent_w - 12.0;
        painter.line_segment(
            [egui::pos2(div2_x, cy - 14.0), egui::pos2(div2_x, cy + 14.0)],
            egui::Stroke::new(1.0, LINE.gamma_multiply(cap_alpha)),
        );
        painter.text(
            egui::pos2(rect.right() - 16.0, cy - 8.0),
            egui::Align2::RIGHT_CENTER,
            agent,
            egui::FontId::monospace(12.0),
            TEXT.gamma_multiply(cap_alpha),
        );
        painter.text(
            egui::pos2(rect.right() - 16.0, cy + 8.0),
            egui::Align2::RIGHT_CENTER,
            state,
            egui::FontId::proportional(10.0),
            MUTED.gamma_multiply(cap_alpha),
        );
    }

    if resp.double_clicked() {
        *capsule_open = !*capsule_open;
    }
    if resp.dragged_by(egui::PointerButton::Primary) {
        ctx.send_viewport_cmd(egui::ViewportCommand::StartDrag);
    }
}

impl eframe::App for TokenSpeedApp {
    fn clear_color(&self, _: &egui::Visuals) -> [f32; 4] {
        // 无边框圆角 HUD：窗口四角必须全透明
        [0.0, 0.0, 0.0, 0.0]
    }

    fn update(&mut self, ctx: &egui::Context, _: &mut eframe::Frame) {
        if !self.paused {
            self.drain_updates();
        }
        while let Ok(event) = self.wake_rx.try_recv() {
            match event {
                InternalEvent::ShowMain => {
                    self.mode = Mode::Main;
                    self.capsule_open = false;
                }
                #[cfg(target_os = "macos")]
                InternalEvent::SetInteractive(want) => self.set_interactive(want, ctx),
                #[cfg(not(target_os = "macos"))]
                InternalEvent::SetInteractive(_) => {}
            }
        }
        // 托盘延迟到事件循环运行后的首帧创建：创建回调时机太早 NSStatusItem 不显示
        if self.menubar.is_none() {
            match Menubar::new() {
                Ok(menubar) => self.menubar = Some(menubar),
                Err(error) => eprintln!("TS-TRAY error: {error}"),
            }
        }
        if let Some(command) = self.menubar.as_ref().and_then(|menubar| menubar.poll()) {
            match command {
                TrayCommand::ShowMain => {
                    self.mode = Mode::Main;
                    self.capsule_open = false;
                }
                TrayCommand::Collapse => {
                    self.mode = Mode::Ball;
                    self.capsule_open = false;
                }
                TrayCommand::Quit => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
            }
        }

        // 双击主卡任意处收起为悬浮球（原仅标题栏）；「设置」按钮热区与设置抽屉除外
        if self.mode == Mode::Main
            && !self.show_settings
            && ctx.input(|input| {
                input
                    .pointer
                    .button_double_clicked(egui::PointerButton::Primary)
            })
        {
            let pos = ctx.input(|input| input.pointer.hover_pos());
            let over_btn = pos.is_some_and(|pointer| {
                self.titlebar_btn_zones
                    .iter()
                    .any(|zone| zone.contains(pointer))
            });
            if !over_btn {
                self.mode = Mode::Ball;
                self.capsule_open = false;
            }
        }

        let running = self.report.as_ref().is_some_and(|report| report.running);
        let active = running && !self.paused;
        // 悬浮球 ping 光环 / 标题栏状态点呼吸需要逐帧动画才丝滑；其余状态低频即可
        let cadence = match self.mode {
            Mode::Ball if active => 16,
            Mode::Main if active => 16,
            _ => 250,
        };
        ctx.request_repaint_after(Duration::from_millis(cadence));

        // 球 ⇄ 胶囊条宽度动画：自定义 250ms ease-in-out 三次曲线（起停对称，
        // 比 egui 内置指数缓动的"快起慢尾"更利落）；动画期间 16ms 连续重绘
        let capsule_agent = self.display.unwrap_or(self.config.selected_agent);
        let capsule_running = self.report.as_ref().is_some_and(|report| report.running);
        let capsule_state = if self.paused {
            "已暂停"
        } else if capsule_running {
            "生成中"
        } else {
            "空闲"
        };
        let right_w = ctx.fonts(|fonts| {
            let label = fonts.layout_no_wrap(
                Self::agent_label(capsule_agent).to_string(),
                egui::FontId::monospace(12.0),
                egui::Color32::WHITE,
            );
            let state = fonts.layout_no_wrap(
                capsule_state.to_string(),
                egui::FontId::proportional(10.0),
                egui::Color32::WHITE,
            );
            label.size().x.max(state.size().x)
        });
        // 40px = 基准 Agent 段宽（ZCode/Codex）；超出部分整体加宽，分割线位置不变
        let capsule_w = CAPSULE_W + (right_w - 40.0).max(0.0);
        let target_w = if self.capsule_open { capsule_w } else { BALL };
        if self.capsule_target != target_w {
            self.capsule_target = target_w;
            // 窗口恒定尺寸（穿透架构），展开/收起都是固定画布内的纯绘制动画
            self.capsule_anim = Some((std::time::Instant::now(), self.ball_w, target_w));
        }
        if let Some((start, from, to)) = self.capsule_anim {
            let t = (start.elapsed().as_secs_f32() / 0.25).min(1.0);
            let ease = if t < 0.5 {
                4.0 * t * t * t
            } else {
                1.0 - (-2.0 * t + 2.0).powi(3) / 2.0
            };
            self.ball_w = from + (to - from) * ease;
            if t >= 1.0 {
                self.ball_w = to;
                self.capsule_anim = None;
            } else {
                ctx.request_repaint_after(Duration::from_millis(16));
            }
        }
        let ball_w = self.ball_w;
        // 球态窗口恒为胶囊最大尺寸（展开/收起零 resize）；透明区靠鼠标穿透放行点击
        // 主模式窗口恒定（卡片伸缩、下方透明穿透）；屏幕装不下恒定高度时
        // macOS 钳制窗口，卡片收缩到实际窗口内（列表视口随之变矮但仍可滚）
        let desired = match self.mode {
            Mode::Main => (WIN_W, MAIN_MAX_H),
            Mode::Ball => (BALL_WIN_W, BALL_WIN_H),
        };
        // 屏幕放不下时收缩期望高度（列表视口随之变矮但仍可滚），保住四角圆角
        #[cfg(target_os = "macos")]
        let desired = (desired.0, desired.1.min(screen_available_height()));
        // 自愈式窗口尺寸同步：Intent（last_size）与实际（outer_rect）任何一处
        // 对不上期望值都重发 InnerSize——resize 事件偶尔会丢，一旦丢失 egui 会
        // 按旧尺寸绘制卡片（底部被切、圆角"消失"），且旧逻辑只发一次永不重试
        {
            let outer = ctx.input(|input| {
                input
                    .viewport()
                    .outer_rect
                    .map(|rect| (rect.width(), rect.height()))
            });
            let intent_stale = self.last_size != Some(desired);
            let actual_mismatch = outer.is_some_and(|actual| {
                (actual.0 - desired.0).abs() > 1.0 || (actual.1 - desired.1).abs() > 1.0
            });
            if intent_stale || actual_mismatch {
                ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(egui::vec2(
                    desired.0, desired.1,
                )));
                self.last_size = Some(desired);
            }
        }

        // 主卡片高度：内容态高度，且不超过实际窗口（macOS 对超高窗口的钳制）
        let main_card_h = if self.show_settings {
            SETTINGS_H
        } else {
            main_window_height(
                self.recents_open,
                self.report.as_ref().map_or(0, |report| report.turns.len()),
            )
        }
        .min(
            ctx.input(|input| {
                input
                    .viewport()
                    .outer_rect
                    .map(|rect| rect.height())
                    .unwrap_or(MAIN_MAX_H)
            })
            .max(280.0),
        );
        self.main_card_h = main_card_h;
        // 列表滚动视口随卡片实际高度收缩：屏幕矮时少显示几行（仍可滚），
        // 保证状态行与四角圆角不被裁掉
        let list_viewport = ((main_card_h - MAIN_CLOSED_H).max(26.0))
            .min(recents_list_height(
                self.report.as_ref().map_or(0, |report| report.turns.len()),
            ))
            .max(26.0);

        // 穿透状态机：光标线程按"可交互矩形"驱动（光标在矩形内=可交互，离开=穿透）
        // - 主界面 = 卡片矩形（窗口底部透明区穿透）
        // - 球态静止 = 球体矩形
        // - 胶囊展开/收起动画中 = 全窗口可交互
        #[cfg(target_os = "macos")]
        {
            if let Some(shared) = &self.cursor_shared {
                let rect =
                    ctx.input(|input| input.viewport().outer_rect)
                        .map(|outer| match self.mode {
                            Mode::Main => {
                                let max = outer.min + egui::vec2(WIN_W, self.main_card_h);
                                [outer.min.x, outer.min.y, max.x, max.y]
                            }
                            _ => {
                                let min = outer.min + egui::vec2(BODY_MARGIN, BODY_MARGIN);
                                [min.x, min.y, min.x + BALL, min.y + BALL]
                            }
                        });
                let settled = match self.mode {
                    Mode::Main => true,
                    Mode::Ball => !self.capsule_open && self.capsule_anim.is_none(),
                };
                *shared.interactive_rect.lock().unwrap() = if settled { rect } else { None };
                if !settled {
                    self.set_interactive(true, ctx);
                }
            }
        }

        let has_data = self.report.is_some();
        let agent = self.display.unwrap_or(self.config.selected_agent);
        let running = self.report.as_ref().is_some_and(|report| report.running);
        let paused = self.paused;
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        let time = ctx.input(|input| input.time) as f32;
        let turn0 = self.report.as_ref().and_then(|report| report.turns.first());
        let big = turn0.map(|turn| turn.effective_speed);
        let avg_value = self.report.as_ref().and_then(|report| {
            weighted_average_speed(report.session_total_tokens, report.session_total_elapsed_ms)
        });
        let avg_accuracy = self.report.as_ref().map(|report| report.session_accuracy);
        let avg_cell = avg_value.map_or_else(
            || ("—".to_string(), None),
            |value| {
                let (label, color) = avg_accuracy.map_or(("--".to_string(), MUTED), |a| {
                    let (l, c) = accuracy_cell(a);
                    (l.to_string(), c)
                });
                (format!("{value:.3}"), Some((label, color)))
            },
        );

        match self.mode {
            Mode::Main => {
                let big_acc = turn0.map(|turn| turn.accuracy);
                let completed_at = turn0.map(|turn| turn.completed_at);
                let model_cell = turn0.map_or_else(
                    || ("—".to_string(), None),
                    |turn| match turn.model_speed {
                        Some(value) => {
                            let (label, color) = accuracy_cell(turn.model_accuracy);
                            (format!("{value:.3}"), Some((label.to_string(), color)))
                        }
                        None => ("—".to_string(), Some(("无可靠区间".to_string(), MUTED))),
                    },
                );
                let last_cell = turn0.map_or_else(
                    || ("—".to_string(), None),
                    |turn| {
                        let (label, color) = accuracy_cell(turn.accuracy);
                        (
                            format!("{:.3}", turn.effective_speed),
                            Some((label.to_string(), color)),
                        )
                    },
                );
                let turns: Vec<TurnSnapshot> = self
                    .report
                    .as_ref()
                    .map(|report| {
                        report
                            .turns
                            .iter()
                            .take(10)
                            .map(|turn| TurnSnapshot {
                                tokens: turn.output_tokens,
                                speed: turn.effective_speed,
                                accuracy: turn.accuracy,
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                let agent_name = Self::agent_label(agent).to_string();
                let dot = if running && !paused {
                    AMBER
                } else if has_data {
                    GREEN
                } else {
                    MUTED_DIM
                };

                // 窗口恒定 MAIN_MAX_H：卡片自绘（高度 = main_card_h），
                // 下方透明区点击穿透；内容裁剪到卡片内，杜绝溢出裁角
                egui::CentralPanel::default()
                    .frame(egui::Frame::new())
                    .show(ctx, |ui| {
                        let card = egui::Rect::from_min_size(
                            egui::Pos2::ZERO,
                            egui::vec2(WIN_W, self.main_card_h),
                        );
                        ui.painter()
                            .rect_filled(card, egui::CornerRadius::same(10), BG);
                        ui.painter().rect_stroke(
                            card,
                            egui::CornerRadius::same(10),
                            egui::Stroke::new(1.0, WINDOW_EDGE),
                            egui::StrokeKind::Inside,
                        );
                        ui.set_clip_rect(card);
                        // 设置抽屉覆盖主卡：打开时整屏画设置，关闭后还原
                        if self.show_settings {
                            self.paint_settings(ui, ctx);
                            return;
                        }
                        self.paint_titlebar(ui, ctx, dot, &agent_name, active);
                        egui::Frame::new()
                            .inner_margin(egui::Margin {
                                left: 16,
                                right: 16,
                                top: 12,
                                bottom: 12,
                            })
                            .show(ui, |ui| {
                                ui.horizontal(|ui| {
                                    ui.spacing_mut().item_spacing.x = 6.0;
                                    ui.label(
                                        egui::RichText::new(big.map_or_else(
                                            || "—".to_string(),
                                            |speed| format!("{speed:.3}"),
                                        ))
                                        .size(42.0)
                                        .family(egui::FontFamily::Name(NUM_FAMILY.into()))
                                        .color(TEXT),
                                    );
                                    ui.vertical(|ui| {
                                        ui.add_space(7.0);
                                        ui.label(
                                            egui::RichText::new("tok/s").size(13.0).color(MUTED),
                                        );
                                    });
                                });
                                ui.add_space(3.0);
                                ui.horizontal(|ui| {
                                    ui.spacing_mut().item_spacing.x = 4.0;
                                    ui.label(
                                        egui::RichText::new("上一完整轮").size(11.0).color(MUTED),
                                    );
                                    if let Some(accuracy) = big_acc {
                                        let (label, color) = accuracy_cell(accuracy);
                                        ui.label(
                                            egui::RichText::new(format!("· {label}"))
                                                .size(11.0)
                                                .color(color),
                                        );
                                    }
                                    if running && !paused {
                                        ui.label(
                                            egui::RichText::new("· 生成中 · 保留上一结果")
                                                .size(11.0)
                                                .color(MUTED),
                                        );
                                    } else if let Some(completed_at) = completed_at {
                                        ui.label(
                                            egui::RichText::new(format!(
                                                "· {}",
                                                relative_time(completed_at, now_ms)
                                            ))
                                            .size(11.0)
                                            .color(MUTED),
                                        );
                                    }
                                });

                                ui.add_space(9.0);
                                paint_tri(ui, [model_cell, last_cell, avg_cell]);

                                ui.add_space(5.0);
                                paint_recent(ui, &turns, &mut self.recents_open, list_viewport);

                                ui.add_space(5.0);
                                self.paint_status_row(ui, running, has_data);
                            });
                    });
            }
            Mode::Ball => {
                let speed_text = big.map_or_else(|| "—".to_string(), |speed| format!("{speed:.1}"));
                let average_text = avg_cell.0.clone();
                let state = if paused {
                    "已暂停"
                } else if running {
                    "生成中"
                } else {
                    "空闲"
                };
                let active = running && !paused && has_data;

                egui::CentralPanel::default()
                    .frame(egui::Frame::new().fill(egui::Color32::TRANSPARENT))
                    .show(ctx, |ui| {
                        let mut capsule_open = self.capsule_open;
                        paint_ball(
                            ui,
                            ctx,
                            BallView {
                                width: ball_w,
                                speed_text: &speed_text,
                                average_text: &average_text,
                                agent: Self::agent_label(agent),
                                state,
                                active,
                                time,
                            },
                            &mut capsule_open,
                        );
                        self.capsule_open = capsule_open;
                    });
            }
        }
    }
}

impl TokenSpeedApp {
    fn paint_status_row(&mut self, ui: &mut egui::Ui, running: bool, has_data: bool) {
        let short = self
            .report
            .as_ref()
            .map(|report| {
                let head: String = report.session.chars().take(8).collect();
                format!("{head}…")
            })
            .unwrap_or_default();
        let text = if self.paused {
            format!("已暂停 · 本地会话 {short}")
        } else if running {
            "生成中 · 保留上一完整结果".to_string()
        } else if has_data {
            format!("就绪 · 本地会话 {short}")
        } else {
            self.status.clone()
        };
        ui.horizontal(|ui| {
            ui.add_sized(
                [ui.available_width(), 28.0],
                egui::Label::new(egui::RichText::new(&text).size(11.0).color(MUTED)).truncate(),
            );
        });
    }
}
