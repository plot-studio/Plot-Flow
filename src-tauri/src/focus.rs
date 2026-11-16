//! "Is the keyboard cursor in a text field right now?"
//!
//! Smart insert pastes only when the answer is yes (or unknown). The probe
//! runs in two steps:
//!
//! 1. Cheap Win32 checks on the caller's thread: no foreground window, a
//!    minimized one, our own window, or the desktop/taskbar → not editable;
//!    a system caret in the foreground thread → editable.
//! 2. UI Automation on a dedicated MTA thread: the focused element's control
//!    type, ValuePattern (read-only or not) and TextPattern + keyboard focus.
//!    Browsers, Electron, Telegram, Office expose their inputs this way.
//!
//! The UIA step is bounded by [`UIA_TIMEOUT`]; a slow or failing answer is
//! `Unknown`, which callers treat as editable, so an application we cannot
//! read keeps the old "always paste" behaviour.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FocusTarget {
    /// The focus is in a text field: pasting lands where the user looks.
    Editable,
    /// The focus is clearly not in a text field (desktop, list, web page
    /// body, minimized app, our own window): a paste would go nowhere.
    NotEditable,
    /// Could not tell. Treated as editable.
    Unknown,
}

impl FocusTarget {
    /// Whether a paste should be attempted.
    pub fn accepts_paste(self) -> bool {
        !matches!(self, FocusTarget::NotEditable)
    }
}

// UI Automation control type ids (UIAutomationClient.h), kept as plain
// numbers so the decision logic below is testable on every platform.
mod control {
    pub const BUTTON: i32 = 50000;
    pub const CALENDAR: i32 = 50001;
    pub const CHECK_BOX: i32 = 50002;
    pub const COMBO_BOX: i32 = 50003;
    pub const EDIT: i32 = 50004;
    pub const HYPERLINK: i32 = 50005;
    pub const IMAGE: i32 = 50006;
    pub const LIST_ITEM: i32 = 50007;
    pub const LIST: i32 = 50008;
    pub const MENU: i32 = 50009;
    pub const MENU_BAR: i32 = 50010;
    pub const MENU_ITEM: i32 = 50011;
    pub const PROGRESS_BAR: i32 = 50012;
    pub const RADIO_BUTTON: i32 = 50013;
    pub const SCROLL_BAR: i32 = 50014;
    pub const SLIDER: i32 = 50015;
    pub const STATUS_BAR: i32 = 50017;
    pub const TAB: i32 = 50018;
    pub const TAB_ITEM: i32 = 50019;
    pub const TOOL_BAR: i32 = 50021;
    pub const TOOL_TIP: i32 = 50022;
    pub const TREE: i32 = 50023;
    pub const TREE_ITEM: i32 = 50024;
    pub const THUMB: i32 = 50027;
    pub const DATA_GRID: i32 = 50028;
    pub const DATA_ITEM: i32 = 50029;
    pub const DOCUMENT: i32 = 50030;
    pub const SPLIT_BUTTON: i32 = 50031;
    pub const HEADER: i32 = 50034;
    pub const HEADER_ITEM: i32 = 50035;
    pub const TABLE: i32 = 50036;
    pub const TITLE_BAR: i32 = 50037;
}

/// Decide from what UI Automation reports about the focused element.
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
///
/// * `value_read_only` — `Some(IsReadOnly)` when the element supports
///   ValuePattern, `None` otherwise.
/// * `text_focus` — the element supports TextPattern and has keyboard focus.
pub(crate) fn classify_element(
    control_type: i32,
    value_read_only: Option<bool>,
    text_focus: bool,
) -> FocusTarget {
    use control::*;
    match control_type {
        EDIT => match value_read_only {
            Some(true) => FocusTarget::NotEditable,
            _ => FocusTarget::Editable,
        },
        // A browser page body is a read-only Document (its value is the URL);
        // designMode / rich editors report a writable value. Word-like
        // documents without ValuePattern stay Unknown (= paste).
        DOCUMENT | COMBO_BOX => match value_read_only {
            Some(false) => FocusTarget::Editable,
            Some(true) => FocusTarget::NotEditable,
            None => FocusTarget::Unknown,
        },
        _ if value_read_only == Some(false) => FocusTarget::Editable,
        // Terminals and custom editors: a focused text surface.
        _ if text_focus => FocusTarget::Editable,
        BUTTON | CALENDAR | CHECK_BOX | HYPERLINK | IMAGE | LIST_ITEM | LIST | MENU
        | MENU_BAR | MENU_ITEM | PROGRESS_BAR | RADIO_BUTTON | SCROLL_BAR | SLIDER
        | STATUS_BAR | TAB | TAB_ITEM | TOOL_BAR | TOOL_TIP | TREE | TREE_ITEM | THUMB
        | DATA_GRID | DATA_ITEM | SPLIT_BUTTON | HEADER | HEADER_ITEM | TABLE | TITLE_BAR => {
            FocusTarget::NotEditable
        }
        // Pane / Window / Group / Custom / Text…: custom-drawn apps often
        // report these for their editors, so don't claim "not editable".
        _ => FocusTarget::Unknown,
    }
}

