use std::ffi::{c_char, c_void};
use std::ptr;

use color_eyre::eyre::{Result, eyre};

type CfTypeRef = *const c_void;
type CfStringRef = *const c_void;
type AxUiElementRef = *const c_void;

const CF_STRING_ENCODING_UTF8: u32 = 0x0800_0100;
/// `kAXValueTypeCFRange`.
const AX_VALUE_CF_RANGE: u32 = 4;
/// `kAXValueTypeCGRect`.
const AX_VALUE_CG_RECT: u32 = 3;

/// A `CGRect`: screen points with the origin at the top left of the main display.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ScreenRect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct CfRange {
    location: isize,
    length: isize,
}

#[link(name = "ApplicationServices", kind = "framework")]
unsafe extern "C" {
    fn AXUIElementCreateApplication(pid: i32) -> AxUiElementRef;
    fn AXUIElementCopyAttributeValue(
        element: AxUiElementRef,
        attribute: CfStringRef,
        value: *mut CfTypeRef,
    ) -> i32;
    fn AXUIElementSetMessagingTimeout(element: AxUiElementRef, timeout_in_seconds: f32) -> i32;
    fn AXUIElementCreateSystemWide() -> AxUiElementRef;
    fn AXUIElementCopyParameterizedAttributeValue(
        element: AxUiElementRef,
        parameterized_attribute: CfStringRef,
        parameter: CfTypeRef,
        result: *mut CfTypeRef,
    ) -> i32;
    fn AXValueCreate(value_type: u32, value: *const c_void) -> CfTypeRef;
    fn AXValueGetValue(value: CfTypeRef, value_type: u32, value: *mut c_void) -> bool;
    fn AXUIElementGetPid(element: AxUiElementRef, pid: *mut i32) -> i32;
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFGetTypeID(value: CfTypeRef) -> usize;
    fn CFStringGetTypeID() -> usize;
    fn CFStringGetLength(value: CfStringRef) -> isize;
    fn CFStringGetMaximumSizeForEncoding(length: isize, encoding: u32) -> isize;
    fn CFStringGetCString(
        value: CfStringRef,
        buffer: *mut c_char,
        buffer_size: isize,
        encoding: u32,
    ) -> bool;
    fn CFStringCreateWithCString(
        allocator: *const c_void,
        string: *const c_char,
        encoding: u32,
    ) -> CfStringRef;
    fn CFRelease(value: CfTypeRef);
    fn CFRetain(value: CfTypeRef) -> CfTypeRef;
    fn CFArrayGetTypeID() -> usize;
    fn CFArrayGetCount(array: CfTypeRef) -> isize;
    fn CFArrayGetValueAtIndex(array: CfTypeRef, index: isize) -> CfTypeRef;
    fn CFHash(value: CfTypeRef) -> usize;
}

/// The focused text field as Hello Type sees it: who owns it, the text before the caret,
/// and where the caret is on screen.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct FocusedText {
    pub pid: i32,
    /// Identifies the field, so a change of focus is noticed.
    pub element: usize,
    pub text_before_caret: String,
    /// A password field: never read for learning, never suggested into.
    pub secure: bool,
    /// Where the caret is, when the app says.
    pub caret: Option<ScreenRect>,
}

/// The focused text field, with up to `limit` characters before the caret. `None` when no
/// text field has focus or the app does not expose its text.
pub fn focused_text(limit: usize) -> Option<FocusedText> {
    capture_focused_text(limit).ok()
}

/// Up to `limit` characters (UTF-16 units) right before the caret in the focused text
/// field, read locally and never stored. An empty string means the caret is at the
/// start. `None` when the app doesn't expose its text (secure fields, some web views).
pub fn text_before_caret(limit: usize) -> Option<String> {
    capture_text_before_caret(limit).ok()
}

pub fn focused_window_title(pid: i32) -> Option<String> {
    capture_focused_window_title(pid)
        .ok()
        .filter(|title| !title.is_empty())
}

/// One top-level window of an application, with the labels of its toolbar
/// controls (where browsers show the signed-in profile).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct WindowLabels {
    pub title: String,
    pub controls: Vec<String>,
}

