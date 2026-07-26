use crate::response;
use chrono::Local;
use regex::Regex;
use serde_json::{json, Value};
use std::collections::VecDeque;
use std::io::{BufRead, BufReader};
use std::process::{Child, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock, Mutex};
use std::thread::{self, JoinHandle};

const RAW_EVENT_CAPACITY: usize = 100;
const SEMANTIC_EVENT_CAPACITY: usize = 50;

static EVENT_BUFFER: LazyLock<Arc<Mutex<VecDeque<String>>>> =
    LazyLock::new(|| Arc::new(Mutex::new(VecDeque::with_capacity(RAW_EVENT_CAPACITY))));
static SEMANTIC_BUFFER: LazyLock<Arc<Mutex<VecDeque<Value>>>> =
    LazyLock::new(|| Arc::new(Mutex::new(VecDeque::with_capacity(SEMANTIC_EVENT_CAPACITY))));

/// Owns both the long-running ADB child and its stdout reader. Killing the
/// child closes the pipe, allowing the reader to finish before it is joined.
struct EventMonitor {
    cancelled: Arc<AtomicBool>,
    child: Option<Child>,
    reader: Option<JoinHandle<()>>,
}

impl EventMonitor {
    fn from_child(
        mut child: Child,
        on_line: Arc<dyn Fn(String) + Send + Sync + 'static>,
    ) -> Result<Self, String> {
        let Some(stdout) = child.stdout.take() else {
            let _ = child.kill();
            let _ = child.wait();
            return Err("event monitor child did not expose stdout".to_string());
        };
        let cancelled = Arc::new(AtomicBool::new(false));
        let reader_cancelled = cancelled.clone();
        let reader = match thread::Builder::new()
            .name("droidsight-event-monitor".to_string())
            .spawn(move || {
                for line in BufReader::new(stdout).lines() {
                    if reader_cancelled.load(Ordering::Acquire) {
                        break;
                    }
                    match line {
                        Ok(line) => on_line(line),
                        Err(error) => {
                            if !reader_cancelled.load(Ordering::Acquire) {
                                tracing::warn!("Event monitor output failed: {}", error);
                            }
                            break;
                        }
                    }
                }
            }) {
            Ok(reader) => reader,
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("failed to spawn event monitor reader: {error}"));
            }
        };

        Ok(Self {
            cancelled,
            child: Some(child),
            reader: Some(reader),
        })
    }

    fn start(serial: &str) -> Result<Self, String> {
        let touch_pattern =
            Regex::new(r"InputDispatcher.*Delivering touch").map_err(|error| error.to_string())?;
        let launch_pattern =
            Regex::new(r"ActivityManager.*Start proc").map_err(|error| error.to_string())?;
        let crash_pattern =
            Regex::new(r"AndroidRuntime.*FATAL EXCEPTION").map_err(|error| error.to_string())?;
        let raw_buffer = EVENT_BUFFER.clone();
        let semantic_buffer = SEMANTIC_BUFFER.clone();
        let on_line = Arc::new(move |line: String| {
            if let Ok(mut buffer) = raw_buffer.lock() {
                if buffer.len() >= RAW_EVENT_CAPACITY {
                    buffer.pop_front();
                }
                buffer.push_back(line.clone());
            }

            let event_type = if touch_pattern.is_match(&line) {
                Some("TOUCH_DETECTED")
            } else if launch_pattern.is_match(&line) {
                Some("APP_LAUNCH")
            } else if crash_pattern.is_match(&line) {
                Some("CRASH")
            } else {
                None
            };

            if let Some(event_type) = event_type {
                if let Ok(mut buffer) = semantic_buffer.lock() {
                    if buffer.len() >= SEMANTIC_EVENT_CAPACITY {
                        buffer.pop_front();
                    }
                    buffer.push_back(json!({
                        "type": event_type,
                        "timestamp": Local::now().format("%H:%M:%S%.3f").to_string(),
                        "raw": line
                    }));
                }
            }
        });

        let child = crate::adb::Adb::event_monitor_command(serial)
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|error| format!("failed to start ADB event monitor: {error}"))?;

        Self::from_child(child, on_line)
    }

    fn is_running(&mut self) -> bool {
        self.child
            .as_mut()
            .is_some_and(|child| matches!(child.try_wait(), Ok(None)))
    }

    fn shutdown(&mut self) {
        self.cancelled.store(true, Ordering::Release);
        if let Some(child) = self.child.as_mut() {
            let _ = child.kill();
        }
        if let Some(reader) = self.reader.take() {
            if reader.join().is_err() {
                tracing::warn!("Event monitor reader panicked during shutdown");
            }
        }
        if let Some(mut child) = self.child.take() {
            let _ = child.wait();
        }
    }
}

