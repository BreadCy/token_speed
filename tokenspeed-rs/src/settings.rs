//! 数据路径设置：
//! - 托盘菜单“设置…”打开设置窗口，可修改四个工具的数据位置
//! - 配置存于注册表 HKCU\Software\tokenspeed；每次读取时实时解析，
//!   留空/删除键值即回退默认路径，保存后下一秒刷新立即生效
//! - `--db` 命令行参数仍优先于这里的 ZCode 数据库设置

use std::ffi::c_void;
use std::path::PathBuf;
use std::ptr;
use std::sync::Mutex;

use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, SIZE, WPARAM};
use windows_sys::Win32::Graphics::Dwm::DwmSetWindowAttribute;
use windows_sys::Win32::Graphics::Gdi::{
    BeginPaint, CreateFontW, CreatePen, CreateRoundRectRgn, CreateSolidBrush, DeleteObject,
    DrawTextW, Ellipse, FillRect, GetStockObject, GetTextExtentPoint32W, InvalidateRect,
    MapWindowPoints, PtInRect, RoundRect, ScreenToClient, SelectObject, SetBkColor, SetBkMode,
    SetTextColor, EndPaint, TextOutW, SetWindowRgn, HDC, HBRUSH, PAINTSTRUCT, NULL_BRUSH,
    TRANSPARENT, DT_CENTER, DT_SINGLELINE, DT_VCENTER, PS_SOLID,
};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::Registry::{
    RegDeleteKeyValueW, RegGetValueW, RegSetKeyValueW, HKEY_CURRENT_USER, REG_SZ, RRF_RT_REG_SZ,
};
use windows_sys::Win32::UI::Controls::{
    ODS_FOCUS, ODS_SELECTED, DRAWITEMSTRUCT, WM_MOUSELEAVE,
};
use windows_sys::Win32::UI::Controls::Dialogs::{
    GetOpenFileNameW, OFN_FILEMUSTEXIST, OFN_HIDEREADONLY, OFN_NOCHANGEDIR, OFN_PATHMUSTEXIST,
    OPENFILENAMEW,
};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    EnableWindow, SetFocus, TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT,
};
use windows_sys::Win32::UI::Shell::{
    ILFree, SHBrowseForFolderW, SHGetPathFromIDListW, BFFM_INITIALIZED, BFFM_SETSELECTIONW,
    BIF_EDITBOX, BIF_NEWDIALOGSTYLE, BIF_RETURNONLYFSDIRS, BROWSEINFOW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::*;
#[derive(Clone)]
pub struct Paths {
    pub zcode_db: PathBuf,
    pub codex_dir: PathBuf,
    pub opencode_db: PathBuf,
    pub claude_dir: PathBuf,
}

fn home() -> PathBuf {
    PathBuf::from(std::env::var("USERPROFILE").unwrap_or_else(|_| ".".into()))
}

pub fn default_zcode_db() -> PathBuf {
    home().join(".zcode").join("cli").join("db").join("db.sqlite")
}
pub fn default_codex_dir() -> PathBuf {
    home().join(".codex").join("sessions")
}
pub fn default_opencode_db() -> PathBuf {
    home().join(".local").join("share").join("opencode").join("opencode.db")
}
pub fn default_claude_dir() -> PathBuf {
    home().join(".claude").join("projects")
}

// ---------- 注册表存取 ----------

const REG_KEY: &str = "Software\\tokenspeed";
const VAL_ZC: &str = "zcode_db";
const VAL_CX: &str = "codex_dir";
const VAL_OC: &str = "opencode_db";
const VAL_CC: &str = "claude_dir";

fn widens(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// 读取一个路径键值；不存在返回 None（= 使用默认路径）；空串返回 Some("")
unsafe fn reg_get_raw(name: &str) -> Option<String> {
    let mut buf = [0u16; 1024];
    let mut cb = (buf.len() * 2) as u32;
    let rc = RegGetValueW(
        HKEY_CURRENT_USER,
        widens(REG_KEY).as_ptr(),
        widens(name).as_ptr(),
        RRF_RT_REG_SZ,
        ptr::null_mut(),
        buf.as_mut_ptr() as *mut c_void,
        &mut cb,
    );
    if rc != 0 {
        return None;
    }
    let mut len = (cb as usize / 2).min(buf.len());
    while len > 0 && buf[len - 1] == 0 {
        len -= 1;
    }
    Some(String::from_utf16_lossy(&buf[..len]))
}

/// 读取一个路径键值；不存在/为空返回 None（= 使用默认路径）
unsafe fn reg_get(name: &str) -> Option<String> {
    reg_get_raw(name).filter(|s| !s.trim().is_empty())
}

unsafe fn reg_del(name: &str) {
    RegDeleteKeyValueW(HKEY_CURRENT_USER, widens(REG_KEY).as_ptr(), widens(name).as_ptr());
}

unsafe fn reg_set(name: &str, v: &str) {
    if v.trim().is_empty() {
        reg_del(name); // 留空 = 恢复默认
    } else {
        let w = widens(v);
        RegSetKeyValueW(
            HKEY_CURRENT_USER,
            widens(REG_KEY).as_ptr(),
            widens(name).as_ptr(),
            REG_SZ,
            w.as_ptr() as *const c_void,
            (w.len() * 2) as u32,
        );
    }
}

/// 当前生效的四个路径（注册表 → 默认值），每次实时读，改完立即生效
pub fn get() -> Paths {
    unsafe {
        let g = |name: &str, dflt: PathBuf| -> PathBuf { reg_get(name).map(PathBuf::from).unwrap_or(dflt) };
        Paths {
            zcode_db: g(VAL_ZC, default_zcode_db()),
            codex_dir: g(VAL_CX, default_codex_dir()),
            opencode_db: g(VAL_OC, default_opencode_db()),
            claude_dir: g(VAL_CC, default_claude_dir()),
        }
    }
}

// ---------- 本机 Code Agent 检测 ----------

use super::Src;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

const VAL_AGENTS: &str = "agents"; // 启用工具的有序列表，逗号分隔 tag；缺失 = 未检测，空串 = 用户清空

/// 每个工具的候选路径：环境变量优先，回退用户主目录固定位置。
/// 不同电脑安装位置不同（还可能设了 CODEX_HOME / XDG_DATA_HOME 等变量），逐个探测。
fn candidates(t: Src) -> Vec<PathBuf> {
    let home = home();
    let env = |k: &str| {
        std::env::var(k).ok().filter(|v| !v.trim().is_empty()).map(PathBuf::from)
    };
    let mut v = Vec::new();
    match t {
        Src::Zcode => {
            if let Some(h) = env("ZCODE_HOME") {
                v.push(h.join("cli").join("db").join("db.sqlite"));
            }
            v.push(home.join(".zcode").join("cli").join("db").join("db.sqlite"));
        }
        Src::Codex => {
            if let Some(h) = env("CODEX_HOME") {
                v.push(h.join("sessions"));
            }
            v.push(home.join(".codex").join("sessions"));
        }
        Src::Opencode => {
            if let Some(x) = env("XDG_DATA_HOME") {
                v.push(x.join("opencode").join("opencode.db"));
            }
            v.push(home.join(".local").join("share").join("opencode").join("opencode.db"));
        }
        Src::Claude => {
            if let Some(x) = env("CLAUDE_CONFIG_DIR") {
                v.push(x.join("projects"));
            }
            v.push(home.join(".claude").join("projects"));
        }
    }
    v
}

#[derive(PartialEq, Clone, Copy)]
enum Status {
    Ok,
    Missing,
    BadDb,
}

fn is_db_file(p: &Path) -> bool {
    std::fs::File::open(p)
        .and_then(|mut f| {
            use std::io::Read;
            let mut h = [0u8; 16];
            f.read_exact(&mut h)?;
            Ok(h == *b"SQLite format 3\0")
        })
        .unwrap_or(false)
}

/// 路径是否“正确”：目录工具要求目录存在；数据库工具要求文件存在且为 SQLite
fn check(t: Src, p: &str) -> Status {
    let path = Path::new(p);
    let is_db = matches!(t, Src::Zcode | Src::Opencode);
    if is_db {
        if !path.is_file() {
            Status::Missing
        } else if is_db_file(path) {
            Status::Ok
        } else {
            Status::BadDb
        }
    } else if path.is_dir() {
        Status::Ok
    } else {
        Status::Missing
    }
}

/// 检测本机已装的 Code Agent：按固定顺序，返回首个有效候选路径
fn detect_all() -> Vec<(Src, String)> {
    let mut out = Vec::new();
    for t in [Src::Zcode, Src::Codex, Src::Opencode, Src::Claude] {
        for c in candidates(t) {
            if check(t, &c.to_string_lossy()) == Status::Ok {
                out.push((t, c.to_string_lossy().into_owned()));
                break;
            }
        }
    }
    out
}

/// 工具的展示名 / 行副标签
fn tool_hint(t: Src) -> &'static str {
    match t {
        Src::Zcode => "db.sqlite",
        Src::Codex => "sessions",
        Src::Opencode => "opencode.db",
        Src::Claude => "projects",
    }
}

static ORDER_CACHE: Mutex<Option<Vec<Src>>> = Mutex::new(None);

/// 收集时工具的显示顺序 = 注册表 agents 列表；缺失（从未进过设置）时用检测结果。
/// 结果缓存到下次保存列表时（order 每秒都会被采集调用）。
pub fn order() -> Vec<Src> {
    let mut c = ORDER_CACHE.lock().unwrap();
    if c.is_none() {
        *c = Some(unsafe {
            match reg_get_raw(VAL_AGENTS) {
                Some(s) if !s.trim().is_empty() => {
                    s.split(',').filter_map(|x| Src::parse(x.trim())).collect()
                }
                Some(_) => Vec::new(), // 空串 = 用户清空了列表
                None => detect_all().iter().map(|(t, _)| *t).collect(),
            }
        });
    }
    c.clone().unwrap_or_default()
}

fn order_cache_reset() {
    *ORDER_CACHE.lock().unwrap() = None;
}

// ---------- 设置窗口（深色主题，与悬浮条同一套配色） ----------

// 行按“槽位”组织（最多 4 行）：槽位 i 的控件 id = 基址 + i，槽位 ↔ 工具由 MODEL 决定
const ED0: usize = 100; // 输入框 100..103
const BR0: usize = 110; // 浏览… 110..113
const UP0: usize = 150; // 上移 150..153
const DN0: usize = 160; // 下移 160..163
const DEL0: usize = 170; // 删除 170..173
const LBL0: usize = 400; // 行名 400..403
const SUB0: usize = 410; // 行副标签 410..413
const ID_RESET: usize = 104; // 恢复默认（检测后才出现）
const ID_DETECT: usize = 105; // 检测 / 重新检测
const ID_SAVE: usize = 1; // = IDOK（Enter）
const ID_CANCEL: usize = 2; // = IDCANCEL（Esc）
const ID_CLOSE: usize = 120; // 右上角 ×
const IDC_HINT: usize = 220; // 副标题/行副标签（暗灰）
const IDC_WARN: usize = 230; // 路径校验警告行（琥珀色）
const EM_SETMARGINS: u32 = 0x00D3; // 输入框文字左右留白

// 颜色 (COLORREF = 0x00BBGGRR)
const BG_WIN: u32 = 0x00202024; // 窗口底
const BG_EDIT: u32 = 0x002e2e36; // 输入框底
const TXT_MAIN: u32 = 0x00f0f0f2; // 主文字
const TXT_DIM: u32 = 0x009898a0; // 次要文字
const SEP: u32 = 0x0028282e; // 头部与列表间的细分隔线
const BTN_BG: u32 = 0x002a2a31; // 普通按钮底
const BTN_HV: u32 = 0x0034343d; // 悬停
const BTN_PR: u32 = 0x003f3f4a; // 按下
const BTN_BR: u32 = 0x0042424c; // 按钮描边
const SAVE_BG: u32 = 0x0080de4a; // 保存按钮 = 工具绿
const SAVE_HV: u32 = 0x0090ea5c;
const SAVE_PR: u32 = 0x0068cc38;
const SAVE_TX: u32 = 0x00141a10; // 绿底上用近黑文字
const TXT_BAD: u32 = 0x007171f8; // 路径无效警示（红，与悬浮条"生成中"同色）

fn tool_color(i: usize) -> u32 {
    match i {
        0 => 0x0080de4a, // ZCode 绿
        1 => 0x003c92fb, // Codex 橙
        2 => 0x00faa560, // OpenCode 蓝
        _ => 0x00fc84c0, // Claude 紫
    }
}

const CLS_STATIC: &[u16] = &[83, 84, 65, 84, 73, 67, 0]; // "STATIC\0"
const CLS_EDIT: &[u16] = &[69, 68, 73, 84, 0]; // "EDIT\0"
const CLS_BUTTON: &[u16] = &[66, 85, 84, 84, 79, 78, 0]; // "BUTTON\0"

const DM_GETDEFID: u32 = 0x0400; // 让 Enter 触发默认按钮（保存）
const DC_HASDEFID: isize = 0x534B;

/// 发这个消息给悬浮条窗口可直接打开设置窗口（托盘菜单"设置…"内部也走这里）
pub const WM_OPEN: u32 = WM_APP + 2;

static DLG_HWND: Mutex<isize> = Mutex::new(0);

// 字体档位：标题 / 副标题 / 行副标签 / 正文(输入框·行名) / 按钮(半粗)
const F_TITLE: usize = 0;
const F_SUB: usize = 1;
const F_MICRO: usize = 2;
const F_BODY: usize = 3;
const F_BTN: usize = 4;
static FONTS: Mutex<[isize; 5]> = Mutex::new([0; 5]);

unsafe fn font(idx: usize) -> isize {
    let mut f = FONTS.lock().unwrap();
    if f[idx] == 0 {
        let (h, w) = match idx {
            F_TITLE => (-19, 600),
            F_SUB => (-13, 400),
            F_MICRO => (-11, 400),
            F_BTN => (-14, 600),
            _ => (-14, 400),
        };
        let face: Vec<u16> = "Segoe UI\0".encode_utf16().collect();
        f[idx] = CreateFontW(h, 0, 0, 0, w, 0, 0, 0, 0, 0, 0, 5, 0, face.as_ptr()) as isize;
    }
    f[idx]
}

unsafe fn set_ctl_font(h: HWND, idx: usize) {
    SendMessageW(h, WM_SETFONT, font(idx) as usize, 1);
}

// ctlcolor 返回的画刷由系统缓存，必须常驻；按颜色缓存 4 个槽位足够
static BRUSHES: Mutex<[(u32, isize); 4]> = Mutex::new([(0, 0); 4]);

unsafe fn ctl_brush(color: u32) -> isize {
    let mut b = BRUSHES.lock().unwrap();
    for s in b.iter_mut() {
        if s.0 == color && s.1 != 0 {
            return s.1;
        }
    }
    let fresh = CreateSolidBrush(color) as isize;
    for s in b.iter_mut() {
        if s.1 == 0 {
            *s = (color, fresh);
            return fresh;
        }
    }
    // 槽满（当前只用 3 种颜色），淘汰第一个
    DeleteObject(b[0].1 as *mut c_void);
    b[0] = (color, fresh);
    fresh
}

#[allow(clippy::too_many_arguments)]
unsafe fn create_ctl(
    parent: HWND,
    class: &[u16],
    text: &str,
    style: u32,
    ex: u32,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    id: usize,
) -> HWND {
    let t = widens(text);
    CreateWindowExW(
        ex,
        class.as_ptr(),
        t.as_ptr(),
        style,
        x,
        y,
        w,
        h,
        parent,
        id as HMENU,
        GetModuleHandleW(ptr::null()),
        ptr::null(),
    )
}

// ---------- 列表模型：槽位 ↔ (工具, 路径)，检测驱动 ----------

type Entry = (Src, String);
static MODEL: Mutex<Vec<Entry>> = Mutex::new(Vec::new());
static DETECTED: Mutex<Vec<Entry>> = Mutex::new(Vec::new()); // 最近一次「检测」的结果 = 可恢复的默认
static DETECTED_ONCE: AtomicBool = AtomicBool::new(false); // 按过「检测」后才显示「恢复默认」
static WARN_AS_HINT: AtomicBool = AtomicBool::new(false); // 警告行当前是空列表引导提示
static COLLAPSED: AtomicBool = AtomicBool::new(false); // 路径列表收起中（每次打开重置为展开）

fn slot_ids(i: usize) -> [usize; 7] {
    [ED0 + i, BR0 + i, UP0 + i, DN0 + i, DEL0 + i, LBL0 + i, SUB0 + i]
}

/// 打开窗口时载入列表：注册表有 agents 列表用之；否则用本机检测结果（首次安装场景）
unsafe fn load_model() {
    let mut m: Vec<Entry> = Vec::new();
    match reg_get_raw(VAL_AGENTS) {
        Some(s) if !s.trim().is_empty() => {
            let paths = get();
            for t in s.split(',').filter_map(|x| Src::parse(x.trim())) {
                let p = match t {
                    Src::Zcode => &paths.zcode_db,
                    Src::Codex => &paths.codex_dir,
                    Src::Opencode => &paths.opencode_db,
                    Src::Claude => &paths.claude_dir,
                };
                m.push((t, p.to_string_lossy().into_owned()));
            }
        }
        Some(_) => {} // 空串 = 用户上次清空了列表
        None => {
            for (t, p) in detect_all() {
                m.push((t, p));
            }
        }
    }
    *MODEL.lock().unwrap() = m;
}

/// 把模型刷到 7 组控件上；多余槽位隐藏；首末行禁用上/下移
const ROW_Y0: i32 = 104; // 首行 y（分隔线之下，留呼吸感）
const ROW_PITCH: i32 = 46; // 行距
const TOG_Y: i32 = 74; // 折叠标题行 y（副标题 66 之下、分隔线之上）

/// 折叠标题行点击热区（整行宽，含上下少量余量）
fn toggle_rc() -> RECT {
    RECT { left: 16, top: TOG_Y - 6, right: 584, bottom: TOG_Y + 20 }
}

/// 折叠标题行上画一段文字（左上对齐），返回结束 x
unsafe fn toggle_text(hdc: HDC, x: i32, s: &str, color: u32, f: usize) -> i32 {
    SelectObject(hdc, font(f) as *mut c_void);
    SetTextColor(hdc, color);
    let w: Vec<u16> = s.encode_utf16().collect();
    TextOutW(hdc, x, TOG_Y, w.as_ptr(), w.len() as i32);
    let mut sz = SIZE { cx: 0, cy: 0 };
    GetTextExtentPoint32W(hdc, w.as_ptr(), w.len() as i32, &mut sz);
    x + sz.cx
}

unsafe fn apply_model_to_ui(hwnd: HWND) {
    let m = MODEL.lock().unwrap().clone();
    let n = m.len();
    let collapsed = COLLAPSED.load(Ordering::Relaxed);
    for i in 0..4usize {
        let vis = i < n && !collapsed;
        for id in slot_ids(i) {
            ShowWindow(GetDlgItem(hwnd, id as i32), if vis { SW_SHOW } else { SW_HIDE });
        }
        if vis {
            let (t, ref path) = m[i];
            SetWindowTextW(GetDlgItem(hwnd, (LBL0 + i) as i32), widens(t.name()).as_ptr());
            SetWindowTextW(GetDlgItem(hwnd, (SUB0 + i) as i32), widens(tool_hint(t)).as_ptr());
            SetWindowTextW(GetDlgItem(hwnd, (ED0 + i) as i32), widens(path).as_ptr());
            EnableWindow(GetDlgItem(hwnd, (UP0 + i) as i32), (i > 0) as i32);
            EnableWindow(GetDlgItem(hwnd, (DN0 + i) as i32), (i + 1 < n) as i32);
        }
    }
    // 高度自适应：折叠 = 标题行 + 按钮区；展开 = 警告行与按钮区随行数移动
    let btn_y = if collapsed { TOG_Y + 36 } else { ROW_Y0 + n as i32 * ROW_PITCH + 4 + 22 };
    let warn_y = ROW_Y0 + n as i32 * ROW_PITCH + 4;
    MoveWindow(GetDlgItem(hwnd, IDC_WARN as i32), 24, warn_y, 552, 16, 1);
    for (id, x, w) in
        [(ID_RESET, 200i32, 96i32), (ID_DETECT, 304, 92), (ID_CANCEL, 404, 64), (ID_SAVE, 476, 100)]
    {
        MoveWindow(GetDlgItem(hwnd, id as i32), x, btn_y, w, 32, 1);
    }
    SetWindowPos(
        hwnd,
        ptr::null_mut(),
        0,
        0,
        600,
        btn_y + 32 + 14,
        SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE,
    );
    InvalidateRect(hwnd, ptr::null(), 0); // 圆点颜色随工具变化，需整窗重绘
    refresh_status(hwnd);
}

/// 逐行校验路径，无效行文字标红 + 底部汇总警告
unsafe fn refresh_status(hwnd: HWND) {
    let m = MODEL.lock().unwrap().clone();
    let mut probs: Vec<String> = Vec::new();
    for (i, (t, _)) in m.iter().enumerate() {
        let p = read_edit(hwnd, ED0 + i);
        match check(*t, &p) {
            Status::Ok => {}
            Status::Missing => probs.push(format!("{} 路径不存在", t.name())),
            Status::BadDb => probs.push(format!("{} 不是有效的 SQLite 数据库", t.name())),
        }
        InvalidateRect(GetDlgItem(hwnd, (ED0 + i) as i32), ptr::null(), 0);
    }
    let hw = GetDlgItem(hwnd, IDC_WARN as i32);
    if COLLAPSED.load(Ordering::Relaxed) {
        // 收起时警告/引导都随列表隐藏
        ShowWindow(hw, SW_HIDE);
        return;
    }
    if m.is_empty() {
        // 空列表：显示引导提示（暗灰色）
        WARN_AS_HINT.store(true, Ordering::Relaxed);
        SetWindowTextW(
            hw,
            widens("未检测到已安装的 Code Agent —— 点击「重新检测」添加").as_ptr(),
        );
        ShowWindow(hw, SW_SHOW);
    } else if probs.is_empty() {
        WARN_AS_HINT.store(false, Ordering::Relaxed);
        ShowWindow(hw, SW_HIDE);
    } else {
        WARN_AS_HINT.store(false, Ordering::Relaxed);
        SetWindowTextW(hw, widens(&format!("⚠ {}", probs.join("  ·  "))).as_ptr());
        ShowWindow(hw, SW_SHOW);
    }
}

/// 保存：列表写 agents，路径逐行写各自键值（留空 = 回默认）
unsafe fn save_model(hwnd: HWND) {
    let m = MODEL.lock().unwrap().clone();
    let tags: Vec<&str> = m.iter().map(|(t, _)| t.tag()).collect();
    reg_set(VAL_AGENTS, &tags.join(","));
    order_cache_reset();
    for (i, (t, _)) in m.iter().enumerate() {
        let p = read_edit(hwnd, ED0 + i);
        let name = match t {
            Src::Zcode => VAL_ZC,
            Src::Codex => VAL_CX,
            Src::Opencode => VAL_OC,
            Src::Claude => VAL_CC,
        };
        reg_set(name, &p);
    }
}

/// 「检测 / 重新检测」：探测本机候选路径，更新已有行路径，补回被删除的工具行
unsafe fn do_detect(hwnd: HWND) {
    let det = detect_all();
    {
        let mut m = MODEL.lock().unwrap();
        for (t, p) in &det {
            match m.iter_mut().find(|(mt, _)| mt == t) {
                Some(e) => e.1 = p.clone(),
                None => m.push((*t, p.clone())),
            }
        }
        *DETECTED.lock().unwrap() = det;
    }
    DETECTED_ONCE.store(true, Ordering::Relaxed);
    apply_model_to_ui(hwnd);
    ShowWindow(GetDlgItem(hwnd, ID_RESET as i32), SW_SHOW);
    InvalidateRect(GetDlgItem(hwnd, ID_DETECT as i32), ptr::null(), 0); // 换标签
}

/// 「恢复默认」= 恢复到最近一次检测到的路径（未检测到的工具回退候选默认）
unsafe fn restore_default(hwnd: HWND) {
    let det = DETECTED.lock().unwrap().clone();
    let mut m = MODEL.lock().unwrap().clone();
    for (t, p) in m.iter_mut() {
        if let Some((_, dp)) = det.iter().find(|(dt, _)| dt == t) {
            *p = dp.clone();
        } else if let Some(c) = candidates(*t).first() {
            *p = c.to_string_lossy().into_owned();
        }
    }
    *MODEL.lock().unwrap() = m;
    apply_model_to_ui(hwnd);
}

unsafe fn move_row(hwnd: HWND, slot: usize, dir: i32) {
    let mut m = MODEL.lock().unwrap();
    let j = slot as i32 + dir;
    if j < 0 || j as usize >= m.len() {
        return;
    }
    m.swap(slot, j as usize);
    drop(m);
    apply_model_to_ui(hwnd);
}

unsafe fn delete_row(hwnd: HWND, slot: usize) {
    MODEL.lock().unwrap().remove(slot);
    apply_model_to_ui(hwnd);
}

unsafe fn read_ctl_text(h: HWND) -> String {
    let len = SendMessageW(h, WM_GETTEXTLENGTH, 0, 0) as usize;
    let mut buf = vec![0u16; len + 1];
    SendMessageW(h, WM_GETTEXT, len + 1, buf.as_mut_ptr() as isize);
    buf.truncate(len);
    String::from_utf16_lossy(&buf).trim().to_string()
}

unsafe fn read_edit(hwnd: HWND, id: usize) -> String {
    read_ctl_text(GetDlgItem(hwnd, id as i32))
}

/// 文件选择对话框（GetOpenFileNameW），初始目录取自 current 的父目录
unsafe fn browse_file(parent: HWND, title: &str, current: &str) -> Option<String> {
    let mut buf = [0u16; 1024];
    // comdlg 校验严格（FNERR_INVALIDFILENAME），混合斜杠会被拒，统一成反斜杠
    let init: Vec<u16> = current.replace('/', "\\").encode_utf16().collect();
    let n = init.len().min(buf.len() - 1);
    buf[..n].copy_from_slice(&init[..n]);
    let title_w = widens(title);
    let filter: Vec<u16> =
        "数据库文件\0*.db;*.sqlite;*.sqlite3;*.db3\0所有文件\0*.*\0\0".encode_utf16().collect();
    let mut ofn: OPENFILENAMEW = std::mem::zeroed();
    ofn.lStructSize = std::mem::size_of::<OPENFILENAMEW>() as u32;
    ofn.hwndOwner = parent;
    ofn.lpstrFilter = filter.as_ptr();
    ofn.lpstrFile = buf.as_mut_ptr();
    ofn.nMaxFile = buf.len() as u32;
    ofn.lpstrTitle = title_w.as_ptr();
    ofn.Flags = OFN_FILEMUSTEXIST | OFN_PATHMUSTEXIST | OFN_HIDEREADONLY | OFN_NOCHANGEDIR;
    if GetOpenFileNameW(&mut ofn) == 0 {
        return None; // 用户取消或失败；需 STA COM（main 里已 CoInitializeEx），否则会死锁
    }
    let len = buf.iter().position(|&c| c == 0).unwrap_or(0);
    let s = String::from_utf16_lossy(&buf[..len]);
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

/// 目录选择对话框（SHBrowseForFolder），初始定位到 current
unsafe fn browse_dir(parent: HWND, title: &str, current: &str) -> Option<String> {
    let title_w = widens(title);
    let init_w = if current.trim().is_empty() {
        None
    } else {
        Some(widens(current))
    };
    let mut display = [0u16; 260];
    let mut bi: BROWSEINFOW = std::mem::zeroed();
    bi.hwndOwner = parent;
    bi.pszDisplayName = display.as_mut_ptr();
    bi.lpszTitle = title_w.as_ptr();
    bi.ulFlags = BIF_RETURNONLYFSDIRS | BIF_NEWDIALOGSTYLE | BIF_EDITBOX;
    bi.lpfn = Some(browse_cb);
    // 缓冲必须活到对话框关闭（模态返回后才释放），lParam 才是有效指针
    bi.lParam = match &init_w {
        Some(w) => w.as_ptr() as isize,
        None => 0,
    };
    let pidl = SHBrowseForFolderW(&bi);
    if pidl.is_null() {
        return None;
    }
    let mut buf = [0u16; 260];
    let ok = SHGetPathFromIDListW(pidl, buf.as_mut_ptr());
    ILFree(pidl);
    if ok == 0 {
        return None;
    }
    let len = buf.iter().position(|&c| c == 0).unwrap_or(0);
    let s = String::from_utf16_lossy(&buf[..len]);
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

unsafe extern "system" fn browse_cb(hwnd: HWND, msg: u32, lp: LPARAM, _data: LPARAM) -> i32 {
    // BIF_NEWDIALOGSTYLE 下 SendMessage 会被对话框线程吞掉，须用 PostMessage 异步定位
    if msg == BFFM_INITIALIZED && lp != 0 {
        PostMessageW(hwnd, BFFM_SETSELECTIONW, 1, lp); // TRUE = 路径字符串
    }
    0
}

/// 某一行点"浏览…"：按该行工具类型弹文件/目录选择器，选中后填回输入框（不自动保存）
unsafe fn browse_slot(hwnd: HWND, slot: usize) {
    let tool = MODEL.lock().unwrap().get(slot).map(|(t, _)| *t);
    let Some(t) = tool else { return };
    let is_db = matches!(t, Src::Zcode | Src::Opencode);
    let cur = read_edit(hwnd, ED0 + slot);
    let title = format!("选择 {} 数据路径", t.name());
    let picked = if is_db {
        browse_file(hwnd, &title, &cur)
    } else {
        browse_dir(hwnd, &title, &cur)
    };
    if let Some(p) = picked {
        let w = widens(&p);
        SetWindowTextW(GetDlgItem(hwnd, (ED0 + slot) as i32), w.as_ptr());
        refresh_status(hwnd);
    }
}

// ---------- 自绘按钮（扁平圆角，悬停/按下换色，保存键工具绿） ----------

/// 所有自绘按钮 id（底部 4 个 + 每行 浏览/上移/下移/删除 ×4 槽位）
fn all_buttons() -> Vec<usize> {
    let mut v = vec![ID_RESET, ID_DETECT, ID_SAVE, ID_CANCEL, ID_CLOSE];
    for i in 0..4usize {
        v.extend_from_slice(&[BR0 + i, UP0 + i, DN0 + i, DEL0 + i]);
    }
    v
}
static HOVER: Mutex<usize> = Mutex::new(0);
static TRACKING: Mutex<bool> = Mutex::new(false);
static FOCUS_EDIT: Mutex<usize> = Mutex::new(0); // 聚焦中的输入框 id（画亮边框）

fn btn_label(id: usize) -> &'static str {
    match id {
        ID_RESET => "恢复默认",
        ID_DETECT => {
            if DETECTED_ONCE.load(Ordering::Relaxed) {
                "重新检测"
            } else {
                "检测"
            }
        }
        ID_SAVE => "保存",
        ID_CANCEL => "取消",
        ID_CLOSE => "\u{d7}", // ×
        _ => {
            for (base, label) in [(BR0, "浏览…"), (UP0, "\u{2191}"), (DN0, "\u{2193}"), (DEL0, "\u{2715}")] {
                if id >= base && id < base + 4 {
                    return label;
                }
            }
            ""
        }
    }
}


unsafe fn draw_btn(ds: &DRAWITEMSTRUCT) {
    let id = ds.CtlID as usize;
    let hovered = *HOVER.lock().unwrap() == id;
    let pressed = (ds.itemState & ODS_SELECTED) != 0;
    let focused = (ds.itemState & ODS_FOCUS) != 0;
    let ghost = id == ID_CLOSE; // 只有右上角 × 是纯文字样式，其余都有按钮底色
    let hdc = ds.hDC;
    let rc = ds.rcItem;

    if id == ID_SAVE {
        let fill = if pressed { SAVE_PR } else if hovered { SAVE_HV } else { SAVE_BG };
        let pen = CreatePen(PS_SOLID, 1, SAVE_PR);
        let br = CreateSolidBrush(fill);
        let old_pen = SelectObject(hdc, pen);
        let old_br = SelectObject(hdc, br);
        RoundRect(hdc, rc.left, rc.top, rc.right, rc.bottom, 8, 8);
        SelectObject(hdc, old_pen);
        SelectObject(hdc, old_br);
        DeleteObject(pen);
        DeleteObject(br);
    } else if !ghost {
        // 恢复默认 / 检测 / 取消 / 浏览 / ↑↓✕：暗底描边
        let fill = if pressed { BTN_PR } else if hovered { BTN_HV } else { BTN_BG };
        let pen = CreatePen(PS_SOLID, 1, BTN_BR);
        let br = CreateSolidBrush(fill);
        let old_pen = SelectObject(hdc, pen);
        let old_br = SelectObject(hdc, br);
        RoundRect(hdc, rc.left, rc.top, rc.right, rc.bottom, 8, 8);
        SelectObject(hdc, old_pen);
        SelectObject(hdc, old_br);
        DeleteObject(pen);
        DeleteObject(br);
    }

    let txt = if id == ID_SAVE {
        SAVE_TX
    } else if ghost {
        if hovered || focused || pressed { TXT_MAIN } else { TXT_DIM }
    } else {
        TXT_MAIN
    };
    SetBkMode(hdc, TRANSPARENT as i32);
    SetTextColor(hdc, txt);
    let t: Vec<u16> = btn_label(id).encode_utf16().chain(std::iter::once(0)).collect();
    let mut trc = rc;
    DrawTextW(hdc, t.as_ptr(), -1, &mut trc, DT_CENTER | DT_VCENTER | DT_SINGLELINE);

    // 键盘聚焦的实心按钮：内缩实线圆角描边
    if focused && !ghost {
        let fp = CreatePen(PS_SOLID, 1, if id == ID_SAVE { SAVE_TX } else { TXT_DIM });
        let old_p = SelectObject(hdc, fp);
        let old_b = SelectObject(hdc, GetStockObject(NULL_BRUSH));
        RoundRect(hdc, rc.left + 2, rc.top + 2, rc.right - 2, rc.bottom - 2, 6, 6);
        SelectObject(hdc, old_p);
        SelectObject(hdc, old_b);
        DeleteObject(fp);
    }
}

/// 悬停高亮：鼠标移动时算出命中的按钮，变化才重绘；配合 TME_LEAVE 离开时清零
unsafe fn update_hover(hwnd: HWND) {
    let mut pt = POINT { x: 0, y: 0 };
    GetCursorPos(&mut pt);
    ScreenToClient(hwnd, &mut pt);
    let mut hit = 0usize;
    for id in all_buttons() {
        let h = GetDlgItem(hwnd, id as i32);
        if h.is_null() {
            continue;
        }
        let mut rc = RECT { left: 0, top: 0, right: 0, bottom: 0 };
        GetClientRect(h, &mut rc);
        MapWindowPoints(h, hwnd, &mut rc as *mut RECT as *mut POINT, 2);
        if PtInRect(&rc, pt) != 0 {
            hit = id;
            break;
        }
    }
    {
        let mut hv = HOVER.lock().unwrap();
        if *hv != hit {
            let old = *hv;
            *hv = hit;
            drop(hv);
            for x in [old, hit] {
                if x != 0 {
                    let h = GetDlgItem(hwnd, x as i32);
                    if !h.is_null() {
                        InvalidateRect(h, ptr::null(), 0);
                    }
                }
            }
        }
    }
    let mut tk = TRACKING.lock().unwrap();
    if !*tk {
        let mut tme: TRACKMOUSEEVENT = std::mem::zeroed();
        tme.cbSize = std::mem::size_of::<TRACKMOUSEEVENT>() as u32;
        tme.dwFlags = TME_LEAVE;
        tme.hwndTrack = hwnd;
        if TrackMouseEvent(&mut tme) != 0 {
            *tk = true;
        }
    }
}

/// 输入框视觉框（26px 高）在父窗口客户区的矩形（外扩 pad px，供边框绘制/失效区域用）。
/// EDIT 实际控件 22px 居中放在框内（单行 EDIT 文字顶部对齐，缩矮后文字即视觉居中）。
unsafe fn edit_frame_rc(_hwnd: HWND, id: usize, pad: i32) -> Option<RECT> {
    let slot = id.checked_sub(ED0)?;
    if slot > 3 || slot >= MODEL.lock().unwrap().len() {
        return None;
    }
    let y = ROW_Y0 + slot as i32 * ROW_PITCH;
    Some(RECT {
        left: 140 - pad,
        top: y - pad,
        right: 140 + 292 + pad,
        bottom: y + 26 + pad,
    })
}

unsafe extern "system" fn dlg_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_COMMAND => {
            let id = (wp & 0xFFFF) as usize;
            let note = (wp >> 16) & 0xFFFF;
            // 输入框焦点变化：更新高亮边框
            if note == 0x0100 || note == 0x0200 {
                // EN_SETFOCUS / EN_KILLFOCUS
                *FOCUS_EDIT.lock().unwrap() = if note == 0x0100 { id } else { 0 };
                if let Some(rc) = edit_frame_rc(hwnd, id, 3) {
                    InvalidateRect(hwnd, &rc, 0);
                }
                return 0;
            }
            // 输入内容变化：重校验路径（逐字符触发，开销只有几次 stat）
            if note == 0x0300 && (ED0..ED0 + 4).contains(&id) {
                refresh_status(hwnd);
                return 0;
            }
            match id {
                ID_SAVE => {
                    save_model(hwnd);
                    DestroyWindow(hwnd);
                }
                ID_CANCEL | ID_CLOSE => {
                    DestroyWindow(hwnd);
                }
                ID_DETECT => do_detect(hwnd),
                ID_RESET => restore_default(hwnd),
                _ => {
                    let slot_hit = |base: usize| (base..base + 4).contains(&id).then(|| id - base);
                    if let Some(slot) = slot_hit(BR0) {
                        browse_slot(hwnd, slot);
                    } else if let Some(slot) = slot_hit(UP0) {
                        move_row(hwnd, slot, -1);
                    } else if let Some(slot) = slot_hit(DN0) {
                        move_row(hwnd, slot, 1);
                    } else if let Some(slot) = slot_hit(DEL0) {
                        delete_row(hwnd, slot);
                    }
                }
            }
        }
        DM_GETDEFID => return (DC_HASDEFID << 16) | ID_SAVE as isize,
        WM_PAINT => {
            let mut ps: PAINTSTRUCT = std::mem::zeroed();
            let hdc = BeginPaint(hwnd, &mut ps);
            let mut rc = RECT { left: 0, top: 0, right: 0, bottom: 0 };
            GetClientRect(hwnd, &mut rc);
            FillRect(hdc, &rc, ctl_brush(BG_WIN) as HBRUSH);
            let m = MODEL.lock().unwrap().clone();
            let collapsed = COLLAPSED.load(Ordering::Relaxed);
            // 折叠标题行：▸/▾ 路径列表 · N；收起时右侧内联工具摘要
            SetBkMode(hdc, TRANSPARENT as i32);
            let mut tx = 24i32;
            tx = toggle_text(hdc, tx, if collapsed { "▸ " } else { "▾ " }, TXT_DIM, F_BTN);
            tx = toggle_text(hdc, tx, "路径列表", TXT_DIM, F_BTN);
            tx = toggle_text(hdc, tx, &format!("  ·  {}", m.len()), TXT_DIM, F_MICRO);
            if collapsed {
                if m.is_empty() {
                    toggle_text(hdc, tx + 12, "未检测 —— 点「检测」扫描本机", TXT_DIM, F_MICRO);
                } else {
                    for (t, _) in m.iter() {
                        tx = toggle_text(
                            hdc,
                            tx + 12,
                            &format!("\u{25cf} {}", t.tag()),
                            tool_color(match t {
                                Src::Zcode => 0,
                                Src::Codex => 1,
                                Src::Opencode => 2,
                                Src::Claude => 3,
                            }),
                            F_MICRO,
                        );
                    }
                }
            }
            // 标题行下的细分隔线（贯穿版心）
            let sep_br = CreateSolidBrush(SEP);
            let sep_rc = RECT { left: 24, top: TOG_Y + 24, right: 576, bottom: TOG_Y + 25 };
            FillRect(hdc, &sep_rc, sep_br as HBRUSH);
            DeleteObject(sep_br);
            // 行首工具色圆点（自绘实心圆，随行工具变化）——仅展开时
            if !collapsed {
                for (i, (t, _)) in m.iter().enumerate() {
                    let cy = ROW_Y0 + i as i32 * ROW_PITCH + 13;
                    let br = CreateSolidBrush(tool_color(match t {
                        Src::Zcode => 0,
                        Src::Codex => 1,
                        Src::Opencode => 2,
                        Src::Claude => 3,
                    }));
                    let old = SelectObject(hdc, br);
                    Ellipse(hdc, 24, cy - 5, 34, cy + 5);
                    SelectObject(hdc, old);
                    DeleteObject(br);
                }
                // 输入框外扩 1px 的圆角描边；聚焦行用工具绿
                let focus = *FOCUS_EDIT.lock().unwrap();
                for id in [ED0, ED0 + 1, ED0 + 2, ED0 + 3] {
                    if let Some(fr) = edit_frame_rc(hwnd, id, 1) {
                        let color = if focus == id { SAVE_BG } else { BTN_BR };
                        let p = CreatePen(PS_SOLID, 1, color);
                        let old_p = SelectObject(hdc, p);
                        let old_b = SelectObject(hdc, GetStockObject(NULL_BRUSH));
                        RoundRect(hdc, fr.left, fr.top, fr.right, fr.bottom, 6, 6);
                        SelectObject(hdc, old_p);
                        SelectObject(hdc, old_b);
                        DeleteObject(p);
                    }
                }
            }
            EndPaint(hwnd, &ps);
        }
        // 点击「路径列表」标题行：收起/展开列表
        WM_LBUTTONDOWN => {
            let pt = POINT {
                x: ((lp & 0xFFFF) as u16) as i16 as i32,
                y: ((lp >> 16) as u16) as i16 as i32,
            };
            if PtInRect(&toggle_rc(), pt) != 0 {
                COLLAPSED.store(!COLLAPSED.load(Ordering::Relaxed), Ordering::Relaxed);
                apply_model_to_ui(hwnd);
            }
        }
        // 折叠标题行显示手型光标
        WM_SETCURSOR => {
            let mut pt = POINT { x: 0, y: 0 };
            GetCursorPos(&mut pt);
            ScreenToClient(hwnd, &mut pt);
            if PtInRect(&toggle_rc(), pt) != 0 {
                SetCursor(LoadCursorW(ptr::null_mut(), IDC_HAND));
                return 1;
            }
            return DefWindowProcW(hwnd, msg, wp, lp);
        }
        // 无边框窗口：头部区域当标题栏拖动；双击不最大化
        WM_NCHITTEST => {
            let mut pt = POINT {
                x: ((lp & 0xFFFF) as u16) as i16 as i32,
                y: ((lp >> 16) as u16) as i16 as i32,
            };
            ScreenToClient(hwnd, &mut pt);
            if pt.y < 56 {
                // × 按钮在头部内，让它正常接收点击
                let hx = GetDlgItem(hwnd, ID_CLOSE as i32);
                if !hx.is_null() {
                    let mut rc = RECT { left: 0, top: 0, right: 0, bottom: 0 };
                    GetWindowRect(hx, &mut rc);
                    let mut tl = POINT { x: rc.left, y: rc.top };
                    let mut brp = POINT { x: rc.right, y: rc.bottom };
                    ScreenToClient(hwnd, &mut tl);
                    ScreenToClient(hwnd, &mut brp);
                    let crc = RECT { left: tl.x, top: tl.y, right: brp.x, bottom: brp.y };
                    if PtInRect(&crc, pt) != 0 {
                        return HTCLIENT as LRESULT;
                    }
                }
                return HTCAPTION as LRESULT;
            }
            return HTCLIENT as LRESULT;
        }
        WM_NCLBUTTONDBLCLK => {}
        WM_CTLCOLORSTATIC => {
            let hdc = wp as HDC;
            let id = GetDlgCtrlID(lp as HWND) as usize;
            let (bg, txt) = if id == IDC_WARN {
                (BG_WIN, if WARN_AS_HINT.load(Ordering::Relaxed) { TXT_DIM } else { TXT_BAD })
            } else if id == IDC_HINT || (SUB0..SUB0 + 4).contains(&id) {
                (BG_WIN, TXT_DIM)
            } else {
                (BG_WIN, TXT_MAIN)
            };
            SetBkColor(hdc, bg);
            SetTextColor(hdc, txt);
            return ctl_brush(bg) as LRESULT;
        }
        WM_CTLCOLOREDIT => {
            let hdc = wp as HDC;
            let id = GetDlgCtrlID(lp as HWND) as usize;
            let bad = (ED0..ED0 + 4).contains(&id) && {
                let m = MODEL.lock().unwrap();
                let slot = id - ED0;
                match m.get(slot) {
                    Some((t, _)) => check(*t, &read_ctl_text(lp as HWND)) != Status::Ok,
                    None => false,
                }
            };
            SetBkColor(hdc, BG_EDIT);
            SetTextColor(hdc, if bad { TXT_BAD } else { TXT_MAIN });
            return ctl_brush(BG_EDIT) as LRESULT;
        }
        // owner-draw 按钮绘制前用它擦底；不处理会返回白刷，文字式按钮会露出白底
        WM_CTLCOLORBTN => return ctl_brush(BG_WIN) as LRESULT,
        WM_DRAWITEM => {
            if lp != 0 {
                draw_btn(&*(lp as *const DRAWITEMSTRUCT));
            }
            return 1;
        }
        WM_MOUSEMOVE => {
            update_hover(hwnd);
        }
        WM_MOUSELEAVE => {
            *HOVER.lock().unwrap() = 0;
            *TRACKING.lock().unwrap() = false;
            for id in all_buttons() {
                let h = GetDlgItem(hwnd, id as i32);
                if !h.is_null() {
                    InvalidateRect(h, ptr::null(), 0);
                }
            }
        }
        WM_CLOSE => {
            DestroyWindow(hwnd);
        }
        WM_DESTROY => {
            *DLG_HWND.lock().unwrap() = 0;
        }
        _ => return DefWindowProcW(hwnd, msg, wp, lp),
    }
    0
}