/// Titles of every window the application exposes, front-most first.
pub fn window_titles(pid: i32) -> Vec<String> {
    application_windows(pid)
        .unwrap_or_default()
        .iter()
        .filter_map(|window| optional_string(window, c"AXTitle"))
        .filter(|title| !title.is_empty())
        .collect()
}

/// Window titles plus the button labels inside each window, read to a bounded
/// depth so a large web page cannot stall the caller.
pub fn window_labels(pid: i32) -> Vec<WindowLabels> {
    const MAX_NODES: usize = 600;
    const MAX_DEPTH: usize = 9;
    let Ok(windows) = application_windows(pid) else {
        return Vec::new();
    };
    windows
        .iter()
        .map(|window| {
            let title = optional_string(window, c"AXTitle").unwrap_or_default();
            let mut controls = Vec::new();
            let mut queue: std::collections::VecDeque<(CfObject, usize)> = children(window)
                .into_iter()
                .map(|child| (child, 1))
                .collect();
            let mut visited = 0;
            while let Some((element, depth)) = queue.pop_front() {
                visited += 1;
                if visited > MAX_NODES {
                    break;
                }
                let role = optional_string(&element, c"AXRole").unwrap_or_default();
                // Web content is skipped: profile and account controls live in
                // the browser's own toolbar.
                if role == "AXWebArea" {
                    continue;
                }
                if matches!(role.as_str(), "AXButton" | "AXMenuButton" | "AXPopUpButton") {
                    for attribute in [c"AXTitle", c"AXDescription", c"AXHelp"] {
                        if let Some(label) = optional_string(&element, attribute)
                            && !label.is_empty()
                            && label.len() <= 160
                        {
                            controls.push(label);
                        }
                    }
                }
                if depth < MAX_DEPTH {
                    queue.extend(
                        children(&element)
                            .into_iter()
                            .map(|child| (child, depth + 1)),
                    );
                }
            }
            WindowLabels { title, controls }
        })
        .collect()
}

fn application_windows(pid: i32) -> Result<Vec<CfObject>> {
    // SAFETY: The create/copy APIs return retained Core Foundation objects.
    let application = bounded_element(
        unsafe { AXUIElementCreateApplication(pid) },
        "could not inspect the application",
        "could not bound communication with the application",
    )?;
    let windows = copy_attribute(&application, c"AXWindows", "the application has no windows")?;
    Ok(array_elements(&windows))
}

fn children(element: &CfObject) -> Vec<CfObject> {
    copy_attribute(element, c"AXChildren", "no children")
        .map(|children| array_elements(&children))
        .unwrap_or_default()
}

fn array_elements(array: &CfObject) -> Vec<CfObject> {
    if unsafe { CFGetTypeID(array.0) } != unsafe { CFArrayGetTypeID() } {
        return Vec::new();
    }
    let count = unsafe { CFArrayGetCount(array.0) }.max(0);
    (0..count)
        .filter_map(|index| {
            let value = unsafe { CFArrayGetValueAtIndex(array.0, index) };
            if value.is_null() {
                return None;
            }
            // SAFETY: Values borrowed from the array are retained before use.
            let element = CfObject(unsafe { CFRetain(value) });
            unsafe { AXUIElementSetMessagingTimeout(element.0, 0.25) };
            Some(element)
        })
        .collect()
}

fn optional_string(element: &CfObject, attribute: &std::ffi::CStr) -> Option<String> {
    copy_attribute(element, attribute, "missing")
        .ok()
        .and_then(|value| cf_string(value.0).ok())
}

struct CfObject(CfTypeRef);

impl Drop for CfObject {
    fn drop(&mut self) {
        unsafe { CFRelease(self.0) };
    }
}

fn bounded_element(
    element: AxUiElementRef,
    null_error: &'static str,
    timeout_error: &'static str,
) -> Result<CfObject> {
    if element.is_null() {
        return Err(eyre!(null_error));
    }
    let element = CfObject(element);
    if unsafe { AXUIElementSetMessagingTimeout(element.0, 0.25) } != 0 {
        return Err(eyre!(timeout_error));
    }
    Ok(element)
}

