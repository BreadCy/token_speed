//! 置顶悬浮条（重新设计的 UI）：
//! - 双行布局：第一行 工具色标 + 大号速度数字；第二行 模型与耗时细节
//! - 每个工具一个专属强调色（左侧竖条 + 圆点 + 标签）
//! - Win11 DWM 圆角、双缓冲绘制无闪烁、宽度随内容自适应
//! - 左键拖动、右键关闭，1s 定时刷新

use super::{collect, fetch_session_rows, fmt_model, fmt_secs, now_ms, open_ro, speed, zcode_db, Args, Src};
use chrono::Local;
use std::ffi::c_void;
use std::ptr;
use windows_sys::Win32::Foundation::{
    GetLastError, ERROR_ALREADY_EXISTS, HWND, LPARAM, LRESULT, POINT, RECT, SIZE, WPARAM,
};
use windows_sys::Win32::Graphics::Dwm::DwmSetWindowAttribute;
use windows_sys::Win32::Graphics::Gdi::{
    BeginPaint, BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, CreateFontW, CreateSolidBrush,
    DeleteObject, EndPaint, FillRect, GetDC, GetTextExtentPoint32W, GetTextMetricsW, InvalidateRect,
    ReleaseDC, SelectObject, SetBkMode, SetTextColor, TextOutW, PAINTSTRUCT, SRCCOPY, TEXTMETRICW,
};
use windows_sys::Win32::System::Console::FreeConsole;
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::Threading::{AttachThreadInput, CreateMutexW, GetCurrentThreadId};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::ReleaseCapture;
use windows_sys::Win32::UI::WindowsAndMessaging::*;

use super::settings;
use super::tray;

// 托盘菜单命令 ID
const ID_TOOL_ALL: usize = 1;
const ID_TOOL_ZC: usize = 2;
const ID_TOOL_CX: usize = 3;
const ID_TOOL_OC: usize = 4;
const ID_TOOL_CC: usize = 5;
const ID_AUTOSTART: usize = 10;
const ID_SETTINGS: usize = 12;
const ID_EXIT: usize = 11;

const TIMER_ID: usize = 1;
const CLASS_NAME: &[u16] = &[116, 111, 107, 101, 110, 115, 112, 101, 101, 100, 0]; // "tokenspeed\0"

// 布局（物理像素）
const STRIP_W: i32 = 4; // 左侧工具色竖条
const PAD_L: i32 = 14;
const PAD_R: i32 = 16;
const PAD_T: i32 = 10;
const PAD_B: i32 = 12;
const GAP: i32 = 6; // 两行间距

// 颜色 (COLORREF = 0x00BBGGRR)
const BG: u32 = 0x001a1a1a; // RGB(26,26,26)
const TXT_MAIN: u32 = 0x00f5f5f5; // 速度数字，近白
const TXT_MODEL: u32 = 0x00d8d8d8; // 第二行
const TXT_DIM: u32 = 0x00a0a0a0; // 次要信息
const TXT_EST: u32 = 0x00b8b8b8; // "~" 估算标记
const TXT_RUN: u32 = 0x007171f8; // 生成中 RGB(248,113,113)

fn tool_color(src: Src) -> u32 {
    match src {
        Src::Zcode => 0x0080de4a,    // 绿 RGB(74,222,128)
        Src::Codex => 0x003c92fb,    // 橙 RGB(251,146,60)
        Src::Opencode => 0x00faa560, // 蓝 RGB(96,165,250)
        Src::Claude => 0x00fc84c0,   // 紫 RGB(192,132,252)
    }
}

// 文字规格下标
const F_BIG: usize = 0; // 大号速度数字
const F_MAIN: usize = 1; // 主体
const F_SMALL: usize = 2; // 小号细节

struct Snap {
    has_data: bool,
    src: Src,
    speed: f64,
    est: bool,
    line2: String, // 均值/耗时/时间
    line3: String, // 模型名
    running: Option<String>, // "生成中 …"（仅 ZCode 有运行态）
}

