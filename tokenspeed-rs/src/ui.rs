use crate::collectors::{Accuracy, Agent};
use crate::config::{normalize_project, Config};
use crate::monitor::{watch_with_stop, FollowerReport, MonitorError, Selector};
use eframe::egui;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

pub(crate) fn selector_for_config(config: &Config) -> Selector {
    Selector {
        agent: config.selected_agent,
        project: config.pinned_project.clone(),
        session: None,
    }
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
    let Some(path) = system_cjk_font_path() else {
        return;
    };
    let Ok(bytes) = std::fs::read(path) else {
        return;
    };
    let mut fonts = egui::FontDefinitions::default();
    let mut font = egui::FontData::from_owned(bytes);
    font.index = 0;
    fonts.font_data.insert("system-cjk".into(), font.into());
    for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
        let family_fonts = fonts.families.entry(family).or_default();
        if !family_fonts.iter().any(|name| name == "system-cjk") {
            family_fonts.insert(0, "system-cjk".into());
        }
    }
    ctx.set_fonts(fonts);
}

fn configure_style(ctx: &egui::Context) {
    const BG: egui::Color32 = egui::Color32::from_rgb(247, 246, 243);
    const TEXT: egui::Color32 = egui::Color32::from_rgb(23, 23, 23);
    const MUTED: egui::Color32 = egui::Color32::from_rgb(119, 115, 108);
    const LINE: egui::Color32 = egui::Color32::from_rgb(231, 229, 224);
    const ACCENT: egui::Color32 = egui::Color32::from_rgb(46, 125, 91);

    ctx.set_visuals(egui::Visuals::light());
    ctx.style_mut(|style| {
        style.spacing.item_spacing = egui::vec2(8.0, 6.0);
        style.spacing.window_margin = egui::Margin::same(16);
        style.spacing.button_padding = egui::vec2(8.0, 4.0);
        style.visuals.override_text_color = Some(TEXT);
        style.visuals.weak_text_color = Some(MUTED);
        style.visuals.panel_fill = BG;
        style.visuals.window_fill = egui::Color32::WHITE;
        style.visuals.window_stroke = egui::Stroke::new(1.0, LINE);
        style.visuals.window_corner_radius = egui::CornerRadius::same(10);
        style.visuals.window_shadow = egui::Shadow::NONE;
        style.visuals.hyperlink_color = ACCENT;
        style.visuals.warn_fg_color = egui::Color32::from_rgb(155, 103, 32);
        style.visuals.widgets.noninteractive.bg_fill = egui::Color32::TRANSPARENT;
        style.visuals.widgets.noninteractive.bg_stroke = egui::Stroke::new(1.0, LINE);
        style.visuals.widgets.inactive.bg_fill = egui::Color32::WHITE;
        style.visuals.widgets.inactive.bg_stroke = egui::Stroke::new(1.0, LINE);
        style.visuals.widgets.hovered.bg_fill = BG;
        style.visuals.widgets.hovered.bg_stroke = egui::Stroke::new(1.0, MUTED);
        style.visuals.widgets.active.bg_fill = BG;
        style.visuals.widgets.active.bg_stroke = egui::Stroke::new(1.0, TEXT);
        style.visuals.widgets.open.bg_fill = BG;
        style.visuals.widgets.open.bg_stroke = egui::Stroke::new(1.0, TEXT);
        style.visuals.button_frame = false;
        style.visuals.collapsing_header_frame = false;
    });
}

pub fn run() -> Result<(), String> {
    let config = Config::load().map_err(|error| error.to_string())?;
    let viewport = egui::ViewportBuilder::default()
        .with_title("TokenSpeed")
        .with_inner_size([420.0, 240.0])
        .with_min_inner_size([360.0, 180.0])
        .with_always_on_top();
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
            Ok(Box::new(TokenSpeedApp::new(config)))
        }),
    )
    .map_err(|error| error.to_string())
}

struct TokenSpeedApp {
    config: Config,
    report: Option<FollowerReport>,
    status: String,
    project_input: String,
    show_settings: bool,
    updates: Receiver<Result<Option<FollowerReport>, MonitorError>>,
    stop: Arc<AtomicBool>,
}

