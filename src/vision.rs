use crate::adb::Adb;
use crate::response::{self, ToolResult};
use anyhow::{anyhow, Result as AnyhowResult};
use quick_xml::events::Event;
use quick_xml::reader::Reader;
use serde::Serialize;
use serde_json::json;
use std::env;
use std::fs;
use std::io::{self, Read};
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

const TESSERACT_PROBE_TIMEOUT: Duration = Duration::from_secs(5);
const TESSERACT_OCR_TIMEOUT: Duration = Duration::from_secs(30);

/// Shared by every OCR entry point so a missing dependency is reported the same
/// way, with the install hint, no matter which tool the caller reached for.
const TESSERACT_MISSING: &str = "OCR failed: the 'tesseract' binary was not found on PATH or in the usual install locations.\n\nInstall Tesseract OCR and make sure it is reachable on PATH:\n  Windows  https://github.com/UB-Mannheim/tesseract/wiki\n  macOS    brew install tesseract\n  Linux    apt install tesseract-ocr";

enum CommandResult {
    Completed(Output),
    TimedOut,
}

/// Run a child with bounded wall-clock time. Output pipes are drained on
/// dedicated threads so a noisy child cannot block while the parent waits.
/// A timed-out child is killed and reaped before this function returns.
fn run_command_with_timeout(command: &mut Command, timeout: Duration) -> io::Result<CommandResult> {
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = command.spawn()?;
    let mut stdout = child.stdout.take().expect("configured piped stdout");
    let mut stderr = child.stderr.take().expect("configured piped stderr");
    let stdout_reader = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        stdout.read_to_end(&mut bytes).map(|_| bytes)
    });
    let stderr_reader = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        stderr.read_to_end(&mut bytes).map(|_| bytes)
    });

    let start = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) if start.elapsed() < timeout => {
                std::thread::sleep(Duration::from_millis(20));
            }
            Ok(None) => {
                let _ = child.kill();
                child.wait()?;
                break None;
            }
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = stdout_reader.join();
                let _ = stderr_reader.join();
                return Err(error);
            }
        }
    };

    let stdout = stdout_reader
        .join()
        .map_err(|_| io::Error::other("stdout reader thread panicked"))??;
    let stderr = stderr_reader
        .join()
        .map_err(|_| io::Error::other("stderr reader thread panicked"))??;
    Ok(match status {
        Some(status) => CommandResult::Completed(Output {
            status,
            stdout,
            stderr,
        }),
        None => CommandResult::TimedOut,
    })
}

fn find_tesseract() -> Option<PathBuf> {
    let mut probe = Command::new("tesseract");
    probe.arg("--version");
    if matches!(
        run_command_with_timeout(&mut probe, TESSERACT_PROBE_TIMEOUT),
        Ok(CommandResult::Completed(output)) if output.status.success()
    ) {
        return Some(PathBuf::from("tesseract"));
    }

    let mut paths = vec![
        PathBuf::from(r"C:\Program Files\Tesseract-OCR\tesseract.exe"),
        PathBuf::from(r"C:\Program Files (x86)\Tesseract-OCR\tesseract.exe"),
    ];
    if let Ok(local_app_data) = env::var("LOCALAPPDATA") {
        paths.push(
            PathBuf::from(&local_app_data)
                .join("Programs")
                .join("Tesseract-OCR")
                .join("tesseract.exe"),
        );
        paths.push(
            PathBuf::from(local_app_data)
                .join("Tesseract-OCR")
                .join("tesseract.exe"),
        );
    }
    paths.into_iter().find(|path| path.is_file())
}

#[cfg(all(test, unix))]
mod command_timeout_tests {
    use super::*;

    #[test]
    fn captures_output_from_completed_child() {
        let mut command = Command::new("sh");
        command.arg("-c").arg("printf ready; printf warning >&2");
        match run_command_with_timeout(&mut command, Duration::from_secs(2)).unwrap() {
            CommandResult::Completed(output) => {
                assert!(output.status.success());
                assert_eq!(output.stdout, b"ready");
                assert_eq!(output.stderr, b"warning");
            }
            CommandResult::TimedOut => panic!("short-lived child unexpectedly timed out"),
        }
    }

    #[test]
    fn kills_and_reaps_timed_out_child() {
        let mut command = Command::new("sh");
        // exec makes sleep the direct child, so killing the child cannot leave
        // a shell-owned descendant behind.
        command.arg("-c").arg("exec sleep 5");
        let start = Instant::now();
        assert!(matches!(
            run_command_with_timeout(&mut command, Duration::from_millis(75)).unwrap(),
            CommandResult::TimedOut
        ));
        assert!(start.elapsed() < Duration::from_secs(2));
    }
}

#[derive(Debug, Serialize, Default, Clone)]
pub struct UiNode {
    #[serde(skip_serializing_if = "String::is_empty")]
    pub class: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub resource_id: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub text: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub content_desc: String,
    pub bounds: String,
    pub clickable: bool,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub children: Vec<UiNode>,
}