/// Decide from the foreground window's class name alone, when it is enough.
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
pub(crate) fn classify_window_class(class: &str) -> Option<FocusTarget> {
    match class {
        // Desktop and taskbar.
        "Progman" | "WorkerW" | "Shell_TrayWnd" | "Shell_SecondaryTrayWnd" => {
            Some(FocusTarget::NotEditable)
        }
        // Consoles accept Ctrl+V / Shift+Insert but expose little via UIA.
        "ConsoleWindowClass" | "CASCADIA_HOSTING_WINDOW_CLASS" | "PseudoConsoleWindow" => {
            Some(FocusTarget::Unknown)
        }
        _ => None,
    }
}

#[cfg(not(target_os = "windows"))]
pub fn focused_text_target() -> FocusTarget {
    FocusTarget::Unknown
}

#[cfg(target_os = "windows")]
pub use win::focused_text_target;

#[cfg(target_os = "windows")]
mod win {
    use super::{classify_element, classify_window_class, FocusTarget};
    use std::sync::mpsc;
    use std::sync::OnceLock;
    use std::time::{Duration, Instant};

    use windows::Win32::System::Com::{
        CoCreateInstance, CoInitializeEx, CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED,
    };
    use windows::Win32::UI::Accessibility::{
        CUIAutomation, IUIAutomation, IUIAutomationValuePattern, UIA_TextPatternId,
        UIA_ValuePatternId,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        GetClassNameW, GetForegroundWindow, GetGUIThreadInfo, GetWindowThreadProcessId,
        IsIconic, GUITHREADINFO,
    };

    /// Upper bound for the UI Automation answer.
    const UIA_TIMEOUT: Duration = Duration::from_millis(40);

    struct Request {
        deadline: Instant,
        reply: mpsc::Sender<FocusTarget>,
    }

    static PROBE: OnceLock<Option<mpsc::Sender<Request>>> = OnceLock::new();

    pub fn focused_text_target() -> FocusTarget {
        let started = Instant::now();
        let result = probe();
        log::debug!(
            "Focus probe: {:?} in {:?}",
            result,
            started.elapsed()
        );
        result
    }

    fn probe() -> FocusTarget {
        unsafe {
            let foreground = GetForegroundWindow();
            if foreground.is_invalid() {
                return FocusTarget::NotEditable;
            }
            if IsIconic(foreground).as_bool() {
                return FocusTarget::NotEditable;
            }

            let mut pid = 0u32;
            let thread_id = GetWindowThreadProcessId(foreground, Some(&mut pid as *mut u32));
            if pid == std::process::id() {
                // Our own main window or overlay: never paste into ourselves.
                return FocusTarget::NotEditable;
            }

            let mut class_buf = [0u16; 256];
            let len = GetClassNameW(foreground, &mut class_buf);
            if len > 0 {
                let class = String::from_utf16_lossy(&class_buf[..len as usize]);
                if let Some(target) = classify_window_class(&class) {
                    return target;
                }
            }

            // A blinking system caret means a classic text control has focus.
            if thread_id != 0 {
                let mut info = GUITHREADINFO {
                    cbSize: std::mem::size_of::<GUITHREADINFO>() as u32,
                    ..Default::default()
                };
                if GetGUIThreadInfo(thread_id, &mut info).is_ok() && !info.hwndCaret.is_invalid()
                {
                    return FocusTarget::Editable;
                }
            }
        }

        uia_probe()
    }

    /// Ask the UIA worker, waiting at most [`UIA_TIMEOUT`].
    fn uia_probe() -> FocusTarget {
        let Some(sender) = PROBE.get_or_init(spawn_worker) else {
            return FocusTarget::Unknown;
        };
        let (reply, answer) = mpsc::channel();
        let deadline = Instant::now() + UIA_TIMEOUT;
        if sender.send(Request { deadline, reply }).is_err() {
            return FocusTarget::Unknown;
        }
        match answer.recv_timeout(UIA_TIMEOUT) {
            Ok(target) => target,
            Err(_) => {
                log::debug!("Focus probe: UI Automation timed out");
                FocusTarget::Unknown
            }
        }
    }

