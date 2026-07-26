use crate::vision::{parse_bounds, UiNode};
use serde::Serialize;
use std::collections::HashMap;

const POSITION_BUCKET_PX: i32 = 40;
const MAX_LISTED_CHANGES: usize = 6;

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct ElementKey {
    label: String,
    class: String,
    x_bucket: i32,
    y_bucket: i32,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ElementChange {
    pub label: String,
    pub class: String,
    pub bounds: String,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct HierarchyDiff {
    pub baseline_created: bool,
    pub changed: bool,
    pub added: Vec<ElementChange>,
    pub removed: Vec<ElementChange>,
    pub added_overflow: usize,
    pub removed_overflow: usize,
}

fn label(node: &UiNode) -> &str {
    if node.text.trim().is_empty() {
        node.content_desc.trim()
    } else {
        node.text.trim()
    }
}

fn key(node: &UiNode) -> Option<ElementKey> {
    let label = label(node);
    if label.is_empty() {
        return None;
    }
    let (x1, y1, x2, y2) = parse_bounds(&node.bounds)?;
    Some(ElementKey {
        label: label.chars().take(30).collect(),
        class: node.class.clone(),
        x_bucket: (i64::midpoint(i64::from(x1), i64::from(x2)) / i64::from(POSITION_BUCKET_PX))
            as i32,
        y_bucket: (i64::midpoint(i64::from(y1), i64::from(y2)) / i64::from(POSITION_BUCKET_PX))
            as i32,
    })
}

fn flatten(node: &UiNode, output: &mut Vec<(ElementKey, ElementChange)>) {
    if let Some(key) = key(node) {
        output.push((
            key,
            ElementChange {
                label: label(node).to_string(),
                class: node.class.clone(),
                bounds: node.bounds.clone(),
            },
        ));
    }
    for child in &node.children {
        flatten(child, output);
    }
}

pub fn diff(previous: Option<&UiNode>, current: &UiNode) -> HierarchyDiff {
    let Some(previous) = previous else {
        return HierarchyDiff {
            baseline_created: true,
            ..HierarchyDiff::default()
        };
    };

    let mut old_elements = Vec::new();
    let mut new_elements = Vec::new();
    flatten(previous, &mut old_elements);
    flatten(current, &mut new_elements);
    let mut unmatched_old = counts(&old_elements);
    let mut unmatched_new = counts(&new_elements);
    let mut all_added = Vec::new();
    for (key, element) in new_elements {
        if !consume(&mut unmatched_old, &key) {
            all_added.push(element);
        }
    }

    let mut all_removed = Vec::new();
    for (key, element) in old_elements {
        if !consume(&mut unmatched_new, &key) {
            all_removed.push(element);
        }
    }

    HierarchyDiff {
        baseline_created: false,
        changed: !all_added.is_empty() || !all_removed.is_empty(),
        added_overflow: all_added.len().saturating_sub(MAX_LISTED_CHANGES),
        removed_overflow: all_removed.len().saturating_sub(MAX_LISTED_CHANGES),
        added: all_added.into_iter().take(MAX_LISTED_CHANGES).collect(),
        removed: all_removed.into_iter().take(MAX_LISTED_CHANGES).collect(),
    }
}

fn counts(elements: &[(ElementKey, ElementChange)]) -> HashMap<ElementKey, usize> {
    let mut counts = HashMap::new();
    for (key, _) in elements {
        *counts.entry(key.clone()).or_insert(0) += 1;
    }
    counts
}

fn consume(counts: &mut HashMap<ElementKey, usize>, key: &ElementKey) -> bool {
    match counts.get_mut(key) {
        Some(count) if *count > 0 => {
            *count -= 1;
            true
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::diff;
    use crate::vision::UiNode;

    fn node(text: &str, x: i32, y: i32) -> UiNode {
        UiNode {
            class: "Button".to_string(),
            text: text.to_string(),
            bounds: format!("[{x},{y}][{},{}]", x + 20, y + 20),
            clickable: true,
            ..UiNode::default()
        }
    }

    fn root(children: Vec<UiNode>) -> UiNode {
        UiNode {
            class: "root".to_string(),
            bounds: "[0,0][1080,2400]".to_string(),
            children,
            ..UiNode::default()
        }
    }

    #[test]
    fn first_snapshot_only_creates_a_baseline() {
        let result = diff(None, &root(vec![node("Send", 100, 200)]));
        assert!(result.baseline_created);
        assert!(!result.changed);
    }

    #[test]
    fn reports_added_and_removed_labeled_elements() {
        let before = root(vec![node("Draft", 100, 200)]);
        let after = root(vec![node("Sent", 100, 200)]);
        let result = diff(Some(&before), &after);
        assert!(result.changed);
        assert_eq!(result.added[0].label, "Sent");
        assert_eq!(result.removed[0].label, "Draft");
    }

    #[test]
    fn ignores_small_coordinate_jitter() {
        let before = root(vec![node("Send", 81, 201)]);
        let after = root(vec![node("Send", 88, 205)]);
        assert!(!diff(Some(&before), &after).changed);
    }

    #[test]
    fn caps_verbose_change_lists() {
        let before = root(Vec::new());
        let after = root(
            (0..9)
                .map(|index| node(&format!("new{index}"), index * 50, 100))
                .collect(),
        );
        let result = diff(Some(&before), &after);
        assert_eq!(result.added.len(), 6);
        assert_eq!(result.added_overflow, 3);
    }

    #[test]
    fn reports_duplicate_count_changes() {
        let duplicate = node("Same", 100, 200);
        let before = root(vec![duplicate.clone()]);
        let after = root(vec![duplicate.clone(), duplicate]);
        let result = diff(Some(&before), &after);
        assert!(result.changed);
        assert_eq!(result.added.len(), 1);
        assert!(result.removed.is_empty());

        let result = diff(Some(&after), &before);
        assert!(result.changed);
        assert_eq!(result.removed.len(), 1);
        assert!(result.added.is_empty());
    }

    #[test]
    fn extreme_coordinates_do_not_overflow() {
        let before = root(vec![node("Edge", i32::MAX - 20, i32::MAX - 20)]);
        let after = before.clone();
        assert!(!diff(Some(&before), &after).changed);
    }
}
