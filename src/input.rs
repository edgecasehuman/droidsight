use crate::adb::Adb;
use crate::device_metrics::{CoordinateSource, DisplayMetrics};
use crate::response::{self, ToolResult};
use crate::stream::StreamManager;
use crate::vision;
use serde_json::json;
use std::sync::Arc;
use unicode_normalization::UnicodeNormalization;

const FEEDBACK_MAX_WIDTH: u32 = 720;
const FEEDBACK_JPEG_QUALITY: u8 = 75;
/// Mean per-pixel difference between consecutive frames, below which the screen
/// is treated as settled rather than mid-animation.
const STABLE_FRAME_THRESHOLD: f32 = 0.1;

fn encode_feedback_screenshot(bytes: &[u8]) -> anyhow::Result<vision::EncodedImage> {
    vision::encode_full(bytes, FEEDBACK_MAX_WIDTH, FEEDBACK_JPEG_QUALITY)
}

/// Builds a tool result that carries the action's logs plus the screen that
/// followed it, read from the stream's cached frame. The view hierarchy is
/// deliberately not fetched here: it costs seconds per action, and callers
/// that need it can request it through the hierarchy tool.
async fn make_smart_response(stream_manager: &Arc<StreamManager>, logs: Vec<String>) -> ToolResult {
    // Consecutive decoded frames differ while the screen is still animating.
    // Wait up to two seconds for that difference to settle before capturing, so
    // the returned frame shows the finished state rather than a transition.
    let start_wait = std::time::Instant::now();
    while start_wait.elapsed().as_millis() < 2000 {
        // A poisoned lock means the decode thread died mid-update. Treat that as
        // "settled" and stop waiting rather than panicking this action too; the
        // capture below already degrades gracefully when no frame is available.
        let stability = stream_manager
            .stability_score
            .lock()
            .map_or(0.0, |score| *score);
        if stability < STABLE_FRAME_THRESHOLD {
            break;
        }
        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
    }

    let mut logs = logs;

    // Wait for the screen to settle before capturing the result.
    // Wait for the UI to update (ripple effect, scroll, transition) before capturing the frame.
    // Standard ripple is ~200-300ms.
    tokio::time::sleep(tokio::time::Duration::from_millis(300)).await;

    // Capture from the continuously maintained stream cache (FAST: <10ms).
    // A static screen may legitimately retain an older timestamp, so running
    // state—not frame age—determines whether the cache is authoritative.
    let (screenshot, mime_type, warning) = {
        // Try to get cached frame from stream first
        let img_opt = {
            let running = stream_manager.running.lock().is_ok_and(|running| *running);
            if running {
                stream_manager
                    .latest_image
                    .lock()
                    .ok()
                    .and_then(|image| image.clone())
            } else {
                None
            }
        };

        if let Some(img) = img_opt {
            // Fast path: encode the cached frame directly. It is bounded to
            // the same width as the fallback so an action's feedback image
            // never costs more than an explicitly requested screenshot.
            match crate::vision::encode_frame(&img, FEEDBACK_MAX_WIDTH, FEEDBACK_JPEG_QUALITY) {
                Ok(encoded) => (encoded, "image/jpeg", None),
                Err(error) => {
                    return Ok(json!({
                        "content": [{
                            "type": "text",
                            "text": json!({
                                "status": "success",
                                "logs": logs,
                                "observation_warning": format!("Input succeeded, but feedback frame encoding failed: {}", error)
                            }).to_string()
                        }]
                    }))
                }
            }
        } else {
            // None or Stale
            // Fallback: stream not ready or stale, use slow screenshot
            tracing::warn!("Stream cache unavailable - falling back to screenshot");
            let msg = Some("Stream cache unavailable; used screenshot fallback");
            match vision::screenshot().await {
                Ok(data) => {
                    let encoded =
                        tokio::task::spawn_blocking(move || encode_feedback_screenshot(&data))
                            .await;
                    match encoded {
                        Ok(Ok(data)) => (data, "image/jpeg", msg),
                        Ok(Err(error)) => {
                            return Ok(json!({
                                "content": [{
                                    "type": "text",
                                    "text": json!({
                                        "status": "success",
                                        "logs": logs,
                                        "observation_warning": format!("Input succeeded, but feedback screenshot encoding failed: {}", error)
                                    }).to_string()
                                }]
                            }))
                        }
                        Err(error) => {
                            return Ok(json!({
                                "content": [{
                                    "type": "text",
                                    "text": json!({
                                        "status": "success",
                                        "logs": logs,
                                        "observation_warning": format!("Input succeeded, but feedback screenshot encoding task failed: {}", error)
                                    }).to_string()
                                }]
                            }))
                        }
                    }
                }
                Err(error) => {
                    return Ok(json!({
                        "content": [{
                            "type": "text",
                            "text": json!({
                                "status": "success",
                                "logs": logs,
                                "observation_warning": format!("Input succeeded, but feedback screenshot failed: {}", error)
                            }).to_string()
                        }]
                    }))
                }
            }
        }
    };

    if let Some(w) = warning {
        logs.push(format!("WARNING: {w}"));
    }

    let smart_payload = json!({
        "status": "success",
        "logs": logs
    });
    Ok(json!({
        "content": [
            {"type": "text", "text": smart_payload.to_string()},
            {"type": "image", "data": screenshot.data, "mimeType": mime_type}
        ],
        "metadata": screenshot.metadata()
    }))
}

