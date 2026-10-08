//! Windows implementation: Win32 APIs for paste, keys, startup, icons, eyedropper…

use std::{
    ffi::c_void,
    fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicIsize, Ordering},
    thread,
    time::Duration,
};

use crate::util;
use windows_sys::Win32::{
    System::Com::CoTaskMemFree,
    Foundation::{CloseHandle, GetLastError, ERROR_ALREADY_EXISTS, HANDLE, HWND, POINT, RECT},
    Graphics::Gdi::{CombineRgn, CreateRectRgn, GetDC, GetPixel, ReleaseDC, SetWindowRgn, BI_BITFIELDS, HRGN, RGN_OR},
    System::{
        DataExchange::{
            CloseClipboard, EmptyClipboard, EnumClipboardFormats, GetClipboardData, GetClipboardFormatNameW, GetClipboardSequenceNumber, OpenClipboard,
            RegisterClipboardFormatW, SetClipboardData,
        },
        Memory::{GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock, GMEM_MOVEABLE},
        Ole::{CF_DIB, CF_UNICODETEXT},
        Diagnostics::Debug::MessageBeep,
        Power::{SetThreadExecutionState, ES_CONTINUOUS, ES_DISPLAY_REQUIRED, ES_SYSTEM_REQUIRED},
        Registry::{RegDeleteKeyValueW, RegDeleteTreeW, RegGetValueW, RegSetKeyValueW, HKEY_CURRENT_USER, REG_SZ, RRF_RT_REG_SZ},
        Threading::CreateMutexW,
        Shutdown::LockWorkStation,
    },
    UI::Input::KeyboardAndMouse::{
        GetAsyncKeyState, SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_KEYUP, VK_CONTROL, VK_D, VK_ESCAPE, VK_LBUTTON, VK_LWIN,
        VK_MEDIA_NEXT_TRACK, VK_MEDIA_PLAY_PAUSE, VK_MEDIA_PREV_TRACK, VK_V, VK_VOLUME_DOWN, VK_VOLUME_MUTE, VK_VOLUME_UP,
    },
    UI::WindowsAndMessaging::{
        GetCursorPos, GetForegroundWindow, SystemParametersInfoW, SPI_GETWORKAREA, GetWindowLongPtrW, GetWindowTextW, PostMessageW, SetForegroundWindow, SetWindowPos, GWL_EXSTYLE, HWND_NOTOPMOST,
        HWND_TOPMOST, MB_ICONASTERISK, SC_MONITORPOWER, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, WM_SYSCOMMAND, WS_EX_TOPMOST,
    },
};

pub const PLATFORM: &str = "windows";
const RUN_KEY: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Run";
const RUN_NAME: &str = "VestaEdge";

static PREV_FG: AtomicIsize = AtomicIsize::new(0);
static SELF_HWND: AtomicIsize = AtomicIsize::new(0);

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

pub fn data_dir() -> PathBuf {
    let base = PathBuf::from(std::env::var("APPDATA").unwrap_or_else(|_| ".".into()));
    let dir = base.join("VestaEdge");
    // carry settings and notes over from the earlier Right Panel / Edge Dock folders
    // (copy, not move: the old folder can still be locked by WebView2)
    let _ = fs::create_dir_all(&dir);
    if !dir.join("migrated").exists() {
        if let Some(old) = ["RightPanel", "EdgeDock"].iter().map(|n| base.join(n)).find(|p| p.join("settings.json").exists()) {
            for f in ["settings.json", "notes.txt"] {
                let _ = fs::copy(old.join(f), dir.join(f));
            }
        }
        let _ = fs::write(dir.join("migrated"), "");
    }
    dir
}

pub fn home() -> String {
    std::env::var("USERPROFILE").unwrap_or_default()
}

pub fn set_self_window(hwnd: isize) {
    SELF_HWND.store(hwnd, Ordering::Relaxed);
}

pub fn set_scale(_: f64) {}

/* ---------------- startup (HKCU\...\Run) ---------------- */
fn reg_get(name: &str) -> bool {
    let (k, v) = (wide(RUN_KEY), wide(name));
    unsafe { RegGetValueW(HKEY_CURRENT_USER, k.as_ptr(), v.as_ptr(), RRF_RT_REG_SZ, std::ptr::null_mut(), std::ptr::null_mut(), std::ptr::null_mut()) == 0 }
}

