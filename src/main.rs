//! maya-mcp：在已打开的 Maya 中执行脚本的 MCP 服务器（stdio 传输）。
//!
//! 环境变量：
//! - `MAYA_MCP_HOST`：Maya listener 地址，默认 127.0.0.1
//! - `MAYA_MCP_PORT`：Maya listener 端口，默认 5055
//! - `MAYA_MCP_TIMEOUT_SECS`：等待 Maya 执行的超时秒数，默认 120
//! - `MAYA_MCP_LOG_FILE`：日志文件路径，默认 %LOCALAPPDATA%\maya_mcp\maya_mcp.log

mod log;
mod maya;
mod status;
mod tools;

use rmcp::ServiceExt;
use rmcp::transport::stdio;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    log::init();
    log_info!("server started");
    let service = tools::MayaMcp::new();
    let running = service.serve(stdio()).await?;
    running.waiting().await?;
    log_info!("server stopped");
    Ok(())
}