    fn spawn_worker() -> Option<mpsc::Sender<Request>> {
        let (tx, rx) = mpsc::channel::<Request>();
        let spawned = std::thread::Builder::new()
            .name("focus-probe".into())
            .spawn(move || worker(rx));
        match spawned {
            Ok(_) => Some(tx),
            Err(e) => {
                log::error!("Focus probe: cannot start worker thread: {e}");
                None
            }
        }
    }

    fn worker(rx: mpsc::Receiver<Request>) {
        let automation: Option<IUIAutomation> = unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            match CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER) {
                Ok(a) => Some(a),
                Err(e) => {
                    log::error!("Focus probe: UI Automation unavailable: {e}");
                    None
                }
            }
        };
        while let Ok(request) = rx.recv() {
            // The caller already gave up on this one: skip the work.
            if Instant::now() > request.deadline {
                let _ = request.reply.send(FocusTarget::Unknown);
                continue;
            }
            let target = match &automation {
                Some(automation) => classify_focused(automation),
                None => FocusTarget::Unknown,
            };
            let _ = request.reply.send(target);
        }
    }

    fn classify_focused(automation: &IUIAutomation) -> FocusTarget {
        let Ok(element) = (unsafe { automation.GetFocusedElement() }) else {
            return FocusTarget::Unknown;
        };
        unsafe {
            if let Ok(pid) = element.CurrentProcessId() {
                if pid as u32 == std::process::id() {
                    return FocusTarget::NotEditable;
                }
            }
            let Ok(control_type) = element.CurrentControlType() else {
                return FocusTarget::Unknown;
            };
            let value_read_only = element
                .GetCurrentPatternAs::<IUIAutomationValuePattern>(UIA_ValuePatternId)
                .ok()
                .and_then(|pattern| pattern.CurrentIsReadOnly().ok())
                .map(|read_only| read_only.as_bool());
            let text_focus = element.GetCurrentPattern(UIA_TextPatternId).is_ok()
                && element
                    .CurrentHasKeyboardFocus()
                    .map(|focus| focus.as_bool())
                    .unwrap_or(false);
            let target = classify_element(control_type.0, value_read_only, text_focus);
            log::debug!(
                "Focus probe UIA: control={} value_read_only={:?} text_focus={} -> {:?}",
                control_type.0,
                value_read_only,
                text_focus,
                target
            );
            target
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writable_edit_is_editable() {
        assert_eq!(
            classify_element(control::EDIT, Some(false), true),
            FocusTarget::Editable
        );
        assert_eq!(classify_element(control::EDIT, None, false), FocusTarget::Editable);
    }

    #[test]
    fn read_only_edit_is_not_editable() {
        assert_eq!(
            classify_element(control::EDIT, Some(true), false),
            FocusTarget::NotEditable
        );
    }

    #[test]
    fn browser_page_body_is_not_editable() {
        // Chrome/Edge root web area: Document, read-only value, keyboard focus.
        assert_eq!(
            classify_element(control::DOCUMENT, Some(true), true),
            FocusTarget::NotEditable
        );
    }

    #[test]
    fn document_without_value_pattern_is_unknown() {
        assert_eq!(
            classify_element(control::DOCUMENT, None, true),
            FocusTarget::Unknown
        );
    }

    #[test]
    fn list_and_buttons_are_not_editable() {
        assert_eq!(
            classify_element(control::LIST_ITEM, None, false),
            FocusTarget::NotEditable
        );
        assert_eq!(
            classify_element(control::BUTTON, Some(true), false),
            FocusTarget::NotEditable
        );
    }

    #[test]
    fn focused_text_surface_is_editable() {
        // e.g. a terminal: Text/Pane with TextPattern and keyboard focus.
        assert_eq!(
            classify_element(50033, None, true),
            FocusTarget::Editable
        );
    }

    #[test]
    fn generic_pane_is_unknown() {
        assert_eq!(classify_element(50033, None, false), FocusTarget::Unknown);
        assert!(FocusTarget::Unknown.accepts_paste());
        assert!(!FocusTarget::NotEditable.accepts_paste());
    }

    #[test]
    fn desktop_and_consoles() {
        assert_eq!(
            classify_window_class("Progman"),
            Some(FocusTarget::NotEditable)
        );
        assert_eq!(
            classify_window_class("ConsoleWindowClass"),
            Some(FocusTarget::Unknown)
        );
        assert_eq!(classify_window_class("Chrome_WidgetWin_1"), None);
    }
}