fn reg_del(name: &str) {
    let (k, v) = (wide(RUN_KEY), wide(name));
    unsafe { RegDeleteKeyValueW(HKEY_CURRENT_USER, k.as_ptr(), v.as_ptr()) };
}

pub fn startup_enabled() -> bool {
    reg_get(RUN_NAME)
}

pub fn set_startup(on: bool) {
    if !on {
        return reg_del(RUN_NAME);
    }
    let (k, v) = (wide(RUN_KEY), wide(RUN_NAME));
    let exe = std::env::current_exe().map(|p| format!("\"{}\"", p.display())).unwrap_or_default();
    let data = wide(&exe);
    unsafe { RegSetKeyValueW(HKEY_CURRENT_USER, k.as_ptr(), v.as_ptr(), REG_SZ, data.as_ptr() as *const c_void, (data.len() * 2) as u32) };
}

/* ---------------- pointer ---------------- */
pub fn cursor_pos() -> Option<(i32, i32)> {
    let mut p = POINT { x: 0, y: 0 };
    (unsafe { GetCursorPos(&mut p) } != 0).then_some((p.x, p.y))
}

pub fn left_down() -> bool {
    unsafe { GetAsyncKeyState(VK_LBUTTON as i32) < 0 }
}

/// Remember the app the user was in, so paste / pin can target it.
pub fn remember_foreground() {
    let fg = unsafe { GetForegroundWindow() } as isize;
    if fg != SELF_HWND.load(Ordering::Relaxed) {
        PREV_FG.store(fg, Ordering::Relaxed);
    }
}

pub fn clip_seq() -> u64 {
    unsafe { GetClipboardSequenceNumber() as u64 }
}

/* ---------------- keyboard ---------------- */
fn key(vk: u16, up: bool) -> INPUT {
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 { ki: KEYBDINPUT { wVk: vk, wScan: 0, dwFlags: if up { KEYEVENTF_KEYUP } else { 0 }, time: 0, dwExtraInfo: 0 } },
    }
}

fn send_combo(combo: &[u16]) {
    let mut inputs: Vec<INPUT> = combo.iter().map(|&k| key(k, false)).collect();
    inputs.extend(combo.iter().rev().map(|&k| key(k, true)));
    unsafe { SendInput(inputs.len() as u32, inputs.as_ptr(), std::mem::size_of::<INPUT>() as i32) };
}

/// Give focus back to the app the user was in, then press Ctrl+V once it really has the focus.
pub fn paste_into_previous() {
    thread::spawn(|| {
        let prev = PREV_FG.load(Ordering::Relaxed);
        if prev != 0 {
            unsafe { SetForegroundWindow(prev as HWND) };
            for _ in 0..30 {
                if unsafe { GetForegroundWindow() } as isize == prev {
                    break;
                }
                thread::sleep(Duration::from_millis(10));
            }
        }
        thread::sleep(Duration::from_millis(60));
        send_combo(&[VK_CONTROL, VK_V]);
    });
}

/// Media / volume keys and Win+D, sent as real key presses.
pub fn press(name: &str) {
    let combo: &[u16] = match name {
        "play" => &[VK_MEDIA_PLAY_PAUSE],
        "next" => &[VK_MEDIA_NEXT_TRACK],
        "prev" => &[VK_MEDIA_PREV_TRACK],
        "mute" => &[VK_VOLUME_MUTE],
        "volup" => &[VK_VOLUME_UP],
        "voldown" => &[VK_VOLUME_DOWN],
        "desktop" => &[VK_LWIN, VK_D],
        _ => return,
    };
    send_combo(combo);
}

