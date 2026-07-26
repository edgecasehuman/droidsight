use crate::response::ToolResult;
use crate::tools::Tool;
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct SmartWaitTool;

#[async_trait]
impl Tool for SmartWaitTool {
    fn name(&self) -> &'static str {
        "mcp_android_smart_wait"
    }

    fn description(&self) -> &'static str {
        "Wait for element"
    }

    fn schema(&self) -> Value {
        json!({
            "inputSchema": {
                "type": "object",
                "properties": {
                    "text": { "type": "string" },
                    "resource_id": { "type": "string" },
                    "content_desc": { "type": "string" },
                    "timeout": { "type": "integer" }
                    ,"condition": {"type":"string","enum":["element_present","element_absent","text_present","text_absent","screen_changed","screen_stable"]}
                    ,"poll_ms":{"type":"integer","minimum":400,"maximum":5000}
                }
            }
        })
    }

    async fn execute(&self, args: &Value, _ctx: &crate::tools::ToolContext) -> ToolResult {
        let text = args.get("text").and_then(|v| v.as_str()).unwrap_or("");
        let id = args
            .get("resource_id")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let desc = args
            .get("content_desc")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let timeout = args
            .get("timeout")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(10000);
        let condition = args
            .get("condition")
            .and_then(Value::as_str)
            .unwrap_or("element_present");
        if text.is_empty()
            && id.is_empty()
            && desc.is_empty()
            && !matches!(condition, "screen_changed" | "screen_stable")
        {
            return crate::response::error_response(
                "text, resource_id, or content_desc is required",
            );
        }
        if timeout == 0 || timeout > 120_000 {
            return crate::response::error_response(
                "timeout must be between 1 and 120000 milliseconds",
            );
        }
        let poll = args.get("poll_ms").and_then(Value::as_u64).unwrap_or(400);
        if !(400..=5000).contains(&poll) {
            return crate::response::error_response("poll_ms must be between 400 and 5000");
        }
        generic_wait(condition, text, id, desc, timeout, poll).await
    }

    fn needs_unlock(&self, _args: &Value) -> bool {
        true
    }
    fn holds_device_lock(&self) -> bool {
        false
    }
}

fn contains(node: &crate::vision::UiNode, text: &str, id: &str, desc: &str) -> bool {
    (!text.is_empty() && node.text.contains(text))
        || (!id.is_empty() && node.resource_id.contains(id))
        || (!desc.is_empty() && node.content_desc.contains(desc))
        || node.children.iter().any(|c| contains(c, text, id, desc))
}

async fn generic_wait(
    condition: &str,
    text: &str,
    id: &str,
    desc: &str,
    timeout: u64,
    poll: u64,
) -> ToolResult {
    if !matches!(
        condition,
        "element_present"
            | "element_absent"
            | "text_present"
            | "text_absent"
            | "screen_changed"
            | "screen_stable"
    ) {
        return crate::response::error_response("unknown wait condition");
    }
    let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_millis(timeout);
    let mut baseline = None;
    let mut stable_count = 0;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return crate::response::error_response(format!("wait timed out: {condition}"));
        }
        let hierarchy = async {
            let _device_guard = crate::tools::DEVICE_OPERATION_LOCK.lock().await;
            crate::vision::fetch_parsed_hierarchy().await
        };
        let root = match tokio::time::timeout(remaining, hierarchy).await {
            Ok(Ok(root)) => root,
            Ok(Err(error)) => {
                if tokio::time::Instant::now() >= deadline {
                    return crate::response::error_response(format!(
                        "wait timed out after hierarchy error: {error}"
                    ));
                }
                tokio::time::sleep(poll_duration(deadline, poll)).await;
                continue;
            }
            Err(_) => {
                return crate::response::error_response(format!("wait timed out: {condition}"))
            }
        };
        let snapshot = crate::element_snapshots::build(&root);
        let found = contains(&root, text, id, desc);
        let met = match condition {
            "element_present" | "text_present" => found,
            "element_absent" | "text_absent" => !found,
            "screen_changed" => baseline
                .as_ref()
                .is_some_and(|v| v != &snapshot.snapshot_id),
            "screen_stable" => {
                if baseline.as_ref() == Some(&snapshot.snapshot_id) {
                    stable_count += 1;
                } else {
                    stable_count = 0;
                }
                stable_count >= 2
            }
            _ => false,
        };
        if met {
            return crate::response::text_response(
                json!({"condition":condition,"met":true,"snapshot_id":snapshot.snapshot_id})
                    .to_string(),
            );
        }
        if baseline.is_none() || condition == "screen_stable" {
            baseline = Some(snapshot.snapshot_id);
        }
        tokio::time::sleep(poll_duration(deadline, poll)).await;
    }
}

fn poll_duration(deadline: tokio::time::Instant, poll_ms: u64) -> tokio::time::Duration {
    deadline
        .saturating_duration_since(tokio::time::Instant::now())
        .min(tokio::time::Duration::from_millis(poll_ms))
}

#[cfg(test)]
mod generic_wait_tests {
    use super::*;
    #[test]
    fn recursive_match_checks_all_identity_fields() {
        let mut root = crate::vision::UiNode::default();
        root.children.push(crate::vision::UiNode {
            text: "Done".into(),
            resource_id: "pkg:id/ok".into(),
            content_desc: "finish".into(),
            ..Default::default()
        });
        assert!(contains(&root, "Done", "", ""));
        assert!(contains(&root, "", "pkg:id", ""));
        assert!(!contains(&root, "Missing", "", ""));
    }
    #[tokio::test]
    async fn poll_sleep_never_exceeds_deadline() {
        let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_millis(5);
        assert!(poll_duration(deadline, 400) <= tokio::time::Duration::from_millis(5));
    }
    #[test]
    fn smart_wait_releases_lock_between_probes() {
        let tool = SmartWaitTool;
        assert!(!tool.holds_device_lock());
        assert!(tool.needs_unlock(&json!({})));
    }
}
