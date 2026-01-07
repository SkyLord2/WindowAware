use std::path::Path;
use windows::{
    core::*,
    Win32::Foundation::{HWND, CloseHandle},
    Win32::UI::WindowsAndMessaging::{GetWindowTextW, GetWindowThreadProcessId},
    Win32::System::Threading::{
        OpenProcess, QueryFullProcessImageNameW, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_NAME_WIN32
    },
};

// =============================================================================
// 辅助函数
// =============================================================================

pub unsafe fn get_process_name(process_id: u32) -> String {
    let handle_result = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, process_id) };
    let handle = match handle_result {
        Ok(h) => h,
        Err(_) => return String::from("<Access Denied>"),
    };

    let mut buffer = [0u16; 1024];
    let mut size = buffer.len() as u32;
    let result = unsafe { QueryFullProcessImageNameW(handle, PROCESS_NAME_WIN32, PWSTR(buffer.as_mut_ptr()), &mut size) };
    let _ = unsafe { CloseHandle(handle) };

    if result.is_err() {
        return String::from("<Unknown>");
    }

    let full_path = String::from_utf16_lossy(&buffer[..size as usize]);
    Path::new(&full_path).file_name().and_then(|name| name.to_str()).unwrap_or(&full_path).to_string()
}

pub unsafe fn get_window_details(hwnd_val: isize) -> String {
    let hwnd = HWND(hwnd_val as *mut _);
    
    let mut buffer = [0u16; 512];
    let len = unsafe { GetWindowTextW(hwnd, &mut buffer) };
    let title = if len > 0 { String::from_utf16_lossy(&buffer[..len as usize]) } else { String::from("No Title") };

    let mut process_id = 0;
    unsafe { GetWindowThreadProcessId(hwnd, Some(&mut process_id)) };
    let process_name = unsafe { get_process_name(process_id) };

    format!("[{}] (PID: {}) - {}", process_name, process_id, title)
}