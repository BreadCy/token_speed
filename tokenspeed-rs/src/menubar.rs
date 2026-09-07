//! 系统托盘（macOS 菜单栏 / Windows 通知区）：
//! - 左键单击托盘图标 = 唤起主界面
//! - 右键菜单 = 显示主界面 / 收起为悬浮球 / 退出
//!
//! 事件通过 tray-icon 的全局 channel 轮询获取，由 UI 线程在每帧消费。

use tray_icon::menu::{Menu, MenuEvent, MenuItem};
use tray_icon::{TrayIcon, TrayIconBuilder, TrayIconEvent};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayCommand {
    ShowMain,
    Collapse,
    Quit,
}

pub struct Menubar {
    // TrayIcon drop 时托盘图标随之移除，必须持有到底
    _tray: TrayIcon,
}

impl Menubar {
    pub fn new() -> Result<Self, String> {
        let menu = Menu::new();
        let show = MenuItem::with_id("show-main", "显示主界面", true, None);
        let collapse = MenuItem::with_id("collapse", "收起为悬浮球", true, None);
        let quit = MenuItem::with_id("quit", "退出", true, None);
        menu.append_items(&[&show, &collapse, &quit])
            .map_err(|error| error.to_string())?;

        let tray = TrayIconBuilder::new()
            .with_id("tokenspeed-tray")
            .with_menu(Box::new(menu))
            // 左键留给「唤起主界面」，菜单只在右键弹出
            .with_menu_on_left_click(false)
            .with_tooltip("TokenSpeed")
            .with_icon(icon_rgba()?)
            .build()
            .map_err(|error| error.to_string())?;
        Ok(Self { _tray: tray })
    }

    /// 消费一次托盘事件；一帧内多余的事件留到后续帧处理
    pub fn poll(&self) -> Option<TrayCommand> {
        while let Ok(event) = TrayIconEvent::receiver().try_recv() {
            if let TrayIconEvent::Click {
                button: tray_icon::MouseButton::Left,
                button_state: tray_icon::MouseButtonState::Up,
                ..
            } = event
            {
                return Some(TrayCommand::ShowMain);
            }
        }
        while let Ok(event) = MenuEvent::receiver().try_recv() {
            match event.id().as_ref() {
                "show-main" => return Some(TrayCommand::ShowMain),
                "collapse" => return Some(TrayCommand::Collapse),
                "quit" => return Some(TrayCommand::Quit),
                _ => {}
            }
        }
        None
    }
}

/// 程序化生成 32×32 图标：深色圆底 + 绿色内点，无需资源文件
fn icon_rgba() -> Result<tray_icon::Icon, String> {
    let size = 32u32;
    let center = (size as f32 - 1.0) / 2.0;
    let mut rgba = Vec::with_capacity((size * size * 4) as usize);
    for y in 0..size {
        for x in 0..size {
            let dx = x as f32 - center;
            let dy = y as f32 - center;
            let dist = (dx * dx + dy * dy).sqrt();
            let (r, g, b, a) = if dist <= 6.0 {
                (127, 185, 138, 255)
            } else if dist <= 15.0 {
                (29, 30, 32, 255)
            } else {
                (0, 0, 0, 0)
            };
            rgba.extend_from_slice(&[r, g, b, a]);
        }
    }
    tray_icon::Icon::from_rgba(rgba, size, size).map_err(|error| error.to_string())
}