pub fn parse_bounds(bounds: &str) -> Option<(i32, i32, i32, i32)> {
    let parts: Vec<&str> = bounds.split("][").collect();
    if parts.len() != 2 {
        return None;
    }

    let p1 = parts[0].trim_start_matches('[');
    let p2 = parts[1].trim_end_matches(']');

    let c1: Vec<&str> = p1.split(',').collect();
    let c2: Vec<&str> = p2.split(',').collect();

    if c1.len() != 2 || c2.len() != 2 {
        return None;
    }

    Some((
        c1[0].parse().ok()?,
        c1[1].parse().ok()?,
        c2[0].parse().ok()?,
        c2[1].parse().ok()?,
    ))
}

fn node_from_attributes(event: &quick_xml::events::BytesStart<'_>) -> AnyhowResult<UiNode> {
    let mut node = UiNode::default();
    for attr in event.attributes() {
        let attr = attr?;
        match attr.key.as_ref() {
            b"class" => node.class = String::from_utf8_lossy(&attr.value).to_string(),
            b"resource-id" => node.resource_id = String::from_utf8_lossy(&attr.value).to_string(),
            b"text" => node.text = String::from_utf8_lossy(&attr.value).to_string(),
            b"content-desc" => node.content_desc = String::from_utf8_lossy(&attr.value).to_string(),
            b"bounds" => node.bounds = String::from_utf8_lossy(&attr.value).to_string(),
            b"clickable" => node.clickable = attr.value.as_ref() == b"true",
            _ => (),
        }
    }
    Ok(node)
}

fn recursive_parse(reader: &mut Reader<&[u8]>, mut current_node: UiNode) -> AnyhowResult<UiNode> {
    let mut buf = Vec::new();

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) => {
                if e.name().as_ref() == b"node" {
                    let child = node_from_attributes(&e)?;
                    let processed_child = recursive_parse(reader, child)?;
                    current_node.children.push(processed_child);
                }
            }
            Ok(Event::Empty(e)) if e.name().as_ref() == b"node" => {
                current_node.children.push(node_from_attributes(&e)?);
            }
            Ok(Event::End(e)) => {
                if e.name().as_ref() == b"node" || e.name().as_ref() == b"hierarchy" {
                    return Ok(current_node);
                }
            }
            Ok(Event::Eof) => break,
            Err(e) => return Err(anyhow!("XML Error: {e}")),
            _ => (),
        }
        buf.clear();
    }
    Ok(current_node)
}

fn parse_hierarchy_xml(xml_content: &str) -> AnyhowResult<UiNode> {
    let mut reader = Reader::from_str(xml_content);
    reader.config_mut().trim_text(true);

    let mut buf = Vec::new();
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) => {
                if e.name().as_ref() == b"node" {
                    let node = node_from_attributes(&e)?;
                    return recursive_parse(&mut reader, node);
                }
            }
            Ok(Event::Eof) => break,
            _ => {}
        }
    }
    Err(anyhow!("No hierarchy found"))
}

pub async fn fetch_parsed_hierarchy() -> AnyhowResult<UiNode> {
    Adb::shell_native("uiautomator dump")
        .await
        .map_err(|error| anyhow!("Dump failed: {error}"))?;
    let xml_content = Adb::shell_native("cat /sdcard/window_dump.xml")
        .await
        .map_err(|error| anyhow!("Read failed: {error}"))?;
    parse_hierarchy_xml(&xml_content)
}

#[cfg(test)]
mod hierarchy_tests {
    use super::{hierarchy_response, parse_bounds, parse_hierarchy_xml, UiNode};
    use crate::response::DEFAULT_TEXT_BUDGET_BYTES;

    #[test]
    fn retains_self_closing_leaf_nodes() {
        let xml = r#"<hierarchy><node class="root" bounds="[0,0][10,10]"><node class="leaf" text="OK" clickable="true" bounds="[1,2][3,4]" /></node></hierarchy>"#;
        let root = parse_hierarchy_xml(xml).unwrap();
        assert_eq!(root.children.len(), 1);
        assert_eq!(root.children[0].text, "OK");
        assert!(root.children[0].clickable);
    }

    #[test]
    fn rejects_malformed_bounds_instead_of_inventing_zeroes() {
        assert_eq!(parse_bounds("[a,2][3,4]"), None);
        assert_eq!(parse_bounds("[1,2][3,4]"), Some((1, 2, 3, 4)));
    }

    #[test]
    fn hierarchy_adapter_reports_head_truncation() {
        let node = UiNode {
            class: "root".into(),
            text: "é".repeat(DEFAULT_TEXT_BUDGET_BYTES),
            ..UiNode::default()
        };
        let result = hierarchy_response(&node).unwrap();
        let text = result["content"][0]["text"].as_str().unwrap();
        assert!(text.starts_with("{\"class\":\"root\""));
        assert!(text.is_char_boundary(text.len()));
        assert_eq!(result["metadata"]["truncation"]["strategy"], "head");
        assert_eq!(
            result["metadata"]["truncation"]["limit_bytes"],
            DEFAULT_TEXT_BUDGET_BYTES
        );
    }
}

