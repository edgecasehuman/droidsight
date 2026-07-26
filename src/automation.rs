use crate::device_metrics::CoordinateSource;
use crate::input;
use crate::response::{self, ToolResult};
use crate::vision::{self, UiNode};
use serde_json::json;

fn find_node(node: &UiNode, text: &str, resource_id: &str, content_desc: &str) -> Option<UiNode> {
    let text_lower = text.to_lowercase();
    let rid_lower = resource_id.to_lowercase();
    let desc_lower = content_desc.to_lowercase();

    if (!text.is_empty() && node.text.to_lowercase().contains(&text_lower))
        || (!resource_id.is_empty() && node.resource_id.to_lowercase().contains(&rid_lower))
        || (!content_desc.is_empty() && node.content_desc.to_lowercase().contains(&desc_lower))
    {
        return Some(node.clone());
    }

    for child in &node.children {
        if let Some(n) = find_node(child, text, resource_id, content_desc) {
            return Some(n);
        }
    }
    None
}

pub async fn tap_element(text: &str, resource_id: &str, content_desc: &str) -> ToolResult {
    let root = match vision::fetch_parsed_hierarchy().await {
        Ok(root) => root,
        Err(error) => {
            tracing::warn!("SmartTap hierarchy failed, falling back to OCR: {error}");
            return tap_element_ocr(text).await;
        }
    };

    if let Some(node) = find_node(&root, text, resource_id, content_desc) {
        if let Some((x1, y1, x2, y2)) = vision::parse_bounds(&node.bounds) {
            let cx = i32::midpoint(x1, x2);
            let cy = i32::midpoint(y1, y2);

            match input::tap_raw(cx, cy, CoordinateSource::Native).await {
                Ok(_) => {
                    return response::text_response(format!(
                        "Tapped element at {cx} {cy} (Hierarchy)"
                    ));
                }
                Err(error) => tracing::warn!("SmartTap hierarchy tap failed: {error}"),
            }
        }
    }

    tap_element_ocr(text).await
}

async fn tap_element_ocr(text: &str) -> ToolResult {
    if text.is_empty() {
        return response::error_response(
            "Element not found via Hierarchy, and no text provided for OCR fallback.",
        );
    }

    match vision::find_text(text).await {
        Ok(value) => {
            if let Some(content) = value.get("content").and_then(|content| content.as_array()) {
                if let Some(item) = content.first() {
                    if let Some(text_json) = item.get("text").and_then(|text| text.as_str()) {
                        if let Ok(matches) = serde_json::from_str::<serde_json::Value>(text_json) {
                            if let Some(first_match) =
                                matches.as_array().and_then(|array| array.first())
                            {
                                let coordinate = |name| {
                                    first_match
                                        .get(name)
                                        .and_then(serde_json::Value::as_i64)
                                        .and_then(|value| i32::try_from(value).ok())
                                };
                                let (x, y, width, height) = match (
                                    coordinate("x"),
                                    coordinate("y"),
                                    coordinate("w"),
                                    coordinate("h"),
                                ) {
                                    (Some(x), Some(y), Some(width), Some(height))
                                        if width > 0 && height > 0 =>
                                    {
                                        (x, y, width, height)
                                    }
                                    _ => {
                                        return response::error_response(
                                            "OCR returned invalid bounds",
                                        );
                                    }
                                };

                                return input::tap_raw(
                                    x + width / 2,
                                    y + height / 2,
                                    CoordinateSource::Native,
                                )
                                .await;
                            }
                        }
                    }
                }
            }
            response::error_response("Element not found via OCR")
        }
        Err(error) => response::error_response(format!("OCR failed: {error}")),
    }
}

pub async fn find_element(text: &str, resource_id: &str, content_desc: &str) -> ToolResult {
    let root = match vision::fetch_parsed_hierarchy().await {
        Ok(root) => root,
        Err(error) => return response::error_response(error.to_string()),
    };

    if let Some(node) = find_node(&root, text, resource_id, content_desc) {
        if let Some((x1, y1, x2, y2)) = vision::parse_bounds(&node.bounds) {
            Ok(json!({
                "content": [{
                    "type": "text",
                    "text": format!("Found element at bounds: {}", node.bounds)
                }],
                "data": {
                    "bounds": node.bounds,
                    "x": i32::midpoint(x1, x2),
                    "y": i32::midpoint(y1, y2),
                    "w": x2 - x1,
                    "h": y2 - y1,
                    "class": node.class,
                    "text": node.text,
                    "id": node.resource_id
                }
            }))
        } else {
            response::error_response("Failed to parse bounds")
        }
    } else {
        response::error_response("Element not found")
    }
}
