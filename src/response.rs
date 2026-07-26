use anyhow::Result;
use serde_json::{json, Value};

pub type ToolResult = Result<Value, Value>;

/// Maximum text payload returned by high-volume diagnostic tools.
pub const DEFAULT_TEXT_BUDGET_BYTES: usize = 256 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TruncationStrategy {
    /// Retain the beginning of the output (useful for files and structured dumps).
    Head,
    /// Retain the end of the output (useful for chronological logs and events).
    Tail,
}

/// Create a standard text response
#[allow(
    clippy::unnecessary_wraps,
    reason = "returns ToolResult so tool bodies can pair it with error_response \
              in the same tail position; unwrapping it would churn ~100 sites"
)]
pub fn text_response(msg: impl Into<String>) -> ToolResult {
    Ok(json!({
        "content": [{
            "type": "text",
            "text": msg.into()
        }]
    }))
}

/// Create a text response with a strict UTF-8-safe byte budget. When truncation
/// occurs, machine-readable metadata is included alongside the normal MCP
/// content so clients do not mistake a partial result for complete output.
pub fn bounded_text_response(
    msg: impl Into<String>,
    limit_bytes: usize,
    strategy: TruncationStrategy,
) -> ToolResult {
    let msg = msg.into();
    let original_bytes = msg.len();
    if original_bytes <= limit_bytes {
        return text_response(msg);
    }

    let text = match strategy {
        TruncationStrategy::Head => {
            let mut end = limit_bytes.min(msg.len());
            while end > 0 && !msg.is_char_boundary(end) {
                end -= 1;
            }
            msg[..end].to_owned()
        }
        TruncationStrategy::Tail => {
            let mut start = msg.len().saturating_sub(limit_bytes);
            while start < msg.len() && !msg.is_char_boundary(start) {
                start += 1;
            }
            msg[start..].to_owned()
        }
    };
    let returned_bytes = text.len();
    let strategy_name = match strategy {
        TruncationStrategy::Head => "head",
        TruncationStrategy::Tail => "tail",
    };

    Ok(json!({
        "content": [{
            "type": "text",
            "text": text
        }],
        "metadata": {
            "truncation": {
                "truncated": true,
                "strategy": strategy_name,
                "original_bytes": original_bytes,
                "returned_bytes": returned_bytes,
                "limit_bytes": limit_bytes
            }
        }
    }))
}

/// Build the JSON-RPC error payload for a message. Exposed separately from
/// [`error_response`] so argument validation can raise the same error through
/// `?` without having to unwrap a `Result` that is only ever `Err`.
pub fn error_payload(msg: impl Into<String>) -> Value {
    let msg = msg.into();
    if msg.len() <= DEFAULT_TEXT_BUDGET_BYTES {
        return json!({
            "code": -32000,
            "message": msg
        });
    }
    // Oversized errors are truncated the same way oversized successes are, so a
    // runaway message cannot exceed the transport budget.
    match bounded_text_response(msg, DEFAULT_TEXT_BUDGET_BYTES, TruncationStrategy::Head) {
        Ok(bounded) => json!({
            "code": -32000,
            "message": bounded["content"][0]["text"],
            "data": bounded["metadata"]
        }),
        Err(error) => error,
    }
}

/// Create a standard JSON-RPC error response
pub fn error_response(msg: impl Into<String>) -> ToolResult {
    Err(error_payload(msg))
}

#[cfg(test)]
mod tests {
    use super::{
        bounded_text_response, error_response, TruncationStrategy, DEFAULT_TEXT_BUDGET_BYTES,
    };

    #[test]
    fn bounded_response_preserves_complete_utf8_from_head() {
        let result = bounded_text_response("abéé", 3, TruncationStrategy::Head).unwrap();
        assert_eq!(result["content"][0]["text"], "ab");
        assert_eq!(result["metadata"]["truncation"]["original_bytes"], 6);
        assert_eq!(result["metadata"]["truncation"]["returned_bytes"], 2);
        assert_eq!(result["metadata"]["truncation"]["strategy"], "head");
    }

    #[test]
    fn bounded_response_preserves_complete_utf8_from_tail() {
        let result = bounded_text_response("abéé", 3, TruncationStrategy::Tail).unwrap();
        assert_eq!(result["content"][0]["text"], "é");
        assert_eq!(result["metadata"]["truncation"]["returned_bytes"], 2);
        assert_eq!(result["metadata"]["truncation"]["strategy"], "tail");
    }

    #[test]
    fn bounded_response_omits_metadata_when_complete() {
        let result = bounded_text_response("small", 5, TruncationStrategy::Head).unwrap();
        assert_eq!(result["content"][0]["text"], "small");
        assert!(result.get("metadata").is_none());
    }

    #[test]
    fn oversized_error_messages_are_bounded_with_metadata() {
        let result = error_response("é".repeat(DEFAULT_TEXT_BUDGET_BYTES)).unwrap_err();
        let message = result["message"].as_str().unwrap();
        assert!(message.len() <= DEFAULT_TEXT_BUDGET_BYTES);
        assert!(message.is_char_boundary(message.len()));
        assert_eq!(result["data"]["truncation"]["truncated"], true);
        assert_eq!(result["data"]["truncation"]["strategy"], "head");
    }
}
