use crate::network;
use crate::response::{self, ToolResult};
use crate::tools::Tool;
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct NetworkControlTool;

#[async_trait]
impl Tool for NetworkControlTool {
    fn name(&self) -> &'static str {
        "mcp_android_network_control"
    }

    fn description(&self) -> &'static str {
        "Network control (Wi-Fi, mobile data, proxy, calls, SMS, wireless ADB pairing)"
    }

    fn schema(&self) -> Value {
        json!({
            "inputSchema": {
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["set_wifi", "set_data", "scan_wifi", "connect_wifi", "forget_wifi", "set_proxy", "call", "sms", "pair_wireless"]
                    },
                    "enabled": { "type": "boolean", "description": "Target state for set_wifi and set_data" },
                    "ssid": { "type": "string", "description": "Network name for connect_wifi and forget_wifi" },
                    "password": { "type": "string", "description": "WPA2 passphrase for connect_wifi. Sent to the device in clear text" },
                    "host": { "type": "string", "description": "Proxy host for set_proxy, or device address for pair_wireless" },
                    "port": { "type": "integer", "description": "Proxy port for set_proxy (0 clears the proxy), or pairing port for pair_wireless" },
                    "number": { "type": "string", "description": "Destination phone number for call and sms" },
                    "message": { "type": "string", "description": "Body text prefilled into the SMS composer" },
                    "code": { "type": "string", "description": "6-digit pairing code" },
                    "wait_ms": crate::tools::wait_ms_property(1000)
                },
                "required": ["action"]
            }
        })
    }

    async fn execute(&self, args: &Value, ctx: &crate::tools::ToolContext) -> ToolResult {
        let action = args
            .get("action")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let wait_ms = args
            .get("wait_ms")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(1000);

        let args = args.clone();

        ctx.run_with_observation(wait_ms, || async move {
            match action.as_str() {
                "set_wifi" => {
                    let enabled = args
                        .get("enabled")
                        .and_then(serde_json::Value::as_bool)
                        .unwrap_or(true);
                    network::set_wifi(enabled).await
                }
                "set_data" => {
                    let enabled = args
                        .get("enabled")
                        .and_then(serde_json::Value::as_bool)
                        .unwrap_or(true);
                    network::set_mobile_data(enabled).await
                }
                "scan_wifi" => network::scan_wifi().await,
                "connect_wifi" => {
                    let ssid = args.get("ssid").and_then(|v| v.as_str()).unwrap_or("");
                    let pwd = args.get("password").and_then(|v| v.as_str()).unwrap_or("");
                    if ssid.is_empty() || pwd.is_empty() {
                        return response::error_response(
                            "ssid and password are required for WPA2 connection",
                        );
                    }
                    network::connect_wifi(ssid, pwd).await
                }
                "forget_wifi" => {
                    let ssid = args.get("ssid").and_then(|v| v.as_str()).unwrap_or("");
                    if ssid.is_empty() {
                        return response::error_response("ssid is required for forget_wifi");
                    }
                    network::forget_wifi(ssid).await
                }
                "set_proxy" => {
                    let host = args.get("host").and_then(|v| v.as_str()).unwrap_or("");
                    let port = match args
                        .get("port")
                        .and_then(serde_json::Value::as_i64)
                        .unwrap_or(0)
                    {
                        value @ 0..=65_535 => value as i32,
                        _ => return response::error_response("port must be between 0 and 65535"),
                    };
                    if port != 0 && host.is_empty() {
                        return response::error_response("host is required when enabling a proxy");
                    }
                    network::set_proxy(host, port).await
                }
                "call" => {
                    let number = args.get("number").and_then(|v| v.as_str()).unwrap_or("");
                    if number.is_empty() {
                        return response::error_response("number is required for call");
                    }
                    network::make_call(number).await
                }
                "sms" => {
                    let number = args.get("number").and_then(|v| v.as_str()).unwrap_or("");
                    let message = args.get("message").and_then(|v| v.as_str()).unwrap_or("");
                    if number.is_empty() || message.is_empty() {
                        return response::error_response(
                            "number and message are required for sms composition",
                        );
                    }
                    network::send_sms(number, message).await
                }
                "pair_wireless" => {
                    let host = args.get("host").and_then(|v| v.as_str()).unwrap_or("");
                    let port = args
                        .get("port")
                        .and_then(serde_json::Value::as_i64)
                        .unwrap_or(0);
                    let code = args.get("code").and_then(|v| v.as_str()).unwrap_or("");

                    if host.is_empty()
                        || !(1..=65_535).contains(&port)
                        || code.len() != 6
                        || !code.chars().all(|ch| ch.is_ascii_digit())
                    {
                        response::error_response(
                            "pair_wireless requires host, port 1-65535, and a 6-digit code",
                        )
                    } else {
                        network::pair_wireless(host, port, code).await
                    }
                }
                _ => response::error_response(format!("Unknown network action: {action}")),
            }
        })
        .await
    }
}
