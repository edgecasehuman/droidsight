use crate::adb::Adb;
use anyhow::Result;
use serde_json::{json, Value};
use tokio::sync::Mutex;

static RECORDING_PID: Mutex<Option<u32>> = Mutex::const_new(None);

pub async fn start_recording() -> Result<Value, Value> {
    let mut recording_pid = RECORDING_PID.lock().await;
    if let Some(pid) = *recording_pid {
        if Adb::device_shell(&format!("kill -0 {pid}")).await.is_ok() {
            return Err(
                json!({"code": -32000, "message": format!("Recording is already running with PID {}", pid)}),
            );
        }
        *recording_pid = None;
    }

    let output = Adb::device_shell(
        "screenrecord --time-limit 180 /sdcard/mcp_rec.mp4 >/dev/null 2>&1 & echo $!",
    )
    .await
    .map_err(|error| json!({"code": -32000, "message": error.to_string()}))?;
    let pid = output.lines().last()
        .and_then(|line| line.trim().parse::<u32>().ok())
        .filter(|pid| *pid > 0)
        .ok_or_else(|| json!({"code": -32000, "message": "Recording started without a parseable process ID"}))?;
    *recording_pid = Some(pid);
    Ok(
        json!({"content": [{"type": "text", "text": format!("Recording started with PID {} (saved to /sdcard/mcp_rec.mp4)", pid)}]}),
    )
}

pub async fn stop_recording() -> Result<Value, Value> {
    let mut recording_pid = RECORDING_PID.lock().await;
    let pid = (*recording_pid).ok_or_else(
        || json!({"code": -32000, "message": "No recording owned by this server is running"}),
    )?;
    if let Err(error) = Adb::device_shell(&format!("kill -2 {pid}")).await {
        return Err(
            json!({"code": -32000, "message": format!("Failed to stop recording PID {}: {}", pid, error)}),
        );
    }
    *recording_pid = None;
    Ok(json!({"content": [{"type": "text", "text": format!("Recording PID {} stopped", pid)}]}))
}
