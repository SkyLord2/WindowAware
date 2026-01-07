use std::sync::atomic::AtomicIsize;
use std::sync::{Mutex, OnceLock};
use std::time::Instant;
use crate::state::MonitorState;

pub static GLOBAL_STATE: OnceLock<Mutex<MonitorState>> = OnceLock::new();
pub static LAST_HWND: AtomicIsize = AtomicIsize::new(0);

// [NEW] 用于记录最后一次检测到的输入源及其发生时间
// (输入源描述, 发生时的Instant)
pub static LAST_INPUT_EVENT: OnceLock<Mutex<(String, Instant)>> = OnceLock::new();