/* ---------------- system actions ---------------- */
/// Toggle "always on top" for the window the user was in before opening the panel.
pub fn toggle_topmost() -> String {
    let hwnd = PREV_FG.load(Ordering::Relaxed) as HWND;
    if hwnd.is_null() {
        return "No window to pin".into();
    }
    unsafe {
        let mut buf = [0u16; 120];
        let n = GetWindowTextW(hwnd, buf.as_mut_ptr(), buf.len() as i32).max(0) as usize;
        let title = String::from_utf16_lossy(&buf[..n]);
        let title = if title.chars().count() > 28 { format!("{}…", title.chars().take(28).collect::<String>()) } else { title };
        let on_top = GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32 & WS_EX_TOPMOST != 0;
        SetWindowPos(hwnd, if on_top { HWND_NOTOPMOST } else { HWND_TOPMOST }, 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
        if on_top { format!("Unpinned {title}") } else { format!("📌 {title} stays on top") }
    }
}

pub fn screen_off() {
    thread::spawn(|| {
        thread::sleep(Duration::from_millis(600)); // let the panel slide away first
        unsafe { PostMessageW(SELF_HWND.load(Ordering::Relaxed) as HWND, WM_SYSCOMMAND, SC_MONITORPOWER as usize, 2) };
    });
}

pub fn lock() {
    unsafe { LockWorkStation() };
}

pub fn beep() {
    unsafe { MessageBeep(MB_ICONASTERISK) };
}

/// ES_CONTINUOUS on the (long-lived) main thread keeps PC + display awake until turned off.
pub fn keep_awake(on: bool) {
    let f = if on { ES_CONTINUOUS | ES_SYSTEM_REQUIRED | ES_DISPLAY_REQUIRED } else { ES_CONTINUOUS };
    unsafe { SetThreadExecutionState(f) };
}

pub fn screenshot() {
    thread::spawn(|| {
        thread::sleep(Duration::from_millis(350));
        let _ = std::process::Command::new("explorer.exe").arg("ms-screenclip:").spawn();
    });
}

/// Screen eyedropper: waits for the next click, returns the pixel color.
pub fn pick_color(done: impl FnOnce(Option<String>) + Send + 'static) {
    thread::spawn(move || unsafe {
        while GetAsyncKeyState(VK_LBUTTON as i32) < 0 {
            thread::sleep(Duration::from_millis(10));
        }
        loop {
            thread::sleep(Duration::from_millis(10));
            if GetAsyncKeyState(VK_ESCAPE as i32) < 0 {
                return done(None);
            }
            if GetAsyncKeyState(VK_LBUTTON as i32) < 0 {
                let mut p = POINT { x: 0, y: 0 };
                GetCursorPos(&mut p);
                let dc = GetDC(std::ptr::null_mut());
                let c = GetPixel(dc, p.x, p.y);
                ReleaseDC(std::ptr::null_mut(), dc);
                return done(Some(format!("#{:02x}{:02x}{:02x}", c & 0xff, (c >> 8) & 0xff, (c >> 16) & 0xff)));
            }
        }
    });
}



use windows_sys::Win32::{
    Graphics::Gdi::{CreateCompatibleDC, DeleteDC, DeleteObject, GetDIBits, GetObjectW, BITMAP, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS},
    Storage::FileSystem::FILE_ATTRIBUTE_NORMAL,
    UI::{
        Controls::Dialogs::{GetOpenFileNameW, OFN_FILEMUSTEXIST, OFN_NODEREFERENCELINKS, OFN_PATHMUSTEXIST, OPENFILENAMEW},
        Controls::{ImageList_GetIcon, HIMAGELIST, ILD_TRANSPARENT},
        Shell::{
            SHBrowseForFolderW, SHGetFileInfoW, SHGetImageList, SHGetPathFromIDListW, ShellExecuteW, BIF_NEWDIALOGSTYLE, BIF_RETURNONLYFSDIRS, BROWSEINFOW,
            SHFILEINFOW, SHGFI_SYSICONINDEX, SHIL_EXTRALARGE,
        },
        WindowsAndMessaging::{DestroyIcon, GetIconInfo, PrivateExtractIconsW, HICON, ICONINFO, SW_SHOWNORMAL},
    },
};

/// Native "Open" dialog, starting in the Start Menu so shortcuts are easy to pick.
pub fn pick_file() -> Option<String> {
    let mut buf = vec![0u16; 1024];
    let filter: Vec<u16> = "Apps and shortcuts\0*.exe;*.lnk;*.url;*.bat;*.cmd\0All files\0*.*\0\0".encode_utf16().collect();
    let start = wide(r"C:\ProgramData\Microsoft\Windows\Start Menu\Programs");
    let title = wide("Add app to Vesta Edge");
    unsafe {
        let mut ofn: OPENFILENAMEW = std::mem::zeroed();
        ofn.lStructSize = std::mem::size_of::<OPENFILENAMEW>() as u32;
        ofn.lpstrFilter = filter.as_ptr();
        ofn.lpstrFile = buf.as_mut_ptr();
        ofn.nMaxFile = buf.len() as u32;
        ofn.lpstrInitialDir = start.as_ptr();
        ofn.lpstrTitle = title.as_ptr();
        ofn.Flags = OFN_FILEMUSTEXIST | OFN_PATHMUSTEXIST | OFN_NODEREFERENCELINKS;
        if GetOpenFileNameW(&mut ofn) == 0 {
            return None;
        }
    }
    let len = buf.iter().position(|&c| c == 0).unwrap_or(0);
    Some(String::from_utf16_lossy(&buf[..len]))
}

pub fn launch(path: &str) {
    let (op, p) = (wide("open"), wide(path));
    let dir = Path::new(path).parent().filter(|d| d.is_dir()).map(|d| wide(&d.to_string_lossy()));
    let dir_ptr = dir.as_ref().map_or(std::ptr::null(), |d| d.as_ptr());
    unsafe { ShellExecuteW(std::ptr::null_mut(), op.as_ptr(), p.as_ptr(), std::ptr::null(), dir_ptr, SW_SHOWNORMAL) };
}

/// Icon of any file (exe gets a crisp 64px icon, shortcuts use the shell icon) as a PNG data URI.
pub fn icon_data_uri(path: &str) -> Option<String> {
    let p = wide(path);
    let mut icon: HICON = std::ptr::null_mut();
    unsafe {
        if path.to_lowercase().ends_with(".exe") {
            let mut id = 0u32;
            PrivateExtractIconsW(p.as_ptr(), 0, 64, 64, &mut icon, &mut id, 1, 0);
        }
        if icon.is_null() {
            // system image list index → 48px icon; ImageList_GetIcon without ILD_OVERLAYMASK has no shortcut arrow
            let mut info: SHFILEINFOW = std::mem::zeroed();
            SHGetFileInfoW(p.as_ptr(), FILE_ATTRIBUTE_NORMAL, &mut info, std::mem::size_of::<SHFILEINFOW>() as u32, SHGFI_SYSICONINDEX);
            const IID_IIMAGELIST: windows_sys::core::GUID = windows_sys::core::GUID::from_u128(0x46eb5926_582e_4017_9fdf_e8998daa0950);
            let mut list: *mut core::ffi::c_void = std::ptr::null_mut();
            if SHGetImageList(SHIL_EXTRALARGE as i32, &IID_IIMAGELIST, &mut list) >= 0 && !list.is_null() {
                icon = ImageList_GetIcon(list as HIMAGELIST, info.iIcon, ILD_TRANSPARENT);
            }
        }
        if icon.is_null() {
            return None;
        }
        let out = icon_to_rgba(icon).map(|(w, h, px)| format!("data:image/png;base64,{}", util::base64(&util::png(w, h, &px))));
        DestroyIcon(icon);
        out
    }
}

unsafe fn icon_to_rgba(icon: HICON) -> Option<(u32, u32, Vec<u8>)> {
    unsafe {
        let mut ii: ICONINFO = std::mem::zeroed();
        if GetIconInfo(icon, &mut ii) == 0 || ii.hbmColor.is_null() {
            if !ii.hbmMask.is_null() { DeleteObject(ii.hbmMask); }
            return None;
        }
        let mut bm: BITMAP = std::mem::zeroed();
        GetObjectW(ii.hbmColor, std::mem::size_of::<BITMAP>() as i32, &mut bm as *mut _ as *mut _);
        let (w, h) = (bm.bmWidth as u32, bm.bmHeight as u32);
        let mut bi: BITMAPINFO = std::mem::zeroed();
        bi.bmiHeader = BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: w as i32,
            biHeight: -(h as i32),
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB,
            ..std::mem::zeroed()
        };
        let mut px = vec![0u8; (w * h * 4) as usize];
        let dc = CreateCompatibleDC(std::ptr::null_mut());
        let ok = GetDIBits(dc, ii.hbmColor, 0, h, px.as_mut_ptr() as *mut _, &mut bi, DIB_RGB_COLORS);
        DeleteDC(dc);
        DeleteObject(ii.hbmColor);
        DeleteObject(ii.hbmMask);
        if ok == 0 {
            return None;
        }
        let has_alpha = px.chunks(4).any(|c| c[3] != 0);
        for c in px.chunks_mut(4) {
            c.swap(0, 2); // BGRA → RGBA
            if !has_alpha { c[3] = 255; }
        }
        Some((w, h, px))
    }
}


