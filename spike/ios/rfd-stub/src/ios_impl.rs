//! iOS backing for the `rfd` stub: real `UIDocumentPickerViewController` import
//! flow plus sandboxed-Documents fallbacks for folder/save panels.

use std::collections::HashMap;
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::{Mutex, OnceLock};
use std::task::{Context, Poll, Waker};

use objc2::rc::Retained;
use objc2::runtime::{NSObjectProtocol, ProtocolObject};
use objc2::{define_class, msg_send, ClassType, MainThreadMarker};
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
    // Kept alive until the delegate callback runs (`setDelegate` is weak).
    _delegate: Option<Retained<PickerDelegate>>,
    _picker: Option<Retained<UIDocumentPickerViewController>>,
}

type SharedCell = std::sync::Arc<Mutex<Shared>>;

static PENDING: OnceLock<Mutex<HashMap<usize, SharedCell>>> = OnceLock::new();

fn pending() -> &'static Mutex<HashMap<usize, SharedCell>> {
    PENDING.get_or_init(|| Mutex::new(HashMap::new()))
}

define_class!(
    #[unsafe(super(NSObject))]
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
/// resolve with the sandbox copies. Must run on the main thread; `false`
/// means "could not present, behave as dismissed".
fn present_import(multiple: bool, shared: &SharedCell) -> bool {
    let Some(_mtm) = MainThreadMarker::new() else {
        return false;
    };
    let app = UIApplication::sharedApplication(_mtm);
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
    picker.setDelegate(Some(ProtocolObject::from_ref(&delegate)));
    {
        let mut s = shared.lock().unwrap();
        s._delegate = Some(delegate);
        s._picker = Some(picker.clone());
    }
    root.presentViewController_animated_completion(&picker, true, None);
    true
}

#[allow(clippy::needless_pass_by_value)]
fn register(shared: SharedCell, delegate_key: usize) {
    pending().lock().unwrap().insert(delegate_key, shared);
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
            // Pre-register under a stable key before presenting: the delegate
            // looks the future up by its own address. Register under the
            // shared cell's address instead, then hand the delegate the same
            // key via a side channel.
            let key = std::sync::Arc::as_ptr(&self.shared) as usize;
            drop(s);
            register(self.shared.clone(), key);
            if !present_import(self.multiple, &self.shared) {
                pending().lock().unwrap().remove(&key);
                let mut s = self.shared.lock().unwrap();
                s.done = Some(Vec::new());
                return Poll::Ready(Vec::new());
            }
            // Re-key to the delegate's address once it exists.
            let mut s = self.shared.lock().unwrap();
            s.waker = Some(cx.waker().clone());
            drop(s);
            rekey_to_delegate(&self.shared, key);
            return Poll::Pending;
        }
        s.waker = Some(cx.waker().clone());
        Poll::Pending
    }
}

/// Move the pending entry from the temporary cell-address key to the live
/// delegate's address (what the callbacks actually see as `self`).
fn rekey_to_delegate(shared: &SharedCell, old_key: usize) {
    let delegate_addr = {
        let s = shared.lock().unwrap();
        match &s._delegate {
            Some(d) => &**d as *const PickerDelegate as usize,
            None => return,
        }
    };
    if delegate_addr == old_key {
        return;
    }
    let mut map = pending().lock().unwrap();
    if let Some(cell) = map.remove(&old_key) {
        map.insert(delegate_addr, cell);
    }
}

pub(super) async fn pick(_files: bool, multiple: bool) -> Vec<PathBuf> {
    let shared: SharedCell = std::sync::Arc::new(Mutex::new(Shared {
        presented: false,
        done: None,
        waker: None,
        _delegate: None,
        _picker: None,
    }));
    PickFuture { shared, multiple }.await
}

#[allow(dead_code)]
fn _use_file_handle(_h: FileHandle) {}
