//! 真实鼠标键盘输入（SendInput）与窗口控制。坐标一律是虚拟屏幕物理像素。

use anyhow::{anyhow, bail, Result};
use std::time::Duration;
use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, HWND};
use windows_sys::Win32::Security::{GetTokenInformation, TokenElevation, TOKEN_ELEVATION, TOKEN_QUERY};
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcess, OpenProcessToken, PROCESS_QUERY_LIMITED_INFORMATION};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::*;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    GetForegroundWindow, GetWindowThreadProcessId, IsIconic, IsWindow, IsZoomed, MoveWindow, PostMessageW, SetCursorPos,
    SetForegroundWindow, ShowWindow, SW_MAXIMIZE, SW_MINIMIZE, SW_RESTORE, WM_CLOSE,
};

fn send(inputs: &[INPUT]) -> Result<()> {
    let n = unsafe { SendInput(inputs.len() as u32, inputs.as_ptr(), std::mem::size_of::<INPUT>() as i32) };
    if n as usize != inputs.len() {
        bail!("系统拒绝了模拟输入（目标可能是管理员权限窗口，或当前处于锁屏/UAC 安全桌面）");
    }
    Ok(())
}

fn key_input(vk: u16, scan: u16, flags: u32) -> INPUT {
    let mut i: INPUT = unsafe { std::mem::zeroed() };
    i.r#type = INPUT_KEYBOARD;
    i.Anonymous.ki = KEYBDINPUT { wVk: vk, wScan: scan, dwFlags: flags, time: 0, dwExtraInfo: 0 };
    i
}

fn mouse_input(flags: u32, data: u32) -> INPUT {
    let mut i: INPUT = unsafe { std::mem::zeroed() };
    i.r#type = INPUT_MOUSE;
    i.Anonymous.mi = MOUSEINPUT { dx: 0, dy: 0, mouseData: data, dwFlags: flags, time: 0, dwExtraInfo: 0 };
    i
}

pub fn move_to(x: i32, y: i32) -> Result<()> {
    let ok = crate::recorder::screenshot::win::dpi_aware(|| unsafe { SetCursorPos(x, y) });
    if ok == 0 {
        bail!("无法移动鼠标到 ({x}, {y})");
    }
    std::thread::sleep(Duration::from_millis(30));
    Ok(())
}

#[derive(Clone, Copy)]
pub enum Button {
    Left,
    Right,
    Middle,
}

pub fn click(x: i32, y: i32, button: Button, count: u32) -> Result<()> {
    move_to(x, y)?;
    let (down, up) = match button {
        Button::Left => (MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP),
        Button::Right => (MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP),
        Button::Middle => (MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP),
    };
    for i in 0..count.max(1) {
        if i > 0 {
            std::thread::sleep(Duration::from_millis(60));
        }
        send(&[mouse_input(down, 0), mouse_input(up, 0)])?;
    }
    Ok(())
}

pub fn drag(from: (i32, i32), to: (i32, i32)) -> Result<()> {
    move_to(from.0, from.1)?;
    send(&[mouse_input(MOUSEEVENTF_LEFTDOWN, 0)])?;
    let steps = 12;
    for s in 1..=steps {
        let x = from.0 + (to.0 - from.0) * s / steps;
        let y = from.1 + (to.1 - from.1) * s / steps;
        move_to(x, y)?;
    }
    send(&[mouse_input(MOUSEEVENTF_LEFTUP, 0)])
}

/// 滚轮：正数向上/向左，单位为「格」
pub fn wheel(x: i32, y: i32, vertical: i32, horizontal: i32) -> Result<()> {
    move_to(x, y)?;
    if vertical != 0 {
        send(&[mouse_input(MOUSEEVENTF_WHEEL, (vertical * 120) as u32)])?;
    }
    if horizontal != 0 {
        send(&[mouse_input(MOUSEEVENTF_HWHEEL, (-horizontal * 120) as u32)])?;
    }
    Ok(())
}

/// 逐字输入 Unicode 文本；换行发 Enter，制表符发 Tab
pub fn type_text(text: &str) -> Result<()> {
    for ch in text.chars() {
        match ch {
            '\r' => continue,
            '\n' => tap(VK_RETURN)?,
            '\t' => tap(VK_TAB)?,
            _ => {
                let mut buf = [0u16; 2];
                let mut inputs = Vec::new();
                for unit in ch.encode_utf16(&mut buf).iter() {
                    inputs.push(key_input(0, *unit, KEYEVENTF_UNICODE));
                    inputs.push(key_input(0, *unit, KEYEVENTF_UNICODE | KEYEVENTF_KEYUP));
                }
                send(&inputs)?;
            }
        }
        std::thread::sleep(Duration::from_millis(4));
    }
    Ok(())
}

fn extended(vk: u16) -> bool {
    matches!(
        vk,
        VK_LEFT | VK_RIGHT | VK_UP | VK_DOWN | VK_HOME | VK_END | VK_PRIOR | VK_NEXT | VK_INSERT | VK_DELETE | VK_LWIN | VK_RWIN | VK_APPS
    )
}

