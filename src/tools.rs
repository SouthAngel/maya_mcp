//! MCP 工具定义：eval 与常用场景查询。

use std::time::Duration;

use rmcp::{
    ErrorData as McpError, ServerHandler,
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{CallToolResult, ContentBlock},
    schemars::JsonSchema,
    serde::Deserialize,
    serde_json::{Value, json},
    tool, tool_handler, tool_router,
};

use crate::maya::MayaClient;
use crate::status;
use crate::{log_error, log_info};

/// 单个工具输出的最大字符数
const MAX_OUTPUT_CHARS: usize = 30_000;

fn truncate(s: String) -> String {
    let count = s.chars().count();
    if count <= MAX_OUTPUT_CHARS {
        return s;
    }
    let head: String = s.chars().take(MAX_OUTPUT_CHARS).collect();
    format!("{head}\n\n…[输出已截断，原始长度 {count} 字符]")
}

fn value_to_text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => serde_json::to_string_pretty(other).unwrap_or_else(|_| format!("{other}")),
    }
}

/// python op 的成功响应 → 展示文本
fn render_python_result(resp: &Value) -> String {
    let mut parts = Vec::new();
    if let Some(so) = resp.get("stdout").and_then(Value::as_str) {
        if !so.trim().is_empty() {
            parts.push(format!("stdout:\n{}", so.trim_end()));
        }
    }
    if let Some(r) = resp.get("result") {
        parts.push(format!("result:\n{}", value_to_text(r)));
    }
    if parts.is_empty() {
        "执行成功（无返回值）".to_string()
    } else {
        parts.join("\n\n")
    }
}

