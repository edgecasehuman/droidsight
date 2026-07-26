// Device Metrics Module - Coordinate Normalization Layer
// Central source of display parameters for coordinate transformation

use crate::adb::Adb;
use anyhow::{anyhow, Result as AnyhowResult};
use std::sync::Mutex;

#[derive(Debug, Clone)]
pub struct DisplayMetrics {
    pub width: u32,
    pub height: u32,
    pub stream_width: u32,
    pub stream_height: u32,
}

impl Default for DisplayMetrics {
    fn default() -> Self {
        Self {
            width: 1080,
            height: 2400,
            stream_width: 1080,
            stream_height: 2400,
        }
    }
}

static CACHED_METRICS: Mutex<Option<DisplayMetrics>> = Mutex::new(None);

impl DisplayMetrics {
    /// Fetch display metrics from device (caches result)
    pub async fn fetch() -> AnyhowResult<Self> {
        if let Some(cached) = CACHED_METRICS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
        {
            return Ok(cached);
        }

        let size_output = Adb::shell_native("wm size")
            .await
            .map_err(|e| anyhow!("Failed to get display size: {e}"))?;

        let (width, height) = Self::parse_size(&size_output)?;

        // stream.rs runs `screenrecord` without `--size`, so the encoder uses the
        // display's native resolution and stream space equals native space.
        let metrics = Self {
            width,
            height,
            stream_width: width,
            stream_height: height,
        };

        *CACHED_METRICS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(metrics.clone());

        Ok(metrics)
    }

    /// Transform coordinates from stream space to native space.
    pub fn scale_to_native(&self, x: i32, y: i32) -> (i32, i32) {
        if self.stream_width == 0 || self.stream_height == 0 {
            return (x, y);
        }

        let scale_x = self.width as f32 / self.stream_width as f32;
        let scale_y = self.height as f32 / self.stream_height as f32;

        let nx = (x as f32 * scale_x).round() as i32;
        let ny = (y as f32 * scale_y).round() as i32;

        (nx, ny)
    }

    /// Clamp coordinates to screen bounds
    pub fn clamp_to_screen(&self, x: i32, y: i32) -> (i32, i32) {
        let max_x = self.width.saturating_sub(1).min(i32::MAX as u32) as i32;
        let max_y = self.height.saturating_sub(1).min(i32::MAX as u32) as i32;
        let clamped_x = x.clamp(0, max_x);
        let clamped_y = y.clamp(0, max_y);
        (clamped_x, clamped_y)
    }

    /// Parse "Physical size: 1080x2400" output
    fn parse_size(output: &str) -> AnyhowResult<(u32, u32)> {
        // Handle both "Physical size: WxH" and "Override size: WxH"
        for line in output.lines() {
            if line.contains("size:") {
                if let Some(dims) = line.split(':').nth(1) {
                    let parts: Vec<&str> = dims.trim().split('x').collect();
                    if parts.len() == 2 {
                        let w = parts[0]
                            .parse::<u32>()
                            .map_err(|_| anyhow!("Invalid width"))?;
                        let h = parts[1]
                            .parse::<u32>()
                            .map_err(|_| anyhow!("Invalid height"))?;
                        return Ok((w, h));
                    }
                }
            }
        }
        Err(anyhow!("Could not parse display size from: {output}"))
    }
}

/// Keyboard state used by the keyboard-displacement logic.
#[derive(Debug, Clone, Default)]
pub struct KeyboardState {
    pub visible: bool,
    pub height: u32,
}

/// Detect soft keyboard visibility and height
/// Uses `dumpsys input_method` to check IME state
pub async fn detect_keyboard_state() -> anyhow::Result<KeyboardState> {
    // Run dumpsys input_method to get keyboard info
    let output = match Adb::shell_native(
        "dumpsys input_method | grep -E 'mInputShown|mShowRequested|mVisibleHeight'",
    )
    .await
    {
        Ok(o) => o,
        Err(error) => return Err(error),
    };

    let mut visible = false;
    let mut height: u32 = 0;

    for line in output.lines() {
        let line = line.trim();

        // Check if keyboard is shown
        if line.contains("mInputShown=true") || line.contains("mShowRequested=true") {
            visible = true;
        }

        // Try to extract visible height (varies by Android version)
        if line.contains("mVisibleHeight=") {
            if let Some(val_str) = line.split("mVisibleHeight=").nth(1) {
                // Take first numeric portion
                let num_str: String = val_str.chars().take_while(char::is_ascii_digit).collect();
                if let Ok(h) = num_str.parse() {
                    height = h;
                }
            }
        }
    }

    Ok(KeyboardState { visible, height })
}

/// Coordinate source identifier for transformation logic
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum CoordinateSource {
    /// From hierarchy bounds, OCR, screenshot - native resolution
    Native,
    /// From the H.264 stream, whose space equals native unless the encoder
    /// was asked for a different size
    Stream,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_scale_to_native() {
        let metrics = DisplayMetrics {
            width: 1080,
            height: 2400,
            stream_width: 720,
            stream_height: 1600,
        };

        // Center of stream (360, 800) should map to center of native (540, 1200)
        let (nx, ny) = metrics.scale_to_native(360, 800);
        assert_eq!(nx, 540);
        assert_eq!(ny, 1200);
    }

    #[test]
    fn test_clamp_to_screen() {
        let metrics = DisplayMetrics::default();

        // Negative should clamp to 0
        assert_eq!(metrics.clamp_to_screen(-10, -20), (0, 0));

        // Overflow should clamp to max
        assert_eq!(metrics.clamp_to_screen(2000, 3000), (1079, 2399));

        let zero_sized = DisplayMetrics {
            width: 0,
            height: 0,
            ..DisplayMetrics::default()
        };
        assert_eq!(zero_sized.clamp_to_screen(10, 10), (0, 0));
    }

    #[test]
    fn test_parse_size() {
        let output = "Physical size: 1080x2400";
        let result = DisplayMetrics::parse_size(output);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), (1080, 2400));
    }
}
