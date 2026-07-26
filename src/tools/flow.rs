use crate::response::{self, ToolResult};
use crate::tools::Tool;
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct RunFlowTool;

fn validate(steps: &[Value]) -> Result<(), String> {
    if steps.is_empty() || steps.len() > 20 {
        return Err("flow must contain between 1 and 20 steps".into());
    }
    for (index, step) in steps.iter().enumerate() {
        let tool = step
            .get("tool")
            .and_then(Value::as_str)
            .ok_or_else(|| format!("step {index}: tool is required"))?;
        let action = step
            .get("action")
            .and_then(Value::as_str)
            .ok_or_else(|| format!("step {index}: action is required"))?;
        let capability = crate::capabilities::classify(tool, action)
            .ok_or_else(|| format!("step {index}: unsupported tool/action"))?;
        if !crate::capabilities::batch_allowed(capability) {
            return Err(format!(
                "step {index}: capability {capability:?} is not batch-safe"
            ));
        }
        if !matches!(
            (tool, action),
            ("input", "tap" | "text" | "key")
                | (
                    "app",
                    "list" | "get_foreground" | "list_crashes" | "get_crash" | "launch" | "stop"
                )
                | ("vision", "hierarchy" | "elements")
        ) {
            return Err(format!(
                "step {index}: action has no bounded flow dispatcher"
            ));
        }
        if step.get("args").is_some_and(|args| !args.is_object()) {
            return Err(format!("step {index}: args must be an object"));
        }
        validate_args(index, tool, action, step.get("args"))?;
    }
    Ok(())
}

fn validate_args(
    index: usize,
    tool: &str,
    action: &str,
    args: Option<&Value>,
) -> Result<(), String> {
    let empty = serde_json::Map::new();
    let args = args.and_then(Value::as_object).unwrap_or(&empty);
    let error = |message: &str| Err(format!("step {index}: {message}"));
    let nonempty_string = |name: &str| {
        args.get(name)
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| format!("step {index}: {name} must be a non-empty string"))
    };
    let optional_string = |name: &str| {
        if args.get(name).is_some_and(|value| !value.is_string()) {
            error(&format!("{name} must be a string"))
        } else {
            Ok(())
        }
    };
    let optional_bool = |name: &str| {
        if args.get(name).is_some_and(|value| !value.is_boolean()) {
            error(&format!("{name} must be a boolean"))
        } else {
            Ok(())
        }
    };

    match (tool, action) {
        ("input", "tap") => {
            for name in ["x", "y"] {
                if args
                    .get(name)
                    .and_then(Value::as_i64)
                    .and_then(|value| i32::try_from(value).ok())
                    .is_none()
                {
                    return error(&format!("{name} must be a 32-bit integer"));
                }
            }
        }
        ("input", "text") => {
            nonempty_string("text")?;
            if let Some(mode) = args.get("mode") {
                let mode = mode
                    .as_str()
                    .ok_or_else(|| format!("step {index}: mode must be a string"))?;
                if crate::input::TextMode::parse(mode).is_none() {
                    return error("mode must be auto, ascii, unicode, or clipboard");
                }
            }
        }
        ("input", "key") => {
            let keycode = nonempty_string("keycode")?;
            if !keycode
                .chars()
                .all(|character| character.is_ascii_alphanumeric() || character == '_')
            {
                return error("keycode may contain only ASCII letters, digits, and underscores");
            }
            optional_bool("force")?;
        }
        ("app", "list") => optional_bool("third_party")?,
        ("app", "list_crashes") => {
            optional_string("package_name")?;
            if let Some(limit) = args.get("limit") {
                if !matches!(limit.as_u64(), Some(1..=100)) {
                    return error("limit must be an integer between 1 and 100");
                }
            }
        }
        ("app", "get_crash") => optional_string("package_name")?,
        ("app", "launch") => {
            nonempty_string("package_name")?;
            optional_bool("force_stop")?;
        }
        ("app", "stop") => {
            nonempty_string("package_name")?;
        }
        ("app", "get_foreground") | ("vision", "hierarchy" | "elements") => {}
        _ => return error("unsupported flow action"),
    }
    Ok(())
}