pub async fn get_view_hierarchy() -> ToolResult {
    match fetch_parsed_hierarchy().await {
        Ok(node) => hierarchy_response(&node),
        Err(e) => response::error_response(e.to_string()),
    }
}

fn hierarchy_response(node: &UiNode) -> ToolResult {
    match serde_json::to_string(node) {
        Ok(serialized) => response::bounded_text_response(
            serialized,
            response::DEFAULT_TEXT_BUDGET_BYTES,
            response::TruncationStrategy::Head,
        ),
        Err(error) => response::error_response(format!("Serialization failed: {error}")),
    }
}

use base64::{engine::general_purpose, Engine as _};
use image::{DynamicImage, RgbImage};
use imageproc::drawing::draw_hollow_rect_mut;
use imageproc::rect::Rect;
use imageproc::template_matching::match_template;

/// A base64 JPEG together with the coordinate space it was produced from.
///
/// Every tool that returns an image also returns this, because the encoders may
/// downscale. A model that reads a coordinate off the returned pixels and taps
/// it without rescaling lands in the wrong place by exactly `scale()`, which is
/// the single most common failure mode in this category of server.
#[derive(Debug, Clone)]
pub struct EncodedImage {
    pub data: String,
    /// Pixel size of the JPEG in `data`.
    pub width: u32,
    pub height: u32,
    /// Pixel size before any downscale. This is the device coordinate space
    /// shared by uiautomator bounds, OCR boxes, and input taps.
    pub source_width: u32,
    pub source_height: u32,
    /// Device-space position of this image's top-left pixel. Non-zero only for
    /// crops, where a coordinate must be offset as well as scaled.
    pub origin_x: u32,
    pub origin_y: u32,
}

impl EncodedImage {
    /// Factor converting a coordinate on the returned image to a device
    /// coordinate. `1.0` when the image was not downscaled.
    pub fn scale(&self) -> f64 {
        if self.width == 0 {
            return 1.0;
        }
        f64::from(self.source_width) / f64::from(self.width)
    }

    /// Coordinate-space description published as the tool result `metadata`.
    pub fn metadata(&self) -> serde_json::Value {
        let scale = self.scale();
        let identity =
            (scale - 1.0).abs() < f64::EPSILON && self.origin_x == 0 && self.origin_y == 0;
        let note = if identity {
            "Image pixels are device pixels. Tap coordinates read from this image directly."
                .to_string()
        } else {
            format!(
                "To convert a coordinate (x, y) read from this image into a device \
                 coordinate: device_x = {} + x * {scale}, device_y = {} + y * {scale}.",
                self.origin_x, self.origin_y
            )
        };
        serde_json::json!({
            "image": {
                "width": self.width,
                "height": self.height,
                "device_width": self.source_width,
                "device_height": self.source_height,
                "origin_x": self.origin_x,
                "origin_y": self.origin_y,
                "scale": scale,
                "coordinate_space": if identity { "device" } else { "image" },
                "note": note,
            }
        })
    }
}

/// Encode an already-decoded frame, downscaling to `max_width` if needed.
pub fn encode_frame(img: &RgbImage, max_width: u32, quality: u8) -> AnyhowResult<EncodedImage> {
    validate_encoding_options(max_width, quality)?;
    let (source_width, source_height) = (img.width(), img.height());
    let mut image = img.clone();
    resize_to_max_width(&mut image, max_width);
    Ok(EncodedImage {
        data: encode_jpeg(&image, quality)?,
        width: image.width(),
        height: image.height(),
        source_width,
        source_height,
        origin_x: 0,
        origin_y: 0,
    })
}

pub fn encode_crop(
    bytes: &[u8],
    x: u32,
    y: u32,
    width: u32,
    height: u32,
    max_width: u32,
    quality: u8,
) -> AnyhowResult<EncodedImage> {
    validate_encoding_options(max_width, quality)?;
    if width == 0 || height == 0 {
        return Err(anyhow!("invalid crop bounds"));
    }
    let image = image::load_from_memory(bytes)?.to_rgb8();
    let x2 = x
        .checked_add(width)
        .ok_or_else(|| anyhow!("crop overflow"))?;
    let y2 = y
        .checked_add(height)
        .ok_or_else(|| anyhow!("crop overflow"))?;
    if x2 > image.width() || y2 > image.height() {
        return Err(anyhow!("crop lies outside screenshot"));
    }
    let mut crop = image::imageops::crop_imm(&image, x, y, width, height).to_image();
    let (source_width, source_height) = (crop.width(), crop.height());
    resize_to_max_width(&mut crop, max_width);
    Ok(EncodedImage {
        data: encode_jpeg(&crop, quality)?,
        width: crop.width(),
        height: crop.height(),
        source_width,
        source_height,
        origin_x: x,
        origin_y: y,
    })
}

