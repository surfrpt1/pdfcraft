//! iOS backing for the `rfd` stub: real `UIDocumentPickerViewController` import
//! flow.
//!
//! Presentation happens eagerly in [`pick_now`] (called on the UI thread,
//! like real `rfd`); the returned future only waits for the delegate
//! callback on whatever thread polls it. The delegate class carries no state:
//! pending picks live in a global map keyed by delegate address (plain
//! `usize`, so the map stays `Send`), and the delegate itself is retained
//! manually with `into_raw` / `from_raw` because `setDelegate` is weak.

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
    UIDocumentPickerViewController, UIViewController, UIWindow,
};

struct Shared {
    done: Option<Vec<PathBuf>>,
    waker: Option<Waker>,
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

/// Present an Import-mode picker (`public.item`: everything importable).
/// Runs on the calling (UI) thread. On failure returns a reason slug which
/// becomes a fake path, so the app surfaces it in its own "Couldn't read …"
/// toast instead of failing silently (remote-debugging aid for sideloaded
/// builds where we cannot see logs).
#[allow(deprecated)]
fn present_import(multiple: bool, shared: &SharedCell) -> Result<(), &'static str> {
    let Some(mtm) = MainThreadMarker::new() else {
        return Err("no-main-thread");
    };
    let app = UIApplication::sharedApplication(mtm);
    // `keyWindow` is scene-deprecated and often nil (e.g. under app hosts);
    // fall back to any key window, then any window at all.
    let window: Retained<UIWindow> = match app.keyWindow() {
        Some(w) => w,
        None => {
            let windows = app.windows();
            let mut found: Option<Retained<UIWindow>> = None;
            for i in 0..windows.len() {
                let w = windows.objectAtIndex(i);
                if w.isKeyWindow() {
                    found = Some(w);
                    break;
                }
                if found.is_none() {
                    found = Some(w);
                }
            }
            match found {
                Some(w) => w,
                None => return Err("no-window"),
            }
        }
    };
    let root: Retained<UIViewController> = match window.rootViewController() {
        Some(r) => r,
        None => return Err("no-root-vc"),
    };
    let types = NSMutableArray::<NSString>::new();
    types.addObject(&NSString::from_str("public.item"));
    let picker = UIDocumentPickerViewController::initWithDocumentTypes_inMode(
        UIDocumentPickerViewController::alloc(mtm),
        &types,
        UIDocumentPickerMode::Import,
    );
    picker.setAllowsMultipleSelection(multiple);
    let delegate: Retained<PickerDelegate> = unsafe { msg_send![PickerDelegate::class(), new] };
    picker.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
    let raw = Retained::into_raw(delegate) as usize;
    pending().lock().unwrap().insert(raw, shared.clone());
    root.presentViewController_animated_completion(&picker, true, None);
    Ok(())
}

pub(super) struct PickFuture {
    shared: Option<SharedCell>,
}

impl Future for PickFuture {
    type Output = Vec<PathBuf>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Vec<PathBuf>> {
        let this = self.as_mut().get_mut();
        match this.shared.take() {
            // Eager (already-resolved) path.
            None => Poll::Pending,
            Some(shared) => {
                let mut s = shared.lock().unwrap();
                if let Some(done) = s.done.take() {
                    return Poll::Ready(done);
                }
                s.waker = Some(cx.waker().clone());
                drop(s);
                this.shared = Some(shared);
                Poll::Pending
            }
        }
    }
}

impl PickFuture {
    fn ready(paths: Vec<PathBuf>) -> Self {
        let shared = std::sync::Arc::new(Mutex::new(Shared {
            done: Some(paths),
            waker: None,
        }));
        Self {
            shared: Some(shared),
        }
    }
}

/// Called synchronously on the UI thread. Presents the picker now and hands
/// back a future the worker thread will drive to completion. A presentation
/// failure resolves to a sentinel path so the app toasts the reason instead
/// of failing silently.
pub(super) fn pick_now(multiple: bool) -> PickFuture {
    let shared: SharedCell = std::sync::Arc::new(Mutex::new(Shared {
        done: None,
        waker: None,
    }));
    match present_import(multiple, &shared) {
        Ok(()) => PickFuture {
            shared: Some(shared),
        },
        Err(reason) => PickFuture::ready(vec![PathBuf::from(format!("/__PICKER_FAILED_{reason}"))]),
    }
}
