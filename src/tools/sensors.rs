use crate::response::{self, ToolResult};
use crate::sensors;
use crate::tools::Tool;
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct SensorControlTool;

#[async_trait]
impl Tool for SensorControlTool {
    fn name(&self) -> &'static str {
        "mcp_android_sensor_control"
    }

    fn description(&self) -> &'static str {
        "Sensor mocking (GPS location, battery level and charging status)"
    }

    fn schema(&self) -> Value {
        json!({
            "inputSchema": {
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["set_gps", "set_battery", "reset_battery"],
                        "description": "The sensor action to perform"
                    },
                    "lat": {
                        "type": "number",
                        "description": "Latitude for GPS (required for set_gps)"
                    },
                    "lng": {
                        "type": "number",
                        "description": "Longitude for GPS (required for set_gps)"
                    },
                    "level": {
                        "type": "integer",
                        "description": "Battery level 0-100 (required for set_battery)"
                    },
                    "status": {
                        "type": "string",
                        "enum": ["charging", "discharging", "full", "not-charging"],
                        "description": "Battery charging status (optional for set_battery)"
                    },
                    "wait_ms": crate::tools::wait_ms_property(200)
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
            .unwrap_or(200);

        let args = args.clone();
        ctx.run_with_observation(wait_ms, || async move {
            match action.as_str() {
                "set_gps" => {
                    let lat = match args.get("lat").and_then(serde_json::Value::as_f64) {
                        Some(value) if value.is_finite() && (-90.0..=90.0).contains(&value) => {
                            value
                        }
                        _ => {
                            return response::error_response(
                                "lat must be a finite value between -90 and 90",
                            )
                        }
                    };
                    let lng = match args.get("lng").and_then(serde_json::Value::as_f64) {
                        Some(value) if value.is_finite() && (-180.0..=180.0).contains(&value) => {
                            value
                        }
                        _ => {
                            return response::error_response(
                                "lng must be a finite value between -180 and 180",
                            )
                        }
                    };
                    sensors::set_gps(lat, lng).await
                }
                "set_battery" => {
                    let level = match args.get("level").and_then(serde_json::Value::as_i64) {
                        Some(value @ 0..=100) => value as i32,
                        _ => {
                            return response::error_response(
                                "level must be an integer between 0 and 100",
                            )
                        }
                    };
                    let status = args.get("status").and_then(|v| v.as_str());
                    sensors::set_battery(level, status).await
                }
                "reset_battery" => sensors::reset_battery().await,
                _ => response::error_response(format!("Unknown sensor action: {action}")),
            }
        })
        .await
    }
}
