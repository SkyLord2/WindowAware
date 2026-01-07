use std::sync::Mutex;
use crate::global::{GLOBAL_STATE};
use crate::state::MonitorState;
use crate::utils::get_window_details;

// =============================================================================
// 核心逻辑
// =============================================================================

pub fn analyze_behavior(hwnd_val: isize, process_name: &str, input_source: &str) {
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
        // 增加了 Source 字段
        println!("Process: {}, Source: [{}], Pattern: [{}], Daily Avg: {:.2}, Short Avg: {:.2}, Total Time: {} ms", 
            process_name, input_source, pattern_type, daily_avg, short_term_avg, total_duration);
    }
}