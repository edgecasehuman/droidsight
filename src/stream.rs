use std::sync::{Arc, Mutex};

use crate::adb::Adb;
use crate::adb_protocol::AdbClient;
use image::RgbImage;
use openh264::decoder::Decoder;
use openh264::formats::YUVSource;

const STREAM_READ_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_secs(1);
const FIRST_FRAME_DEADLINE: std::time::Duration = std::time::Duration::from_secs(20);

#[derive(Clone)]
pub struct StreamManager {
    // The inner Arc lets a reader clone the pointer instead of the frame, so
    // reading the latest image never blocks the decode loop.
    pub latest_image: Arc<Mutex<Option<Arc<RgbImage>>>>,
    pub running: Arc<Mutex<bool>>,
    pub stability_score: Arc<Mutex<f32>>,
    /// Timestamp (ms since epoch) when current frame was decoded
    pub frame_timestamp_ms: Arc<Mutex<u64>>,
}

impl StreamManager {
    pub fn new() -> Self {
        StreamManager {
            latest_image: Arc::new(Mutex::new(None)),
            running: Arc::new(Mutex::new(false)),
            stability_score: Arc::new(Mutex::new(0.0)),
            frame_timestamp_ms: Arc::new(Mutex::new(0)),
        }
    }

    pub fn start(&self) {
        // Claim the stream synchronously. Besides coalescing concurrent starts,
        // this ensures an immediate stop cannot be undone later by the spawned
        // task and an immediate read cannot observe a previous session's frame.
        {
            let Ok(mut running) = self.running.lock() else {
                tracing::error!("Stream start failed: Running lock poisoned");
                return;
            };
            if *running {
                return;
            }
            *running = true;
        }
        if let Ok(mut image) = self.latest_image.lock() {
            *image = None;
        }
        if let Ok(mut timestamp) = self.frame_timestamp_ms.lock() {
            *timestamp = 0;
        }

        let running_clone = self.running.clone();
        let latest_image_clone = self.latest_image.clone();
        let stability_clone = self.stability_score.clone();
        let timestamp_clone = self.frame_timestamp_ms.clone();

        tokio::spawn(async move {
            let mut previous_frame: Option<Vec<u8>> = None;

            tracing::debug!("Screen stream task started");

            loop {
                // Check running state before each iteration
                {
                    if let Ok(lock) = running_clone.lock() {
                        if !*lock {
                            break;
                        }
                    } else {
                        tracing::error!("Stream loop failed: Running lock poisoned");
                        break;
                    }
                }

                tracing::debug!("Starting Stream Loop Iteration...");

                // `--size` is deliberately omitted. screenrecord then encodes at
                // the display's native resolution, which is also the coordinate
                // space that uiautomator bounds, OCR boxes, and input taps use.
                // Pinning an explicit size here silently places every cached
                // frame in a different space than the hierarchy on any device
                // whose panel is not exactly that size.
                let stream_res = match Adb::selected_serial().await {
                    Ok(serial) => AdbClient::exec_stream(
                        "screenrecord --output-format=h264 --bit-rate 8000000 --time-limit=180 -",
                        Some(&serial),
                    )
                    .await,
                    Err(error) => Err(error),
                };

                match stream_res {
                    Ok(tcp_stream) => {
                        use tokio::io::AsyncReadExt;
                        let mut stdout = tokio::io::BufReader::new(tcp_stream);
                        let mut buffer = [0u8; 4096];
                        let mut parser = NalParser::new();
                        let connection_started = std::time::Instant::now();
                        let mut decoded_frame = false;

                        // A decoder is created per connection so a reconnect
                        // cannot inherit reference frames from the old stream.
                        let mut decoder = match Decoder::new() {
                            Ok(d) => d,
                            Err(e) => {
                                tracing::error!("[Stream] Decoder init failed: {:?}", e);
                                if let Ok(mut lock) = running_clone.lock() {
                                    *lock = false;
                                }
                                return;
                            }
                        };

                        loop {
                            {
                                match running_clone.lock() {
                                    Ok(running) => {
                                        if !*running {
                                            // Dropping the stream closes the ADB
                                            // connection, which ends screenrecord.
                                            break;
                                        }
                                    }
                                    Err(_) => {
                                        break;
                                    }
                                }
                            }

                            // Some devices need many seconds to produce their
                            // initial key frame, which is why the deadline is
                            // generous. Still recycle a connection that never
                            // decodes anything,
                            // but do not treat a quiet, static screen as dead once
                            // at least one valid frame has arrived.
                            if should_reconnect_stream(decoded_frame, connection_started.elapsed())
                            {
                                tracing::warn!(
                                    "Stream produced no decodable frame within {}s - reconnecting",
                                    FIRST_FRAME_DEADLINE.as_secs()
                                );
                                break;
                            }

                            // Poll reads so explicit stop changes are observed
                            // promptly even when a static device emits no bytes.
                            // Silence alone is not a reconnect signal after the
                            // first decoded frame.
                            let read_future = stdout.read(&mut buffer);
                            match tokio::time::timeout(STREAM_READ_POLL_INTERVAL, read_future).await
                            {
                                Ok(read_result) => match read_result {
                                    Ok(n) if n > 0 => {
                                        let nals = parser.push(&buffer[..n]);
                                        for nal in nals {
                                            // Decode and Update
                                            match decoder.decode(&nal) {
                                                Ok(Some(yuv)) => {
                                                    decoded_frame = true;
                                                    let (w, h) = yuv.dimensions();
                                                    let mut rgb_data = vec![0u8; w * h * 3];
                                                    yuv.write_rgb8(&mut rgb_data);

                                                    // Stability score: diff against the previous frame,
                                                    // which is kept in a local (~6MB) so the shared lock
                                                    // is only held for the final update below.

                                                    let current_len = rgb_data.len();
                                                    let mut diff_sum: u64 = 0;
                                                    let mut pixel_count: u64 = 0;

                                                    // Compare with previous frame if exists
                                                    if let Some(ref prev_data) = previous_frame {
                                                        if prev_data.len() == current_len {
                                                            // Sparse sampling (Stride = 10 pixels = 30 bytes)
                                                            // RGB = 3 bytes
                                                            let stride = 30;
                                                            let mut i = 0;
                                                            while i < current_len {
                                                                let p1 = i32::from(rgb_data[i]);
                                                                let p2 = i32::from(prev_data[i]);
                                                                diff_sum += u64::from(
                                                                    (p1 - p2).unsigned_abs(),
                                                                );
                                                                pixel_count += 1;
                                                                i += stride;
                                                            }
                                                        }
                                                    }

                                                    // Average absolute diff per sampled channel (0-255).
                                                    let avg_diff = if pixel_count > 0 {
                                                        diff_sum as f32 / pixel_count as f32
                                                    } else {
                                                        0.0
                                                    };

                                                    if let Ok(mut score_lock) =
                                                        stability_clone.lock()
                                                    {
                                                        *score_lock = avg_diff;
                                                    }

                                                    // Update shared state
                                                    if let Some(img) = RgbImage::from_raw(
                                                        w as u32,
                                                        h as u32,
                                                        rgb_data.clone(),
                                                    ) {
                                                        if let Ok(mut lock) =
                                                            latest_image_clone.lock()
                                                        {
                                                            *lock = Some(Arc::new(img));
                                                        }
                                                        // Update frame timestamp
                                                        if let Ok(mut ts_lock) =
                                                            timestamp_clone.lock()
                                                        {
                                                            use std::time::{
                                                                SystemTime, UNIX_EPOCH,
                                                            };
                                                            *ts_lock = SystemTime::now()
                                                                .duration_since(UNIX_EPOCH)
                                                                .unwrap_or_default()
                                                                .as_millis()
                                                                as u64;
                                                        }
                                                    }

                                                    previous_frame = Some(rgb_data);
                                                }
                                                Ok(None) => {} // Need more data
                                                // A corrupt NAL is expected on
                                                // reconnect; drop it and keep
                                                // reading rather than tearing
                                                // the session down.
                                                Err(error) => {
                                                    tracing::trace!("H264 decode error: {error}");
                                                }
                                            }
                                        }
                                    }
                                    // EOF or a read error: either way the
                                    // stream is finished.
                                    Ok(_) | Err(_) => break,
                                },
                                // No data within the poll interval. That is the
                                // normal idle case, so poll again.
                                Err(_) => continue,
                            }
                        }

                        // Stream dropped naturally or broken
                    }
                    Err(e) => {
                        tracing::error!("[Stream] Failed to connect: {}", e);
                    }
                }

                // Backoff before restart
                tokio::time::sleep(std::time::Duration::from_millis(1000)).await;
            }

            if let Ok(mut lock) = running_clone.lock() {
                *lock = false;
            }
        });
    }

