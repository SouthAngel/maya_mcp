//! Maya listener 的 TCP 客户端：每次请求新建连接，JSON-lines 协议。

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;

/// 建立连接的超时
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
/// 发送请求的写入超时（防止对端不读时无限挂起）
const WRITE_TIMEOUT: Duration = Duration::from_secs(5);
/// 等待 Maya 响应的默认超时
const DEFAULT_TIMEOUT_SECS: u64 = 120;

#[derive(Debug, thiserror::Error)]
pub enum MayaError {
    #[error(
        "无法连接 Maya listener ({host}:{port}): {reason}\n\
         请先在 Maya 的 Script Editor (Python 标签页) 中执行:\n\
         \x20 p = r\"<路径>/maya/maya_mcp_listener.py\"; exec(compile(open(p, \"rb\").read(), p, \"exec\"))"
    )]
    NotListening { host: String, port: u16, reason: String },
    #[error(
        "等待 Maya 响应超时 ({timeout}s)。Maya 主线程可能被模态对话框阻塞，或脚本仍在运行"
    )]
    Timeout { timeout: u64 },
    #[error("Maya listener 协议错误: {0}")]
    Protocol(String),
    #[error("{0}")]
    Script(String),
}

#[derive(Debug, Clone)]
pub struct MayaClient {
    pub host: String,
    pub port: u16,
    pub timeout: Duration,
}

static REQ_ID: AtomicU64 = AtomicU64::new(1);

impl MayaClient {
    pub fn from_env() -> Self {
        let host = std::env::var("MAYA_MCP_HOST").unwrap_or_else(|_| "127.0.0.1".into());
        let port = std::env::var("MAYA_MCP_PORT")
            .ok()
            .and_then(|p| p.parse().ok())
            .unwrap_or(5055);
        let timeout_secs = std::env::var("MAYA_MCP_TIMEOUT_SECS")
            .ok()
            .and_then(|p| p.parse().ok())
            .filter(|&v| v > 0)
            .unwrap_or(DEFAULT_TIMEOUT_SECS);
        Self {
            host,
            port,
            timeout: Duration::from_secs(timeout_secs),
        }
    }

    fn not_listening(&self, reason: String) -> MayaError {
        MayaError::NotListening {
            host: self.host.clone(),
            port: self.port,
            reason,
        }
    }

    /// 发送请求并等待响应。成功时返回完整响应 JSON（含 result/stdout 字段）。
    pub async fn call(&self, op: &str, code: &str, args: Value) -> Result<Value, MayaError> {
        let addr = (self.host.as_str(), self.port);
        let stream = match tokio::time::timeout(CONNECT_TIMEOUT, TcpStream::connect(addr)).await {
            Ok(Ok(s)) => s,
            Ok(Err(e)) => return Err(self.not_listening(e.to_string())),
            Err(_) => return Err(self.not_listening("连接超时".into())),
        };

        let req = json!({
            "id": REQ_ID.fetch_add(1, Ordering::Relaxed),
            "op": op,
            "code": code,
            "args": args,
        });

        let mut stream = stream;
        let payload = req.to_string();
        tokio::time::timeout(WRITE_TIMEOUT, async {
            stream.write_all(payload.as_bytes()).await?;
            stream.write_all(b"\n").await?;
            stream.flush().await?;
            Ok::<(), std::io::Error>(())
        })
        .await
        .map_err(|_| MayaError::Protocol("发送请求超时".into()))?
        .map_err(|e| MayaError::Protocol(format!("发送请求失败: {e}")))?;

        let mut line = String::new();
        let mut reader = BufReader::new(stream);
        match tokio::time::timeout(self.timeout, reader.read_line(&mut line)).await {
            Ok(Ok(0)) => return Err(MayaError::Protocol("连接被 Maya 端关闭".into())),
            Ok(Ok(_)) => {}
            Ok(Err(e)) => return Err(MayaError::Protocol(format!("读取响应失败: {e}"))),
            Err(_) => {
                return Err(MayaError::Timeout {
                    timeout: self.timeout.as_secs(),
                })
            }
        }

        let resp: Value = serde_json::from_str(line.trim())
            .map_err(|e| MayaError::Protocol(format!("响应不是合法 JSON: {e}")))?;

        match resp.get("ok").and_then(Value::as_bool) {
            Some(true) => Ok(resp),
            Some(false) => {
                let mut msg = resp
                    .get("error")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown error")
                    .to_string();
                if let Some(tb) = resp.get("traceback").and_then(Value::as_str) {
                    msg.push_str("\n\n");
                    msg.push_str(tb);
                }
                if let Some(so) = resp.get("stdout").and_then(Value::as_str) {
                    if !so.trim().is_empty() {
                        msg.push_str("\n\nstdout:\n");
                        msg.push_str(so.trim_end());
                    }
                }
                Err(MayaError::Script(msg))
            }
            None => Err(MayaError::Protocol("响应缺少 ok 字段".into())),
        }
    }
}