impl Snap {
    fn empty() -> Self {
        Snap {
            has_data: false,
            src: Src::Zcode,
            speed: 0.0,
            est: false,
            line2: String::new(),
            line3: String::new(),
            running: None,
        }
    }
}

struct Run {
    text: String,
    font: usize,
    color: u32,
}

struct FloatState {
    args: Args,
    snap: Snap,
    fonts: [*mut c_void; 3],
    ascent: [i32; 3],
    cell: [i32; 3],
    strip_brush: *mut c_void, // 跟随当前工具色
    strip_src: Src,
    bg_brush: *mut c_void,
    tray: Option<tray::Tray>,
}

/// 每秒采集一次，组装三行内容（速度 / 明细 / 模型）
fn snapshot(args: &Args) -> Snap {
    let col = collect(args, 1);
    let mut snap = match col.latest() {
        None => return Snap::empty(),
        Some(r) => Snap {
            has_data: true,
            src: r.src,
            speed: speed(r),
            est: r.estimated,
            line2: {
                let mut d = match r.src {
                    Src::Zcode => {
                        let mut d = String::new();
                        if let Ok(con) = open_ro(&zcode_db(args)) {
                            let rows = fetch_session_rows(&con, &r.session_id);
                            let (to, tg) = (
                                rows.iter().map(|x| x.output_tokens).sum::<i64>(),
                                rows.iter().map(|x| x.gen_ms).sum::<i64>(),
                            );
                            if tg > 0 {
                                d += &format!("均值 {:.1} · ", to as f64 * 1000.0 / tg as f64);
                            }
                        }
                        d += &format!("{} tok/{}", r.output_tokens, fmt_secs(r.gen_ms));
                        d
                    }
                    _ => format!("{} tok/{}", r.output_tokens, fmt_secs(r.gen_ms)),
                };
                // 最近一次请求的完成时间
                if let Some(t) = chrono::DateTime::from_timestamp_millis(r.completed_at) {
                    d += &format!(" · {}", t.with_timezone(&Local).format("%H:%M:%S"));
                }
                d
            },
            line3: fmt_model(&r.model, &r.variant),
            running: None,
        },
    };
    // ZCode 生成中的请求（其他工具没有运行态数据）
    if let Some((started, first_tok, out)) = col.running {
        let elapsed = now_ms() - first_tok.unwrap_or(started);
        if (0..=180_000).contains(&elapsed) {
            let label = if snap.src == Src::Zcode { "" } else { "ZC " };
            let txt = if out > 0 {
                let s = elapsed as f64 / 1000.0;
                format!("{}生成中 {:.1} tok/s", label, out as f64 * 1000.0 / s)
            } else {
                format!("{}生成中 {}", label, fmt_secs(elapsed.max(0)))
            };
            snap.running = Some(txt);
        }
    }
    snap
}

fn line1_runs(s: &Snap) -> Vec<Run> {
    let mut v = vec![
        Run { text: "\u{25cf} ".into(), font: F_SMALL, color: tool_color(s.src) }, // ●
        Run { text: format!("{}  ", s.src.tag()), font: F_MAIN, color: tool_color(s.src) },
    ];
    if s.est {
        v.push(Run { text: "~".into(), font: F_SMALL, color: TXT_EST });
    }
    v.push(Run { text: format!("{:.1}", s.speed), font: F_BIG, color: TXT_MAIN });
    v.push(Run { text: " tok/s".into(), font: F_SMALL, color: TXT_DIM });
    if let Some(txt) = &s.running {
        v.push(Run { text: "   \u{25cf} ".into(), font: F_SMALL, color: TXT_RUN });
        v.push(Run { text: txt.clone(), font: F_SMALL, color: TXT_RUN });
    }
    v
}

fn line2_runs(s: &Snap) -> Vec<Run> {
    vec![Run { text: s.line2.clone(), font: F_SMALL, color: TXT_DIM }]
}

fn line3_runs(s: &Snap) -> Vec<Run> {
    vec![Run { text: s.line3.clone(), font: F_SMALL, color: TXT_MODEL }]
}