    pub fn stop(&self) {
        if let Ok(mut lock) = self.running.lock() {
            *lock = false;
        } else {
            tracing::error!("Stream stop failed: Lock poisoned");
        }

        if let Ok(mut image) = self.latest_image.lock() {
            *image = None;
        }
        if let Ok(mut timestamp) = self.frame_timestamp_ms.lock() {
            *timestamp = 0;
        }

        // Dropping the native ADB stream terminates the `exec:` service without
        // killing unrelated screenrecord processes owned by the user/tools.
    }
}

fn should_reconnect_stream(decoded_frame: bool, connection_age: std::time::Duration) -> bool {
    !decoded_frame && connection_age >= FIRST_FRAME_DEADLINE
}

// Basic NAL Parser
struct NalParser {
    buffer: Vec<u8>,
}

impl NalParser {
    fn new() -> Self {
        Self {
            buffer: Vec::with_capacity(1024 * 1024),
        }
    }

    fn push(&mut self, chunk: &[u8]) -> Vec<Vec<u8>> {
        self.buffer.extend_from_slice(chunk);

        let mut nals = Vec::new();
        let mut i = 0;
        let mut start_indices = Vec::new();

        // Scan buffer for 00 00 01
        while i < self.buffer.len().saturating_sub(2) {
            if self.buffer[i] == 0 && self.buffer[i + 1] == 0 && self.buffer[i + 2] == 1 {
                // Split on the 3-byte start code. A 4-byte code (00 00 00 01)
                // leaves its leading 00 as a harmless trailing byte on the
                // preceding NAL, which openh264 ignores.
                start_indices.push(i);
                i += 3;
            } else {
                i += 1;
            }
        }

        if start_indices.len() > 1 {
            for k in 0..start_indices.len() - 1 {
                let start = start_indices[k];
                let end = start_indices[k + 1];

                let nal_data = self.buffer[start..end].to_vec();
                nals.push(nal_data);
            }

            if let Some(last_start) = start_indices.last() {
                let last_start = *last_start;

                // Retain the trailing (incomplete) NAL for the next chunk. If the
                // last start code was 4-byte, back up one byte so its leading 00 is
                // preserved rather than split off.
                let cutoff = if last_start > 0 && self.buffer[last_start - 1] == 0 {
                    last_start - 1
                } else {
                    last_start
                };

                self.buffer = self.buffer[cutoff..].to_vec();
            }
        }

        if self.buffer.len() > 10 * 1024 * 1024 {
            self.buffer.clear();
        }

        nals
    }
}