fn tap(vk: u16) -> Result<()> {
    let ext = if extended(vk) { KEYEVENTF_EXTENDEDKEY } else { 0 };
    send(&[key_input(vk, 0, ext), key_input(vk, 0, ext | KEYEVENTF_KEYUP)])
}

/// 解析一个按键名
pub fn vk_of(name: &str) -> Option<u16> {
    let n = name.trim().to_lowercase();
    let vk = match n.as_str() {
        "ctrl" | "control" => VK_CONTROL,
        "shift" => VK_SHIFT,
        "alt" | "menu" => VK_MENU,
        "win" | "windows" | "meta" | "cmd" | "super" => VK_LWIN,
        "enter" | "return" => VK_RETURN,
        "esc" | "escape" => VK_ESCAPE,
        "tab" => VK_TAB,
        "space" | "空格" => VK_SPACE,
        "backspace" | "back" => VK_BACK,
        "delete" | "del" => VK_DELETE,
        "insert" | "ins" => VK_INSERT,
        "home" => VK_HOME,
        "end" => VK_END,
        "pageup" | "pgup" => VK_PRIOR,
        "pagedown" | "pgdn" => VK_NEXT,
        "up" | "arrowup" => VK_UP,
        "down" | "arrowdown" => VK_DOWN,
        "left" | "arrowleft" => VK_LEFT,
        "right" | "arrowright" => VK_RIGHT,
        "apps" | "contextmenu" => VK_APPS,
        "printscreen" | "prtsc" => VK_SNAPSHOT,
        "capslock" => VK_CAPITAL,
        "plus" => VK_OEM_PLUS,
        "minus" => VK_OEM_MINUS,
        "comma" => VK_OEM_COMMA,
        "period" => VK_OEM_PERIOD,
        _ => {
            if let Some(num) = n.strip_prefix('f').and_then(|d| d.parse::<u16>().ok()) {
                if (1..=24).contains(&num) {
                    return Some(VK_F1 + num - 1);
                }
            }
            let mut chars = n.chars();
            let (Some(c), None) = (chars.next(), chars.next()) else { return None };
            return match c {
                'a'..='z' => Some(c.to_ascii_uppercase() as u16),
                '0'..='9' => Some(c as u16),
                '=' => Some(VK_OEM_PLUS),
                '-' => Some(VK_OEM_MINUS),
                ',' => Some(VK_OEM_COMMA),
                '.' => Some(VK_OEM_PERIOD),
                '/' => Some(VK_OEM_2),
                ';' => Some(VK_OEM_1),
                '[' => Some(VK_OEM_4),
                ']' => Some(VK_OEM_6),
                '\\' => Some(VK_OEM_5),
                '\'' => Some(VK_OEM_7),
                '`' => Some(VK_OEM_3),
                _ => None,
            };
        }
    };
    Some(vk)
}

/// 组合键，如 "Ctrl+S"、"Alt+F4"、"Ctrl+Shift+Esc"；多个组合用空格分隔依次按，如 "Alt F"
pub fn press(combo: &str) -> Result<()> {
    for chord in combo.split_whitespace() {
        let keys: Vec<u16> = chord
            .split('+')
            .filter(|k| !k.trim().is_empty())
            .map(|k| vk_of(k).ok_or_else(|| anyhow!("不认识的按键「{k}」（可用 Ctrl/Shift/Alt/Win、Enter、Esc、Tab、F1-F24、方向键、字母数字等）")))
            .collect::<Result<_>>()?;
        if keys.is_empty() {
            continue;
        }
        let mut inputs = Vec::new();
        for &vk in &keys {
            inputs.push(key_input(vk, 0, if extended(vk) { KEYEVENTF_EXTENDEDKEY } else { 0 }));
        }
        for &vk in keys.iter().rev() {
            inputs.push(key_input(vk, 0, KEYEVENTF_KEYUP | if extended(vk) { KEYEVENTF_EXTENDEDKEY } else { 0 }));
        }
        send(&inputs)?;
        std::thread::sleep(Duration::from_millis(60));
    }
    Ok(())
}

// ---------- 窗口 ----------

pub fn foreground() -> isize {
    unsafe { GetForegroundWindow() as isize }
}

pub fn exists(hwnd: isize) -> bool {
    unsafe { IsWindow(hwnd as HWND) != 0 }
}

/// 切到前台。Windows 限制后台进程抢焦点：先发一次 Alt 解除限制再 SetForegroundWindow。
pub fn activate(hwnd: isize) -> Result<()> {
    let h = hwnd as HWND;
    unsafe {
        if IsWindow(h) == 0 {
            bail!("窗口已关闭");
        }
        if IsIconic(h) != 0 {
            ShowWindow(h, SW_RESTORE);
        }
        if GetForegroundWindow() == h {
            return Ok(());
        }
        for attempt in 0..3 {
            if attempt > 0 || SetForegroundWindow(h) == 0 {
                let _ = send(&[key_input(VK_MENU, 0, 0), key_input(VK_MENU, 0, KEYEVENTF_KEYUP)]);
                SetForegroundWindow(h);
            }
            std::thread::sleep(Duration::from_millis(120));
            if GetForegroundWindow() == h {
                return Ok(());
            }
        }
    }
    bail!("无法把窗口切到前台（可能被系统阻止或有模态对话框挡着），请检查窗口状态")
}