pub fn encode_full(bytes: &[u8], max_width: u32, quality: u8) -> AnyhowResult<EncodedImage> {
    validate_encoding_options(max_width, quality)?;
    let mut image = image::load_from_memory(bytes)?.to_rgb8();
    let (source_width, source_height) = (image.width(), image.height());
    resize_to_max_width(&mut image, max_width);
    Ok(EncodedImage {
        data: encode_jpeg(&image, quality)?,
        width: image.width(),
        height: image.height(),
        source_width,
        source_height,
        origin_x: 0,
        origin_y: 0,
    })
}

pub fn encode_annotated(
    bytes: &[u8],
    root: &UiNode,
    clickable_only: bool,
    max_width: u32,
    quality: u8,
) -> AnyhowResult<EncodedImage> {
    validate_encoding_options(max_width, quality)?;
    let mut image = image::load_from_memory(bytes)?.to_rgb8();
    let (source_width, source_height) = (image.width(), image.height());
    fn draw(image: &mut RgbImage, node: &UiNode, clickable_only: bool) {
        if (!clickable_only || node.clickable)
            && (node.clickable || !node.text.is_empty() || !node.content_desc.is_empty())
        {
            if let Some((x1, y1, x2, y2)) = parse_bounds(&node.bounds) {
                if x2 > x1 && y2 > y1 && x1 >= 0 && y1 >= 0 {
                    draw_hollow_rect_mut(
                        image,
                        Rect::at(x1, y1).of_size((x2 - x1) as u32, (y2 - y1) as u32),
                        image::Rgb([255, 0, 0]),
                    );
                }
            }
        }
        for child in &node.children {
            draw(image, child, clickable_only);
        }
    }
    draw(&mut image, root, clickable_only);
    resize_to_max_width(&mut image, max_width);
    Ok(EncodedImage {
        data: encode_jpeg(&image, quality)?,
        width: image.width(),
        height: image.height(),
        source_width,
        source_height,
        origin_x: 0,
        origin_y: 0,
    })
}

/// Widest image any encoder will emit. Frames wider than this are downscaled and
/// report the resulting `scale` in their metadata.
pub const MAX_ENCODE_WIDTH: u32 = 1440;
/// Narrowest image any encoder will emit.
pub const MIN_ENCODE_WIDTH: u32 = 64;

fn validate_encoding_options(max_width: u32, quality: u8) -> AnyhowResult<()> {
    if !(MIN_ENCODE_WIDTH..=MAX_ENCODE_WIDTH).contains(&max_width) || !(1..=95).contains(&quality) {
        return Err(anyhow!(
            "encoding limits: max_width {MIN_ENCODE_WIDTH}..{MAX_ENCODE_WIDTH}, quality 1..95"
        ));
    }
    Ok(())
}

fn resize_to_max_width(image: &mut RgbImage, max_width: u32) {
    if image.width() > max_width {
        let height = ((u64::from(image.height()) * u64::from(max_width)) / u64::from(image.width()))
            .max(1) as u32;
        *image = image::imageops::resize(
            image,
            max_width,
            height,
            image::imageops::FilterType::Triangle,
        );
    }
}

fn encode_jpeg(image: &RgbImage, quality: u8) -> AnyhowResult<String> {
    let mut bytes = Vec::new();
    let mut encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut bytes, quality);
    encoder.encode_image(image)?;
    Ok(general_purpose::STANDARD.encode(bytes))
}

#[cfg(test)]
mod image_variant_tests {
    use super::*;

    fn png(width: u32, height: u32) -> Vec<u8> {
        let image = RgbImage::from_pixel(width, height, image::Rgb([1, 2, 3]));
        let mut bytes = Vec::new();
        DynamicImage::ImageRgb8(image)
            .write_to(
                &mut std::io::Cursor::new(&mut bytes),
                image::ImageFormat::Png,
            )
            .unwrap();
        bytes
    }

    #[test]
    fn crop_is_bounded_and_resized() {
        let bytes = png(100, 80);
        let cropped = encode_crop(&bytes, 10, 10, 50, 40, 64, 70).unwrap();
        assert!(!cropped.data.is_empty());
        // A 50-wide crop is already under the 64 cap, so it is returned intact
        // and stays in device coordinates apart from its origin offset.
        assert_eq!((cropped.width, cropped.height), (50, 40));
        assert_eq!((cropped.origin_x, cropped.origin_y), (10, 10));
        assert_eq!(cropped.scale(), 1.0);
        assert!(encode_crop(&bytes, 90, 0, 20, 20, 64, 70).is_err());
        assert!(encode_crop(&bytes, u32::MAX, 0, 2, 2, 64, 70).is_err());
    }

