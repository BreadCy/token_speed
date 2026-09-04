//! 托盘图标与开机自启：
//! - 图标为程序内绘制的"速度柱状图"（深色底 + 三根递增柱），颜色跟随当前工具
//! - 右键/左键弹出菜单：切换工具、开机自启（HKCU Run 键）、退出
//! - `--autostart=on|off` 可在命令行直接设置自启，便于脚本化

use std::ffi::c_void;
use std::ptr;
use windows_sys::Win32::Foundation::{HWND, RECT};
use windows_sys::Win32::Graphics::Gdi::{
    CreateBitmap, CreateCompatibleBitmap, CreateCompatibleDC, CreateSolidBrush, DeleteDC, DeleteObject,
    FillRect, GetDC, ReleaseDC, SelectObject,
};
use windows_sys::Win32::System::Registry::{
    RegDeleteKeyValueW, RegGetValueW, RegSetKeyValueW, HKEY_CURRENT_USER, REG_SZ, RRF_RT_REG_SZ,
};
use windows_sys::Win32::UI::Shell::{Shell_NotifyIconW, NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NIM_MODIFY, NOTIFYICONDATAW};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateIconIndirect, DestroyIcon, HICON, ICONINFO, WM_APP,
};

pub const WM_TRAY: u32 = WM_APP + 1;
const TRAY_ID: u32 = 1;

pub struct Tray {
    pub nid: NOTIFYICONDATAW,
    pub icon: HICON,
}

fn widens(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// 画一枚 32x32 图标：深色底 + 三根递增柱（tokenspeed 的"速度图"）
unsafe fn make_icon(color: u32) -> HICON {
    let wdc = GetDC(ptr::null_mut());
    let mem = CreateCompatibleDC(wdc);
    let bmp = CreateCompatibleBitmap(wdc, 32, 32);
    let old_bmp = SelectObject(mem, bmp);

    let bg = CreateSolidBrush(0x001a1a1a); // 与悬浮条同底色
    let rc = RECT { left: 0, top: 0, right: 32, bottom: 32 };
    FillRect(mem, &rc, bg);
    DeleteObject(bg);

    let br = CreateSolidBrush(color);
    for (x, h) in [(6usize, 8i32), (14, 14), (22, 20)] {
        let rc = RECT { left: x as i32, top: 26 - h, right: x as i32 + 4, bottom: 26 };
        FillRect(mem, &rc, br);
    }
    DeleteObject(br);

    SelectObject(mem, old_bmp);
    DeleteDC(mem);
    ReleaseDC(ptr::null_mut(), wdc);

    let mask = CreateBitmap(32, 32, 1, 1, ptr::null());
    let ii = ICONINFO { fIcon: 1, xHotspot: 0, yHotspot: 0, hbmMask: mask, hbmColor: bmp };
    let icon = CreateIconIndirect(&ii);
    DeleteObject(mask);
    DeleteObject(bmp);
    icon
}

/// 注册托盘图标；失败返回 None（不影响悬浮条本体）
pub unsafe fn add(hwnd: HWND, color: u32) -> Option<Tray> {
    let icon = make_icon(color);
    let mut nid: NOTIFYICONDATAW = std::mem::zeroed();
    nid.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
    nid.hWnd = hwnd;
    nid.uID = TRAY_ID;
    nid.uFlags = NIF_MESSAGE | NIF_ICON | NIF_TIP;
    nid.uCallbackMessage = WM_TRAY;
    nid.hIcon = icon;
    let tip = widens("tokenspeed · 模型生成速度");
    nid.szTip[..tip.len()].copy_from_slice(&tip[..tip.len().min(127)]);
    if Shell_NotifyIconW(NIM_ADD, &nid) == 0 {
        DestroyIcon(icon);
        return None;
    }
    Some(Tray { nid, icon })
}

/// 换色（当前工具变化时）
pub unsafe fn update_icon(tray: &mut Tray, color: u32) {
    let new_icon = make_icon(color);
    tray.nid.hIcon = new_icon;
    Shell_NotifyIconW(NIM_MODIFY, &tray.nid);
    DestroyIcon(tray.icon);
    tray.icon = new_icon;
}

pub unsafe fn remove(tray: Tray) {
    Shell_NotifyIconW(NIM_DELETE, &tray.nid);
    DestroyIcon(tray.icon);
}

const RUN_KEY: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Run";
const VALUE_NAME: &str = "tokenspeed";

pub unsafe fn autostart_enabled() -> bool {
    let mut buf = [0u16; 320];
    let mut cb = (buf.len() * 2) as u32;
    RegGetValueW(
        HKEY_CURRENT_USER,
        widens(RUN_KEY).as_ptr(),
        widens(VALUE_NAME).as_ptr(),
        RRF_RT_REG_SZ,
        ptr::null_mut(),
        buf.as_mut_ptr() as *mut c_void,
        &mut cb,
    ) == 0
}

pub unsafe fn autostart_set(on: bool) -> bool {
    if on {
        let Ok(exe) = std::env::current_exe() else { return false };
        let v = widens(&format!("\"{}\" --float", exe.display()));
        RegSetKeyValueW(
            HKEY_CURRENT_USER,
            widens(RUN_KEY).as_ptr(),
            widens(VALUE_NAME).as_ptr(),
            REG_SZ,
            v.as_ptr() as *const c_void,
            (v.len() * 2) as u32,
        ) == 0
    } else {
        RegDeleteKeyValueW(HKEY_CURRENT_USER, widens(RUN_KEY).as_ptr(), widens(VALUE_NAME).as_ptr()) == 0
    }
}

/// 命令行 `--autostart=on|off`：设置后退出
pub fn handle_cli_autostart(v: &str) -> bool {
    match v.to_ascii_lowercase().as_str() {
        "on" | "true" | "1" => unsafe { autostart_set(true) },
        "off" | "false" | "0" => unsafe { autostart_set(false) },
        _ => return false,
    }
}
