use std::collections::{VecDeque, HashMap, HashSet};
use std::path::Path;
use std::sync::atomic::{AtomicIsize, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Instant, Duration}; // [NEW] 引入时间处理
use chrono::{DateTime, Local};

use windows::{
    core::*,
    Win32::Foundation::{HWND, CloseHandle, LPARAM, WPARAM, LRESULT, HINSTANCE},
    Win32::UI::Accessibility::{SetWinEventHook, HWINEVENTHOOK},
    // 引入钩子相关的 API
    Win32::UI::WindowsAndMessaging::{
        DispatchMessageW, GetMessageW, GetWindowTextW, GetWindowThreadProcessId, 
        GetClassNameW, TranslateMessage, MSG, EVENT_SYSTEM_FOREGROUND, EVENT_SYSTEM_MINIMIZEEND, WINEVENT_OUTOFCONTEXT,
        SetWindowsHookExW, UnhookWindowsHookEx, CallNextHookEx, WH_KEYBOARD_LL, WH_MOUSE_LL, 
        KBDLLHOOKSTRUCT, WM_LBUTTONDOWN, WM_SYSKEYDOWN, HC_ACTION,
    },
    Win32::UI::Input::KeyboardAndMouse::{VK_TAB},
    Win32::System::Threading::{
        OpenProcess, QueryFullProcessImageNameW, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_NAME_WIN32
    },
    Win32::System::LibraryLoader::GetModuleHandleW,
};

// =============================================================================
// 数据结构与全局状态
// =============================================================================

#[derive(Clone, Debug)]
struct SwitchEvent {
    hwnd: isize,
    timestamp: DateTime<Local>,
}

struct WindowStats {
    first_seen: DateTime<Local>,
    // 记录该窗口累计在前台的总时长 (毫秒)
    total_duration_ms: i64, 
}

struct MonitorState {
    history: VecDeque<SwitchEvent>,
    daily_stats: HashMap<isize, WindowStats>, 
    // 记录上一次切换发生的时间，用于计算停留时长
    last_switch_time: Option<DateTime<Local>>,
    // 记录上一个处于前台的窗口句柄
    last_active_hwnd: Option<isize>,
}

impl MonitorState {
    // 历史记录必须保留 24 小时 (1440 分钟)，原本是 5 分钟
    const HISTORY_LIMIT_MINUTES: i64 = 24 * 60;

    fn new() -> Self {
        Self {
            history: VecDeque::new(),
            daily_stats: HashMap::new(),
            // 初始化为空
            last_switch_time: None,
            last_active_hwnd: None,
        }
    }

    fn add_event(&mut self, hwnd: isize) {
        let now = Local::now();

        // 计算并更新 **上一个** 窗口的停留时长
        // 逻辑：当前时间 - 上次切换时间 = 上一个窗口在前台停留的时间
        if let Some(last_time) = self.last_switch_time {
            if let Some(last_hwnd) = self.last_active_hwnd {
                let duration = now.signed_duration_since(last_time).num_milliseconds();
                
                // 更新上一个窗口的统计信息
                self.daily_stats.entry(last_hwnd)
                    .and_modify(|stats| stats.total_duration_ms += duration)
                    .or_insert(WindowStats {
                        first_seen: last_time,
                        total_duration_ms: duration,
                    });
            }
        }

        // 更新状态，将当前窗口标记为活跃窗口，并记录开始时间
        self.last_switch_time = Some(now);
        self.last_active_hwnd = Some(hwnd);
        
        // 1. 添加新事件
        self.history.push_back(SwitchEvent { hwnd, timestamp: now });
        
        // 2. 清理过期数据 (超过 24 小时的数据)
        while let Some(front) = self.history.front() {
            if now.signed_duration_since(front.timestamp).num_minutes() >= Self::HISTORY_LIMIT_MINUTES {
                self.history.pop_front();
            } else {
                break;
            }
        }

        // 3. 记录首次出现时间 (用于计算分母)
        // 注意：如果是新窗口，total_duration_ms 初始化为 0，因为它是刚切进来的
        self.daily_stats.entry(hwnd)
            .or_insert(WindowStats {
                first_seen: now,
                total_duration_ms: 0, 
            });
    }

