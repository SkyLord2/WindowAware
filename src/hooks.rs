use std::time::{Instant, Duration};
use std::sync::atomic::Ordering;
use chrono::Local;

use windows::{
    Win32::Foundation::{HWND, LPARAM, WPARAM, LRESULT},
    Win32::UI::Accessibility::HWINEVENTHOOK,
    Win32::UI::WindowsAndMessaging::{
        GetWindowTextW, GetWindowThreadProcessId, CallNextHookEx,
        GetClassNameW, EVENT_SYSTEM_FOREGROUND, EVENT_SYSTEM_MINIMIZEEND, 
        KBDLLHOOKSTRUCT, WM_LBUTTONDOWN, WM_SYSKEYDOWN, HC_ACTION,
    },
    Win32::UI::Input::KeyboardAndMouse::VK_TAB,
};

use crate::global::{LAST_HWND, LAST_INPUT_EVENT};
use crate::utils::get_process_name;
use crate::analysis::analyze_behavior;

// =============================================================================
// 钩子回调与辅助函数
// =============================================================================

// [MODIFIED] 键盘钩子：监听 Alt+Tab 和 其他按键
pub unsafe extern "system" fn keyboard_hook_proc(n_code: i32, w_param: WPARAM, l_param: LPARAM) -> LRESULT {
    if n_code == HC_ACTION as i32 {
        let w_param_u32 = w_param.0 as u32;
        
        // 修复：解引用裸指针需要 unsafe 块
        let vk_code = unsafe { (*(l_param.0 as *const KBDLLHOOKSTRUCT)).vkCode as u16 };

        // WM_SYSKEYDOWN 专门捕获 Alt 组合键 (例如 Alt+Tab)
        if w_param_u32 == WM_SYSKEYDOWN {
            if vk_code == VK_TAB.0 {
                // 更新全局状态：检测到 Alt+Tab
                if let Some(mutex) = LAST_INPUT_EVENT.get() {
                    if let Ok(mut data) = mutex.lock() {
                        *data = (String::from("Keyboard (Alt+Tab)"), Instant::now());
                    }
                }
            }
        }
    }
    // 修复：调用 unsafe 函数需要 unsafe 块
    unsafe { CallNextHookEx(None, n_code, w_param, l_param) }
}

// [MODIFIED] 鼠标钩子：监听左键点击
pub unsafe extern "system" fn mouse_hook_proc(n_code: i32, w_param: WPARAM, l_param: LPARAM) -> LRESULT {
    if n_code == HC_ACTION as i32 {
        // 如果检测到左键按下
        if w_param.0 as u32 == WM_LBUTTONDOWN {
             if let Some(mutex) = LAST_INPUT_EVENT.get() {
                if let Ok(mut data) = mutex.lock() {
                    *data = (String::from("Mouse Click"), Instant::now());
                }
            }
        }
    }
    // 修复：调用 unsafe 函数需要 unsafe 块
    unsafe { CallNextHookEx(None, n_code, w_param, l_param) }
}

unsafe fn print_wnd_info(hwnd: HWND) {
    let mut buffer = [0u16; 512];
    let len = unsafe { GetWindowTextW(hwnd, &mut buffer) };
    let title = if len > 0 { String::from_utf16_lossy(&buffer[..len as usize]) } else { String::from("No Title") };

    let mut class_buf = [0u16; 512];
    let class_len = unsafe { GetClassNameW(hwnd, &mut class_buf) };
    let class_name = if class_len > 0 { String::from_utf16_lossy(&class_buf[..class_len as usize]) } else { String::from("Unknown") };

    let mut process_id = 0;
    unsafe { GetWindowThreadProcessId(hwnd, Some(&mut process_id)) };
    let process_name = unsafe { get_process_name(process_id) };

    // [NEW] 获取钩子记录的最后一次输入源，并检查时间有效性
    let mut input_source = String::from("Keyboard (Shortcut/Other)");
    if let Some(mutex) = LAST_INPUT_EVENT.get() {
        if let Ok(data) = mutex.lock() {
            let (source, time) = &*data;
            // 只有当输入事件发生在最近 1000ms 内，才认为是由于该输入导致的切换
            // 否则可能是系统自动弹窗，或者上一次记录已经过时
            if time.elapsed() < Duration::from_millis(1000) {
                input_source = source.clone();
            }
        }
    }

    let local_now = Local::now();
    let formatted = local_now.format("%Y-%m-%d %H:%M:%S").to_string();
    println!("--------------------------{}------------------------", formatted);
    println!("Process: {} | PID: {}", process_name, process_id);
    println!("Title:   {}", title);
    println!("Class:   {}", class_name);
    
    // 传入输入源信息
    analyze_behavior(hwnd.0 as isize, &process_name, &input_source);
}

unsafe fn handle_foreground_change(_hwnd: HWND) {
    if _hwnd.0 as isize == 0 { return; }

    let current_val = _hwnd.0 as isize;
    let last_val = LAST_HWND.swap(current_val, Ordering::Relaxed);
    if last_val == current_val { return; }

    unsafe { print_wnd_info(_hwnd) };
}

pub unsafe extern "system" fn win_event_proc(
    _h_win_event_hook: HWINEVENTHOOK,
    event: u32,
    hwnd: HWND,
    _id_object: i32,
    _id_child: i32,
    _id_event_thread: u32,
    _dwms_event_time: u32,
) {
    if event == EVENT_SYSTEM_FOREGROUND || event == EVENT_SYSTEM_MINIMIZEEND {
        unsafe { handle_foreground_change(hwnd) };
    }
}