/// 打开（或前置已有的）设置窗口；非模态，保存后下一秒刷新生效
pub unsafe fn open() {
    let cur = *DLG_HWND.lock().unwrap();
    if cur != 0 {
        let h = cur as HWND;
        ShowWindow(h, SW_SHOW);
        SetForegroundWindow(h);
        return;
    }
    COLLAPSED.store(false, Ordering::Relaxed); // 每次打开默认展开
    let hinstance = GetModuleHandleW(ptr::null());
    let cls: Vec<u16> = "tokenspeed_settings\0".encode_utf16().collect();
    let mut wc: WNDCLASSW = std::mem::zeroed();
    wc.lpfnWndProc = Some(dlg_proc);
    wc.hInstance = hinstance;
    wc.hCursor = LoadCursorW(ptr::null_mut(), IDC_ARROW);
    wc.hIcon = LoadIconW(ptr::null_mut(), IDI_APPLICATION);
    wc.hbrBackground = CreateSolidBrush(BG_WIN); // 深色底，与悬浮条同色系
    wc.lpszClassName = cls.as_ptr();
    RegisterClassW(&wc);

    // 无边框窗口：600x320 即客户区，四周圆角（Win11 DWM；失败退化为区域圆角）
    let (ww, wh) = (600, 314); // 默认 3 行展开的高度，open 尾部 apply 会按实际行数校正
    let hwnd = CreateWindowExW(
        WS_EX_TOOLWINDOW,
        cls.as_ptr(),
        ptr::null(),
        WS_POPUP,
        ((GetSystemMetrics(SM_CXSCREEN) - ww) / 2).max(0),
        ((GetSystemMetrics(SM_CYSCREEN) - wh) / 3).max(0),
        ww,
        wh,
        ptr::null_mut(),
        ptr::null_mut(),
        hinstance,
        ptr::null(),
    );
    if hwnd.is_null() {
        return;
    }
    let corner: u32 = 2; // DWMWCP_ROUND
    if DwmSetWindowAttribute(hwnd, 33, &corner as *const u32 as *const c_void, 4) != 0 {
        SetWindowRgn(hwnd, CreateRoundRectRgn(0, 0, ww, wh, 18, 18), 1);
    }

    // 头部：标题 + 右上角关闭；标题栏区域可拖动（WM_NCHITTEST）
    let t = create_ctl(hwnd, CLS_STATIC, "数据路径", WS_CHILD | WS_VISIBLE, 0, 24, 16, 300, 30, 0);
    set_ctl_font(t, F_TITLE);
    let x = create_ctl(
        hwnd,
        CLS_BUTTON,
        "",
        WS_CHILD | WS_VISIBLE | BS_OWNERDRAW as u32,
        0,
        550,
        16,
        28,
        28,
        ID_CLOSE,
    );
    set_ctl_font(x, F_TITLE);
    let s = create_ctl(
        hwnd,
        CLS_STATIC,
        "留空 = 使用检测路径；保存后下一秒生效。",
        WS_CHILD | WS_VISIBLE,
        0,
        24,
        50,
        480,
        16,
        IDC_HINT,
    );
    set_ctl_font(s, F_SUB);

    // 行槽位：每行 = 工具名 + 类型 + 路径 + 浏览 + 上移/下移/删除
    // 右侧按钮组统一 4px 间距：浏览(438..494) ↑(498..522) ↓(526..550) ✕(554..576)
    for i in 0..4usize {
        let y = ROW_Y0 + i as i32 * ROW_PITCH;
        let l = create_ctl(hwnd, CLS_STATIC, "", WS_CHILD | WS_VISIBLE, 0, 42, y, 94, 16, LBL0 + i);
        set_ctl_font(l, F_BTN);
        let sl = create_ctl(hwnd, CLS_STATIC, "", WS_CHILD | WS_VISIBLE, 0, 42, y + 18, 94, 13, SUB0 + i);
        set_ctl_font(sl, F_MICRO);
        let e = create_ctl(
            hwnd,
            CLS_EDIT,
            "",
            WS_CHILD | WS_VISIBLE | WS_TABSTOP | ES_AUTOHSCROLL as u32,
            0,
            140,
            y + 2,
            292,
            22,
            ED0 + i,
        );
        set_ctl_font(e, F_BODY);
        SendMessageW(e, EM_SETMARGINS, 3, 8 | (8 << 16)); // 文字左右留白 8px
        let small = [(BR0 + i, 438, 56, F_BTN), (UP0 + i, 498, 24, F_MICRO), (DN0 + i, 526, 24, F_MICRO), (DEL0 + i, 554, 22, F_MICRO)];
        for (id, x, w, f) in small {
            let b = create_ctl(
                hwnd,
                CLS_BUTTON,
                "",
                WS_CHILD | WS_VISIBLE | BS_OWNERDRAW as u32,
                0,
                x,
                y,
                w,
                26,
                id,
            );
            set_ctl_font(b, f);
        }
    }
    // 路径校验警告行（有无效路径才显示）
    create_ctl(hwnd, CLS_STATIC, "", WS_CHILD, 0, 24, 256, 552, 16, IDC_WARN);

    // 底部按钮：恢复默认（检测后才出现）/ 检测 / 取消 / 保存
    let btns: [(usize, i32, i32); 4] =
        [(ID_RESET, 200, 96), (ID_DETECT, 304, 92), (ID_CANCEL, 404, 64), (ID_SAVE, 476, 100)];
    for (id, x, w) in btns {
        let b = create_ctl(
            hwnd,
            CLS_BUTTON,
            "",
            WS_CHILD | WS_VISIBLE | WS_TABSTOP | BS_OWNERDRAW as u32,
            0,
            x,
            276,
            w,
            32,
            id,
        );
        set_ctl_font(b, F_BTN);
    }

    load_model();
    apply_model_to_ui(hwnd);
    // 没按过「检测」就没有「恢复默认」（自动检测填充的首屏不算）
    if !DETECTED_ONCE.load(Ordering::Relaxed) {
        ShowWindow(GetDlgItem(hwnd, ID_RESET as i32), SW_HIDE);
    }
    *DLG_HWND.lock().unwrap() = hwnd as isize;
    ShowWindow(hwnd, SW_SHOW);
    SetForegroundWindow(hwnd);
    SetFocus(GetDlgItem(hwnd, ED0 as i32));
}

/// 主消息循环里让设置窗口处理 Tab / Enter / Esc
pub unsafe fn pump(msg: &mut MSG) -> bool {
    let h = *DLG_HWND.lock().unwrap();
    h != 0 && IsDialogMessageW(h as HWND, msg) != 0
}
