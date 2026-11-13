//! Open With on macOS. Finder doesn't pass the file on the command line: the
//! app is sent `application:openFiles:`, at launch (before it has finished
//! launching) or while it's running. winit owns the application delegate and
//! doesn't forward that, so the method is added to `NSObject`, which every
//! delegate inherits from; only `NSApplication` ever sends it, to its delegate.
//! It queues each path for the app to pick up.

use std::ffi::c_void;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

use objc2::runtime::Sel;
use objc2::{class, sel};
use objc2_foundation::{NSArray, NSString};

static OPENED: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());
static WAKE: OnceLock<egui::Context> = OnceLock::new();

extern "C" fn application_open_files(
    _this: *mut c_void,
    _cmd: Sel,
    _app: *mut c_void,
    files: &NSArray<NSString>,
) {
    if let Ok(mut opened) = OPENED.lock() {
        opened.extend(files.iter().map(|f| PathBuf::from(f.to_string())));
    }
    if let Some(ctx) = WAKE.get() {
        ctx.request_repaint();
    }
}

/// Starts listening for Open With. Call before the event loop starts, or the
/// file Finder sends a launch is missed.
pub fn install() {
    type Imp = extern "C" fn(*mut c_void, Sel, *mut c_void, &NSArray<NSString>);
    // SAFETY: the method's signature is `-(void)application:(id)app openFiles:(NSArray *)files`.
    unsafe {
        objc2::ffi::class_addMethod(
            class!(NSObject) as *const _ as *mut _,
            sel!(application:openFiles:),
            std::mem::transmute::<Imp, unsafe extern "C-unwind" fn()>(application_open_files),
            c"v@:@@".as_ptr(),
        );
    }
}

/// Lets a file opened while running wake the window.
pub fn set_window(ctx: &egui::Context) {
    let _ = WAKE.set(ctx.clone());
}

/// The files opened since last asked.
pub fn take() -> Vec<PathBuf> {
    OPENED.lock().map(|mut o| std::mem::take(&mut *o)).unwrap_or_default()
}