#[cfg(test)]
mod feedback_image_tests {
    use super::{encode_feedback_screenshot, FEEDBACK_MAX_WIDTH};
    use base64::{engine::general_purpose, Engine as _};
    use image::{DynamicImage, ImageFormat, RgbImage};

    #[test]
    fn fallback_feedback_is_resized_jpeg() {
        let image = RgbImage::from_pixel(1080, 2400, image::Rgb([10, 20, 30]));
        let mut png = Vec::new();
        DynamicImage::ImageRgb8(image)
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap();

        let encoded = encode_feedback_screenshot(&png).unwrap();
        let jpeg = general_purpose::STANDARD.decode(&encoded.data).unwrap();
        assert_eq!(image::guess_format(&jpeg).unwrap(), ImageFormat::Jpeg);
        let decoded = image::load_from_memory(&jpeg).unwrap();
        assert_eq!(decoded.width(), FEEDBACK_MAX_WIDTH);
        assert_eq!(decoded.height(), 1600);

        // The caller taps in device coordinates, so the downscale factor must
        // travel with the image rather than being left for the model to guess.
        assert_eq!(encoded.scale(), 1.5);
        assert_eq!(encoded.metadata()["image"]["device_width"], 1080);
    }
}

/// Tap at coordinates with optional transformation from stream to native resolution
pub async fn tap(stream_manager: &Arc<StreamManager>, x: i32, y: i32) -> ToolResult {
    tap_with_source(stream_manager, x, y, CoordinateSource::Native).await
}

/// Tap at coordinates from a specific source (handles stream→native scaling)
pub async fn tap_with_source(
    stream_manager: &Arc<StreamManager>,
    x: i32,
    y: i32,
    source: CoordinateSource,
) -> ToolResult {
    let metrics = match DisplayMetrics::fetch().await {
        Ok(metrics) => metrics,
        Err(error) => return response::error_response(error.to_string()),
    };

    // Transform coordinates based on source
    let (tx, ty) = match source {
        CoordinateSource::Native => (x, y),
        CoordinateSource::Stream => metrics.scale_to_native(x, y),
    };

    // Clamp to screen bounds for safety
    let (fx, fy) = metrics.clamp_to_screen(tx, ty);

    let log_entry = if source == CoordinateSource::Stream {
        format!("Tap (75ms hold): Transformed stream coords ({x}, {y}) -> native ({fx}, {fy})")
    } else {
        format!("Tap (75ms hold): Native coords ({x}, {y}) -> clamped ({fx}, {fy})")
    };

    // A zero-distance swipe with an explicit 75 ms duration puts a measurable
    // gap between the press and the release. `input tap` emits the pair almost
    // simultaneously, which some view-level gesture detectors discard as too
    // brief to be a touch.
    match Adb::device_shell(&format!("input swipe {fx} {fy} {fx} {fy} 75")).await {
        Ok(_) => {
            make_smart_response(
                stream_manager,
                vec![log_entry, "Input event sent".to_string()],
            )
            .await
        }
        Err(e) => response::error_response(e.to_string()),
    }
}