    // 规则 1: 实时感知 (60秒内 A-B-A-B 震荡)
    fn check_oscillation(&self, current_hwnd: isize) -> Option<(isize, isize)> {
        let now = Local::now();
        // 因为 history 现在很长，使用 rev() 从最新的数据开始查找效率更高
        let recent_events: Vec<&SwitchEvent> = self.history.iter()
            .rev()
            .take_while(|e| now.signed_duration_since(e.timestamp).num_seconds() <= 60)
            .collect();

        if recent_events.len() < 5 { return None; }

        let event_a1 = recent_events[0];
        let event_b1 = recent_events[1];
        let event_a2 = recent_events[2];
        let event_b2 = recent_events[3];
        let event_a3 = recent_events[4];

        if event_a1.hwnd == current_hwnd 
           && event_a1.hwnd == event_a2.hwnd 
           && event_a1.hwnd == event_a3.hwnd
           && event_b1.hwnd == event_b2.hwnd
           && event_a1.hwnd != event_b1.hwnd 
        {
             return Some((event_a1.hwnd, event_b1.hwnd));
        }
        None
    }

    // 模式识别：判断当前是震荡模式 (A-B) 还是随机模式 (A-C-B-D)
    fn identify_pattern(&self, current_hwnd: isize) -> String {
        // 1. 优先检测震荡 (Strict Oscillation)
        if self.check_oscillation(current_hwnd).is_some() {
            return String::from("Oscillation (A-B-A-B)");
        }

        // 2. 检测随机模式 (Random / Multitasking)
        // 逻辑：取最近的 10 次切换记录，如果其中包含了 3 个或以上的唯一窗口，则认为是随机多任务模式
        let check_depth = 10;
        let recent_events: Vec<isize> = self.history.iter()
            .rev()
            .take(check_depth)
            .map(|e| e.hwnd)
            .collect();

        // 如果数据不足，无法判断
        if recent_events.len() < 3 {
            return String::from("Gathering Data...");
        }

        // 使用 HashSet 统计唯一窗口数
        let unique_count = recent_events.iter().collect::<HashSet<_>>().len();

        if unique_count >= 3 {
            return String::from("Random (Multitasking)");
        } else if unique_count == 2 {
            return String::from("Oscillation (A-B-A-B)");
        } else {
            return String::from("Focused (Single Window)");
        }
    }

    // 辅助计算频率的通用函数
    // limit_minutes: 统计的时间窗口大小（例如 5 或 1440）
    fn calculate_frequency(&self, hwnd: isize, limit_minutes: i64) -> f64 {
        let now = Local::now();

        // 1. 确定分母（有效监控时长）
        // 如果窗口是 1 分钟前第一次出现的，即使我们算“过去24小时平均”，分母也应该是 1 分钟，而不是 1440。
        let mut duration_min = 0.0;
        if let Some(stats) = self.daily_stats.get(&hwnd) {
            let duration_since_first = now.signed_duration_since(stats.first_seen).num_seconds() as f64 / 60.0;
            // 分母 = min(窗口首次出现至今的时长, 指定的时间窗口上限)
            duration_min = duration_since_first.min(limit_minutes as f64);
        }

        // 防止除以 0，且如果监控时间太短（小于 0.1 分钟），数据不稳定，返回 0
        if duration_min < 0.1 {
            return 0.0;
        }

        // 2. 确定分子（在该时间窗口内的切换次数）
        // 过滤出 timestamp 在 limit_minutes 之内的事件
        let count = self.history.iter()
            .filter(|e| {
                e.hwnd == hwnd && 
                now.signed_duration_since(e.timestamp).num_minutes() < limit_minutes
            })
            .count();

        // 3. 计算频率
        count as f64 / duration_min
    }

    // 获取从当前时间往前 5 分钟的时间范围内，窗口的每分钟切换频率
    fn get_short_term_average(&self, hwnd: isize) -> f64 {
        self.calculate_frequency(hwnd, 5)
    }

    // 获取从当前时间往前 24 小时的时间范围内，窗口的每分钟切换频率
    fn get_daily_average(&self, hwnd: isize) -> f64 {
        self.calculate_frequency(hwnd, 24 * 60)
    }

    // 获取指定窗口的总前台时长 (ms)
    fn get_total_duration(&self, hwnd: isize) -> i64 {
        self.daily_stats.get(&hwnd).map(|s| s.total_duration_ms).unwrap_or(0)
    }
}

static GLOBAL_STATE: OnceLock<Mutex<MonitorState>> = OnceLock::new();
static LAST_HWND: AtomicIsize = AtomicIsize::new(0);

// [NEW] 用于记录最后一次检测到的输入源及其发生时间
// (输入源描述, 发生时的Instant)
static LAST_INPUT_EVENT: OnceLock<Mutex<(String, Instant)>> = OnceLock::new();

// =============================================================================
// 钩子回调与辅助函数
// =============================================================================

