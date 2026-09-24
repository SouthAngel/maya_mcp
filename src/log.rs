//! 轻量文件日志：无第三方依赖，追加写入 + 5MB 轮转。
//!
//! 日志路径：`MAYA_MCP_LOG_FILE` 环境变量优先，
//! 否则 `%LOCALAPPDATA%\maya_mcp\maya_mcp.log`，无 LOCALAPPDATA 时退回当前目录。
//! 初始化失败时静默禁用（不影响 MCP stdio 协议）。

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

/// 单个日志文件上限，超过后轮转为 .old
const MAX_BYTES: u64 = 5 * 1024 * 1024;

struct Logger {
    file: Mutex<Option<File>>,
    path: PathBuf,
    written: Mutex<u64>,
}

static LOGGER: OnceLock<Logger> = OnceLock::new();

fn resolve_path() -> PathBuf {
    if let Ok(p) = std::env::var("MAYA_MCP_LOG_FILE") {
        if !p.trim().is_empty() {
            return PathBuf::from(p);
        }
    }
    let dir = std::env::var("LOCALAPPDATA")
        .map(|d| PathBuf::from(d).join("maya_mcp"))
        .unwrap_or_else(|_| PathBuf::from("."));
    dir.join("maya_mcp.log")
}

/// UTC 时间戳加固定小时偏移（本地化显示用）
fn timestamp(offset_hours: i64) -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64 + offset_hours * 3600)
        .unwrap_or(0);
    let days = secs.div_euclid(86400);
    let rem = secs.rem_euclid(86400);
    let (y, m, d) = civil_from_days(days);
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
        y,
        m,
        d,
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60
    )
}

/// Howard Hinnant 的 civil_from_days 算法：epoch 天数 -> (年, 月, 日)
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// 初始化日志（幂等，首次调用生效）。
pub fn init() {
    let path = resolve_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let file = OpenOptions::new().create(true).append(true).open(&path).ok();
    let _ = LOGGER.set(Logger {
        file: Mutex::new(file),
        path,
        written: Mutex::new(0),
    });
}

fn rotate_if_needed(logger: &Logger, line_len: u64) {
    let mut written = logger.written.lock().unwrap();
    *written += line_len;
    if *written <= MAX_BYTES {
        return;
    }
    *written = 0;
    *logger.file.lock().unwrap() = None; // 先关闭
    let old = logger.path.with_extension("log.old");
    let _ = std::fs::remove_file(&old);
    let _ = std::fs::rename(&logger.path, &old);
    *logger.file.lock().unwrap() =
        OpenOptions::new().create(true).append(true).open(&logger.path).ok();
}

pub fn write(level: &str, msg: &str) {
    let Some(logger) = LOGGER.get() else { return };
    let line = format!("[{}] [{}] {}\n", timestamp(8), level, msg);
    let mut guard = logger.file.lock().unwrap();
    if let Some(f) = guard.as_mut() {
        if f.write_all(line.as_bytes()).and_then(|_| f.flush()).is_ok() {
            drop(guard);
            rotate_if_needed(logger, line.len() as u64);
        }
    }
}

#[macro_export]
macro_rules! log_info {
    ($($arg:tt)*) => { $crate::log::write("INFO", &format!($($arg)*)) };
}

#[macro_export]
macro_rules! log_error {
    ($($arg:tt)*) => { $crate::log::write("ERROR", &format!($($arg)*)) };
}