/* ---------------- single instance ---------------- */
/// Only one copy runs at a time; a second launch hands its job over through the inbox file.
pub fn single_instance() -> bool {
    let name = wide(r"Local\VestaEdgeSingleton");
    unsafe {
        let h = CreateMutexW(std::ptr::null(), 1, name.as_ptr());
        if h.is_null() {
            return true;
        }
        if GetLastError() == ERROR_ALREADY_EXISTS {
            CloseHandle(h);
            return false;
        }
        let _ = h; // the handle stays open for the life of the process, which is what holds the mutex
        true
    }
}

/* ---------------- Explorer right-click menu ---------------- */
// files, folders, drives, and the empty space inside a folder ("%V" = the folder itself)
const MENU_KEYS: [(&str, &str); 4] = [
    (r"Software\Classes\*\shell\VestaEdge", "%1"),
    (r"Software\Classes\Directory\shell\VestaEdge", "%1"),
    (r"Software\Classes\Drive\shell\VestaEdge", "%1"),
    (r"Software\Classes\Directory\Background\shell\VestaEdge", "%V"),
];

pub fn context_menu_enabled() -> bool {
    let k = wide(MENU_KEYS[0].0);
    unsafe { RegGetValueW(HKEY_CURRENT_USER, k.as_ptr(), std::ptr::null(), RRF_RT_REG_SZ, std::ptr::null_mut(), std::ptr::null_mut(), std::ptr::null_mut()) == 0 }
}

