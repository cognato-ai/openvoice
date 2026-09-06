// ax.rs — Accessibility (AX) tree reader for on-screen context grounding.
//
// macOS exposes a *semantic* tree of every on-screen UI element via the
// Accessibility API (the same one screen readers use). Given the Accessibility
// permission the app already holds (for pasting), we can read the frontmost
// app's focused field, selected text, open-document path, and — walking the
// tree to a user-chosen depth — the labels and text of visible elements (tab
// names, headings, buttons). This grounds the enhancement model so it spells
// names/files the way they actually appear.
//
// Coverage note: AX reads what an app chooses to publish. Browsers, Mail, and
// Electron apps (VS Code, Slack) expose rich trees; GPU-custom-rendered apps
// (Zed, some terminals) publish almost nothing — no non-OCR method can read
// what they don't expose. The walk is bounded by depth + node + text caps so a
// huge browser tree can't blow up latency or the prompt.

#![cfg(target_os = "macos")]

use std::collections::HashSet;
use std::os::raw::c_void;

use core_foundation::array::CFArrayRef;
use core_foundation::base::{CFTypeRef, TCFType};
use core_foundation::string::{CFString, CFStringRef};
use core_foundation_sys::array::{CFArrayGetCount, CFArrayGetTypeID, CFArrayGetValueAtIndex};
use core_foundation_sys::base::{CFGetTypeID, CFRelease};
use core_foundation_sys::string::CFStringGetTypeID;

type AxUiElementRef = *const c_void;
type AxError = i32;

const K_AX_SUCCESS: AxError = 0;

#[link(name = "ApplicationServices", kind = "framework")]
extern "C" {
    fn AXUIElementCreateApplication(pid: i32) -> AxUiElementRef;
    fn AXUIElementCopyAttributeValue(
        element: AxUiElementRef,
        attribute: CFStringRef,
        value: *mut CFTypeRef,
    ) -> AxError;
}

/// Per-stage limits controlling how much of the tree we read. `max_depth == 0`
/// means "cheap signals only" — no recursive walk.
#[derive(Clone, Copy, Debug)]
pub struct Budget {
    pub max_depth: usize,
    pub node_cap: usize,
    pub text_cap: usize,
}

/// Maps the depth setting to a budget. Kept here so the whole policy lives with
/// the code that spends it.
pub fn budget_for(depth: &str) -> Budget {
    match depth {
        "med" => Budget { max_depth: 3, node_cap: 40, text_cap: 800 },
        "high" => Budget { max_depth: 6, node_cap: 120, text_cap: 2000 },
        "xhigh" => Budget { max_depth: 10, node_cap: 400, text_cap: 6000 },
        "ultra" => Budget { max_depth: 24, node_cap: 1500, text_cap: 16000 },
        // "low" and anything unknown: cheap signals, no walk.
        _ => Budget { max_depth: 0, node_cap: 0, text_cap: 500 },
    }
}

/// What we pulled from the frontmost app.
#[derive(Default, Debug)]
pub struct AxRead {
    /// Title of the focused window.
    pub window_title: Option<String>,
    /// Open document's path/URL, if the app is document-based (`AXDocument`).
    pub document: Option<String>,
    /// Text of the element the cursor is in (capped).
    pub focused_value: Option<String>,
    /// Currently-selected text (capped).
    pub selected_text: Option<String>,
    /// Deduped labels/text collected by the tree walk (tab names, headings…).
    pub labels: Vec<String>,
}

// ── CF helpers ───────────────────────────────────────────────────────────────

/// Copies an AX attribute as a raw, +1-retained CFTypeRef (caller releases).
unsafe fn copy_attr(elem: AxUiElementRef, attr: &str) -> Option<CFTypeRef> {
    let key = CFString::new(attr);
    let mut value: CFTypeRef = std::ptr::null();
    let err = AXUIElementCopyAttributeValue(elem, key.as_concrete_TypeRef(), &mut value);
    if err == K_AX_SUCCESS && !value.is_null() {
        Some(value)
    } else {
        None
    }
}

/// Reads a string attribute (returns None if the attribute isn't a CFString).
unsafe fn attr_string(elem: AxUiElementRef, attr: &str) -> Option<String> {
    let v = copy_attr(elem, attr)?;
    let out = if CFGetTypeID(v as *const _) == CFStringGetTypeID() {
        Some(CFString::wrap_under_get_rule(v as CFStringRef).to_string())
    } else {
        None
    };
    CFRelease(v as *const _);
    out
}

