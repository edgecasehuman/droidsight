use crate::vision::{parse_bounds, UiNode};
use serde::Serialize;

const SNAPSHOT_VERSION: &str = "a11y-v1";
const FNV_OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Element {
    pub index: usize,
    pub text: String,
    pub content_desc: String,
    pub resource_id: String,
    pub class: String,
    pub bounds: Bounds,
    pub center: Point,
    pub clickable: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
pub struct Bounds {
    pub x1: i32,
    pub y1: i32,
    pub x2: i32,
    pub y2: i32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct Point {
    pub x: i32,
    pub y: i32,
}

#[derive(Clone, Debug, Serialize)]
pub struct Snapshot {
    pub snapshot_id: String,
    pub elements: Vec<Element>,
}

pub fn build(root: &UiNode) -> Snapshot {
    let mut elements = Vec::new();
    flatten(root, &mut elements);
    let mut hash = FNV_OFFSET_BASIS;
    hash_bytes(&mut hash, SNAPSHOT_VERSION.as_bytes());
    hash_node(root, &mut hash);
    Snapshot {
        snapshot_id: format!("{SNAPSHOT_VERSION}:{hash:016x}"),
        elements,
    }
}

fn hash_bytes(hash: &mut u64, bytes: &[u8]) {
    for byte in bytes.iter().copied().chain(std::iter::once(0xff)) {
        *hash ^= u64::from(byte);
        *hash = hash.wrapping_mul(FNV_PRIME);
    }
}

fn hash_node(node: &UiNode, hash: &mut u64) {
    for value in [
        &node.text,
        &node.content_desc,
        &node.resource_id,
        &node.class,
        &node.bounds,
    ] {
        hash_bytes(hash, value.as_bytes());
    }
    hash_bytes(hash, &[u8::from(node.clickable)]);
    for child in &node.children {
        hash_node(child, hash);
    }
    hash_bytes(hash, b"node-end");
}

fn flatten(node: &UiNode, output: &mut Vec<Element>) {
    if let Some((x1, y1, x2, y2)) = parse_bounds(&node.bounds) {
        let meaningful = node.clickable
            || !node.text.is_empty()
            || !node.content_desc.is_empty()
            || !node.resource_id.is_empty()
            || node.class.contains("Button")
            || node.class.contains("EditText");
        if meaningful && x2 > x1 && y2 > y1 {
            output.push(Element {
                index: output.len(),
                text: node.text.clone(),
                content_desc: node.content_desc.clone(),
                resource_id: node.resource_id.clone(),
                class: node.class.clone(),
                bounds: Bounds { x1, y1, x2, y2 },
                center: Point {
                    x: x1 + (x2 - x1) / 2,
                    y: y1 + (y2 - y1) / 2,
                },
                clickable: node.clickable,
            });
        }
    }
    for child in &node.children {
        flatten(child, output);
    }
}

pub fn select<'a>(
    snapshot: &'a Snapshot,
    expected_id: &str,
    index: usize,
) -> Result<&'a Element, &'static str> {
    if expected_id != snapshot.snapshot_id {
        return Err("stale element snapshot; request elements again");
    }
    snapshot
        .elements
        .get(index)
        .ok_or("element index is out of range")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(text: &str, bounds: &str, clickable: bool) -> UiNode {
        UiNode {
            text: text.into(),
            bounds: bounds.into(),
            clickable,
            ..UiNode::default()
        }
    }

    #[test]
    fn produces_stable_indexed_elements_and_centers() {
        let mut root = node("", "[0,0][100,200]", false);
        root.children.push(node("Continue", "[10,20][90,60]", true));
        let a = build(&root);
        let b = build(&root);
        assert_eq!(a.snapshot_id, b.snapshot_id);
        assert_eq!(a.elements[0].index, 0);
        assert_eq!(a.elements[0].center, Point { x: 50, y: 40 });
    }

    #[test]
    fn rejects_stale_ids_and_bad_indices() {
        let snapshot = build(&node("OK", "[0,0][20,20]", true));
        assert!(select(&snapshot, "old", 0).unwrap_err().contains("stale"));
        assert!(select(&snapshot, &snapshot.snapshot_id, 9)
            .unwrap_err()
            .contains("range"));
    }

    #[test]
    fn ignores_empty_and_malformed_nodes() {
        let mut root = node("", "bad", false);
        root.children.push(node("", "[0,0][10,10]", false));
        assert!(build(&root).elements.is_empty());
    }

    #[test]
    fn non_element_overlay_invalidates_snapshot() {
        let before = node("OK", "[0,0][20,20]", true);
        let mut after = before.clone();
        after.children.push(node("", "[0,0][100,100]", false));
        assert_ne!(build(&before).snapshot_id, build(&after).snapshot_id);
    }
}