impl Drop for EventMonitor {
    fn drop(&mut self) {
        self.shutdown();
    }
}

static EVENT_MONITOR: Mutex<Option<EventMonitor>> = Mutex::new(None);

fn ensure_event_monitor() {
    if std::env::var("DROIDSIGHT_EVENTS").is_err() {
        return;
    }
    let Some(serial) = crate::config::Config::device_serial() else {
        tracing::warn!(
            "DROIDSIGHT_EVENTS requires DROIDSIGHT_DEVICE_SERIAL; event monitoring was not started"
        );
        return;
    };
    let mut monitor = EVENT_MONITOR
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if monitor.as_mut().is_some_and(EventMonitor::is_running) {
        return;
    }
    // Drop and join a monitor whose ADB process exited before attempting a
    // replacement, so repeated reads can recover from an ADB disconnect.
    *monitor = None;
    match EventMonitor::start(&serial) {
        Ok(started) => *monitor = Some(started),
        Err(error) => tracing::warn!("{}", error),
    }
}

/// Stop, reap, and join the event monitor. This is safe to call repeatedly.
pub fn shutdown_event_monitor() {
    let monitor = EVENT_MONITOR
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .take();
    drop(monitor);
}

pub fn read_recent_events(limit: i32) -> Result<Value, Value> {
    ensure_event_monitor();
    let buf = EVENT_BUFFER
        .lock()
        .map_err(|_| json!({"code": -32603, "message": "Event buffer lock poisoned"}))?;
    let start = buf.len().saturating_sub(limit.max(0) as usize);
    let text = buf.range(start..).cloned().collect::<Vec<_>>().join("\n");
    drop(buf);

    response::bounded_text_response(
        text,
        response::DEFAULT_TEXT_BUDGET_BYTES,
        response::TruncationStrategy::Tail,
    )
}

pub fn read_semantic_events(limit: i32) -> Result<Value, Value> {
    ensure_event_monitor();
    let buf = SEMANTIC_BUFFER
        .lock()
        .map_err(|_| json!({"code": -32603, "message": "Semantic buffer lock poisoned"}))?;
    let start = buf.len().saturating_sub(limit.max(0) as usize);
    let events: Vec<Value> = buf.range(start..).cloned().collect();
    drop(buf);
    let text = serde_json::to_string_pretty(&events)
        .unwrap_or_else(|error| format!("Serialization failed: {error}"));

    response::bounded_text_response(
        text,
        response::DEFAULT_TEXT_BUDGET_BYTES,
        response::TruncationStrategy::Tail,
    )
}

#[cfg(all(test, unix))]
mod tests {
    use super::EventMonitor;
    use std::process::{Command, Stdio};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    #[test]
    fn shutdown_kills_reaps_and_joins_reader() {
        let child = Command::new("yes")
            .arg("event")
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("yes should be available on Unix test hosts");
        let lines = Arc::new(AtomicUsize::new(0));
        let observed = lines.clone();
        let mut monitor = EventMonitor::from_child(
            child,
            Arc::new(move |_| {
                observed.fetch_add(1, Ordering::Relaxed);
            }),
        )
        .unwrap();

        let deadline = Instant::now() + Duration::from_secs(1);
        while lines.load(Ordering::Relaxed) == 0 && Instant::now() < deadline {
            std::thread::yield_now();
        }
        assert!(lines.load(Ordering::Relaxed) > 0);

        let started = Instant::now();
        monitor.shutdown();
        assert!(started.elapsed() < Duration::from_secs(1));
        assert!(monitor.reader.is_none());

        // Idempotence is important because explicit shutdown is also followed
        // by Drop during normal server teardown.
        monitor.shutdown();
    }
}