#[cfg(test)]
mod nal_parser_tests {
    use super::{should_reconnect_stream, NalParser, StreamManager, FIRST_FRAME_DEADLINE};

    #[test]
    fn parses_start_codes_split_across_chunks() {
        let mut parser = NalParser::new();
        assert!(parser.push(&[0, 0]).is_empty());
        assert!(parser.push(&[1, 0x67, 1, 2]).is_empty());
        let nals = parser.push(&[0, 0, 1, 0x68, 3]).clone();
        assert_eq!(nals, vec![vec![0, 0, 1, 0x67, 1, 2]]);
    }

    #[test]
    fn caps_unframed_stream_data() {
        let mut parser = NalParser::new();
        assert!(parser.push(&vec![0xff; 10 * 1024 * 1024 + 1]).is_empty());
        assert!(parser.buffer.is_empty());
    }

    #[test]
    fn watchdog_only_reconnects_connections_without_a_first_frame() {
        assert!(!should_reconnect_stream(
            false,
            FIRST_FRAME_DEADLINE - std::time::Duration::from_millis(1)
        ));
        assert!(should_reconnect_stream(false, FIRST_FRAME_DEADLINE));
        assert!(!should_reconnect_stream(
            true,
            FIRST_FRAME_DEADLINE + std::time::Duration::from_secs(300)
        ));
    }

    #[test]
    fn stop_invalidates_the_cached_frame() {
        let manager = StreamManager::new();
        *manager.running.lock().unwrap() = true;
        *manager.latest_image.lock().unwrap() = Some(std::sync::Arc::new(
            image::RgbImage::from_pixel(1, 1, image::Rgb([1, 2, 3])),
        ));
        *manager.frame_timestamp_ms.lock().unwrap() = 123;

        manager.stop();

        assert!(!*manager.running.lock().unwrap());
        assert!(manager.latest_image.lock().unwrap().is_none());
        assert_eq!(*manager.frame_timestamp_ms.lock().unwrap(), 0);
    }

    #[tokio::test]
    async fn start_claims_state_and_invalidates_cache_before_spawning() {
        let manager = StreamManager::new();
        *manager.latest_image.lock().unwrap() = Some(std::sync::Arc::new(
            image::RgbImage::from_pixel(1, 1, image::Rgb([1, 2, 3])),
        ));
        *manager.frame_timestamp_ms.lock().unwrap() = 123;

        manager.start();

        assert!(*manager.running.lock().unwrap());
        assert!(manager.latest_image.lock().unwrap().is_none());
        assert_eq!(*manager.frame_timestamp_ms.lock().unwrap(), 0);

        // An immediate stop must remain authoritative even if the task has not
        // received its first scheduler turn yet.
        manager.stop();
        tokio::task::yield_now().await;
        assert!(!*manager.running.lock().unwrap());
    }
}