/// Adds "Add to Vesta Edge" to the right-click menu of files and folders (per user, no admin).
pub fn set_context_menu(on: bool) {
    let exe = std::env::current_exe().map(|p| p.display().to_string()).unwrap_or_default();
    for (key, arg) in MENU_KEYS {
        let k = wide(key);
        unsafe {
            if !on {
                RegDeleteTreeW(HKEY_CURRENT_USER, k.as_ptr());
                continue;
            }
            let set = |sub: &Vec<u16>, name: &str, data: &str| {
                let (n, d) = (wide(name), wide(data));
                let name = if name.is_empty() { std::ptr::null() } else { n.as_ptr() };
                RegSetKeyValueW(HKEY_CURRENT_USER, sub.as_ptr(), name, REG_SZ, d.as_ptr() as *const c_void, (d.len() * 2) as u32);
            };
            set(&k, "", "Add to Vesta Edge");
            set(&k, "Icon", &exe);
            set(&k, "Position", "Top"); // otherwise it lands at the bottom of a crowded menu
            let cmd = wide(&format!(r"{key}\command"));
            set(&cmd, "", &format!("\"{exe}\" --add \"{arg}\""));
        }
    }
}

/// Opens the clipboard for this app, waiting a moment if another app has it open.
fn open_clipboard() -> bool {
    let owner = SELF_HWND.load(Ordering::Relaxed) as HWND;
    for _ in 0..20 {
        if unsafe { OpenClipboard(owner) } != 0 {
            return true;
        }
        thread::sleep(Duration::from_millis(10));
    }
    false
}

/* ---------------- clipboard images ---------------- */
/// Reads a bitmap off the clipboard as (width, height, RGBA).
pub fn clip_image() -> Option<(u32, u32, Vec<u8>)> {
    unsafe {
        if !open_clipboard() {
            return None;
        }
        let h = GetClipboardData(CF_DIB as u32);
        let out = (!h.is_null()).then(|| dib_to_rgba(h as *const u8)).flatten();
        CloseClipboard();
        out
    }
}

unsafe fn dib_to_rgba(dib: *const u8) -> Option<(u32, u32, Vec<u8>)> {
    unsafe {
        let head = &*(dib as *const BITMAPINFOHEADER);
        let (w, h) = (head.biWidth, head.biHeight);
        if w <= 0 || h == 0 || w > 10_000 || h.abs() > 10_000 || head.biBitCount < 24 {
            return None;
        }
        let (w, flip, rows) = (w as u32, h > 0, h.unsigned_abs());
        let bpp = (head.biBitCount / 8) as usize;
        let stride = ((w as usize * bpp) + 3) & !3;
        let masks = if head.biCompression == BI_BITFIELDS { 12 } else { 0 };
        let pixels = dib.add(head.biSize as usize + masks + head.biClrUsed as usize * 4);
        let mut out = vec![0u8; (w * rows * 4) as usize];
        for y in 0..rows as usize {
            let src_row = if flip { rows as usize - 1 - y } else { y };
            let src = pixels.add(src_row * stride);
            for x in 0..w as usize {
                let p = src.add(x * bpp);
                let i = (y * w as usize + x) * 4;
                out[i] = *p.add(2);
                out[i + 1] = *p.add(1);
                out[i + 2] = *p;
                out[i + 3] = if bpp == 4 { (*p.add(3)).max(1) } else { 255 };
            }
        }
        Some((w, rows, out))
    }
}

