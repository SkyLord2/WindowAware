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
    first_seen: DateTime<Local>, // 第一次检测到该窗口的时间
    total_count: usize,          // 当天总切换次数
}

struct MonitorState {
    // 历史队列：保存最近 5 分钟的所有切换记录
    history: VecDeque<SwitchEvent>,
    // 每日统计：保存窗口的长期汇总信息
    daily_stats: HashMap<isize, WindowStats>, 
}

impl MonitorState {
    const HISTORY_LIMIT_MINUTES: i64 = 5;

    fn new() -> Self {
        Self {
            history: VecDeque::new(),
            daily_stats: HashMap::new(),
        }
    }

    fn add_event(&mut self, hwnd: isize) {
        let now = Local::now();
        
        // 1. 更新滑动窗口历史记录 (用于短期 5 分钟分析)
        self.history.push_back(SwitchEvent { hwnd, timestamp: now });
        
        // 清理超过 5 分钟的数据
        while let Some(front) = self.history.front() {
            if now.signed_duration_since(front.timestamp).num_minutes() >= Self::HISTORY_LIMIT_MINUTES {
                self.history.pop_front();
            } else {
                break;
            }
        }

        // 2. 更新长期统计数据 (用于当天平均分析)
        self.daily_stats.entry(hwnd)
            .and_modify(|stats| stats.total_count += 1)
            .or_insert(WindowStats {
                first_seen: now,
                total_count: 1,
            });
    }

    // 规则 1: 实时感知 (60秒内 A-B-A-B 震荡)
    fn check_oscillation(&self, current_hwnd: isize) -> bool {
        let now = Local::now();
        let recent_events: Vec<&SwitchEvent> = self.history.iter()
            .rev()
            .take_while(|e| now.signed_duration_since(e.timestamp).num_seconds() <= 60)
            .collect();

        if recent_events.len() < 5 { return false; }

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
             return true;
        }
        false
    }

    // [NEW] 规则 2: 短期分析 (5分钟内滑动平均频率)
    // 计算公式：最近5分钟内的出现次数 / 有效监控时长(max 5min)
    fn get_short_term_average(&self, hwnd: isize) -> f64 {
        // 1. 统计历史队列中该窗口出现的次数 (history 已经被限制在5分钟内)
        let count_in_window = self.history.iter().filter(|e| e.hwnd == hwnd).count();
        
        // 2. 计算分母（时间窗口）
        if let Some(stats) = self.daily_stats.get(&hwnd) {
            let now = Local::now();
            let duration_since_start = now.signed_duration_since(stats.first_seen).num_seconds() as f64 / 60.0;
            
            // 如果窗口出现时间 < 5分钟，用实际时长作为分母；否则用 5.0
            let effective_window = if duration_since_start > 5.0 { 5.0 } else { duration_since_start };

            // 过滤噪音：如果监控时长不足 30 秒，数据不稳定，暂不返回高频结果
            if effective_window < 0.5 { return 0.0; }

            return count_in_window as f64 / effective_window;
        }
        0.0
    }

    // 规则 3: 长期学习 (当天平均切换频率)
    fn get_daily_average(&self, hwnd: isize) -> f64 {
        if let Some(stats) = self.daily_stats.get(&hwnd) {
            let now = Local::now();
            let duration_minutes = now.signed_duration_since(stats.first_seen).num_seconds() as f64 / 60.0;
            
            if duration_minutes < 1.0 { return 0.0; } // 不足1分钟忽略
            return stats.total_count as f64 / duration_minutes;
        }
        0.0
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

// =============================================================================
// 核心逻辑
// =============================================================================

fn analyze_behavior(hwnd_val: isize, process_name: &str) {
    let state_lock = GLOBAL_STATE.get_or_init(|| Mutex::new(MonitorState::new()));
    
    if let Ok(mut state) = state_lock.lock() {
        // 1. 记录数据
        state.add_event(hwnd_val);

        // 获取各项指标
        let is_oscillating = state.check_oscillation(hwnd_val);
        let short_term_avg = state.get_short_term_average(hwnd_val); // 5分钟均值
        let daily_avg = state.get_daily_average(hwnd_val);           // 全天均值

        // =======================
        // 触发规则逻辑
        // =======================

        // 1) 实时感知: 震荡检测
        if is_oscillating {
            println!(">>> [ALERT] Oscillation Detected! Rapid switching (A-B-A-B) in 60s.");
        }

        // 2) 长期学习: 全天频率 > 60 (你指定的阈值)
        // 注意：这是一个非常高的阈值
        if daily_avg > 60.0 {
            println!(">>> [CRITICAL] Daily Limit Exceeded for [{}]: {:.1} avg/min (Threshold: 60.0)", process_name, daily_avg);
        }
        // 3) 短期分析: 5分钟内 > 15 (你指定的阈值)
        else if short_term_avg > 15.0 {
            println!(">>> [WARN] Short-term Burst for [{}]: {:.1} avg/min (Last 5 mins > 15.0)", process_name, short_term_avg);
        }
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
    println!("Class: {}", class_name);
    
    // 调用分析逻辑
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
    println!("Updated Rules:");
    println!("  1. Real-time:  Oscillation A-B-A-B >= 4 times (60s)");
    println!("  2. Short-term: Avg > 15 switches/min (Window: 5 mins)");
    println!("  3. Daily:      Avg > 60 switches/min (Window: All day)");
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