/// Reads a single AXUIElement-valued attribute (e.g. AXFocusedWindow). Returns a
/// +1-retained element ref the caller must release, or None.
unsafe fn attr_element(elem: AxUiElementRef, attr: &str) -> Option<AxUiElementRef> {
    // AX element refs are CFTypes; we just carry the pointer and release later.
    copy_attr(elem, attr).map(|v| v as AxUiElementRef)
}

fn clip(s: &str, max: usize) -> String {
    let t = s.trim();
    if t.chars().count() <= max {
        t.to_string()
    } else {
        t.chars().take(max).collect::<String>() + "…"
    }
}

// ── Tree walk ────────────────────────────────────────────────────────────────

struct Walker {
    budget: Budget,
    nodes: usize,
    text_len: usize,
    seen: HashSet<String>,
    labels: Vec<String>,
}

impl Walker {
    fn done(&self) -> bool {
        self.nodes >= self.budget.node_cap || self.text_len >= self.budget.text_cap
    }

    fn push(&mut self, s: String) {
        let s = clip(&s, 240);
        if s.is_empty() || s.chars().count() < 2 {
            return;
        }
        if self.seen.insert(s.to_lowercase()) {
            self.text_len += s.len();
            self.labels.push(s);
        }
    }

    /// Depth-first walk collecting labels/values. `elem` is borrowed (owned by
    /// its parent's children array), so we never release it here.
    unsafe fn walk(&mut self, elem: AxUiElementRef, depth: usize) {
        if depth > self.budget.max_depth || self.done() {
            return;
        }
        self.nodes += 1;

        let role = attr_string(elem, "AXRole").unwrap_or_default();

        // A title/label is the high-signal bit (tab names, buttons, headings).
        if let Some(title) = attr_string(elem, "AXTitle") {
            self.push(title);
        }
        // Text-bearing roles: capture their value too.
        let text_role = matches!(
            role.as_str(),
            "AXStaticText"
                | "AXTextField"
                | "AXTextArea"
                | "AXComboBox"
                | "AXLink"
                | "AXHeading"
        );
        if text_role {
            if let Some(val) = attr_string(elem, "AXValue") {
                self.push(val);
            }
        }
        if self.done() {
            return;
        }

        // Recurse into children (a CFArray of AXUIElement refs).
        if let Some(children) = copy_attr(elem, "AXChildren") {
            if CFGetTypeID(children as *const _) == CFArrayGetTypeID() {
                let arr = children as CFArrayRef;
                let count = CFArrayGetCount(arr);
                for i in 0..count {
                    if self.done() {
                        break;
                    }
                    let child = CFArrayGetValueAtIndex(arr, i) as AxUiElementRef;
                    if !child.is_null() {
                        self.walk(child, depth + 1);
                    }
                }
            }
            CFRelease(children as *const _);
        }
    }
}

/// Reads the frontmost app's accessibility context at the given budget. Safe to
/// call from any thread (AX API is not AppKit-main-thread-bound). Returns an
/// empty read on any failure (permission denied, app not cooperative).
pub fn read(pid: i32, budget: Budget) -> AxRead {
    let mut out = AxRead::default();
    if pid <= 0 {
        return out;
    }

    unsafe {
        let app = AXUIElementCreateApplication(pid);
        if app.is_null() {
            return out;
        }

        // Focused window: title + open-document path.
        if let Some(win) = attr_element(app, "AXFocusedWindow") {
            out.window_title = attr_string(win, "AXTitle").filter(|s| !s.trim().is_empty());
            out.document = attr_string(win, "AXDocument").filter(|s| !s.trim().is_empty());

            // Deep walk starts from the focused window (skips menu bar / other
            // windows), if the budget allows one.
            if budget.max_depth > 0 {
                let mut w = Walker {
                    budget,
                    nodes: 0,
                    text_len: 0,
                    seen: HashSet::new(),
                    labels: Vec::new(),
                };
                w.walk(win, 0);
                out.labels = w.labels;
            }
            CFRelease(win as *const _);
        }

        // Focused element: the field the cursor is in + any selection.
        if let Some(focused) = attr_element(app, "AXFocusedUIElement") {
            out.focused_value = attr_string(focused, "AXValue")
                .map(|s| clip(&s, 600))
                .filter(|s| !s.is_empty());
            out.selected_text = attr_string(focused, "AXSelectedText")
                .map(|s| clip(&s, 600))
                .filter(|s| !s.is_empty());
            CFRelease(focused as *const _);
        }

        CFRelease(app as *const _);
    }

    out
}