    #[test]
    fn downscaled_images_report_the_factor_needed_to_tap_them() {
        let bytes = png(1080, 2400);
        let encoded = encode_full(&bytes, 720, 70).unwrap();
        assert_eq!((encoded.width, encoded.height), (720, 1600));
        assert_eq!((encoded.source_width, encoded.source_height), (1080, 2400));
        assert_eq!(encoded.scale(), 1.5);
        let metadata = encoded.metadata();
        assert_eq!(metadata["image"]["scale"], 1.5);
        assert_eq!(metadata["image"]["coordinate_space"], "image");
        assert_eq!(metadata["image"]["device_width"], 1080);
    }

    #[test]
    fn undownscaled_images_declare_device_coordinates() {
        let image = RgbImage::from_pixel(1080, 2400, image::Rgb([9, 9, 9]));
        let encoded = encode_frame(&image, MAX_ENCODE_WIDTH, 85).unwrap();
        assert_eq!(encoded.scale(), 1.0);
        assert_eq!(encoded.metadata()["image"]["coordinate_space"], "device");
    }

    #[test]
    fn all_encoders_enforce_runtime_limits() {
        let bytes = png(100, 80);
        assert!(encode_full(&bytes, 63, 70).is_err());
        assert!(encode_full(&bytes, 1441, 70).is_err());
        assert!(encode_full(&bytes, 64, 0).is_err());
        assert!(encode_full(&bytes, 64, 96).is_err());
        assert!(!encode_full(&bytes, 64, 70).unwrap().data.is_empty());
    }

    #[test]
    fn annotation_draws_without_panicking() {
        let bytes = png(50, 50);
        let node = UiNode {
            text: "OK".into(),
            bounds: "[1,2][30,20]".into(),
            clickable: true,
            ..UiNode::default()
        };
        assert!(!encode_annotated(&bytes, &node, true, 64, 70)
            .unwrap()
            .data
            .is_empty());
        assert!(encode_annotated(&bytes, &node, true, 64, 96).is_err());
    }
}

use std::sync::Arc;

pub fn get_latest_image_raw(
    stream_manager: &std::sync::Arc<crate::stream::StreamManager>,
) -> Option<Arc<RgbImage>> {
    if !stream_manager.running.lock().is_ok_and(|running| *running) {
        return None;
    }

    // A running encoder may emit no bytes while the screen is static. In that
    // state the last decoded frame is still the current screen, even when its
    // timestamp is old.
    match stream_manager.latest_image.lock() {
        Ok(lock) => lock.clone(),
        Err(_) => None,
    }
}

#[cfg(test)]
mod stream_cache_tests {
    use super::get_latest_image_raw;
    use crate::stream::StreamManager;
    use std::sync::Arc;

    #[test]
    fn running_static_stream_keeps_its_last_frame_available() {
        let manager = Arc::new(StreamManager::new());
        *manager.running.lock().unwrap() = true;
        *manager.latest_image.lock().unwrap() = Some(Arc::new(image::RgbImage::from_pixel(
            2,
            3,
            image::Rgb([1, 2, 3]),
        )));
        // Deliberately old: a static stream's timestamp is not an expiry time.
        *manager.frame_timestamp_ms.lock().unwrap() = 1;

        let frame = get_latest_image_raw(&manager).unwrap();
        assert_eq!(frame.dimensions(), (2, 3));

        *manager.running.lock().unwrap() = false;
        assert!(get_latest_image_raw(&manager).is_none());
    }
}

pub async fn find_template(
    stream_manager: &std::sync::Arc<crate::stream::StreamManager>,
    template_data: &[u8],
    threshold: f32,
) -> ToolResult {
    let stream_manager_arc = stream_manager.clone();
    let template_data_vec = template_data.to_vec();

    // 1. Try to get image from stream (Fast)
    let maybe_stream_img = get_latest_image_raw(&stream_manager_arc);

    // 2. Determine final image source (Stream or Screenshot Fallback)
    let final_img: RgbImage = if let Some(arc_img) = maybe_stream_img {
        arc_img.as_ref().clone()
    } else {
        // Fallback: Screenshot (Slow)
        tracing::warn!("Stream inactive. Falling back to native screenshot for template match.");
        let png_data = match screenshot().await {
            Ok(d) => d,
            Err(e) => {
                return response::error_response(format!(
                    "Both stream and screenshot fallback failed: {e}"
                ))
            }
        };

        // Decode the fallback screenshot PNG into an RgbImage.
        match image::load_from_memory(&png_data) {
            Ok(dyn_img) => dyn_img.to_rgb8(),
            Err(e) => return response::error_response(format!("Screenshot decode failed: {e}")),
        }
    };

    // Spawn blocking for image processing (heavy math)
    tokio::task::spawn_blocking(move || {
        // 1. Load Template
        let template_img = match image::load_from_memory(&template_data_vec) {
            Ok(img) => img.to_rgb8(),
            Err(e) => return response::error_response(format!("Failed to load template: {e}")),
        };

        // 2. Convert to Grayscale for speed
        let scene_gray = image::DynamicImage::ImageRgb8(final_img).to_luma8();
        let template_gray = image::DynamicImage::ImageRgb8(template_img).to_luma8();

        // 3. Match
        let result = match_template(
            &scene_gray,
            &template_gray,
            imageproc::template_matching::MatchTemplateMethod::SumOfSquaredErrorsNormalized,
        );

        let mut min_val = f32::MAX;
        let mut min_x = 0;
        let mut min_y = 0;

        for (x, y, p) in result.enumerate_pixels() {
            if p[0] < min_val {
                min_val = p[0];
                min_x = x;
                min_y = y;
            }
        }

        if min_val < (1.0 - threshold) {
            Ok(json!({
                "content": [{
                    "type": "text",
                    "text": format!("Match found. Error: {:.4}", min_val)
                }],
                "data": {
                    "x": min_x,
                    "y": min_y,
                    "w": template_gray.width(),
                    "h": template_gray.height(),
                    "error": min_val
                }
            }))
        } else {
            response::error_response(format!("No match found. Best error: {min_val:.4}"))
        }
    })
    .await
    .unwrap_or_else(|e| response::error_response(format!("Join Error: {e}")))
}

