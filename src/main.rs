use std::collections::{VecDeque, HashMap};
use std::path::Path;
use std::sync::atomic::{AtomicIsize, Ordering};
use std::sync::{Mutex, OnceLock};
use chrono::{DateTime, Local};

use windows::{
    core::*,
    Win32::Foundation::{HWND, CloseHandle},
    Win32::UI::Accessibility::{SetWinEventHook, HWINEVENTHOOK},
    Win32::UI::WindowsAndMessaging::{
        DispatchMessageW, GetMessageW, GetWindowTextW, GetWindowThreadProcessId, 
        GetClassNameW, TranslateMessage, MSG, EVENT_SYSTEM_FOREGROUND, EVENT_SYSTEM_MINIMIZEEND, WINEVENT_OUTOFCONTEXT,
    },
    Win32::System::Threading::{
        OpenProcess, QueryFullProcessImageNameW, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_NAME_WIN32
    },
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
    // total_count 在滑动窗口模式下不再需要累积，我们通过实时计算 history 队列得出
    // [NEW] 记录该窗口累计在前台的总时长 (毫秒)
    total_duration_ms: i64, 
}

struct MonitorState {
    history: VecDeque<SwitchEvent>,
    daily_stats: HashMap<isize, WindowStats>, 
    // [NEW] 记录上一次切换发生的时间，用于计算停留时长
    last_switch_time: Option<DateTime<Local>>,
    // [NEW] 记录上一个处于前台的窗口句柄
    last_active_hwnd: Option<isize>,
}

impl MonitorState {
    // 历史记录必须保留 24 小时 (1440 分钟)，原本是 5 分钟
    const HISTORY_LIMIT_MINUTES: i64 = 24 * 60;

    fn new() -> Self {
        Self {
            history: VecDeque::new(),
            daily_stats: HashMap::new(),
            // [NEW] 初始化为空
            last_switch_time: None,
            last_active_hwnd: None,
        }
    }

    fn add_event(&mut self, hwnd: isize) {
        let now = Local::now();

        // [NEW] 计算并更新 **上一个** 窗口的停留时长
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

        // [NEW] 更新状态，将当前窗口标记为活跃窗口，并记录开始时间
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

    // [NEW] 获取指定窗口的总前台时长 (ms)
    fn get_total_duration(&self, hwnd: isize) -> i64 {
        self.daily_stats.get(&hwnd).map(|s| s.total_duration_ms).unwrap_or(0)
    }
}

static GLOBAL_STATE: OnceLock<Mutex<MonitorState>> = OnceLock::new();
static LAST_HWND: AtomicIsize = AtomicIsize::new(0);

// =============================================================================
// 辅助函数
// =============================================================================

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

fn analyze_behavior(hwnd_val: isize, process_name: &str) {
    let state_lock = GLOBAL_STATE.get_or_init(|| Mutex::new(MonitorState::new()));
    
    if let Ok(mut state) = state_lock.lock() {
        state.add_event(hwnd_val);

        let oscillation_pair = state.check_oscillation(hwnd_val);
        
        let short_term_avg = state.get_short_term_average(hwnd_val);
        let daily_avg = state.get_daily_average(hwnd_val);
        // [NEW] 获取总时长
        let total_duration = state.get_total_duration(hwnd_val);

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
        // [NEW] 在日志中增加 Duration 输出
        println!("Process: {}, Daily Avg: {:.2}, Short Avg: {:.2}, Total Time: {} ms", 
            process_name, daily_avg, short_term_avg, total_duration);
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

    let local_now = Local::now();
    let formatted = local_now.format("%Y-%m-%d %H:%M:%S").to_string();
    println!("--------------------------{}------------------------", formatted);
    println!("Process: {} | PID: {}", process_name, process_id);
    println!("Title:   {}", title);
    println!("Class:   {}", class_name);
    
    analyze_behavior(hwnd.0 as isize, &process_name);
}

unsafe fn handle_foreground_change(_hwnd: HWND) {
    if _hwnd.0 as isize == 0 { return; }

    let current_val = _hwnd.0 as isize;
    let last_val = LAST_HWND.swap(current_val, Ordering::Relaxed);
    if last_val == current_val { return; }

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
    println!("Starting Smart Window Monitor...");
    println!("Rules applied:");
    println!("  1. Real-time:  Oscillation A-B-A-B >= 4 times (60s)");
    println!("  2. Short-term: Avg > 15 switches/min (Window: Last 5 mins)");
    println!("  3. Daily:      Avg > 20 switches/min (Window: Last 24 hours)");
    println!("(Press Ctrl+C to exit)\n");

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