mod state;
mod utils;
mod analysis;
mod hooks;
mod global;

use std::sync::Mutex;
use std::time::{Instant, Duration};
use windows::{
    core::*,
    Win32::Foundation::HINSTANCE,
    Win32::UI::Accessibility::{SetWinEventHook},
    Win32::UI::WindowsAndMessaging::{
        DispatchMessageW, GetMessageW, TranslateMessage, MSG, 
        EVENT_SYSTEM_FOREGROUND, EVENT_SYSTEM_MINIMIZEEND, WINEVENT_OUTOFCONTEXT,
        SetWindowsHookExW, UnhookWindowsHookEx, WH_KEYBOARD_LL, WH_MOUSE_LL, 
    },
    Win32::System::LibraryLoader::GetModuleHandleW,
};

use crate::global::LAST_INPUT_EVENT;
use crate::hooks::{keyboard_hook_proc, mouse_hook_proc, win_event_proc};

fn main() -> Result<()> {
    // 初始化全局输入记录器，默认时间设为很久以前，避免启动误判
    LAST_INPUT_EVENT.get_or_init(|| Mutex::new((String::from("Ready"), Instant::now() - Duration::from_secs(100))));

    println!("Starting Smart Window Monitor...");
    println!("Rules applied:");
    println!("  1. Real-time:  Oscillation A-B-A-B >= 4 times (60s)");
    println!("  2. Short-term: Avg > 15 switches/min (Window: Last 5 mins)");
    println!("  3. Daily:      Avg > 20 switches/min (Window: Last 24 hours)");
    println!("  4. Input:      Accurate Alt+Tab vs Mouse Click detection via Hooks");
    println!("(Press Ctrl+C to exit)\n");

    unsafe {
        // 安装键盘和鼠标钩子
        // 修复：类型转换错误 (HMODULE -> HINSTANCE)
        // GetModuleHandleW 返回 HMODULE，SetWindowsHookExW 需要 Option<HINSTANCE>
        // 在 Windows API 中，HINSTANCE 和 HMODULE 通常是兼容的，可以通过值转换
        let module_handle = GetModuleHandleW(None).unwrap_or_default();
        let h_instance = HINSTANCE(module_handle.0); // 转换
        
        // WH_KEYBOARD_LL 和 WH_MOUSE_LL 是低级全局钩子，不需要注入 DLL
        let kbd_hook = SetWindowsHookExW(WH_KEYBOARD_LL, Some(keyboard_hook_proc), Some(h_instance), 0);
        let mouse_hook = SetWindowsHookExW(WH_MOUSE_LL, Some(mouse_hook_proc), Some(h_instance), 0);

        if kbd_hook.is_err() || mouse_hook.is_err() {
            eprintln!("Warning: Failed to install input hooks. Input detection might not work.");
        }

        let hook = SetWinEventHook(
            EVENT_SYSTEM_FOREGROUND,
            EVENT_SYSTEM_MINIMIZEEND,
            None,
            Some(win_event_proc),
            0,
            0,
            WINEVENT_OUTOFCONTEXT,
        );

        if hook.is_invalid() {
            eprintln!("Failed to set win event hook!");
            return Ok(());
        }

        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }

        // 退出前清理钩子
        if let Ok(h) = kbd_hook { let _ = UnhookWindowsHookEx(h); }
        if let Ok(h) = mouse_hook { let _ = UnhookWindowsHookEx(h); }
    }
    Ok(())
}