// Helper to write temp image (Sync)
fn write_temp_image(data: &[u8]) -> AnyhowResult<PathBuf> {
    let mut path = env::temp_dir();
    let fname = format!(
        "mcp_vision_{}.png",
        chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0)
    );
    path.push(fname);
    fs::write(&path, data)?;
    Ok(path)
}

/// Dark Mode Detection
/// Samples pixels to determine average brightness (0-255).
/// Returns true if image is "dark mode" (average brightness < 85).
fn is_dark_mode_image(data: &[u8]) -> bool {
    // If it will not decode, assume it is not dark rather than guessing.
    let Ok(img) = image::load_from_memory(data) else {
        return false;
    };

    let rgb = img.to_rgb8();
    let (width, height) = rgb.dimensions();

    // Sample every 50th pixel for performance (sparse sampling)
    let step = 50;
    let mut total_brightness: u64 = 0;
    let mut sample_count: u64 = 0;

    for y in (0..height).step_by(step) {
        for x in (0..width).step_by(step) {
            let pixel = rgb.get_pixel(x, y);
            // Perceived brightness formula: 0.299*R + 0.587*G + 0.114*B
            let brightness = (0.299 * f64::from(pixel[0])
                + 0.587 * f64::from(pixel[1])
                + 0.114 * f64::from(pixel[2])) as u64;
            total_brightness += brightness;
            sample_count += 1;
        }
    }

    if sample_count == 0 {
        return false;
    }

    let avg_brightness = total_brightness / sample_count;

    // Dark mode threshold: average brightness < 85 (out of 255)
    avg_brightness < 85
}

/// Invert image colors for better OCR on dark backgrounds
/// Returns new PNG data with inverted colors
fn invert_image_for_ocr(data: &[u8]) -> AnyhowResult<Vec<u8>> {
    let img = image::load_from_memory(data)?;
    let mut rgb = img.to_rgb8();

    for pixel in rgb.pixels_mut() {
        pixel[0] = 255 - pixel[0]; // R
        pixel[1] = 255 - pixel[1]; // G
        pixel[2] = 255 - pixel[2]; // B
    }

    // Encode back to PNG
    let mut buf = std::io::Cursor::new(Vec::new());
    let dyn_img = DynamicImage::ImageRgb8(rgb);
    dyn_img.write_to(&mut buf, image::ImageFormat::Png)?;

    Ok(buf.into_inner())
}

/// Process image for OCR: applies dark mode inversion if needed
fn prepare_image_for_ocr(data: &[u8]) -> (Vec<u8>, bool) {
    let is_dark = is_dark_mode_image(data);

    if is_dark {
        // Invert the image for better OCR
        match invert_image_for_ocr(data) {
            Ok(inverted) => (inverted, true),
            Err(_) => (data.to_vec(), false), // Fallback to original
        }
    } else {
        (data.to_vec(), false)
    }
}

pub async fn screenshot() -> AnyhowResult<Vec<u8>> {
    Adb::exec_out_native("screencap -p").await
}