unsafe fn runs_width(hwnd: HWND, st: &FloatState, runs: &[Run]) -> i32 {
    let hdc = GetDC(hwnd);
    let mut total = 0;
    for r in runs {
        let old = SelectObject(hdc, st.fonts[r.font]);
        let wide: Vec<u16> = r.text.encode_utf16().collect();
        let mut size = SIZE { cx: 0, cy: 0 };
        GetTextExtentPoint32W(hdc, wide.as_ptr(), wide.len() as i32, &mut size);
        SelectObject(hdc, old);
        total += size.cx;
    }
    ReleaseDC(hwnd, hdc);
    total
}

unsafe fn draw_runs(hdc: HWND, st: &FloatState, runs: &[Run], x0: i32, baseline: i32) {
    let mut x = x0;
    for r in runs {
        SelectObject(hdc, st.fonts[r.font]);
        let mut tm: TEXTMETRICW = std::mem::zeroed();
        GetTextMetricsW(hdc, &mut tm);
        SetTextColor(hdc, r.color);
        let wide: Vec<u16> = r.text.encode_utf16().collect();
        let mut size = SIZE { cx: 0, cy: 0 };
        GetTextExtentPoint32W(hdc, wide.as_ptr(), wide.len() as i32, &mut size);
        TextOutW(hdc, x, baseline - tm.tmAscent, wide.as_ptr(), wide.len() as i32);
        x += size.cx;
    }
}

