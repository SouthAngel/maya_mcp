//! Maya Listener 状态监控：后台周期探测 + 实时状态探测。
//!
//! - `spawn_monitor`：后台任务按固定间隔对 listener 做 connect+ping 探测，
//!   结果样本（成功/延迟/错误）写入环形历史队列。
//! - `probe_once`：单次实时探测，返回 (延迟 ms, Maya 版本串) 或错误文本。
//!
//! 环境变量：`MAYA_MCP_PROBE_SECS`（监控间隔，默认 30）、
//! `MAYA_MCP_PROBE_TIMEOUT_SECS`（单次探测超时，默认 5）。

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::maya::MayaClient;
use crate::log_error;

/// 历史样本保留数量
pub const HISTORY_SIZE: usize = 10;
/// 延迟超过该值判定为 DEGRADED（毫秒）
pub const DEGRADE_MS: u128 = 2000;

#[derive(Debug, Clone)]
pub struct Sample {
    pub ok: bool,
    pub latency_ms: Option<u128>,
    pub version: Option<String>,
    pub error: Option<String>,
    pub at: Instant,
}

pub type History = Arc<Mutex<VecDeque<Sample>>>;

pub fn new_history() -> History {
    Arc::new(Mutex::new(VecDeque::new()))
}

fn read_env_u64(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .filter(|&v| v > 0)
        .unwrap_or(default)
}

pub fn probe_interval() -> Duration {
    Duration::from_secs(read_env_u64("MAYA_MCP_PROBE_SECS", 30))
}

pub fn probe_timeout() -> Duration {
    Duration::from_secs(read_env_u64("MAYA_MCP_PROBE_TIMEOUT_SECS", 5))
}

/// 单次实时探测：connect + ping，测量往返延迟。
pub async fn probe_once(client: &MayaClient) -> Result<(u128, String), String> {
    let start = Instant::now();
    let resp = client
        .call("ping", "", json!({}))
        .await
        .map_err(|e| e.to_string())?;
    let ms = start.elapsed().as_millis();
    let version = resp
        .get("result")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    Ok((ms, version))
}

async fn make_sample(client: &MayaClient) -> Sample {
    match probe_once(client).await {
        Ok((ms, ver)) => Sample {
            ok: true,
            latency_ms: Some(ms),
            version: Some(ver),
            error: None,
            at: Instant::now(),
        },
        Err(e) => {
            log_error!("monitor probe failed: {}", e);
            Sample {
                ok: false,
                latency_ms: None,
                version: None,
                error: Some(e),
                at: Instant::now(),
            }
        }
    }
}

/// 启动后台监控任务（首次探测立即执行）。
pub fn spawn_monitor(client: MayaClient, interval: Duration, history: History) {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(interval);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            ticker.tick().await;
            let sample = make_sample(&client).await;
            let mut q = history.lock().unwrap();
            if q.len() >= HISTORY_SIZE {
                q.pop_front();
            }
            q.push_back(sample);
        }
    });
}