/// Convert text to the closest representation accepted by Android's plain
/// `adb shell input text` path. Full-fidelity Unicode should use an IME-based
/// input path instead.
fn ascii_typeable(text: &str) -> String {
    if text.is_ascii() {
        return text.to_owned();
    }

    text.nfkd().filter(char::is_ascii).collect()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TextMode {
    Auto,
    Ascii,
    Clipboard,
}

impl TextMode {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "auto" | "" => Some(Self::Auto),
            "ascii" => Some(Self::Ascii),
            "clipboard" | "unicode" => Some(Self::Clipboard),
            _ => None,
        }
    }
}

pub async fn text_with_mode(
    stream_manager: &Arc<StreamManager>,
    s: &str,
    mode: TextMode,
) -> ToolResult {
    if matches!(mode, TextMode::Ascii) || (matches!(mode, TextMode::Auto) && s.is_ascii()) {
        return text(stream_manager, s).await;
    }
    // Clipboard preserves full Unicode. Never silently transliterate an auto/unicode
    // request when clipboard access is denied: that would corrupt user data.
    crate::notifications::set_clipboard(s).await?;
    Adb::device_shell("input keyevent KEYCODE_PASTE").await
        .map_err(|error| json!({"code":-32000,"message":format!("Clipboard was set but paste failed: {error}")}))?;
    make_smart_response(
        stream_manager,
        vec!["Unicode text pasted through clipboard".into()],
    )
    .await
}

pub async fn text(stream_manager: &Arc<StreamManager>, s: &str) -> ToolResult {
    let typed = ascii_typeable(s);
    let escaped = typed.replace(' ', "%s");
    match Adb::device_shell(&format!("input text {}", crate::adb::shell_quote(&escaped))).await {
        Ok(_) => {
            let message = if typed == s {
                "Input text sent".to_string()
            } else {
                format!("Input text sent (transliterated non-ASCII to {typed:?})")
            };
            make_smart_response(stream_manager, vec![message]).await
        }
        Err(e) => response::error_response(e.to_string()),
    }
}

pub async fn key(stream_manager: &Arc<StreamManager>, keycode: &str, force: bool) -> ToolResult {
    if !keycode
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_')
    {
        return response::error_response("Invalid keycode");
    }
    // If force=true and keycode=BACK, we apply fallback strategy
    let mut cmd_log = format!("Key Event: {keycode}");

    let result = if force && (keycode == "KEYCODE_BACK" || keycode == "4") {
        // Try key event AND fallback swipe (Left Edge Back Gesture)
        cmd_log.push_str(" (Force+Swipe)");
        let cmd = format!("input keyevent {keycode} && input swipe 0 1000 500 1000 200");
        Adb::device_shell(&cmd).await
    } else {
        Adb::device_shell(&format!("input keyevent {keycode}")).await
    };

    match result {
        Ok(_) => make_smart_response(stream_manager, vec![cmd_log]).await,
        Err(e) => response::error_response(e.to_string()),
    }
}