impl TokenSpeedApp {
    fn new(config: Config) -> Self {
        let (updates, stop) = Self::watch_updates(selector_for_config(&config));
        Self {
            project_input: config.pinned_project.clone().unwrap_or_default(),
            config,
            report: None,
            status: "正在读取本地会话…".into(),
            show_settings: false,
            updates,
            stop,
        }
    }

    fn watch_updates(
        selector: Selector,
    ) -> (
        Receiver<Result<Option<FollowerReport>, MonitorError>>,
        Arc<AtomicBool>,
    ) {
        let (tx, updates) = mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = Arc::clone(&stop);
        thread::spawn(move || {
            if let Err(error) = watch_with_stop(selector, stopped, |update| {
                let _ = tx.send(update);
            }) {
                let _ = tx.send(Err(error));
            }
        });
        (updates, stop)
    }

    fn restart_watch(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        (self.updates, self.stop) = Self::watch_updates(selector_for_config(&self.config));
        self.status = "正在切换本地来源…".into();
        self.report = None;
    }

    fn drain_updates(&mut self) {
        loop {
            match self.updates.try_recv() {
                Ok(Ok(Some(report))) => {
                    self.status = format!("本地会话：{}", report.session);
                    self.report = Some(report);
                }
                Ok(Ok(None)) => {
                    self.status = "未检测到匹配的本地会话".into();
                    self.report = None;
                }
                Ok(Err(error)) => {
                    self.status = error.to_string();
                    self.report = None;
                }
                Err(TryRecvError::Empty | TryRecvError::Disconnected) => break,
            }
        }
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
        }
    }

    fn accuracy_label(accuracy: Accuracy) -> &'static str {
        match accuracy {
            Accuracy::Exact => "精确",
            Accuracy::Estimated => "估算",
            Accuracy::Unavailable => "—",
        }
    }

    fn status_dot(ui: &mut egui::Ui, color: egui::Color32) {
        let (rect, _) = ui.allocate_exact_size(egui::vec2(8.0, 8.0), egui::Sense::hover());
        ui.painter().circle_filled(rect.center(), 3.0, color);
    }

    fn show_settings(&mut self, ctx: &egui::Context) {
        let mut open = self.show_settings;
        egui::Window::new("设置")
            .open(&mut open)
            .resizable(false)
            .show(ctx, |ui| {
                ui.label("跟随 Agent（仅一个）");
                let before = self.config.clone();
                for agent in [
                    Agent::ZCode,
                    Agent::Codex,
                    Agent::OpenCode,
                    Agent::ClaudeCode,
                ] {
                    ui.radio_value(
                        &mut self.config.selected_agent,
                        agent,
                        Self::agent_label(agent),
                    );
                }
                if self.config.selected_agent != before.selected_agent {
                    if self.save_config() {
                        self.restart_watch();
                    } else {
                        self.config = before;
                    }
                }

                ui.separator();
                ui.label("固定项目（留空即跟随最新项目）");
                ui.text_edit_singleline(&mut self.project_input);
                if ui.button("保存项目范围").clicked() {
                    let before = self.config.clone();
                    self.config.pinned_project = if self.project_input.trim().is_empty() {
                        None
                    } else {
                        match normalize_project(std::path::Path::new(self.project_input.trim())) {
                            Ok(path) => Some(path),
                            Err(error) => {
                                self.status = format!("项目路径无效：{error}");
                                return;
                            }
                        }
                    };
                    if self.save_config() {
                        self.restart_watch();
                    } else {
                        self.config = before;
                    }
                }

                ui.separator();
                ui.add_enabled_ui(false, |ui| {
                    ui.checkbox(&mut self.config.autostart, "开机启动（安装包阶段接入）");
                });
                ui.small("仅离线读取 Agent 本地数据；不会上传遥测或对话正文。");
            });
        self.show_settings = open;
    }
}