pub async fn ocr() -> ToolResult {
    // Check if tesseract is available (Sync check is fine here or spawn blocking)
    // Capture the screenshot on the async runtime, then run the temp-file write
    // and tesseract invocation inside spawn_blocking below.
    let img_data = match screenshot().await {
        Ok(d) => d,
        Err(e) => return response::error_response(format!("Screenshot failed: {e}")),
    };

    tokio::task::spawn_blocking(move || {
        let Some(tesseract_cmd) = find_tesseract() else {
            return response::error_response(TESSERACT_MISSING);
        };

        let img_path = match write_temp_image(&img_data) {
            Ok(p) => p,
            Err(e) => return response::error_response(format!("Temp write failed: {e}")),
        };

        let mut out_base = img_path.clone();
        out_base.set_extension(""); // tesseract adds .txt automatically
        let out_base_str = out_base.to_string_lossy().to_string();

        // Run tesseract
        // tesseract <img_path> <out_base>
        let mut command = Command::new(&tesseract_cmd);
        command.arg(&img_path).arg(&out_base_str);
        let output = run_command_with_timeout(&mut command, TESSERACT_OCR_TIMEOUT);

        // Clean up image
        let _ = fs::remove_file(&img_path);
        let txt_path = format!("{out_base_str}.txt");

        match output {
            Ok(CommandResult::Completed(o)) if o.status.success() => {
                let text = fs::read_to_string(&txt_path).unwrap_or_default();
                let _ = fs::remove_file(&txt_path);
                response::bounded_text_response(
                    text.trim(),
                    response::DEFAULT_TEXT_BUDGET_BYTES,
                    response::TruncationStrategy::Head,
                )
            }
            Ok(CommandResult::Completed(o)) => {
                let _ = fs::remove_file(&txt_path);
                let err = String::from_utf8_lossy(&o.stderr);
                response::error_response(format!("Tesseract failed: {err}"))
            }
            Ok(CommandResult::TimedOut) => {
                let _ = fs::remove_file(&txt_path);
                response::error_response(format!(
                    "Tesseract timed out after {} seconds",
                    TESSERACT_OCR_TIMEOUT.as_secs()
                ))
            }
            Err(e) => {
                let _ = fs::remove_file(&txt_path);
                response::error_response(format!("Failed to run tesseract: {e}"))
            }
        }
    })
    .await
    .unwrap_or_else(|e| response::error_response(format!("Join Error: {e}")))
}