/// Puts an image back on the clipboard (as a 32-bit DIB, which every app understands).
pub fn set_clip_image(w: u32, h: u32, rgba: &[u8]) -> bool {
    let header = std::mem::size_of::<BITMAPINFOHEADER>();
    let mut buf = vec![0u8; header + (w * h * 4) as usize];
    unsafe {
        let head = &mut *(buf.as_mut_ptr() as *mut BITMAPINFOHEADER);
        head.biSize = header as u32;
        head.biWidth = w as i32;
        head.biHeight = h as i32; // positive: rows bottom-up
        head.biPlanes = 1;
        head.biBitCount = 32;
        head.biCompression = BI_RGB;
        head.biSizeImage = w * h * 4;
        for y in 0..h as usize {
            let src = (h as usize - 1 - y) * w as usize * 4;
            for x in 0..w as usize {
                let (s, d) = (src + x * 4, header + (y * w as usize + x) * 4);
                buf[d] = rgba[s + 2];
                buf[d + 1] = rgba[s + 1];
                buf[d + 2] = rgba[s];
                buf[d + 3] = rgba[s + 3];
            }
        }
        let mem = GlobalAlloc(GMEM_MOVEABLE, buf.len());
        if mem.is_null() {
            return false;
        }
        let dst = GlobalLock(mem);
        std::ptr::copy_nonoverlapping(buf.as_ptr(), dst as *mut u8, buf.len());
        GlobalUnlock(mem);
        if !open_clipboard() {
            return false;
        }
        EmptyClipboard();
        // on success the clipboard owns the block; on failure Windows frees it when we exit
        let ok = !SetClipboardData(CF_DIB as u32, mem as HANDLE).is_null();
        CloseClipboard();
        ok
    }
}

/* ---------------- styled text (HTML + RTF beside the plain text) ---------------- */
/// Bigger than this and we skip the styling: the history should stay light.
const MAX_RICH: usize = 300_000;

/// Reads the styled formats of the current copy, if the app put any on the clipboard.
pub fn clip_rich() -> (Option<String>, Option<String>) {
    unsafe {
        if !open_clipboard() {
            return (None, None);
        }
        let html = read_format(format_id("HTML Format")).and_then(|raw| cf_html_fragment(&raw));
        let rtf = read_format(format_id("Rich Text Format")).map(|raw| rtf_text(strip_nul(&raw)));
        CloseClipboard();
        (html.filter(|h| h.len() <= MAX_RICH), rtf.filter(|r| r.len() <= MAX_RICH))
    }
}

/// The styling some apps keep only in their own formats, such as Telegram's text tags (Qt names
/// these formats after their MIME type). Pasting them back lets the same app restore its styling.
pub fn clip_private() -> Vec<(String, Vec<u8>)> {
    let mut out = Vec::new();
    unsafe {
        if !open_clipboard() {
            return out;
        }
        let mut format = EnumClipboardFormats(0);
        while format != 0 {
            let mut name = [0u16; 128];
            let n = GetClipboardFormatNameW(format, name.as_mut_ptr(), name.len() as i32).max(0) as usize;
            let name = String::from_utf16_lossy(&name[..n]);
            if name.starts_with("application/x-")
                && let Some(bytes) = read_format(format).filter(|b| b.len() <= MAX_RICH)
            {
                out.push((name, bytes));
            }
            format = EnumClipboardFormats(format);
        }
        CloseClipboard();
    }
    out
}