impl eframe::App for TokenSpeedApp {
    fn update(&mut self, ctx: &egui::Context, _: &mut eframe::Frame) {
        self.drain_updates();
        ctx.request_repaint_after(Duration::from_millis(250));

        const TEXT: egui::Color32 = egui::Color32::from_rgb(23, 23, 23);
        const MUTED: egui::Color32 = egui::Color32::from_rgb(119, 115, 108);
        const GREEN: egui::Color32 = egui::Color32::from_rgb(46, 125, 91);
        const AMBER: egui::Color32 = egui::Color32::from_rgb(155, 103, 32);

        egui::CentralPanel::default().show(ctx, |ui| {
            ui.horizontal(|ui| {
                let has_data = self.report.is_some();
                Self::status_dot(ui, if has_data { GREEN } else { MUTED });
                ui.label(
                    egui::RichText::new(Self::agent_label(self.config.selected_agent))
                        .size(14.0)
                        .strong(),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui
                        .add(egui::Button::new(egui::RichText::new("设置").size(12.0)).frame(false))
                        .clicked()
                    {
                        self.show_settings = true;
                    }
                });
            });

            ui.add_space(10.0);
            let turn = self.report.as_ref().and_then(|report| report.turns.first());
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new(turn.map_or_else(
                        || "—".into(),
                        |value| format!("{:.3}", value.effective_speed),
                    ))
                    .size(40.0)
                    .family(egui::FontFamily::Monospace)
                    .strong()
                    .color(TEXT),
                );
                ui.label(egui::RichText::new("tok/s").size(16.0).color(MUTED));
                if let Some(turn) = turn {
                    ui.label(
                        egui::RichText::new(Self::accuracy_label(turn.accuracy))
                            .size(12.0)
                            .color(GREEN),
                    );
                }
            });

            if self.report.as_ref().is_some_and(|report| report.running) {
                ui.horizontal(|ui| {
                    Self::status_dot(ui, AMBER);
                    ui.small("生成中 · 保留上一完整轮结果");
                });
            }

            ui.add_space(8.0);
            ui.separator();
            if let Some(report) = &self.report {
                egui::Grid::new("summary")
                    .num_columns(2)
                    .spacing(egui::vec2(18.0, 5.0))
                    .show(ui, |ui| {
                        ui.label(egui::RichText::new("模型").size(11.0).color(MUTED));
                        ui.label(
                            egui::RichText::new(
                                report
                                    .turns
                                    .first()
                                    .and_then(|turn| turn.model.as_deref())
                                    .unwrap_or("未提供"),
                            )
                            .size(12.0),
                        );
                        ui.end_row();

                        ui.label(egui::RichText::new("最近一轮").size(11.0).color(MUTED));
                        ui.label(
                            egui::RichText::new(report.turns.first().map_or_else(
                                || "—".into(),
                                |turn| {
                                    format!(
                                        "{} tok · {:.3} tok/s",
                                        turn.output_tokens, turn.effective_speed
                                    )
                                },
                            ))
                            .size(12.0)
                            .family(egui::FontFamily::Monospace),
                        );
                        ui.end_row();

                        ui.label(egui::RichText::new("会话平均").size(11.0).color(MUTED));
                        ui.label(
                            egui::RichText::new(format_session_average(
                                report.session_total_tokens,
                                report.session_total_elapsed_ms,
                                report.session_accuracy,
                            ))
                            .size(12.0)
                            .family(egui::FontFamily::Monospace),
                        );
                        ui.end_row();
                    });

                egui::CollapsingHeader::new(egui::RichText::new("最近 10 轮").size(12.0))
                    .default_open(false)
                    .show(ui, |ui| {
                        egui::ScrollArea::vertical()
                            .max_height(96.0)
                            .show(ui, |ui| {
                                for turn in &report.turns {
                                    ui.horizontal(|ui| {
                                        ui.label(
                                            egui::RichText::new(&turn.turn_id)
                                                .size(11.0)
                                                .color(MUTED),
                                        );
                                        ui.with_layout(
                                            egui::Layout::right_to_left(egui::Align::Center),
                                            |ui| {
                                                ui.label(
                                                    egui::RichText::new(format!(
                                                        "{} tok · {:.3} tok/s · {}",
                                                        turn.output_tokens,
                                                        turn.effective_speed,
                                                        Self::accuracy_label(turn.accuracy)
                                                    ))
                                                    .size(11.0)
                                                    .family(egui::FontFamily::Monospace),
                                                );
                                            },
                                        );
                                    });
                                    ui.separator();
                                }
                            });
                    });
            }

            ui.add_space(4.0);
            ui.separator();
            ui.label(egui::RichText::new(&self.status).size(11.0).color(MUTED));
        });
        if self.show_settings {
            self.show_settings(ctx);
        }
    }
}