pub async fn find_text(query: &str) -> ToolResult {
    let query_string = query.to_string();

    let img_data = match screenshot().await {
        Ok(d) => d,
        Err(e) => return response::error_response(format!("Screenshot failed: {e}")),
    };

    tokio::task::spawn_blocking(move || {
        let Some(tesseract_cmd) = find_tesseract() else {
            return response::error_response(TESSERACT_MISSING);
        };

        // Apply dark mode preprocessing for better OCR
        let (processed_img_data, was_inverted) = prepare_image_for_ocr(&img_data);
        if was_inverted {
            tracing::debug!("OCR: dark mode detected, inverted image for better recognition");
        }

        let img_path = match write_temp_image(&processed_img_data) {
            Ok(p) => p,
            Err(e) => return response::error_response(format!("Temp write failed: {e}"))
        };

        let mut out_base = img_path.clone();
        out_base.set_extension("");
        let out_base_str = out_base.to_string_lossy().to_string();

        // tesseract <img_path> <out_base> tsv
        let mut command = Command::new(&tesseract_cmd);
        command.arg(&img_path).arg(&out_base_str).arg("tsv");
        let output = run_command_with_timeout(&mut command, TESSERACT_OCR_TIMEOUT);

        let _ = fs::remove_file(&img_path);
        let tsv_path = format!("{out_base_str}.tsv");

        match output {
            Ok(CommandResult::Completed(o)) if o.status.success() => {
                let content = fs::read_to_string(&tsv_path).unwrap_or_default();
                let _ = fs::remove_file(&tsv_path);

                #[derive(Debug, Clone)]
                struct TsvWord {
                    text: String,
                    left: i32, top: i32, width: i32, height: i32,
                    conf: f64,
                    block: i32, par: i32, line: i32, word_num: i32
                }

                let mut words = Vec::new();
                // level page_num block_num par_num line_num word_num left top width height conf text
                for line in content.lines().skip(1) {
                    let parts: Vec<&str> = line.split('\t').collect();
                    if parts.len() >= 12 {
                        let text = parts[11].trim();
                        if text.is_empty() { continue; }

                        let conf = parts[10].parse::<f64>().unwrap_or(0.0);
                        if conf < 30.0 { continue; } // Lower threshold slightly for partial words

                        words.push(TsvWord {
                            text: text.to_string(),
                            left: parts[6].parse().unwrap_or(0),
                            top: parts[7].parse().unwrap_or(0),
                            width: parts[8].parse().unwrap_or(0),
                            height: parts[9].parse().unwrap_or(0),
                            conf,
                            block: parts[2].parse().unwrap_or(0),
                            par: parts[3].parse().unwrap_or(0),
                            line: parts[4].parse().unwrap_or(0),
                            word_num: parts[5].parse().unwrap_or(0),
                        });
                    }
                }

                // Group by line
                // Key: (block, par, line) -> Vec<TsvWord>
                use std::collections::BTreeMap;
                let mut lines: BTreeMap<(i32, i32, i32), Vec<TsvWord>> = BTreeMap::new();

                for w in words {
                    lines.entry((w.block, w.par, w.line)).or_default().push(w);
                }

                let mut matches = Vec::new();
                let query_lower = query_string.to_lowercase();

                for (_, mut line_words) in lines {
                    // Sort by word_num to ensure correct reading order
                    line_words.sort_by_key(|w| w.word_num);

                    // Reconstruct full line text
                    let full_text = line_words.iter().map(|w| w.text.as_str()).collect::<Vec<_>>().join(" ");
                    let full_lower = full_text.to_lowercase();

                    if full_lower.contains(&query_lower) {
                        // The line contains the query. Find the specific word slice
                        // that forms it (e.g. "Accept & continue" -> the three tokens)
                        // so the returned box wraps only those words, not the whole line.
                        let query_tokens: Vec<&str> = query_string.split_whitespace().collect();
                        let token_count = query_tokens.len();

                        if token_count == 0 { continue; }

                        // Sliding window check over line_words
                        for i in 0..line_words.len() {
                            if i + token_count > line_words.len() { break; }

                            let slice = &line_words[i..i+token_count];
                            let reconstructed_slice = slice.iter().map(|w| w.text.as_str()).collect::<Vec<_>>().join(" ");

                            // Case-insensitive substring match over the window.
                            if reconstructed_slice.to_lowercase().contains(&query_lower) {
                                // Match! Compute Union Box.
                                let min_x = slice.iter().map(|w| w.left).min().unwrap_or(0);
                                let min_y = slice.iter().map(|w| w.top).min().unwrap_or(0);
                                let max_r = slice.iter().map(|w| w.left + w.width).max().unwrap_or(0);
                                let max_b = slice.iter().map(|w| w.top + w.height).max().unwrap_or(0);

                                let final_w = max_r - min_x;
                                let final_h = max_b - min_y;

                                matches.push(json!({
                                     "text": reconstructed_slice,
                                     "x": min_x,
                                     "y": min_y,
                                     "w": final_w,
                                     "h": final_h,
                                     "conf": slice.iter().map(|w| w.conf).sum::<f64>() / (slice.len() as f64)
                                }));
                            }
                        }

                        // Fallback: if no word window matched (e.g. an extra symbol
                        // split a token) but the line as a whole contains the query,
                        // return the bounds of the whole line.
                        if matches.is_empty() && full_lower.contains(&query_lower) {
                             let min_x = line_words.iter().map(|w| w.left).min().unwrap_or(0);
                             let min_y = line_words.iter().map(|w| w.top).min().unwrap_or(0);
                             let max_r = line_words.iter().map(|w| w.left + w.width).max().unwrap_or(0);
                             let max_b = line_words.iter().map(|w| w.top + w.height).max().unwrap_or(0);

                             matches.push(json!({
                                 "text": full_text,
                                 "x": min_x,
                                 "y": min_y,
                                 "w": max_r - min_x,
                                 "h": max_b - min_y,
                                 "conf": 50.0 // Default for fallback
                             }));
                        }
                    }
                }

                match serde_json::to_string(&matches) {
                    Ok(s) => response::bounded_text_response(
                        s,
                        response::DEFAULT_TEXT_BUDGET_BYTES,
                        response::TruncationStrategy::Head,
                    ),
                    Err(e) => response::error_response(format!("Serialization failed: {e}"))
                }
            },
             Ok(CommandResult::Completed(o)) => {
                 let _ = fs::remove_file(&tsv_path);
                 let err = String::from_utf8_lossy(&o.stderr);
                 response::error_response(format!("Tesseract TSV failed: {err}"))
            },
            Ok(CommandResult::TimedOut) => {
                let _ = fs::remove_file(&tsv_path);
                response::error_response(format!("Tesseract timed out after {} seconds", TESSERACT_OCR_TIMEOUT.as_secs()))
            },
            Err(e) => {
                let _ = fs::remove_file(&tsv_path);
                response::error_response(format!("Failed to run tesseract: {e}"))
            }
        }
    }).await.unwrap_or_else(|e| response::error_response(format!("Join Error: {e}")))
}

/// Stream control is all in-memory state flips, so this stays synchronous; the
/// decode loop it starts and stops runs on its own task.
pub fn vision_stream(
    action: &str,
    stream_manager: &std::sync::Arc<crate::stream::StreamManager>,
) -> ToolResult {
    match action {
        "start" => {
            stream_manager.start();
            response::text_response("H.264 Stream Started (Background)")
        }
        "stop" => {
            stream_manager.stop();
            response::text_response("H.264 Stream Stopped")
        }
        "read" => {
            let Ok(lock) = stream_manager.latest_image.lock() else {
                return response::error_response("Stream lock poisoned");
            };
            match &*lock {
                Some(img) => {
                    let w = img.width();
                    let h = img.height();
                    response::text_response(format!("Latest Decoded Frame: {w}x{h} RGB"))
                }
                None => response::text_response("No frame available yet"),
            }
        }
        _ => response::error_response("Unknown action. Use start, stop, read"),
    }
}