fn copy_attribute(
    element: &CfObject,
    name: &std::ffi::CStr,
    missing_error: &'static str,
) -> Result<CfObject> {
    let attribute = CfObject(cf_string_literal(name)?);
    let mut value = ptr::null();
    let status = unsafe { AXUIElementCopyAttributeValue(element.0, attribute.0, &mut value) };
    if status != 0 || value.is_null() {
        return Err(eyre!(missing_error));
    }
    Ok(CfObject(value))
}

fn capture_focused_window_title(pid: i32) -> Result<String> {
    // SAFETY: The create/copy APIs return retained Core Foundation objects.
    let application = bounded_element(
        unsafe { AXUIElementCreateApplication(pid) },
        "could not inspect the foreground application",
        "could not bound communication with the foreground application",
    )?;
    let window = copy_attribute(
        &application,
        c"AXFocusedWindow",
        "the foreground application has no focused window",
    )?;
    if unsafe { AXUIElementSetMessagingTimeout(window.0, 0.25) } != 0 {
        return Err(eyre!(
            "could not bound communication with the foreground window"
        ));
    }
    let title = copy_attribute(
        &window,
        c"AXTitle",
        "the foreground window does not expose a title",
    )?;
    cf_string(title.0)
}

fn capture_text_before_caret(limit: usize) -> Result<String> {
    // SAFETY: The create/copy APIs return retained Core Foundation objects.
    let system = bounded_element(
        unsafe { AXUIElementCreateSystemWide() },
        "could not inspect the focused application",
        "could not bound communication with the focused application",
    )?;
    let focused = copy_attribute(
        &system,
        c"AXFocusedUIElement",
        "no element has keyboard focus",
    )?;
    if unsafe { AXUIElementSetMessagingTimeout(focused.0, 0.25) } != 0 {
        return Err(eyre!(
            "could not bound communication with the focused element"
        ));
    }
    let selection = copy_attribute(
        &focused,
        c"AXSelectedTextRange",
        "the focused element has no text selection",
    )?;
    let mut caret = CfRange::default();
    if !unsafe { AXValueGetValue(selection.0, AX_VALUE_CF_RANGE, (&raw mut caret).cast()) } {
        return Err(eyre!("the text selection has an unexpected type"));
    }
    if caret.location <= 0 {
        return Ok(String::new());
    }
    let start = caret.location.saturating_sub(limit as isize).max(0);
    let range = CfRange {
        location: start,
        length: caret.location - start,
    };
    let parameter = unsafe { AXValueCreate(AX_VALUE_CF_RANGE, (&raw const range).cast()) };
    if parameter.is_null() {
        return Err(eyre!("could not describe the text range"));
    }
    let parameter = CfObject(parameter);
    let attribute = CfObject(cf_string_literal(c"AXStringForRange")?);
    let mut value = ptr::null();
    let status = unsafe {
        AXUIElementCopyParameterizedAttributeValue(focused.0, attribute.0, parameter.0, &mut value)
    };
    if status != 0 || value.is_null() {
        return Err(eyre!("the focused element does not expose its text"));
    }
    let value = CfObject(value);
    cf_string(value.0)
}

