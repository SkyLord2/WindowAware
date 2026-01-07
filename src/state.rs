use std::collections::{VecDeque, HashMap, HashSet};
use chrono::{DateTime, Local};

// =============================================================================
// 数据结构与全局状态
// =============================================================================

#[derive(Clone, Debug)]
pub struct SwitchEvent {
    pub hwnd: isize,
    pub timestamp: DateTime<Local>,
}

pub struct WindowStats {
    pub first_seen: DateTime<Local>,
    // 记录该窗口累计在前台的总时长 (毫秒)
    pub total_duration_ms: i64, 
}

pub struct MonitorState {
    pub history: VecDeque<SwitchEvent>,
    pub daily_stats: HashMap<isize, WindowStats>, 
    // 记录上一次切换发生的时间，用于计算停留时长
    pub last_switch_time: Option<DateTime<Local>>,
    // 记录上一个处于前台的窗口句柄
    pub last_active_hwnd: Option<isize>,
}

impl MonitorState {
    // 历史记录必须保留 24 小时 (1440 分钟)，原本是 5 分钟
    const HISTORY_LIMIT_MINUTES: i64 = 24 * 60;

    pub fn new() -> Self {
        Self {
            history: VecDeque::new(),
            daily_stats: HashMap::new(),
            // 初始化为空
            last_switch_time: None,
            last_active_hwnd: None,
        }
    }

    pub fn add_event(&mut self, hwnd: isize) {
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
    pub fn check_oscillation(&self, current_hwnd: isize) -> Option<(isize, isize)> {
        let now = Local::now();
        // 因为 history 现在很长，使用 rev() 从最新的数据开始查找效率更高
        let recent_events: Vec<&SwitchEvent> = self.history.iter()
            .rev()
            .take_while(|e| now.signed_duration_since(e.timestamp).num_seconds() <= 60)
            .collect();

        if recent_events.len() < 5 { return None; }

        let event_a1 = &recent_events[0];
        let event_b1 = &recent_events[1];
        let event_a2 = &recent_events[2];
        let event_b2 = &recent_events[3];
        let event_a3 = &recent_events[4];

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
    pub fn identify_pattern(&self, current_hwnd: isize) -> String {
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
    pub fn get_short_term_average(&self, hwnd: isize) -> f64 {
        self.calculate_frequency(hwnd, 5)
    }

    // 获取从当前时间往前 24 小时的时间范围内，窗口的每分钟切换频率
    pub fn get_daily_average(&self, hwnd: isize) -> f64 {
        self.calculate_frequency(hwnd, 24 * 60)
    }

    // 获取指定窗口的总前台时长 (ms)
    pub fn get_total_duration(&self, hwnd: isize) -> i64 {
        self.daily_stats.get(&hwnd).map(|s| s.total_duration_ms).unwrap_or(0)
    }
}