// [MODIFIED] 键盘钩子：监听 Alt+Tab 和 其他按键
unsafe extern "system" fn keyboard_hook_proc(n_code: i32, w_param: WPARAM, l_param: LPARAM) -> LRESULT {
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
        // 如果需要检测 Win+Tab，可以监听 VK_LWIN/VK_RWIN + Tab，但这通常是 OS 级切换，Win32 API 较难精准捕获前台变化
    }
    // 修复：调用 unsafe 函数需要 unsafe 块
    unsafe { CallNextHookEx(None, n_code, w_param, l_param) }
}

// [MODIFIED] 鼠标钩子：监听左键点击
unsafe extern "system" fn mouse_hook_proc(n_code: i32, w_param: WPARAM, l_param: LPARAM) -> LRESULT {
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

unsafe fn get_process_name(process_id: u32) -> String {
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

unsafe fn get_window_details(hwnd_val: isize) -> String {
    let hwnd = HWND(hwnd_val as *mut _);
    
    let mut buffer = [0u16; 512];
    let len = unsafe { GetWindowTextW(hwnd, &mut buffer) };
    let title = if len > 0 { String::from_utf16_lossy(&buffer[..len as usize]) } else { String::from("No Title") };

    let mut process_id = 0;
    unsafe { GetWindowThreadProcessId(hwnd, Some(&mut process_id)) };
    let process_name = unsafe { get_process_name(process_id) };

    format!("[{}] (PID: {}) - {}", process_name, process_id, title)
}

// =============================================================================
// 核心逻辑
// =============================================================================

fn analyze_behavior(hwnd_val: isize, process_name: &str, input_source: &str) {
    let state_lock = GLOBAL_STATE.get_or_init(|| Mutex::new(MonitorState::new()));
    
    if let Ok(mut state) = state_lock.lock() {
        state.add_event(hwnd_val);

        let oscillation_pair = state.check_oscillation(hwnd_val);
        
        let short_term_avg = state.get_short_term_average(hwnd_val);
        let daily_avg = state.get_daily_average(hwnd_val);
        // 获取总时长
        let total_duration = state.get_total_duration(hwnd_val);
        // 获取当前模式
        let pattern_type = state.identify_pattern(hwnd_val);

        // =======================
        // 触发规则逻辑
        // =======================

        // 1) 实时感知: 震荡检测
        if let Some((hwnd_a, hwnd_b)) = oscillation_pair {
            let info_a = unsafe { get_window_details(hwnd_a) };
            let info_b = unsafe { get_window_details(hwnd_b) };

            println!(">>> [ALERT] Oscillation Detected! Rapid switching (A-B-A-B) in 60s.");
            println!("    Window A: {}", info_a);
            println!("    Window B: {}", info_b);
        }

        // 2) 长期学习 (24小时滑动窗口)
        if daily_avg > 20.0 {
            println!(">>> [CRITICAL] Daily Limit Exceeded for [{}]: {:.1} avg/min (Threshold: 20.0, Window: 24h)", process_name, daily_avg);
        }
        // 3) 短期分析 (5分钟滑动窗口)
        else if short_term_avg > 15.0 {
            println!(">>> [WARN] Short-term Burst for [{}]: {:.1} avg/min (Threshold: 15.0, Window: 5m)", process_name, short_term_avg);
        }

        // Debug 输出
        // 在日志中增加 Pattern 和 Input Source 输出
        // [NEW] 增加了 Source 字段
        println!("Process: {}, Source: [{}], Pattern: [{}], Daily Avg: {:.2}, Short Avg: {:.2}, Total Time: {} ms", 
            process_name, input_source, pattern_type, daily_avg, short_term_avg, total_duration);
    }
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

    // 这里不需要 detect_input_source 了，因为逻辑已经移到了 Hook 回调和 print_wnd_info 中

    unsafe { print_wnd_info(_hwnd) };
}

unsafe extern "system" fn win_event_proc(
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

fn main() -> Result<()> {
    // [NEW] 初始化全局输入记录器，默认时间设为很久以前，避免启动误判
    LAST_INPUT_EVENT.get_or_init(|| Mutex::new((String::from("Ready"), Instant::now() - Duration::from_secs(100))));

    println!("Starting Smart Window Monitor...");
    println!("Rules applied:");
    println!("  1. Real-time:  Oscillation A-B-A-B >= 4 times (60s)");
    println!("  2. Short-term: Avg > 15 switches/min (Window: Last 5 mins)");
    println!("  3. Daily:      Avg > 20 switches/min (Window: Last 24 hours)");
    println!("  4. Input:      Accurate Alt+Tab vs Mouse Click detection via Hooks");
    println!("(Press Ctrl+C to exit)\n");

    unsafe {
        // [NEW] 安装键盘和鼠标钩子
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