/// Puts plain text together with its styled formats, so the target app can pick the richest one.
pub fn set_clip_rich(text: &str, html: Option<&str>, rtf: Option<&str>, private: &[(String, Vec<u8>)]) -> bool {
    let wide_text: Vec<u8> = text.encode_utf16().chain(Some(0)).flat_map(|c| c.to_le_bytes()).collect();
    let html_bytes = html.map(cf_html);
    let rtf_bytes = rtf.map(|r| [r.as_bytes(), &[0u8]].concat());
    unsafe {
        if !open_clipboard() {
            return false;
        }
        EmptyClipboard();
        let mut ok = put_format(CF_UNICODETEXT as u32, &wide_text);
        if let Some(bytes) = &html_bytes {
            ok &= put_format(format_id("HTML Format"), bytes);
        }
        if let Some(bytes) = &rtf_bytes {
            ok &= put_format(format_id("Rich Text Format"), bytes);
        }
        for (name, bytes) in private {
            put_format(format_id(name), bytes);
        }
        CloseClipboard();
        ok
    }
}

fn format_id(name: &str) -> u32 {
    unsafe { RegisterClipboardFormatW(wide(name).as_ptr()) }
}

/// RTF is 7-bit text; bytes above that (some apps write their code page raw) become \'hh escapes,
/// which mean the same to every reader and survive the trip through a string.
fn rtf_text(raw: &[u8]) -> String {
    raw.iter().map(|&b| if b < 0x80 { (b as char).to_string() } else { format!("\\'{b:02x}") }).collect()
}

/// Copies the bytes of one clipboard format. The clipboard must be open.
unsafe fn read_format(format: u32) -> Option<Vec<u8>> {
    unsafe {
        let h = GetClipboardData(format);
        if h.is_null() {
            return None;
        }
        let size = GlobalSize(h as _);
        let ptr = GlobalLock(h as _) as *const u8;
        if ptr.is_null() {
            return None;
        }
        let bytes = std::slice::from_raw_parts(ptr, size.min(MAX_RICH * 4)).to_vec();
        GlobalUnlock(h as _);
        Some(bytes)
    }
}

/// Hands one format to the clipboard. The clipboard must be open and emptied.
unsafe fn put_format(format: u32, bytes: &[u8]) -> bool {
    unsafe {
        let mem = GlobalAlloc(GMEM_MOVEABLE, bytes.len());
        if mem.is_null() {
            return false;
        }
        let dst = GlobalLock(mem);
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), dst as *mut u8, bytes.len());
        GlobalUnlock(mem);
        // on success the clipboard owns the block
        !SetClipboardData(format, mem as HANDLE).is_null()
    }
}

fn strip_nul(bytes: &[u8]) -> &[u8] {
    let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    &bytes[..end]
}

/// The "HTML Format" clipboard layout: a text header with byte offsets, then the page.
/// Only the fragment between the two comments is the copied part, the rest is wrapper.
fn cf_html_fragment(raw: &[u8]) -> Option<String> {
    let header = String::from_utf8_lossy(&raw[..raw.len().min(400)]).into_owned();
    let field = |key: &str| {
        header
            .lines()
            .find_map(|line| line.strip_prefix(key))
            .and_then(|v| v.trim().parse::<usize>().ok())
    };
    let (start, end) = (field("StartFragment:")?, field("EndFragment:")?);
    let body = raw.get(start..end)?;
    Some(String::from_utf8_lossy(body).into_owned())
}

/// Wraps an HTML fragment in the "HTML Format" layout Windows expects, with exact byte offsets.
fn cf_html(fragment: &str) -> Vec<u8> {
    // the charset is UTF-8 by definition, but some readers only believe it when the page says so
    const PRE: &str = "<html>\r\n<head><meta http-equiv=\"Content-Type\" content=\"text/html; charset=utf-8\"></head>\r\n<body>\r\n<!--StartFragment-->";
    const POST: &str = "<!--EndFragment-->\r\n</body>\r\n</html>";
    // the offsets have a fixed width, so the header length is the same before and after filling them in
    let empty = "Version:0.9\r\nStartHTML:0000000000\r\nEndHTML:0000000000\r\nStartFragment:0000000000\r\nEndFragment:0000000000\r\n";
    let start_html = empty.len();
    let start_fragment = start_html + PRE.len();
    let end_fragment = start_fragment + fragment.len();
    let end_html = end_fragment + POST.len();
    let page = format!(
        "Version:0.9\r\nStartHTML:{start_html:010}\r\nEndHTML:{end_html:010}\r\nStartFragment:{start_fragment:010}\r\nEndFragment:{end_fragment:010}\r\n{PRE}{fragment}{POST}"
    );
    let mut bytes = page.into_bytes();
    bytes.push(0);
    bytes
}