pub async fn swipe(
    stream_manager: &Arc<StreamManager>,
    x1: i32,
    y1: i32,
    x2: i32,
    y2: i32,
    duration: i32,
) -> ToolResult {
    let metrics = match DisplayMetrics::fetch().await {
        Ok(metrics) => metrics,
        Err(error) => return response::error_response(error.to_string()),
    };
    let (x1, mut y1) = metrics.clamp_to_screen(x1, y1);
    let (x2, y2) = metrics.clamp_to_screen(x2, y2);
    // Keep the gesture clear of the system bars.
    // If swipe starts near top (status bar), clamp it down to avoid notification shade
    // Only applied if swiping DOWN (y2 > y1) from top area
    if y1 < 100 && y2 > y1 {
        tracing::warn!("Swipe start Y={} clamped to 150 (Safe Zone)", y1);
        y1 = 150;
    }

    let dist = (f64::from((x2 - x1).pow(2)) + f64::from((y2 - y1).pow(2))).sqrt();

    // Determine command to run (Curved or Linear)
    let (cmd, mode) = if duration > 300 && dist > 200.0 {
        // Calculate curve points in a localized scope to ensure RNG drops before await
        let (bx1, by1, bx2, by2) = {
            // `RngExt`, not `Rng`: rand 0.10 renamed its own `Rng` to `RngExt` when
            // `rand_core` took the `Rng` name for the low-level trait. `use rand::Rng`
            // still compiles and still resolves, just to the core trait, which has no
            // `random_range`.
            use rand::RngExt;
            let mut rng = rand::rng();

            let mid_x = i32::midpoint(x1, x2);
            let mid_y = i32::midpoint(y1, y2);

            // Random offset: -100 to +100 px
            let offset_x = rng.random_range(-100..100);
            let offset_y = rng.random_range(-50..50);

            let cx = mid_x + offset_x;
            let cy = mid_y + offset_y;

            let bezier = |t: f64, p0: i32, p1: i32, p2: i32| -> i32 {
                ((1.0 - t).powi(2) * f64::from(p0)
                    + 2.0 * (1.0 - t) * t * f64::from(p1)
                    + t.powi(2) * f64::from(p2)) as i32
            };

            let t1 = 0.33;
            let t2 = 0.66;

            (
                bezier(t1, x1, cx, x2),
                bezier(t1, y1, cy, y2),
                bezier(t2, x1, cx, x2),
                bezier(t2, y1, cy, y2),
            )
        };

        let seg_dur = duration / 3;
        (
            format!(
                "input swipe {x1} {y1} {bx1} {by1} {seg_dur} && input swipe {bx1} {by1} {bx2} {by2} {seg_dur} && input swipe {bx2} {by2} {x2} {y2} {seg_dur}"
            ),
            "Curved"
        )
    } else {
        (
            format!("input swipe {x1} {y1} {x2} {y2} {duration}"),
            "Linear",
        )
    };

    // Execute the determined command
    match Adb::device_shell(&cmd).await {
        Ok(_) => {
            make_smart_response(
                stream_manager,
                vec![format!(
                    "Swipe ({}): {},{} -> {},{} ({}ms)",
                    mode, x1, y1, x2, y2, duration
                )],
            )
            .await
        }
        Err(e) => response::error_response(e.to_string()),
    }
}

pub async fn set_ime(stream_manager: &Arc<StreamManager>, ime_id: &str) -> ToolResult {
    match Adb::device_shell(&format!("ime set {}", crate::adb::shell_quote(ime_id))).await {
        Ok(_) => make_smart_response(stream_manager, vec![format!("Set IME: {}", ime_id)]).await,
        Err(e) => response::error_response(e.to_string()),
    }
}

// The `_raw` functions below skip the screenshot and hierarchy work that the
// tool-facing entry points do, for internal callers that only need the action.

/// Raw tap without smart response overhead - for internal automation use
pub async fn tap_raw(x: i32, y: i32, source: CoordinateSource) -> ToolResult {
    let metrics = match DisplayMetrics::fetch().await {
        Ok(metrics) => metrics,
        Err(error) => return response::error_response(error.to_string()),
    };

    let (tx, ty) = match source {
        CoordinateSource::Native => (x, y),
        CoordinateSource::Stream => metrics.scale_to_native(x, y),
    };

    let (fx, fy) = metrics.clamp_to_screen(tx, ty);

    // Use swipe with 75ms duration to simulate a human tap
    match Adb::device_shell(&format!("input swipe {fx} {fy} {fx} {fy} 75")).await {
        Ok(_) => response::text_response(format!("Tapped (Humanized) {fx} {fy}")),
        Err(e) => response::error_response(e.to_string()),
    }
}

#[cfg(test)]
mod text_tests {
    use super::{ascii_typeable, TextMode};

    #[test]
    fn ascii_text_is_unchanged() {
        assert_eq!(ascii_typeable("Meeting notes 123"), "Meeting notes 123");
    }

    #[test]
    fn accented_text_is_folded_to_ascii() {
        assert_eq!(ascii_typeable("Sauté déjà vu"), "Saute deja vu");
    }

    #[test]
    fn unsupported_unicode_is_dropped() {
        assert_eq!(ascii_typeable("hello 世界 👋"), "hello  ");
    }

    #[test]
    fn text_modes_are_explicit_and_unknown_modes_fail_closed() {
        assert_eq!(TextMode::parse("auto"), Some(TextMode::Auto));
        assert_eq!(TextMode::parse("unicode"), Some(TextMode::Clipboard));
        assert_eq!(TextMode::parse("lossy"), None);
    }
}