async fn run_tool(
    op: &str,
    code: &str,
    args: Value,
    render: impl Fn(&Value) -> String,
) -> Result<CallToolResult, McpError> {
    let started = std::time::Instant::now();
    let client = MayaClient::from_env();
    let result = match client.call(op, code, args).await {
        Ok(resp) => Ok(CallToolResult::success(vec![ContentBlock::text(truncate(
            render(&resp),
        ))])),
        Err(e) => Ok(CallToolResult::error(vec![ContentBlock::text(truncate(
            e.to_string(),
        ))])),
    };
    match &result {
        Ok(r) => log_info!(
            "op={} code_len={} ok={} {}ms",
            op,
            code.len(),
            !r.is_error.unwrap_or(false),
            started.elapsed().as_millis()
        ),
        Err(e) => log_error!("op={} code_len={} failed: {}", op, code.len(), e),
    }
    result
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct CodeArgs {
    /// 要执行的代码
    pub code: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ListNodesArgs {
    /// 可选，按节点类型过滤（cmds.ls 的 type 参数），如 "mesh"、"joint"、"nurbsCurve"、"transform"
    #[serde(default)]
    pub type_filter: Option<String>,
    /// 最多返回的节点数，默认 200
    #[serde(default)]
    pub limit: Option<u64>,
}

#[derive(Debug, Clone)]
pub struct MayaMcp {
    tool_router: ToolRouter<Self>,
    history: status::History,
    probe_interval: Duration,
}

#[tool_router]
impl MayaMcp {
    pub fn new() -> Self {
        let history = status::new_history();
        let probe_interval = status::probe_interval();
        status::spawn_monitor(MayaClient::from_env(), probe_interval, history.clone());
        Self {
            tool_router: Self::tool_router(),
            history,
            probe_interval,
        }
    }

    /// 检查与 Maya listener 的连接是否正常
    #[tool(description = "检查与 Maya listener 的连接。返回 Maya 版本号；失败说明 listener 未启动。\
         使用短超时（探测超时而非工具执行超时），适合快速探活。")]
    async fn check_connection(&self) -> Result<CallToolResult, McpError> {
        let mut client = MayaClient::from_env();
        client.timeout = status::probe_timeout();
        match status::probe_once(&client).await {
            Ok((ms, ver)) => {
                log_info!("tool=check_connection ok {}ms", ms);
                Ok(CallToolResult::success(vec![ContentBlock::text(truncate(
                    if ver.is_empty() { "pong".to_string() } else { ver },
                ))]))
            }
            Err(e) => {
                log_error!("tool=check_connection failed: {}", e);
                Ok(CallToolResult::error(vec![ContentBlock::text(truncate(e))]))
            }
        }
    }

    /// Maya Listener 状态报告
    #[tool(description = "生成 Maya Listener 状态报告：实时探测连接可用性与响应延迟，\
         并汇总后台周期监控的历史可用率。状态码: UP(正常) / DEGRADED(可用但延迟高) / DOWN(不可用)。")]
    async fn maya_status_report(&self) -> Result<CallToolResult, McpError> {
        let client = MayaClient::from_env();
        let mut probe_client = client.clone();
        probe_client.timeout = status::probe_timeout();

        // 实时探测（await 期间不持有锁）
        let (status_code, probe_line) = match status::probe_once(&probe_client).await {
            Ok((ms, ver)) => {
                let code = if ms >= status::DEGRADE_MS { "DEGRADED" } else { "UP" };
                (code, format!("{} ms | Maya: {}", ms, if ver.is_empty() { "(无版本信息)" } else { &ver }))
            }
            Err(e) => ("DOWN", format!("error: {}", e)),
        };

        // 历史样本汇总
        let (total, ok_n, avg_ms, last_err, last_ok) = {
            let q = self.history.lock().unwrap();
            let total = q.len();
            let ok_n = q.iter().filter(|s| s.ok).count();
            let lats: Vec<u128> = q.iter().filter_map(|s| s.latency_ms).collect();
            let avg_ms = if lats.is_empty() {
                None
            } else {
                Some(lats.iter().sum::<u128>() / lats.len() as u128)
            };
            let last_err = q
                .iter()
                .rev()
                .find(|s| !s.ok)
                .map(|s| (s.error.clone().unwrap_or_default(), s.at.elapsed().as_secs()));
            let last_ok = q
                .iter()
                .rev()
                .find(|s| s.ok)
                .map(|s| (s.at.elapsed().as_secs(), s.version.clone().unwrap_or_default()));
            (total, ok_n, avg_ms, last_err, last_ok)
        };

        let mut lines = vec![
            "=== Maya Listener 状态报告 ===".to_string(),
            format!("状态: {}", status_code),
            format!("端点: {}:{}", client.host, client.port),
            format!(
                "探测: TCP+ping | 超时 {}s | 监控间隔 {}s",
                status::probe_timeout().as_secs(),
                self.probe_interval.as_secs()
            ),
            format!("本次探测: {}", probe_line),
        ];
        log_info!("tool=maya_status_report status={}", status_code);
        if total == 0 {
            lines.push("历史监控: 尚无样本".to_string());
        } else {
            let pct = ok_n * 100 / total;
            lines.push(format!(
                "历史监控: 近 {} 次成功 {}/{} (可用率 {}%){}",
                total,
                ok_n,
                total,
                pct,
                avg_ms.map(|m| format!(", 平均延迟 {} ms", m)).unwrap_or_default()
            ));
        }
        match last_err {
            Some((err, age)) if status_code == "DOWN" => {
                lines.push(format!("最近失败: {}s 前 - {}", age, err));
            }
            _ => {}
        }
        if status_code == "DOWN" {
            match last_ok {
                Some((age, ver)) if !ver.is_empty() => {
                    lines.push(format!("最后可用: {}s 前 ({})", age, ver));
                }
                Some((age, _)) => lines.push(format!("最后可用: {}s 前", age)),
                None => lines.push("最后可用: 无记录".to_string()),
            }
        }

        Ok(CallToolResult::success(vec![ContentBlock::text(truncate(
            lines.join("\n"),
        ))]))
    }

    /// 在 Maya 中执行 Python 代码
    #[tool(description = "在已打开的 Maya 的主线程中执行 Python 代码并返回结果。\
         支持多条语句，最后一条\"表达式语句\"的值会作为 result 返回；print 输出会作为 stdout 返回。\
         命名空间已预置：cmds (maya.cmds)、mel (maya.mel)、om2 (maya.api.OpenMaya)。\
         执行报错时会返回完整的错误信息，可据此修正代码后重试。")]
    async fn eval_python(&self, Parameters(args): Parameters<CodeArgs>) -> Result<CallToolResult, McpError> {
        run_tool("python", &args.code, json!({}), render_python_result).await
    }

    /// 在 Maya 中执行 MEL 代码
    #[tool(description = "在已打开的 Maya 的主线程中执行 MEL 脚本并返回结果（MEL 的返回值是字符串）。\
         执行报错时会返回 MEL 错误信息。")]
    async fn eval_mel(&self, Parameters(args): Parameters<CodeArgs>) -> Result<CallToolResult, McpError> {
        run_tool("mel", &args.code, json!({}), |resp| {
            resp.get("result")
                .map(value_to_text)
                .unwrap_or_else(|| "执行成功".into())
        })
        .await
    }

    /// 列出场景节点
    #[tool(description = "列出当前 Maya 场景中的节点（长名）。可用 type_filter 按类型过滤，\
         limit 限制返回数量（默认 200）。返回 total/shown/nodes/truncated。")]
    async fn list_scene_nodes(
        &self,
        Parameters(args): Parameters<ListNodesArgs>,
    ) -> Result<CallToolResult, McpError> {
        let limit = args.limit.unwrap_or(200).min(5000);
        let code = r#"import maya.cmds as cmds
__tf = __mcp_args__.get("type_filter") or None
__limit = __mcp_args__.get("limit") or 200
__nodes = cmds.ls(type=__tf, long=True) if __tf else cmds.ls(long=True)
{"total": len(__nodes), "shown": min(len(__nodes), __limit), "nodes": __nodes[:__limit], "truncated": len(__nodes) > __limit}"#;
        run_tool("python", code, json!({"type_filter": args.type_filter, "limit": limit}), |resp| {
            resp.get("result").map(value_to_text).unwrap_or_else(|| "执行成功".into())
        })
        .await
    }

    /// 获取当前选中对象
    #[tool(description = "获取 Maya 当前选中的对象列表（长名 + 节点类型）。")]
    async fn get_selected(&self) -> Result<CallToolResult, McpError> {
        let code = r#"import maya.cmds as cmds
[{"name": n, "type": cmds.nodeType(n)} for n in cmds.ls(selection=True, long=True)]"#;
        run_tool("python", code, json!({}), |resp| {
            let r = resp.get("result").unwrap_or(&Value::Null);
            if r.as_array().is_some_and(|a| a.is_empty()) {
                return "当前没有选中对象".to_string();
            }
            value_to_text(r)
        })
        .await
    }

    /// 获取场景概要信息
    #[tool(description = "获取 Maya 场景概要：文件路径、是否修改、单位、上轴向、帧范围、当前帧、渲染器、按类型的节点计数。")]
    async fn get_scene_info(&self) -> Result<CallToolResult, McpError> {
        let code = r#"import maya.cmds as cmds
__counts = {}
for __n in cmds.ls():
    __t = cmds.nodeType(__n)
    __counts[__t] = __counts.get(__t, 0) + 1
{"file": cmds.file(query=True, sceneName=True) or None,
 "modified": cmds.file(query=True, modified=True),
 "up_axis": cmds.upAxis(query=True, axis=True),
 "linear_unit": cmds.currentUnit(query=True, linear=True),
 "angular_unit": cmds.currentUnit(query=True, angle=True),
 "time_unit": cmds.currentUnit(query=True, time=True),
 "current_frame": cmds.currentTime(query=True),
 "playback_range": [cmds.playbackOptions(query=True, minTime=True), cmds.playbackOptions(query=True, maxTime=True)],
 "animation_range": [cmds.playbackOptions(query=True, animationStartTime=True), cmds.playbackOptions(query=True, animationEndTime=True)],
 "renderer": cmds.getAttr("defaultRenderGlobals.currentRenderer"),
 "node_count_by_type": dict(sorted(__counts.items(), key=lambda kv: -kv[1])[:20])}"#;
        run_tool("python", code, json!({}), |resp| {
            resp.get("result").map(value_to_text).unwrap_or_else(|| "执行成功".into())
        })
        .await
    }
}

#[tool_handler(
    router = self.tool_router,
    name = "maya-mcp",
    version = "0.1.0",
    instructions = "该服务连接到一个已打开的 Maya 实例（通过 Maya 内运行的 listener 脚本），\
        可在其中执行 Python / MEL 代码并查询场景。eval_python 的命名空间已预置 cmds/mel/om2。\
        若连接失败，请提示用户在 Maya 中执行: p = r\"<路径>/maya/maya_mcp_listener.py\"; \
        exec(compile(open(p, \"rb\").read(), p, \"exec\"))"
)]
impl ServerHandler for MayaMcp {}