unsafe fn auto_fit(hwnd: HWND, st: &FloatState) {
    let w = if st.snap.has_data {
        runs_width(hwnd, st, &line1_runs(&st.snap))
            .max(runs_width(hwnd, st, &line2_runs(&st.snap)))
            .max(runs_width(hwnd, st, &line3_runs(&st.snap)))
            + STRIP_W
            + PAD_L
            + PAD_R
    } else {
        260
    };
    // 三行：大号速度 + 两条小号
    let h = PAD_T + st.cell[F_BIG] + GAP + st.cell[F_SMALL] * 2 + GAP + PAD_B;
    let _ = SetWindowPos(hwnd, HWND_TOPMOST, 0, 0, w, h, SWP_NOMOVE | SWP_NOACTIVATE);
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_CREATE => {
            let cs = &*(lp as *const CREATESTRUCTW);
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, cs.lpCreateParams as _);
            0
        }
        WM_ERASEBKGND => 1, // 全部在 WM_PAINT 双缓冲绘制，避免闪烁
        WM_TIMER => {
            let state = &mut *(GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut FloatState);
            state.snap = snapshot(&state.args);
            if refresh_strip(state) {
                if let Some(t) = &mut state.tray {
                    tray::update_icon(t, tool_color(state.strip_src));
                }
            }
            auto_fit(hwnd, state);
            InvalidateRect(hwnd, ptr::null(), 0);
            0
        }
        tray::WM_TRAY => {
            let state = &mut *(GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut FloatState);
            let ev = (lp & 0xFFFF) as u32;
            match ev {
                // 左/右键单击：弹菜单；双击：仅用于找回被隐藏的悬浮条，不会隐藏
                x if x == WM_LBUTTONUP || x == WM_RBUTTONUP => {
                    let cmd = tray_menu(hwnd, state);
                    apply_menu(hwnd, state, cmd);
                }
                x if x == WM_LBUTTONDBLCLK => {
                    ShowWindow(hwnd, SW_SHOW);
                }
                _ => {}
            }
            0
        }
        WM_PAINT => {
            let state = &mut *(GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut FloatState);
            let mut ps: PAINTSTRUCT = std::mem::zeroed();
            let hdc = BeginPaint(hwnd, &mut ps);
            let mut rc: RECT = std::mem::zeroed();
            GetClientRect(hwnd, &mut rc);

            // 内存 DC 双缓冲
            let mem = CreateCompatibleDC(hdc);
            let bmp = CreateCompatibleBitmap(hdc, rc.right, rc.bottom);
            let old_bmp = SelectObject(mem, bmp);

            FillRect(mem, &rc, state.bg_brush);
            let strip_rc = RECT { left: 0, top: 0, right: STRIP_W, bottom: rc.bottom };
            FillRect(mem, &strip_rc, state.strip_brush);
            SetBkMode(mem, 1); // TRANSPARENT

            if state.snap.has_data {
                let base1 = PAD_T + state.ascent[F_BIG];
                let base2 = PAD_T + state.cell[F_BIG] + GAP + state.ascent[F_SMALL];
                // 同字体行的基线间距 = 行高 + 行距
                let base3 = base2 + state.cell[F_SMALL] + GAP;
                draw_runs(mem, state, &line1_runs(&state.snap), STRIP_W + PAD_L, base1);
                draw_runs(mem, state, &line2_runs(&state.snap), STRIP_W + PAD_L, base2);
                draw_runs(mem, state, &line3_runs(&state.snap), STRIP_W + PAD_L, base3);
            } else {
                SelectObject(mem, state.fonts[F_MAIN]);
                SetTextColor(mem, TXT_DIM);
                let wide: Vec<u16> = "等待数据…".encode_utf16().collect();
                TextOutW(mem, STRIP_W + PAD_L, PAD_T + 6, wide.as_ptr(), wide.len() as i32);
            }

            BitBlt(hdc, 0, 0, rc.right, rc.bottom, mem, 0, 0, SRCCOPY);
            SelectObject(mem, old_bmp);
            DeleteObject(bmp);
            DeleteObject(mem);
            EndPaint(hwnd, &ps);
            0
        }
        settings::WM_OPEN => {
            settings::open();
            0
        }
        WM_LBUTTONDOWN => {
            // 经典拖拽技巧：把点击转成标题栏拖动
            ReleaseCapture();
            PostMessageW(hwnd, WM_NCLBUTTONDOWN, HTCAPTION as usize, 0);
            0
        }
        // 双击悬浮条会被系统当成"标题栏双击"手势（最小化/最大化），
        // 工具窗口最小化即从屏幕消失，必须吞掉
        WM_LBUTTONDBLCLK => 0,
        WM_NCLBUTTONDBLCLK => 0,
        WM_RBUTTONDOWN => {
            DestroyWindow(hwnd);
            0
        }
        WM_DESTROY => {
            if let Some(t) = state_tray_take(hwnd) {
                tray::remove(t);
            }
            PostQuitMessage(0);
            0
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

/// WM_DESTROY 里安全取走托盘（需要 &mut Box 不便，直接从 userdata 拿）
unsafe fn state_tray_take(hwnd: HWND) -> Option<tray::Tray> {
    let p = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut FloatState;
    let state = &mut *p;
    state.tray.take()
}

unsafe fn make_font(height: i32, weight: i32) -> *mut c_void {
    CreateFontW(
        height,
        0,
        0,
        0,
        weight,
        0,
        0,
        0,
        0,
        0,
        0,
        5,
        0,
        "Segoe UI\0".encode_utf16().collect::<Vec<u16>>().as_ptr(),
    )
}

/// 工具色变化时重建左侧竖条画刷，返回是否变化
unsafe fn refresh_strip(state: &mut FloatState) -> bool {
    if state.strip_src != state.snap.src {
        state.strip_src = state.snap.src;
        if !state.strip_brush.is_null() {
            DeleteObject(state.strip_brush);
        }
        state.strip_brush = CreateSolidBrush(tool_color(state.snap.src));
        true
    } else {
        false
    }
}

/// UTF-16 且以 \0 结尾（AppendMenuW 需要截断符，否则读出堆上乱码）
fn widens(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// 本窗口是 WS_EX_NOACTIVATE（点击不抢焦点），SetForegroundWindow 会失败，
/// 导致托盘菜单弹出即收起；借 AttachThreadInput 短暂接管前台输入状态强制前置
unsafe fn force_foreground(hwnd: HWND) {
    let cur = GetCurrentThreadId();
    let fg = GetForegroundWindow();
    let fg_thread = if fg.is_null() {
        0
    } else {
        GetWindowThreadProcessId(fg, ptr::null_mut())
    };
    if fg_thread != 0 && fg_thread != cur {
        AttachThreadInput(cur, fg_thread, 1);
    }
    SetForegroundWindow(hwnd);
    if fg_thread != 0 && fg_thread != cur {
        AttachThreadInput(cur, fg_thread, 0);
    }
}

/// 托盘菜单：工具切换 / 开机自启 / 退出
unsafe fn tray_menu(hwnd: HWND, st: &mut FloatState) -> usize {
    let m = CreatePopupMenu();
    let tools = [
        (ID_TOOL_ALL, None, "全部工具"),
        (ID_TOOL_ZC, Some(Src::Zcode), "ZCode"),
        (ID_TOOL_CX, Some(Src::Codex), "Codex"),
        (ID_TOOL_OC, Some(Src::Opencode), "OpenCode"),
        (ID_TOOL_CC, Some(Src::Claude), "Claude Code"),
    ];
    for (id, src, name) in tools {
        let checked = if st.args.tool == src { MF_CHECKED } else { 0 };
        let w = widens(name);
        AppendMenuW(m, MF_STRING | checked, id, w.as_ptr());
    }
    AppendMenuW(m, MF_SEPARATOR, 0, ptr::null());
    let auto_flag = if tray::autostart_enabled() { MF_CHECKED } else { 0 };
    let w = widens("开机自启（悬浮条）");
    AppendMenuW(m, MF_STRING | auto_flag, ID_AUTOSTART, w.as_ptr());
    let w = widens("设置…");
    AppendMenuW(m, MF_STRING, ID_SETTINGS, w.as_ptr());
    AppendMenuW(m, MF_SEPARATOR, 0, ptr::null());
    let w = widens("退出");
    AppendMenuW(m, MF_STRING, ID_EXIT, w.as_ptr());

    let mut pt: POINT = std::mem::zeroed();
    GetCursorPos(&mut pt);
    force_foreground(hwnd); // 让菜单在点击外部时能收起
    let cmd = TrackPopupMenu(
        m,
        TPM_RETURNCMD | TPM_RIGHTBUTTON | TPM_BOTTOMALIGN,
        pt.x,
        pt.y,
        0,
        hwnd,
        ptr::null(),
    );
    PostMessageW(hwnd, WM_NULL, 0, 0);
    DestroyMenu(m);
    cmd as usize
}

/// 应用托盘菜单选择
unsafe fn apply_menu(hwnd: HWND, st: &mut FloatState, cmd: usize) {
    match cmd {
        ID_EXIT => {
            DestroyWindow(hwnd);
        }
        ID_AUTOSTART => {
            let on = tray::autostart_enabled();
            tray::autostart_set(!on);
        }
        ID_SETTINGS => {
            settings::open();
        }
        ID_TOOL_ALL => {
            st.args.tool = None;
            apply_tool_change(hwnd, st);
        }
        ID_TOOL_ZC => {
            st.args.tool = Some(Src::Zcode);
            apply_tool_change(hwnd, st);
        }
        ID_TOOL_CX => {
            st.args.tool = Some(Src::Codex);
            apply_tool_change(hwnd, st);
        }
        ID_TOOL_OC => {
            st.args.tool = Some(Src::Opencode);
            apply_tool_change(hwnd, st);
        }
        ID_TOOL_CC => {
            st.args.tool = Some(Src::Claude);
            apply_tool_change(hwnd, st);
        }
        _ => {}
    }
}

/// 切换工具过滤后：立即重采、换色、更新托盘图标
unsafe fn apply_tool_change(hwnd: HWND, st: &mut FloatState) {
    st.snap = snapshot(&st.args);
    if refresh_strip(st) {
        if let Some(t) = &mut st.tray {
            tray::update_icon(t, tool_color(st.strip_src));
        }
    }
    auto_fit(hwnd, st);
    InvalidateRect(hwnd, ptr::null(), 0);
}

pub fn run_float(args: &Args) {
    unsafe {
        // 单实例：已有悬浮条在跑时，把它唤起后直接退出（避免双击出多条）
        let mutex = CreateMutexW(ptr::null(), 0, widens("Local\\tokenspeed-float").as_ptr());
        if !mutex.is_null() && GetLastError() == ERROR_ALREADY_EXISTS {
            let existing = FindWindowW(CLASS_NAME.as_ptr(), ptr::null());
            if !existing.is_null() {
                ShowWindow(existing, SW_SHOW);
            }
            return;
        }
        FreeConsole();
        let hinstance = GetModuleHandleW(ptr::null());
        let mut wc: WNDCLASSW = std::mem::zeroed();
        wc.lpfnWndProc = Some(wndproc);
        wc.hInstance = hinstance;
        wc.hbrBackground = ptr::null_mut(); // 自绘背景（WM_ERASEBKGND 返回 1）
        wc.hCursor = LoadCursorW(ptr::null_mut(), IDC_ARROW);
        wc.lpszClassName = CLASS_NAME.as_ptr();
        RegisterClassW(&wc);

        // 字体: 大号速度数字 / 主体 / 小号细节
        let fonts = [make_font(-26, 700), make_font(-15, 600), make_font(-13, 400)];
        let mut ascent = [0; 3];
        let mut cell = [0; 3];
        let hdc = GetDC(ptr::null_mut());
        for i in 0..3 {
            let old = SelectObject(hdc, fonts[i]);
            let mut tm: TEXTMETRICW = std::mem::zeroed();
            GetTextMetricsW(hdc, &mut tm);
            SelectObject(hdc, old);
            ascent[i] = tm.tmAscent;
            cell[i] = tm.tmHeight;
        }
        ReleaseDC(ptr::null_mut(), hdc);

        let mut state = Box::new(FloatState {
            args: Args {
                limit: 1,
                session: None,
                db: args.db.clone(),
                hook: false,
                auto_report: "true".into(),
                bench: false,
                watch: None,
                float: false,
                tool: args.tool,
                report: false,
            },
            snap: Snap::empty(),
            fonts,
            ascent,
            cell,
            strip_brush: CreateSolidBrush(tool_color(Src::Zcode)),
            strip_src: Src::Zcode,
            bg_brush: CreateSolidBrush(BG),
            tray: None,
        });

        let title_w: Vec<u16> = "tokenspeed\0".encode_utf16().collect();
        let class_w: Vec<u16> = CLASS_NAME.to_vec();
        let screen_w = GetSystemMetrics(SM_CXSCREEN);
        let hwnd = CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_LAYERED | WS_EX_NOACTIVATE,
            class_w.as_ptr(),
            title_w.as_ptr(),
            WS_POPUP | WS_VISIBLE,
            screen_w - 720, 12, 680, 60,
            ptr::null_mut(), ptr::null_mut(), hinstance,
            &*state as *const FloatState as *const c_void,
        );
        if hwnd.is_null() {
            return;
        }
        // Win11 圆角窗口 (DWMWCP_ROUND)；旧系统调用失败无副作用
        let corner: u32 = 2;
        DwmSetWindowAttribute(hwnd, 33, &corner as *const u32 as *const c_void, 4);
        SetLayeredWindowAttributes(hwnd, 0, 236, LWA_ALPHA);
        SetTimer(hwnd, TIMER_ID, 1000, None);

        // 托盘图标（失败不影响悬浮条本体）
        state.tray = tray::add(hwnd, tool_color(Src::Zcode));

        // 首帧
        state.snap = snapshot(&state.args);
        if refresh_strip(&mut state) {
            if let Some(t) = &mut state.tray {
                tray::update_icon(t, tool_color(state.strip_src));
            }
        }
        auto_fit(hwnd, &state);
        InvalidateRect(hwnd, ptr::null(), 0);

        let mut msg: MSG = std::mem::zeroed();
        while GetMessageW(&mut msg, ptr::null_mut(), 0, 0) != 0 {
            if settings::pump(&mut msg) {
                continue; // 设置窗口消化了 Tab/Enter/Esc
            }
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        drop(state);
    }
}