/// Folder picker (the classic browse dialog: small and dependency-free).
pub fn pick_folder() -> Option<String> {
    let title = wide("Add a folder to Vesta Edge");
    unsafe {
        let mut bi: BROWSEINFOW = std::mem::zeroed();
        bi.lpszTitle = title.as_ptr();
        bi.ulFlags = BIF_RETURNONLYFSDIRS | BIF_NEWDIALOGSTYLE;
        let list = SHBrowseForFolderW(&mut bi);
        if list.is_null() {
            return None;
        }
        let mut buf = [0u16; 520];
        let ok = SHGetPathFromIDListW(list, buf.as_mut_ptr()) != 0;
        CoTaskMemFree(list as *const c_void);
        let len = buf.iter().position(|&c| c == 0).unwrap_or(0);
        ok.then(|| String::from_utf16_lossy(&buf[..len]))
    }
}

/* ---------------- the dock's clickable shape ---------------- */
/// Only these rectangles (physical px, window coordinates) take the mouse. The rest of the window is
/// click-through, so the transparent part of the square window never blocks the apps behind it.
pub fn set_regions(rects: &[(i32, i32, i32, i32)]) {
    let hwnd = SELF_HWND.load(Ordering::Relaxed) as HWND;
    if hwnd.is_null() || rects.is_empty() {
        return;
    }
    unsafe {
        // the system owns the region given to SetWindowRgn
        SetWindowRgn(hwnd, rects_region(rects), 1);
    }
}

/// A region made of the given rectangles (x, y, w, h).
unsafe fn rects_region(rects: &[(i32, i32, i32, i32)]) -> HRGN {
    unsafe {
        let combined = CreateRectRgn(0, 0, 0, 0);
        for &(x, y, w, h) in rects {
            let part = CreateRectRgn(x, y, x + w, y + h);
            CombineRgn(combined, combined, part, RGN_OR);
            let _ = DeleteObject(part);
        }
        combined
    }
}
/// The part of the primary monitor that the taskbar does not cover, as (x, y, w, h) in physical px.
pub fn work_area() -> Option<(i32, i32, i32, i32)> {
    let mut r = RECT { left: 0, top: 0, right: 0, bottom: 0 };
    let ok = unsafe { SystemParametersInfoW(SPI_GETWORKAREA, 0, &mut r as *mut RECT as *mut c_void, 0) };
    (ok != 0).then_some((r.left, r.top, r.right - r.left, r.bottom - r.top))
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cf_html_round_trip_keeps_the_fragment() {
        let fragment = "<b>Bold</b> <span style=\"color: #e53935\">سلام</span> <a href=\"https://vestait.ir\">link</a>";
        let raw = cf_html(fragment);
        assert_eq!(cf_html_fragment(&raw).as_deref(), Some(fragment));
    }

    #[test]
    fn cf_html_header_offsets_are_exact() {
        let raw = cf_html("<i>x</i>");
        let text = String::from_utf8_lossy(strip_nul(&raw)).into_owned();
        let field = |key: &str| text.lines().find_map(|l| l.strip_prefix(key)).unwrap().parse::<usize>().unwrap();
        let (start, end) = (field("StartFragment:"), field("EndFragment:"));
        assert_eq!(&raw[start..end], b"<i>x</i>");
        assert_eq!(field("StartHTML:"), text.find("<html>").unwrap());
    }

    #[test]
    fn clipboard_keeps_styled_formats() {
        let html = "<b>Bold</b> and <i>italic</i>";
        let private = [("application/x-vesta-test".to_string(), vec![0u8, 1, 2, 250])];
        assert!(set_clip_rich("Bold and italic", Some(html), Some("{\\rtf1 Bold}"), &private));
        let (got_html, got_rtf) = clip_rich();
        assert_eq!(got_html.as_deref(), Some(html));
        assert_eq!(got_rtf.as_deref(), Some("{\\rtf1 Bold}"));
        assert!(clip_private().iter().any(|(name, bytes)| name == "application/x-vesta-test" && bytes.starts_with(&[0, 1, 2, 250])));
    }

    #[test]
    fn rtf_keeps_raw_code_page_bytes() {
        assert_eq!(rtf_text(b"{\\rtf1 \xe3\xd1}"), "{\\rtf1 \\'e3\\'d1}");
    }
}
