//! iOS backing for the `rfd` stub: real `UIDocumentPickerViewController` import
//! flow plus sandboxed-Documents fallbacks for folder/save panels.
//!
//! The delegate class carries no state: pending picks live in a global map
//! keyed by delegate address (plain `usize`, so the map stays `Send`), and
//! the delegate itself is retained manually with `into_raw` / `from_raw`
//! because `setDelegate` is weak.

use std::collections::HashMap;
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::{Mutex, OnceLock};
use std::task::{Context, Poll, Waker};

use objc2::rc::Retained;
use objc2::runtime::{NSObjectProtocol, ProtocolObject};
use objc2::{define_class, msg_send, ClassType, MainThreadMarker, MainThreadOnly};
use objc2_foundation::{NSArray, NSMutableArray, NSObject, NSString, NSURL};
use objc2_ui_kit::{
    UIApplication, UIDocumentPickerDelegate, UIDocumentPickerMode,
    UIDocumentPickerViewController, UIViewController,
};

use super::FileHandle;

pub(super) static SAVE_NAME: Mutex<Option<String>> = Mutex::new(None);

pub(super) fn take_name() -> Option<String> {
    SAVE_NAME.lock().unwrap().take()
}

/// Uniquified file name inside `dir` so saves never silently overwrite.
pub(super) fn uniquify(dir: &std::path::Path, name: &str) -> String {
    if !dir.join(name).exists() {
        return name.to_string();
    }
    let (stem, ext) = match name.rfind('.') {
        Some(i) if i > 0 => (&name[..i], &name[i..]),
        _ => (name, ""),
    };
    for n in 2..1000 {
        let candidate = format!("{stem} ({n}){ext}");
        if !dir.join(&candidate).exists() {
            return candidate;
        }
    }
    name.to_string()
}

/// App sandbox Documents directory (visible in Files with
/// `UIFileSharingEnabled`). Created on demand.
pub(super) fn sandbox_fallback_dir() -> Option<PathBuf> {
    let home = std::env::var("HOME").ok()?;
    let docs = PathBuf::from(home).join("Documents");
    std::fs::create_dir_all(&docs).ok()?;
    Some(docs)
}

struct Shared {
    presented: bool,
    done: Option<Vec<PathBuf>>,
    waker: Option<Waker>,
    /// Manually-retained delegate (`setDelegate` is weak). Released in the
    /// callback via `from_raw`; a dropped-without-callback future leaks one
    /// tiny object, which is harmless.
    delegate_raw: usize,
}

type SharedCell = std::sync::Arc<Mutex<Shared>>;

static PENDING: OnceLock<Mutex<HashMap<usize, SharedCell>>> = OnceLock::new();

fn pending() -> &'static Mutex<HashMap<usize, SharedCell>> {
    PENDING.get_or_init(|| Mutex::new(HashMap::new()))
}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "PdfCraftPickerDelegate"]
    struct PickerDelegate;

    unsafe impl NSObjectProtocol for PickerDelegate {}

    unsafe impl UIDocumentPickerDelegate for PickerDelegate {
        #[unsafe(method(documentPicker:didPickDocumentsAtURLs:))]
        fn documentPicker_didPickDocumentsAtURLs(
            &self,
            _controller: &UIDocumentPickerViewController,
            urls: &NSArray<NSURL>,
        ) {
            let key = self as *const Self as usize;
            if let Some(shared) = pending().lock().unwrap().remove(&key) {
                let mut paths = Vec::new();
                for i in 0..urls.len() {
                    let url = urls.objectAtIndex(i);
                    if let Some(ns) = url.path() {
                        let p = PathBuf::from(ns.to_string());
                        if p.exists() {
                            paths.push(p);
                        }
                    }
                }
                // Balance the manual retain from presentation.
                unsafe { drop(Retained::<PickerDelegate>::from_raw(key as *mut PickerDelegate)) };
                let mut s = shared.lock().unwrap();
                s.done = Some(paths);
                if let Some(w) = s.waker.take() {
                    w.wake();
                }
            }
        }

        #[unsafe(method(documentPickerWasCancelled:))]
        fn documentPickerWasCancelled(&self, _controller: &UIDocumentPickerViewController) {
            let key = self as *const Self as usize;
            if let Some(shared) = pending().lock().unwrap().remove(&key) {
                unsafe { drop(Retained::<PickerDelegate>::from_raw(key as *mut PickerDelegate)) };
                let mut s = shared.lock().unwrap();
                s.done = Some(Vec::new());
                if let Some(w) = s.waker.take() {
                    w.wake();
                }
            }
        }
    }
);

/// Present an Import-mode picker (`public.item`: everything importable) and
/// arrange for the delegate callback to complete `shared`. Returns false when
/// not on the main thread or when UIKit has no window (caller then behaves
/// as dismissed, matching the old stub).
#[allow(deprecated)]
fn present_import(multiple: bool, shared: &SharedCell) -> bool {
    let Some(mtm) = MainThreadMarker::new() else {
        return false;
    };
    let app = UIApplication::sharedApplication(mtm);
    let window = match app.keyWindow() {
        Some(w) => w,
        None => return false,
    };
    let root: Retained<UIViewController> = match window.rootViewController() {
        Some(r) => r,
        None => return false,
    };
    let types = NSMutableArray::<NSString>::new();
    types.addObject(&NSString::from_str("public.item"));
    let picker = UIDocumentPickerViewController::initWithDocumentTypes_inMode(
        UIDocumentPickerViewController::alloc(),
        &types,
        UIDocumentPickerMode::Import,
    );
    picker.setAllowsMultipleSelection(multiple);
    let delegate: Retained<PickerDelegate> = unsafe { msg_send![PickerDelegate::class(), new] };
    picker.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
    let raw = Retained::into_raw(delegate) as usize;
    {
        let mut map = pending().lock().unwrap();
        map.insert(raw, shared.clone());
        let mut s = shared.lock().unwrap();
        s.delegate_raw = raw;
    }
    root.presentViewController_animated_completion(&picker, true, None);
    true
}

struct PickFuture {
    shared: SharedCell,
    multiple: bool,
}

impl Future for PickFuture {
    type Output = Vec<PathBuf>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Vec<PathBuf>> {
        let mut s = self.shared.lock().unwrap();
        if let Some(done) = s.done.take() {
            return Poll::Ready(done);
        }
        if !s.presented {
            s.presented = true;
            drop(s);
            if !present_import(self.multiple, &self.shared) {
                let mut s = self.shared.lock().unwrap();
                s.done = Some(Vec::new());
                return Poll::Ready(Vec::new());
            }
            let mut s = self.shared.lock().unwrap();
            s.waker = Some(cx.waker().clone());
            return Poll::Pending;
        }
        s.waker = Some(cx.waker().clone());
        Poll::Pending
    }
}

pub(super) async fn pick(multiple: bool) -> Vec<PathBuf> {
    let shared: SharedCell = std::sync::Arc::new(Mutex::new(Shared {
        presented: false,
        done: None,
        waker: None,
        delegate_raw: 0,
    }));
    PickFuture { shared, multiple }.await
}
