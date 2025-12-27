use std::path::Path;
// 用于处理文件路径
use windows::{
    core::*,
    Win32::Foundation::{HWND, CloseHandle}, // 引入 CloseHandle
    Win32::UI::Accessibility::{SetWinEventHook, HWINEVENTHOOK},
    Win32::UI::WindowsAndMessaging::{
        DispatchMessageW, GetMessageW, GetWindowTextW, GetWindowThreadProcessId, 
        TranslateMessage, MSG, EVENT_SYSTEM_FOREGROUND, WINEVENT_OUTOFCONTEXT
    },
    // 引入进程线程相关的 API
    Win32::System::Threading::{
        OpenProcess, QueryFullProcessImageNameW, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_NAME_WIN32
    },
};

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
}

unsafe fn handle_foreground_change(hwnd: HWND) {
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

    // 获取进程 ID
    let mut process_id = 0;
    unsafe { GetWindowThreadProcessId(hwnd, Some(&mut process_id)); }

    // >>> 新增：调用辅助函数获取进程名 <<<
    let process_name = unsafe {
        get_process_name(process_id)
    };

    println!("--------------------------------------------------");
    println!("检测到窗口切换!");
    println!("程序名称: {}", process_name); // 例如: chrome.exe
    println!("窗口标题: {}", title);
    println!("进程 ID : {}", process_id);
    println!("句柄    : {:?}", hwnd);
}

fn main() -> Result<()> {
    println!("正在启动窗口监控 (含进程名)... (按 Ctrl+C 退出)");

    unsafe {
        let hook = SetWinEventHook(
            EVENT_SYSTEM_FOREGROUND,
            EVENT_SYSTEM_FOREGROUND,
            None,
            Some(win_event_proc),
            0,
            0,
            WINEVENT_OUTOFCONTEXT,
        );

        if hook.is_invalid() {
            eprintln!("设置钩子失败！");
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