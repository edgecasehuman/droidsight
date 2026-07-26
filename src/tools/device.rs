use crate::notifications;
use crate::response::{self, ToolResult};
use crate::system;
use crate::tools::Tool;
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct DeviceControlTool;

#[async_trait]
impl Tool for DeviceControlTool {
    fn name(&self) -> &'static str {
        "mcp_android_device_control"
    }

    fn description(&self) -> &'static str {
        "Control device (clipboard, battery, info, state, unlock, rotate)"
    }

    fn schema(&self) -> Value {
        json!({
            "inputSchema": {
                "type": "object",
                "properties": {
                    "feature": {
                        "type": "string",
                        "enum": ["clipboard_get", "clipboard_set", "battery", "info", "state", "unlock", "rotate"]
                    },
                    "value": { "type": "string" },
                    "unlock": { "type": "object", "properties": { "value": { "type": "string" } } },
                    "rotate": { "type": "object", "properties": { "value": { "type": "string", "enum": ["portrait", "landscape"] } } },
                    "wait_ms": crate::tools::wait_ms_property(200)
                },
                "required": ["feature"]
            }
        })
    }

    async fn execute(&self, args: &Value, ctx: &crate::tools::ToolContext) -> ToolResult {
        let feature = args
            .get("feature")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let wait_ms = args
            .get("wait_ms")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(200);

        let args = args.clone();

        ctx.run_with_observation(wait_ms, || async move {
            match feature.as_str() {
                "clipboard_get" => notifications::get_clipboard().await,
                "clipboard_set" => {
                    let val = args.get("value").and_then(|v| v.as_str()).unwrap_or("");
                    notifications::set_clipboard(val).await
                }
                "battery" => system::get_battery_status().await,
                "info" => system::get_device_info().await,
                "state" => structured_state(&ctx.stream_manager).await,
                "unlock" => {
                    let pin = args
                        .get("value")
                        .and_then(|v| v.as_str())
                        .map(std::string::ToString::to_string);
                    // Handle nested 'unlock' object from schema if present
                    let pin = if pin.is_none() {
                        args.get("unlock")
                            .and_then(|o| o.get("value"))
                            .and_then(|v| v.as_str())
                            .map(std::string::ToString::to_string)
                    } else {
                        pin
                    };

                    system::unlock_device(pin).await
                }
                "rotate" => {
                    // Try top level, then object
                    let mut mode = args.get("value").and_then(|v| v.as_str()).unwrap_or("");
                    if mode.is_empty() {
                        mode = args
                            .get("rotate")
                            .and_then(|o| o.get("value"))
                            .and_then(|v| v.as_str())
                            .unwrap_or("portrait");
                    }
                    system::set_orientation(mode).await
                }
                _ => response::error_response(format!("Unknown device feature: {feature}")),
            }
        })
        .await
    }
}

async fn structured_state(stream: &std::sync::Arc<crate::stream::StreamManager>) -> ToolResult {
    let serial = match crate::adb::Adb::selected_serial().await {
        Ok(v) => v,
        Err(e) => return response::error_response(e.to_string()),
    };
    let (display_result, power_result, windows_result, keyboard_result) = tokio::join!(
        crate::adb::Adb::shell_native("wm size"),
        crate::adb::Adb::shell_native("dumpsys power"),
        crate::adb::Adb::shell_native("dumpsys window windows"),
        crate::device_metrics::detect_keyboard_state(),
    );
    let mut warnings = Vec::new();
    let display = display_result
        .map(|value| value.trim().to_owned())
        .map_err(|error| warnings.push(format!("display: {error}")))
        .ok();
    let power = power_result
        .map_err(|error| warnings.push(format!("power: {error}")))
        .ok();
    let windows = windows_result
        .map_err(|error| warnings.push(format!("window: {error}")))
        .ok();
    let keyboard = keyboard_result
        .map_err(|error| warnings.push(format!("keyboard: {error}")))
        .ok();
    let foreground = windows.as_deref().and_then(|value| {
        value
            .lines()
            .find(|l| l.contains("mCurrentFocus=") || l.contains("mFocusedApp="))
            .map(str::trim)
    });
    let screen_on = power.as_deref().map(|value| {
        value.contains("mWakefulness=Awake") || value.contains("Display Power: state=ON")
    });
    let (stream_running, frame_age_ms) = {
        let running = stream.running.lock().is_ok_and(|v| *v);
        let ts = stream.frame_timestamp_ms.lock().map_or(0, |v| *v);
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        (running, (ts != 0).then(|| now.saturating_sub(ts)))
    };
    response::bounded_text_response(
        json!({
            "serial": serial,
            "display": display,
            "screen_on": screen_on,
            "foreground": foreground,
            "keyboard": keyboard.map(|value| json!({
                "visible": value.visible,
                "height": (value.height != 0).then_some(value.height),
            })),
            "stream": {"running": stream_running, "frame_age_ms": frame_age_ms},
            "warnings": warnings,
        })
        .to_string(),
        response::DEFAULT_TEXT_BUDGET_BYTES,
        response::TruncationStrategy::Head,
    )
}

pub struct CheckHealthTool;

#[async_trait]
impl Tool for CheckHealthTool {
    fn name(&self) -> &'static str {
        "mcp_android_check_health"
    }

    fn description(&self) -> &'static str {
        "Checks device health"
    }

    fn schema(&self) -> Value {
        json!({
             "inputSchema": {
                "type": "object",
                "properties": {}
             }
        })
    }

    async fn execute(&self, _args: &Value, _ctx: &crate::tools::ToolContext) -> ToolResult {
        let serial = match crate::adb::Adb::selected_serial().await {
            Ok(serial) => serial,
            Err(error) => {
                return response::error_response(format!("ADB device unavailable: {error}"))
            }
        };
        let device_state = match crate::adb::Adb::shell(&["get-state"]).await {
            Ok(state) if state == "device" => state,
            Ok(state) => {
                return response::error_response(format!("Device {serial} is in state {state}"))
            }
            Err(error) => {
                return response::error_response(format!(
                    "Device {serial} health check failed: {error}"
                ))
            }
        };

        response::bounded_text_response(
            format!(
                "droidsight {} | device={serial} | state={device_state}",
                env!("CARGO_PKG_VERSION")
            ),
            response::DEFAULT_TEXT_BUDGET_BYTES,
            response::TruncationStrategy::Head,
        )
    }
}

pub struct CheckDebugExposureTool;

#[async_trait]
impl Tool for CheckDebugExposureTool {
    fn name(&self) -> &'static str {
        "mcp_android_check_debug_exposure"
    }

    fn description(&self) -> &'static str {
        "Report which developer settings (ADB, Developer Options, USB debugging) are enabled and therefore visible to apps on the device"
    }

    fn schema(&self) -> Value {
        json!({
            "inputSchema": {
                "type": "object",
                "properties": {}
            }
        })
    }

    async fn execute(&self, _args: &Value, _ctx: &crate::tools::ToolContext) -> ToolResult {
        crate::debug_exposure::check_debug_exposure().await
    }
}