#[async_trait]
impl Tool for RunFlowTool {
    fn name(&self) -> &'static str {
        "mcp_android_run_flow"
    }
    fn description(&self) -> &'static str {
        "Run a fully prevalidated bounded sequence of safe Android actions"
    }
    fn schema(&self) -> Value {
        json!({"inputSchema":{"type":"object","properties":{"steps":{"type":"array","minItems":1,"maxItems":20,"items":{"type":"object","properties":{"tool":{"type":"string","enum":["input","app","vision"]},"action":{"type":"string"},"args":{"type":"object"}},"required":["tool","action"]}}},"required":["steps"]}})
    }
    async fn execute(&self, args: &Value, ctx: &crate::tools::ToolContext) -> ToolResult {
        let steps = args
            .get("steps")
            .and_then(Value::as_array)
            .ok_or_else(|| json!({"code":-32000,"message":"steps must be an array"}))?;
        if let Err(error) = validate(steps) {
            return response::error_response(error);
        }
        let mut results = Vec::with_capacity(steps.len());
        for (index, step) in steps.iter().enumerate() {
            let Some(tool) = step.get("tool").and_then(Value::as_str) else {
                return response::error_response(format!("step {index}: tool is required"));
            };
            let Some(action) = step.get("action").and_then(Value::as_str) else {
                return response::error_response(format!("step {index}: action is required"));
            };
            let a = step.get("args").cloned().unwrap_or_else(|| json!({}));
            let result = dispatch(tool, action, &a, ctx).await;
            match result {
                Ok(value) => results.push(json!({"index":index,"ok":true,"result":value})),
                Err(error) => {
                    results.push(json!({"index":index,"ok":false,"error":error}));
                    return response::bounded_text_response(
                        json!({"completed":false,"failed_at":index,"results":results}).to_string(),
                        response::DEFAULT_TEXT_BUDGET_BYTES,
                        response::TruncationStrategy::Head,
                    );
                }
            }
        }
        response::bounded_text_response(
            json!({"completed":true,"results":results}).to_string(),
            response::DEFAULT_TEXT_BUDGET_BYTES,
            response::TruncationStrategy::Head,
        )
    }
    fn needs_unlock(&self, args: &Value) -> bool {
        args.get("steps")
            .and_then(Value::as_array)
            .is_some_and(|steps| {
                steps.iter().any(|step| {
                    step.get("tool").and_then(Value::as_str) == Some("input")
                        || (step.get("tool").and_then(Value::as_str) == Some("app")
                            && step.get("action").and_then(Value::as_str) == Some("launch"))
                })
            })
    }
}

