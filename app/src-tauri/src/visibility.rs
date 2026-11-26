//! Telling the web view when nobody can see it (docs/performance.md).
//!
//! WebView2 keeps drawing, running timers and holding its full memory while the window is minimised: wry only
//! skips the resize. Marking the web view invisible stops its drawing and throttles its timers (the page's polls
//! check `document.hidden`), and a low memory target lets it trim caches. Both come back on restore.

use std::sync::atomic::{AtomicBool, Ordering};

/// Whether the main web view was last told it's hidden.
static HIDDEN: AtomicBool = AtomicBool::new(false);

/// After a window event: hide the web view while the window is minimised, show it again after.
pub fn on_event(window: &tauri::Window, event: &tauri::WindowEvent) {
    if !matches!(event, tauri::WindowEvent::Resized(_) | tauri::WindowEvent::Focused(_)) {
        return;
    }
    let hidden = window.is_minimized().unwrap_or(false);
    if HIDDEN.swap(hidden, Ordering::SeqCst) == hidden {
        return;
    }
    use tauri::Manager;
    if let Some(w) = window.app_handle().get_webview_window(window.label()) {
        set_visible(&w, !hidden);
    }
}

#[cfg(windows)]
fn set_visible(w: &tauri::WebviewWindow, visible: bool) {
    use webview2_com::Microsoft::Web::WebView2::Win32::{ICoreWebView2_19, COREWEBVIEW2_MEMORY_USAGE_TARGET_LEVEL_LOW, COREWEBVIEW2_MEMORY_USAGE_TARGET_LEVEL_NORMAL};
    use windows::core::Interface;
    let _ = w.with_webview(move |pw| {
        let controller = pw.controller();
        // SAFETY: COM calls on the web view's own controller, on the thread `with_webview` runs us on (its UI
        // thread); failures are ignored (the web view simply keeps drawing).
        unsafe {
            let _ = controller.SetIsVisible(visible);
            if let Ok(core) = controller.CoreWebView2() {
                if let Ok(c19) = core.cast::<ICoreWebView2_19>() {
                    let _ = c19.SetMemoryUsageTargetLevel(if visible { COREWEBVIEW2_MEMORY_USAGE_TARGET_LEVEL_NORMAL } else { COREWEBVIEW2_MEMORY_USAGE_TARGET_LEVEL_LOW });
                }
            }
        }
    });
}

#[cfg(not(windows))]
fn set_visible(_w: &tauri::WebviewWindow, _visible: bool) {}
