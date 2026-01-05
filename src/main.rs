use std::path::Path;
use std::sync::atomic::{AtomicIsize, Ordering};
use chrono::{Local};
// 用于处理文件路径
use windows::{
    core::*,
    Win32::Foundation::{HWND, CloseHandle}, // 引入 CloseHandle
    Win32::UI::Accessibility::{SetWinEventHook, HWINEVENTHOOK},
    Win32::UI::WindowsAndMessaging::{
        DispatchMessageW, GetMessageW, GetWindowTextW, GetWindowThreadProcessId, 
        GetClassNameW, TranslateMessage, MSG, EVENT_SYSTEM_FOREGROUND, EVENT_SYSTEM_MINIMIZEEND, WINEVENT_OUTOFCONTEXT,
    },
    // 引入进程线程相关的 API
    Win32::System::Threading::{
        OpenProcess, QueryFullProcessImageNameW, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_NAME_WIN32
    },
};

// 定义一个全局变量存储上一次的 HWND
// HWND 在底层就是一个 isize (指针地址)
static LAST_HWND: AtomicIsize = AtomicIsize::new(0);

// -----------------------------------------------------------------------------
// 新增：获取进程名称的辅助函数
// -----------------------------------------------------------------------------
unsafe fn get_process_name(process_id: u32) -> String {
    // 1. 打开进程句柄
    // 使用 PROCESS_QUERY_LIMITED_INFORMATION 权限，这样可以访问受保护的进程（如系统服务）
    // 相比 PROCESS_ALL_ACCESS，这个权限更低，失败率更小
    let handle_result = unsafe {
        OpenProcess(
            PROCESS_QUERY_LIMITED_INFORMATION,
            false,
            process_id
        )
    };
    
    let handle = match handle_result {
        Ok(h) => h,
        Err(_) => return String::from("<Access Denied>"), // 可能是系统核心进程，无法访问
    };

    // 2. 获取进程完整路径
    let mut buffer = [0u16; 1024];
    let mut size = buffer.len() as u32;
    
    // 获取路径格式为 Win32 格式 (C:\Path\To\File.exe)
    let result = unsafe {
        QueryFullProcessImageNameW(
            handle,
            PROCESS_NAME_WIN32, 
            PWSTR(buffer.as_mut_ptr()),
            &mut size
        )
    };
    // 这里的句柄必须手动关闭，否则会内存泄漏
    let _ = unsafe {
        CloseHandle(handle)
    };

    if result.is_err() {
        return String::from("<Unknown>");
    }

    // 3. 解析路径并提取文件名
    let full_path = String::from_utf16_lossy(&buffer[..size as usize]);
    
    // 使用 Rust 标准库 Path 提取文件名 (例如 "C:\a\b\code.exe" -> "code.exe")
    Path::new(&full_path)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(&full_path) // 如果提取失败，返回完整路径
        .to_string()
}

// -----------------------------------------------------------------------------
// 原有的回调与处理逻辑
// -----------------------------------------------------------------------------
unsafe extern "system" fn win_event_proc(
    _h_win_event_hook: HWINEVENTHOOK,
    event: u32,
    hwnd: HWND,
    _id_object: i32,
    _id_child: i32,
    _id_event_thread: u32,
    _dwms_event_time: u32,
) {
    if event == EVENT_SYSTEM_FOREGROUND {
        unsafe { handle_foreground_change(hwnd) };
    }
    if event == EVENT_SYSTEM_MINIMIZEEND {
        println!("Minimize end event detected!");
        unsafe { handle_foreground_change(hwnd) };
    }
}

unsafe fn print_wnd_info(hwnd: HWND) {
    // 获取窗口标题
    let mut buffer = [0u16; 512];
    let len = unsafe {
        GetWindowTextW(hwnd, &mut buffer)
    };
    let title = if len > 0 {
        String::from_utf16_lossy(&buffer[..len as usize])
    } else {
        String::from("No Title")
    };

    // 2. >>> 新增：获取窗口类名 <<<
    let mut class_buf = [0u16; 512];
    let class_len = unsafe {
        GetClassNameW(hwnd, &mut class_buf)    
    };
    let class_name = if class_len > 0 {
        String::from_utf16_lossy(&class_buf[..class_len as usize])
    } else {
        String::from("Unknown")
    };

    // 获取进程 ID
    let mut process_id = 0;
    unsafe { GetWindowThreadProcessId(hwnd, Some(&mut process_id)); }

    // >>> 新增：调用辅助函数获取进程名 <<<
    let process_name = unsafe {
        get_process_name(process_id)
    };

    let local_now = Local::now();
    let formatted = local_now.format("%Y-%m-%d %H:%M:%S").to_string();
    println!("--------------------------{}------------------------", formatted);
    println!("Detected window switch!");
    println!("Process name: {}", process_name); // 例如: chrome.exe
    println!("Window title: {}", title);
    println!("Window class name: {}", class_name);
    println!("Process ID: {}", process_id);
    println!("Handle: {:?}", hwnd);
}

unsafe fn handle_foreground_change(_hwnd: HWND) {

    // ---------------------------------------------------------------
    // 步骤 1: 瞬态过滤 (防抖)
    // ---------------------------------------------------------------
    // 收到事件后，先休眠 50ms，让 Windows 完成窗口动画和焦点切换的中间状态
    // thread::sleep(Duration::from_millis(100));

    // 再次获取当前真正的“前台窗口”
    // let real_foreground_hwnd = unsafe {
    //     GetForegroundWindow()    
    // };

    // 3. 安全检查：如果获取不到句柄（比如锁屏时），直接退出
    if _hwnd.0 as isize == 0 {
        println!("The window handle is empty!");
        return;
    }

    // 1. 获取当前句柄的数值
    let current_val = _hwnd.0 as isize;

    // 2. 检查并更新句柄 (去重逻辑)
    // swap 方法会将 LAST_HWND 更新为 current_val，并返回旧值
    let last_val = LAST_HWND.swap(current_val, Ordering::Relaxed);

    // 3. 如果当前句柄 == 上一次句柄，说明是重复事件，直接返回，不打印也不上报
    if last_val == current_val {
        println!("It's the same as the previous foreground window handle: {:?}", _hwnd);
        return;
    }
    unsafe { print_wnd_info(_hwnd) };
}

fn main() -> Result<()> {
    println!("Starting window monitoring (with process name)... (Press Ctrl+C to exit)");

    unsafe {
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
            eprintln!("Failed to set hook!");
            return Ok(());
        }

        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
    Ok(())
}