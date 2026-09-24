# maya_mcp

> 本项目由 AI 生成（AI-generated）

让 AI 助手直接操作已打开的 Maya：执行 Python/MEL、查询场景、监控连接状态。

## 安装

```powershell
powershell -ExecutionPolicy Bypass -File install.ps1
```

脚本自动做三件事（幂等，每次写入前自动备份）：

1. 二进制缺失时执行 `cargo build --release`
2. 注册 MCP 服务器到客户端：

| 客户端 | 配置文件 |
|---|---|
| Trae | `~/.trae-cn/mcp.json`、`~/.trae/mcp.json` |
| CodeBuddy | `~/.codebuddy/mcp.json` |
| OpenCode | `~/.config/opencode/opencode.json` |
| Codex | `~/.codex/config.toml` |

3. 向 `Documents\maya\<版本>\scripts\userSetup.py` 追加 listener 自启动块

完成后重启 Maya（看到 `[maya-mcp] init: success` 即自启成功）和所用客户端即可。

### 手动安装

不跑脚本时，手动注册等效条目（`command` 换成你的 `maya_mcp.exe` 绝对路径）：

```jsonc
// Trae / CodeBuddy: mcp.json
{ "mcpServers": { "maya_mcp": { "command": "D:\\...\\maya_mcp.exe", "args": [], "env": {} } } }
```

```jsonc
// OpenCode: opencode.json
{ "mcp": { "maya_mcp": { "type": "local", "command": ["D:\\...\\maya_mcp.exe"], "enabled": true, "environment": {} } } }
```

```toml
# Codex: config.toml
[mcp_servers.maya_mcp]
command = "D:\\...\\maya_mcp.exe"
args = []
```

Maya 端自启动（追加到 userSetup.py，或 Script Editor 手动执行一次）：

```python
p = r"d:\001M\workspace\ait\maya_mcp\maya\maya_mcp_listener.py"
exec(compile(open(p, "rb").read(), p, "exec"))
```

## 提供的工具

| 工具 | 用途 |
|---|---|
| `eval_python` / `eval_mel` | 在 Maya 主线程执行脚本，支持多语句、print 捕获、表达式返回值 |
| `get_selected` / `list_scene_nodes` / `get_scene_info` | 查询选择集、节点列表、场景概况 |
| `check_connection` / `maya_status_report` | 探活；状态报告含 `UP / DEGRADED / DOWN` 与历史可用率 |

报错以 traceback 形式返回给 AI，便于自动修正。

## 配置

| 环境变量 | 默认 | 说明 |
|---|---|---|
| `MAYA_MCP_PORT` | `5055` | 监听端口（两侧需一致） |
| `MAYA_MCP_HOST` | `127.0.0.1` | 监听地址 |
| `MAYA_MCP_TIMEOUT_SECS` | `120` | 工具执行超时 |
| `MAYA_MCP_PROBE_SECS` | `30` | 后台监控间隔 |
| `MAYA_MCP_LOG_FILE` | `%LOCALAPPDATA%\maya_mcp\maya_mcp.log` | 服务端日志文件（5MB 轮转） |

## 常见问题

- **工具报"未启动"** —— listener 没跑起来，看 Maya 脚本编辑器的 `[maya-mcp] init:` 日志定位原因
- **多版本 Maya 同时开** —— 只有第一个绑到 5055，其余实例改用 `MAYA_MCP_PORT`
- **eval 卡住** —— Maya 主线程被弹窗或长任务占着，先处理 Maya 端

## 结构

```
maya_mcp/
├── src/          # Rust: MCP 服务器（rmcp + tokio，stdio）
├── maya/         # listener 脚本（Py2/Py3 兼容，Maya 2018~2025 实测）
├── install.ps1   # 一键安装
└── build.ps1     # 构建脚本
```
