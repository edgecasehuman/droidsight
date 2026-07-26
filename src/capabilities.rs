#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Capability {
    Observe,
    UiInput,
    AppControl,
    DeviceConfiguration,
    HostFileAccess,
    Destructive,
    ArbitraryExecution,
}

pub fn classify(tool: &str, action: &str) -> Option<Capability> {
    use Capability::{
        AppControl, ArbitraryExecution, Destructive, DeviceConfiguration, HostFileAccess, Observe,
        UiInput,
    };
    Some(match (tool, action) {
        (
            "vision",
            "screenshot"
            | "screenshot_cropped"
            | "screenshot_annotated"
            | "screenshot_sequence"
            | "hierarchy"
            | "elements"
            | "hierarchy_diff"
            | "ocr"
            | "find_text"
            | "find_element"
            | "find_template",
        ) => Observe,
        ("vision", "tap_element") => UiInput,
        ("input", "tap" | "text" | "key" | "swipe" | "smart_tap") => UiInput,
        ("app", "list" | "get_foreground" | "list_crashes" | "get_crash" | "crash_log") => Observe,
        ("app", "launch" | "stop") => AppControl,
        ("app", "permission" | "enable" | "disable") => DeviceConfiguration,
        ("app", "install") => HostFileAccess,
        ("app", "uninstall" | "clear_data") => Destructive,
        ("device", "battery" | "info" | "state" | "clipboard_get") => Observe,
        ("device", "clipboard_set" | "unlock" | "rotate") => DeviceConfiguration,
        ("shell" | "macro" | "instrumentation", _) => ArbitraryExecution,
        _ => return None,
    })
}

pub fn batch_allowed(capability: Capability) -> bool {
    matches!(
        capability,
        Capability::Observe | Capability::UiInput | Capability::AppControl
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn separates_safe_and_dangerous_actions_inside_grouped_tools() {
        assert_eq!(classify("app", "list"), Some(Capability::Observe));
        assert_eq!(classify("app", "clear_data"), Some(Capability::Destructive));
        assert!(!batch_allowed(classify("app", "clear_data").unwrap()));
    }
    #[test]
    fn unknown_actions_fail_closed() {
        assert_eq!(classify("app", "future_action"), None);
    }
}
