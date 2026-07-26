use crate::adb::Adb;
use crate::response::{self, ToolResult};
use crate::tools::Tool;
use async_trait::async_trait;
use serde_json::{json, Value};
use std::time::{SystemTime, UNIX_EPOCH};

fn valid_event_path(path: &str) -> bool {
    path.strip_prefix("/dev/input/event")
        .is_some_and(|suffix| !suffix.is_empty() && suffix.chars().all(|ch| ch.is_ascii_digit()))
}

fn bounded_recording_command(duration: u64, command: &str) -> String {
    // Toybox `timeout` uses 124 when it ends a command at the requested
    // deadline. For gesture capture that is normal completion, not a failure.
    format!(
        "timeout {duration} {command}; status=$?; [ \"$status\" -eq 124 ] && exit 0; exit \"$status\""
    )
}

pub struct RecordGestureTool;

#[async_trait]
impl Tool for RecordGestureTool {
    fn name(&self) -> &'static str {
        "mcp_android_record_gesture"
    }

    fn description(&self) -> &'static str {
        "Record touch input (getevent) for a duration"
    }

    fn schema(&self) -> Value {
        json!({
            "inputSchema": {
                "type": "object",
                "properties": {
                    "duration_seconds": { "type": "integer" },
                    "device_path": { "type": "string", "description": "e.g. /dev/input/event2 (optional, defaults to auto-detect or all)" }
                },
                "required": ["duration_seconds"]
            }
        })
    }

    async fn execute(&self, args: &Value, _ctx: &crate::tools::ToolContext) -> ToolResult {
        let Some(duration @ 1..=60) = args
            .get("duration_seconds")
            .and_then(serde_json::Value::as_u64)
        else {
            return response::error_response("duration_seconds must be between 1 and 60");
        };
        let dev = args.get("device_path").and_then(|v| v.as_str());
        if dev.is_some_and(|path| !valid_event_path(path)) {
            return response::error_response("device_path must match /dev/input/event<N>");
        }

        // `getevent -t` emits timestamped raw hex events (no `-l` labels, which
        // would complicate replay). Without a device path it dumps every input
        // device, which is noisier but still parseable.
        let cmd = if let Some(d) = dev {
            format!("getevent -t {}", crate::adb::shell_quote(d))
        } else {
            "getevent -t".to_string()
        };

        // getevent runs until EOF, so wrap it in the device-side `timeout` from
        // toybox to bound the recording.
        let final_cmd = bounded_recording_command(duration, &cmd);

        let output = match Adb::shell_native(&final_cmd).await {
            Ok(o) => o,
            Err(e) => return response::error_response(format!("Recording failed: {e}")),
        };

        // Parse output. Format with all devices:
        //   [   1234.567890] /dev/input/event2: 0003 0035 00000123
        // With an explicit device path the "/dev/input/...:" prefix is absent:
        //   [   1234.567890] 0003 0035 00000123

        let mut events = Vec::new();
        let mut first_ts = 0.0;

        for line in output.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }

            // Remove brackets
            let parts: Vec<&str> = line.split_whitespace().collect();
            // Expected parts: ["[", "TIMESTAMP]", "DEVICE:", "TYPE", "CODE", "VAL"]
            // OR: ["[", "TIMESTAMP]", "TYPE", "CODE", "VAL"]

            if parts.len() < 4 {
                continue;
            }

            let ts_str = parts[1].trim_end_matches(']');
            let ts: f64 = ts_str.parse().unwrap_or(0.0);

            if first_ts == 0.0 {
                first_ts = ts;
            }
            let delta = ts - first_ts;

            // Handle device prefix if present
            let (dev_node, type_idx, code_idx, val_idx) = if parts[2].ends_with(':') {
                (Some(parts[2].trim_end_matches(':').to_string()), 3, 4, 5)
            } else {
                (dev.map(std::string::ToString::to_string), 2, 3, 4)
            };

            if parts.len() <= val_idx {
                continue;
            }

            // Parse hex
            let type_u16 = u16::from_str_radix(parts[type_idx], 16).unwrap_or(0);
            let code_u16 = u16::from_str_radix(parts[code_idx], 16).unwrap_or(0);
            let val_i32 = i32::from_str_radix(parts[val_idx], 16).unwrap_or(0);

            events.push(json!({
                "ts": ts,
                "delta": delta,
                "dev": dev_node, // Might be null if implicitly known
                "type": type_u16,
                "code": code_u16,
                "val": val_i32
            }));
        }

        Ok(json!({
            "content": [{
                "type": "text",
                "text": format!("Recorded {} events over {} seconds", events.len(), duration)
            }],
            "data": events
        }))
    }
}

pub struct PlayGestureTool;