pub fn show(hwnd: isize, how: &str) -> Result<()> {
    let cmd = match how {
        "minimize" => SW_MINIMIZE,
        "maximize" => SW_MAXIMIZE,
        "restore" => SW_RESTORE,
        _ => bail!("未知窗口状态 {how}"),
    };
    unsafe { ShowWindow(hwnd as HWND, cmd) };
    Ok(())
}

/// 唤出隐藏（缩在托盘）的窗口并切到前台
pub fn unhide(hwnd: isize) -> Result<()> {
    use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOW;
    unsafe {
        if IsWindow(hwnd as HWND) == 0 {
            bail!("窗口已关闭");
        }
        ShowWindow(hwnd as HWND, SW_SHOW);
    }
    activate(hwnd)
}

pub fn is_maximized(hwnd: isize) -> bool {
    unsafe { IsZoomed(hwnd as HWND) != 0 }
}

pub fn close(hwnd: isize) -> Result<()> {
    if unsafe { PostMessageW(hwnd as HWND, WM_CLOSE, 0, 0) } == 0 {
        bail!("发送关闭消息失败");
    }
    Ok(())
}

pub fn move_window(hwnd: isize, [x, y, w, h]: [i32; 4]) -> Result<()> {
    let ok = crate::recorder::screenshot::win::dpi_aware(|| unsafe {
        if IsIconic(hwnd as HWND) != 0 || IsZoomed(hwnd as HWND) != 0 {
            ShowWindow(hwnd as HWND, SW_RESTORE);
        }
        MoveWindow(hwnd as HWND, x, y, w, h, 1)
    });
    if ok == 0 {
        bail!("移动窗口失败");
    }
    Ok(())
}

pub fn window_pid(hwnd: isize) -> u32 {
    let mut pid = 0u32;
    unsafe { GetWindowThreadProcessId(hwnd as HWND, &mut pid) };
    pid
}

fn token_elevated(process: HANDLE) -> Option<bool> {
    unsafe {
        let mut token: HANDLE = std::ptr::null_mut();
        if OpenProcessToken(process, TOKEN_QUERY, &mut token) == 0 {
            return None;
        }
        let mut info: TOKEN_ELEVATION = std::mem::zeroed();
        let mut len = 0u32;
        let ok = GetTokenInformation(
            token,
            TokenElevation,
            &mut info as *mut _ as *mut _,
            std::mem::size_of::<TOKEN_ELEVATION>() as u32,
            &mut len,
        );
        CloseHandle(token);
        (ok != 0).then_some(info.TokenIsElevated != 0)
    }
}

/// 目标进程是管理员权限而拾光不是：系统会挡掉模拟输入和大部分 UI Automation 操作
pub fn blocked_by_elevation(pid: u32) -> bool {
    let me = token_elevated(unsafe { GetCurrentProcess() }).unwrap_or(false);
    if me {
        return false;
    }
    unsafe {
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if h.is_null() {
            return false;
        }
        let elevated = token_elevated(h);
        CloseHandle(h);
        // 打不开令牌通常也意味着权限更高
        elevated.unwrap_or(true)
    }
}

/// 用系统默认方式打开程序/文件/网址（等同于「运行」对话框）
pub fn shell_open(target: &str, args: Option<&str>, dir: Option<&str>) -> Result<()> {
    use windows_sys::Win32::UI::Shell::ShellExecuteW;
    use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
    let wide = |s: &str| s.encode_utf16().chain(std::iter::once(0)).collect::<Vec<u16>>();
    let op = wide("open");
    let file = wide(target);
    let params = args.map(wide);
    let dir = dir.map(wide);
    let r = unsafe {
        ShellExecuteW(
            std::ptr::null_mut(),
            op.as_ptr(),
            file.as_ptr(),
            params.as_ref().map_or(std::ptr::null(), |p| p.as_ptr()),
            dir.as_ref().map_or(std::ptr::null(), |p| p.as_ptr()),
            SW_SHOWNORMAL,
        )
    } as isize;
    if r <= 32 {
        bail!("打开「{target}」失败（错误码 {r}）：确认程序名、路径或网址是否正确");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_names_resolve() {
        assert_eq!(vk_of("Ctrl"), Some(VK_CONTROL));
        assert_eq!(vk_of("f5"), Some(VK_F5));
        assert_eq!(vk_of("a"), Some(b'A' as u16));
        assert_eq!(vk_of("7"), Some(b'7' as u16));
        assert_eq!(vk_of("PageDown"), Some(VK_NEXT));
        assert_eq!(vk_of("f25"), None);
        assert_eq!(vk_of("nope"), None);
    }
}