async fn dispatch(
    tool: &str,
    action: &str,
    args: &Value,
    ctx: &crate::tools::ToolContext,
) -> ToolResult {
    match (tool, action) {
        ("vision", "hierarchy") => crate::vision::get_view_hierarchy().await,
        ("vision", "elements") => match crate::vision::fetch_parsed_hierarchy().await {
            Ok(root) => match serde_json::to_string(&crate::element_snapshots::build(&root)) {
                Ok(value) => response::bounded_text_response(
                    value,
                    response::DEFAULT_TEXT_BUDGET_BYTES,
                    response::TruncationStrategy::Head,
                ),
                Err(error) => {
                    response::error_response(format!("Element serialization failed: {error}"))
                }
            },
            Err(e) => response::error_response(e.to_string()),
        },
        ("input", "tap") => {
            let x = args
                .get("x")
                .and_then(Value::as_i64)
                .and_then(|v| i32::try_from(v).ok());
            let y = args
                .get("y")
                .and_then(Value::as_i64)
                .and_then(|v| i32::try_from(v).ok());
            match (x, y) {
                (Some(x), Some(y)) => crate::input::tap(&ctx.stream_manager, x, y).await,
                _ => response::error_response("tap requires 32-bit x and y"),
            }
        }
        ("input", "text") => match args.get("text").and_then(Value::as_str) {
            Some(v) if !v.is_empty() => {
                let mode = args.get("mode").and_then(Value::as_str).unwrap_or("auto");
                match crate::input::TextMode::parse(mode) {
                    Some(mode) => crate::input::text_with_mode(&ctx.stream_manager, v, mode).await,
                    None => response::error_response("invalid text mode"),
                }
            }
            _ => response::error_response("text is required"),
        },
        ("input", "key") => match args.get("keycode").and_then(Value::as_str) {
            Some(v) if !v.is_empty() => {
                crate::input::key(
                    &ctx.stream_manager,
                    v,
                    args.get("force").and_then(Value::as_bool).unwrap_or(false),
                )
                .await
            }
            _ => response::error_response("keycode is required"),
        },
        ("app", "list") => {
            crate::app::list_apps(
                args.get("third_party")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
            )
            .await
        }
        ("app", "get_foreground") => crate::app::get_foreground_app().await,
        ("app", "list_crashes") => {
            crate::app::structured_crashes(
                args.get("package_name")
                    .and_then(Value::as_str)
                    .unwrap_or(""),
                args.get("limit")
                    .and_then(Value::as_u64)
                    .unwrap_or(10)
                    .clamp(1, 100) as usize,
                false,
            )
            .await
        }
        ("app", "get_crash") => {
            crate::app::structured_crashes(
                args.get("package_name")
                    .and_then(Value::as_str)
                    .unwrap_or(""),
                1,
                true,
            )
            .await
        }
        ("app", "launch") => match args.get("package_name").and_then(Value::as_str) {
            Some(v) => {
                crate::app::launch_app(
                    v,
                    args.get("force_stop")
                        .and_then(Value::as_bool)
                        .unwrap_or(false),
                )
                .await
            }
            None => response::error_response("package_name is required"),
        },
        ("app", "stop") => match args.get("package_name").and_then(Value::as_str) {
            Some(v) => crate::app::stop_app(v).await,
            None => response::error_response("package_name is required"),
        },
        _ => response::error_response("unsupported flow action"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prevalidates_entire_flow_and_rejects_destructive_or_unknown_steps() {
        assert!(validate(&[json!({"tool":"app","action":"list"})]).is_ok());
        assert!(validate(&[
            json!({"tool":"app","action":"list"}),
            json!({"tool":"app","action":"clear_data"})
        ])
        .expect_err("clear_data must be rejected")
        .contains("not batch-safe"));
        assert!(validate(&[json!({"tool":"input","action":"swipe"})])
            .expect_err("swipe has no dispatcher")
            .contains("dispatcher"));
    }

    #[test]
    fn rejects_malformed_later_step_before_execution() {
        let error = validate(&[
            json!({"tool":"app","action":"launch","args":{"package_name":"com.example"}}),
            json!({"tool":"input","action":"tap","args":{"x":10}}),
        ])
        .expect_err("the incomplete second step must fail validation");
        assert!(error.contains("step 1"));
        assert!(error.contains("y must be a 32-bit integer"));
    }

    #[test]
    fn validates_action_specific_types_ranges_and_modes() {
        assert!(
            validate(&[json!({"tool":"app","action":"launch","args":{"package_name":""}})])
                .is_err()
        );
        assert!(
            validate(&[json!({"tool":"app","action":"list_crashes","args":{"limit":101}})])
                .is_err()
        );
        assert!(validate(&[
            json!({"tool":"input","action":"text","args":{"text":"hello","mode":"lossy"}})
        ])
        .is_err());
        assert!(validate(&[
            json!({"tool":"input","action":"key","args":{"keycode":"BACK; reboot"}})
        ])
        .is_err());
    }

    #[test]
    fn launch_only_flow_requests_unlock() {
        use crate::tools::Tool;

        let args = json!({"steps":[{"tool":"app","action":"launch","args":{"package_name":"com.example"}}]});
        assert!(RunFlowTool.needs_unlock(&args));
    }

    #[test]
    fn enforces_bounds_and_object_args() {
        assert!(validate(&[]).is_err());
        assert!(validate(&[json!({"tool":"app","action":"list","args":[]})]).is_err());
    }
}