fn capture_focused_text(limit: usize) -> Result<FocusedText> {
    // SAFETY: The create/copy APIs return retained Core Foundation objects.
    let system = bounded_element(
        unsafe { AXUIElementCreateSystemWide() },
        "could not inspect the focused application",
        "could not bound communication with the focused application",
    )?;
    let focused = copy_attribute(
        &system,
        c"AXFocusedUIElement",
        "no element has keyboard focus",
    )?;
    if unsafe { AXUIElementSetMessagingTimeout(focused.0, 0.25) } != 0 {
        return Err(eyre!(
            "could not bound communication with the focused element"
        ));
    }
    let mut pid = 0;
    unsafe { AXUIElementGetPid(focused.0, &mut pid) };
    let element = unsafe { CFHash(focused.0) };
    // Native password fields have the secure role; web pages put it in the subrole.
    let role = optional_string(&focused, c"AXRole").unwrap_or_default();
    let subrole = optional_string(&focused, c"AXSubrole").unwrap_or_default();
    let secure = role == "AXSecureTextField" || subrole == "AXSecureTextField";
    if secure {
        return Ok(FocusedText {
            pid,
            element,
            secure,
            ..Default::default()
        });
    }
    let selection = copy_attribute(
        &focused,
        c"AXSelectedTextRange",
        "the focused element has no text selection",
    )?;
    let mut caret_range = CfRange::default();
    if !unsafe {
        AXValueGetValue(
            selection.0,
            AX_VALUE_CF_RANGE,
            (&raw mut caret_range).cast(),
        )
    } {
        return Err(eyre!("the text selection has an unexpected type"));
    }
    let text_before_caret = if caret_range.location <= 0 {
        String::new()
    } else {
        let start = caret_range.location.saturating_sub(limit as isize).max(0);
        string_for_range(
            &focused,
            CfRange {
                location: start,
                length: caret_range.location - start,
            },
        )?
    };
    // A zero-length range at the caret gives its bounds in most apps; others only answer
    // for a character, so fall back to the right edge of the one before the caret.
    let caret = bounds_for_range(
        &focused,
        CfRange {
            location: caret_range.location,
            length: 0,
        },
    )
    .filter(|rect| rect.height > 0.0)
    .or_else(|| {
        (caret_range.location > 0)
            .then(|| {
                bounds_for_range(
                    &focused,
                    CfRange {
                        location: caret_range.location - 1,
                        length: 1,
                    },
                )
                .filter(|rect| rect.height > 0.0)
                .map(|rect| ScreenRect {
                    x: rect.x + rect.width,
                    width: 0.0,
                    ..rect
                })
            })
            .flatten()
    });
    Ok(FocusedText {
        pid,
        element,
        text_before_caret,
        secure,
        caret,
    })
}

fn string_for_range(element: &CfObject, range: CfRange) -> Result<String> {
    let value = parameterized_value(element, c"AXStringForRange", range)?;
    cf_string(value.0)
}

fn bounds_for_range(element: &CfObject, range: CfRange) -> Option<ScreenRect> {
    let value = parameterized_value(element, c"AXBoundsForRange", range).ok()?;
    let mut rect = ScreenRect::default();
    unsafe { AXValueGetValue(value.0, AX_VALUE_CG_RECT, (&raw mut rect).cast()) }.then_some(rect)
}

fn parameterized_value(
    element: &CfObject,
    attribute: &std::ffi::CStr,
    range: CfRange,
) -> Result<CfObject> {
    let parameter = unsafe { AXValueCreate(AX_VALUE_CF_RANGE, (&raw const range).cast()) };
    if parameter.is_null() {
        return Err(eyre!("could not describe the text range"));
    }
    let parameter = CfObject(parameter);
    let attribute = CfObject(cf_string_literal(attribute)?);
    let mut value = ptr::null();
    let status = unsafe {
        AXUIElementCopyParameterizedAttributeValue(element.0, attribute.0, parameter.0, &mut value)
    };
    if status != 0 || value.is_null() {
        return Err(eyre!("the focused element does not answer for the range"));
    }
    Ok(CfObject(value))
}

fn cf_string_literal(value: &std::ffi::CStr) -> Result<CfStringRef> {
    let string =
        unsafe { CFStringCreateWithCString(ptr::null(), value.as_ptr(), CF_STRING_ENCODING_UTF8) };
    (!string.is_null())
        .then_some(string)
        .ok_or_else(|| eyre!("could not create an Accessibility attribute name"))
}

fn cf_string(value: CfTypeRef) -> Result<String> {
    if unsafe { CFGetTypeID(value) } != unsafe { CFStringGetTypeID() } {
        return Err(eyre!("the accessibility value has an unsupported type"));
    }
    let value = value.cast();
    let length = unsafe { CFStringGetLength(value) };
    let capacity = unsafe { CFStringGetMaximumSizeForEncoding(length, CF_STRING_ENCODING_UTF8) }
        .saturating_add(1);
    let mut bytes = vec![0_u8; capacity as usize];
    if !unsafe {
        CFStringGetCString(
            value,
            bytes.as_mut_ptr().cast(),
            capacity,
            CF_STRING_ENCODING_UTF8,
        )
    } {
        return Err(eyre!("could not decode the accessibility text"));
    }
    let length = bytes
        .iter()
        .position(|byte| *byte == 0)
        .unwrap_or(bytes.len());
    bytes.truncate(length);
    String::from_utf8(bytes).map_err(Into::into)
}
