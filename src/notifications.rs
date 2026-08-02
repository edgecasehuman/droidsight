use crate::adb::Adb;
use crate::response::{self, ToolResult};

pub async fn get_notifications() -> ToolResult {
    // Return the complete dump when it fits. Host-side budgeting adds explicit
    // metadata when it does not, unlike a device-side `head` pipeline whose
    // partial result was indistinguishable from a complete response.
    match Adb::shell(&["shell", "dumpsys", "notification", "--noredact"]).await {
        Ok(output) => response::bounded_text_response(
            output,
            response::DEFAULT_TEXT_BUDGET_BYTES,
            response::TruncationStrategy::Head,
        ),
        Err(e) => response::error_response(e.to_string()),
    }
}

pub async fn set_clipboard(text: &str) -> ToolResult {
    let cmd = format!(
        "service call clipboard 2 i32 1 i32 0 s16 {}",
        crate::adb::shell_quote(text)
    );

    let result = Adb::device_shell(&cmd).await;
    let output = match &result {
        Ok(s) => s.as_str(),
        Err(e) => return response::error_response(e.to_string()),
    };

    if output.contains("Permission Denial") || output.contains("security exception") {
        return response::error_response("Clipboard Set Failed: Access blocked by Android 13+ security policies. Use 'input_act' to type text instead.");
    }

    match result {
        Ok(_) => response::text_response("Clipboard set (Sent)"),
        Err(e) => response::error_response(e.to_string()),
    }
}

pub async fn get_clipboard() -> ToolResult {
    let cmd_std = "service call clipboard 1";
    let result = Adb::device_shell(cmd_std).await;

    let output = match &result {
        Ok(s) => s.clone(),
        Err(e) => e.to_string(),
    };

    // Check for known Android 13+ restrictions in either stdout or stderr (via Adb Error string)
    if output.contains("Permission Denial")
        || output.contains("checkAndSetPrimaryClip")
        || output.contains("security exception")
    {
        // Try Samsung-specific fallback
        match Adb::device_shell("service call semclipboard 1").await {
            Ok(sem_out)
                if !sem_out.contains("Parcel(00000000    '....')")
                    && !sem_out.contains("Permission Denial") =>
            {
                return clipboard_response(format!("(Samsung Clipboard)\n{sem_out}"));
            }
            _ => {
                return response::error_response("Clipboard Read Failed: Access restricted by Android 13+.\n\nWorkaround: Android prevents shell access to the clipboard for security. Use 'vision_query' to read the screen or manual copy/paste.");
            }
        }
    }

    match result {
        Ok(output) => {
            // 1. Check for Java Exceptions or Stack Traces in the output
            if output.contains("Exception")
                || output.contains("Permission Denial")
                || output.contains("at android.")
                || output.contains("at com.android")
                || output.contains("ClipboardService")
            {
                return response::error_response(format!("Clipboard Read Failed: The system returned an error/exception. (Android 10+ restricts background clipboard access).\nOriginal Output: {output}"));
            }

            // 2. Parse Parcel
            let clean_text = if output.contains("Parcel(") {
                let mut decoded = String::new();

                for line in output.lines() {
                    if let Some(idx) = line.find('\'') {
                        if let Some(end_idx) = line.rfind('\'') {
                            if end_idx > idx {
                                let content = &line[idx + 1..end_idx];
                                // In `service call` output, '.' stands for null or
                                // non-printable bytes; drop those and keep the rest.
                                for c in content.chars() {
                                    if c != '.' {
                                        decoded.push(c);
                                    }
                                }
                            }
                        }
                    }
                }
                if decoded.is_empty() {
                    // A parsed Parcel that yields no text is treated as empty.
                    String::new()
                } else {
                    decoded
                }
            } else {
                output
            };

            clipboard_response(clean_text)
        }
        Err(e) => response::error_response(e.to_string()),
    }
}

fn clipboard_response(text: String) -> ToolResult {
    response::bounded_text_response(
        text,
        response::DEFAULT_TEXT_BUDGET_BYTES,
        response::TruncationStrategy::Head,
    )
}

#[cfg(test)]
mod tests {
    use super::clipboard_response;
    use crate::response::DEFAULT_TEXT_BUDGET_BYTES;

    #[test]
    fn clipboard_adapter_is_utf8_safe_and_reports_truncation() {
        let result = clipboard_response("é".repeat(DEFAULT_TEXT_BUDGET_BYTES)).unwrap();
        let text = result["content"][0]["text"].as_str().unwrap();
        assert!(text.is_char_boundary(text.len()));
        assert_eq!(result["metadata"]["truncation"]["strategy"], "head");
    }
}