#[async_trait]
impl Tool for PlayGestureTool {
    fn name(&self) -> &'static str {
        "mcp_android_play_gesture"
    }

    fn description(&self) -> &'static str {
        "Replay a recorded gesture (timeline of sendevents)"
    }

    fn schema(&self) -> Value {
        json!({
            "inputSchema": {
                "type": "object",
                "properties": {
                    "events": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "delta": { "type": "number" },
                                "dev": { "type": "string" },
                                "type": { "type": "integer" },
                                "code": { "type": "integer" },
                                "val": { "type": "integer" }
                            }
                        }
                    },
                    "speed_multiply": { "type": "number" }
                },
                "required": ["events"]
            }
        })
    }

    async fn execute(&self, args: &Value, _ctx: &crate::tools::ToolContext) -> ToolResult {
        let events = match args.get("events").and_then(|v| v.as_array()) {
            Some(events) if events.len() <= 100_000 => events,
            Some(_) => {
                return response::error_response("events exceeds the 100000-event safety limit")
            }
            None => return response::error_response("Missing events array"),
        };
        let speed = args
            .get("speed_multiply")
            .and_then(serde_json::Value::as_f64)
            .unwrap_or(1.0);
        if !speed.is_finite() || speed <= 0.0 || speed > 100.0 {
            return response::error_response("speed_multiply must be finite and in (0, 100]");
        }

        if events.is_empty() {
            return response::text_response("No events to replay");
        }

        // Generate shell script buffer
        let mut script = String::new();
        script.push_str("echo 'Starting Playback'\n");

        let mut last_delta = 0.0;

        for (index, ev) in events.iter().enumerate() {
            let delta = match ev.get("delta").and_then(serde_json::Value::as_f64) {
                Some(delta) if delta.is_finite() && delta >= last_delta && delta <= 300.0 => delta,
                _ => {
                    return response::error_response(format!(
                        "event {index} has an invalid or non-monotonic delta"
                    ))
                }
            };
            let dev = match ev.get("dev").and_then(|v| v.as_str()) {
                Some(path) if valid_event_path(path) => path,
                _ => {
                    return response::error_response(format!(
                        "event {index} must contain a valid /dev/input/event<N> path"
                    ))
                }
            };

            let Some(type_u) = ev
                .get("type")
                .and_then(serde_json::Value::as_u64)
                .and_then(|v| u16::try_from(v).ok())
            else {
                return response::error_response(format!("event {index} has invalid type"));
            };
            let Some(code_u) = ev
                .get("code")
                .and_then(serde_json::Value::as_u64)
                .and_then(|v| u16::try_from(v).ok())
            else {
                return response::error_response(format!("event {index} has invalid code"));
            };
            let Some(val_i) = ev
                .get("val")
                .and_then(serde_json::Value::as_i64)
                .and_then(|v| i32::try_from(v).ok())
            else {
                return response::error_response(format!("event {index} has invalid val"));
            };

            let wait_sec = (delta - last_delta) / speed;
            if wait_sec > 0.001 {
                // `sleep` in android toybox supports float seconds
                script.push_str(&format!("sleep {wait_sec:.3}\n"));
            }
            last_delta = delta;

            // sendevent <dev> <type> <code> <val> (values in decimal)
            script.push_str(&format!(
                "sendevent {} {} {} {}\n",
                crate::adb::shell_quote(dev),
                type_u,
                code_u,
                val_i
            ));
        }

        // Push script
        let rnd = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let script_path = format!("/data/local/tmp/gesture_{rnd}.sh");

        // Write the script to a local temp file and push it, which handles
        // large scripts more reliably than echoing into a device-side file.
        use std::io::Write;
        let mut local_tmp = std::env::temp_dir();
        local_tmp.push(format!("gesture_{rnd}.sh"));

        {
            let mut f = match std::fs::File::create(&local_tmp) {
                Ok(file) => file,
                Err(error) => {
                    return response::error_response(format!(
                        "Failed to create gesture script: {error}"
                    ))
                }
            };
            if let Err(error) = f.write_all(script.as_bytes()) {
                let _ = std::fs::remove_file(&local_tmp);
                return response::error_response(format!(
                    "Failed to write gesture script: {error}"
                ));
            }
        }

        // Push the script to the device.
        let local_tmp_string = local_tmp.to_string_lossy().to_string();
        let push_result = Adb::shell(&["push", &local_tmp_string, &script_path]).await;

        // Clean local
        let _ = std::fs::remove_file(&local_tmp);

        if let Err(error) = push_result {
            return response::error_response(format!("Failed to push gesture script: {error}"));
        }

        // Run
        let final_res =
            match Adb::shell_native(&format!("sh {}", crate::adb::shell_quote(&script_path))).await
            {
                Ok(out) => response::bounded_text_response(
                    format!("Playback Complete:\n{out}"),
                    response::DEFAULT_TEXT_BUDGET_BYTES,
                    response::TruncationStrategy::Head,
                ),
                Err(e) => response::error_response(format!("Playback Error: {e}")),
            };

        // Cleanup remote
        let _ = Adb::shell_native(&format!(
            "rm -f -- {}",
            crate::adb::shell_quote(&script_path)
        ))
        .await;

        final_res
    }
}

#[cfg(test)]
mod tests {
    use super::{bounded_recording_command, valid_event_path};

    #[test]
    fn accepts_only_kernel_input_event_paths() {
        assert!(valid_event_path("/dev/input/event0"));
        assert!(valid_event_path("/dev/input/event12"));
        assert!(!valid_event_path("/dev/input/event2; reboot"));
        assert!(!valid_event_path("/dev/input/mouse0"));
    }

    #[test]
    fn recording_deadline_is_normal_completion() {
        let command = bounded_recording_command(3, "getevent -t");
        assert!(command.starts_with("timeout 3 getevent -t;"));
        assert!(command.contains("[ \"$status\" -eq 124 ] && exit 0"));
        assert!(command.ends_with("exit \"$status\""));
    }
}
