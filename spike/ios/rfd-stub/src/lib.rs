//! SPIKE-ONLY backing for the `rfd` 0.17 API surface used by pdfcraft.
//!
//! Upstream `rfd` has no iOS backend. This crate is swapped in via
//! `[patch.crates-io]` on the spike branch only.
//!
//! IMPORTANT: pdfcraft creates the pick future on the UI thread but polls it
//! on a worker thread (`pickers.rs`: "creating the future shows the panel").
//! So like real `rfd`, presentation happens eagerly inside the (non-async)
//! `pick_*` constructors, never on first poll.
//!
//! - Other targets: constructors resolve to "picked nothing".
//! - iOS: file picking goes through `UIDocumentPickerViewController` in
//!   Import mode (the system copies picks into the app sandbox, so no
//!   security-scope juggling). Folder/save panels fall back to the app's
//!   sandboxed Documents directory (visible in the Files app once the host
//!   sets `UIFileSharingEnabled`).

use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Waker};

/// Builder for an async file dialog. Settings are accepted; on iOS only the
/// choice of `pick_*` decides what happens.
#[derive(Debug, Clone, Default)]
pub struct AsyncFileDialog;

impl AsyncFileDialog {
    pub fn new() -> Self {
        Self
    }

    pub fn add_filter(
        self,
        _name: impl Into<String>,
        _extensions: &[impl ToString],
    ) -> Self {
        self
    }

    pub fn set_directory<P: AsRef<Path>>(self, _path: P) -> Self {
        self
    }

    pub fn set_file_name(self, file_name: impl Into<String>) -> Self {
        *SAVE_NAME.lock().unwrap() = Some(file_name.into());
        self
    }

    pub fn set_title(self, _title: impl Into<String>) -> Self {
        self
    }

    pub fn set_can_create_directories(self, _can: bool) -> Self {
        self
    }

    pub fn pick_file(self) -> impl Future<Output = Option<FileHandle>> {
        // NOTE: `pick_now` must run HERE (UI thread at tap time), not inside
        // the async block (which first polls on a worker thread).
        #[cfg(target_os = "ios")]
        {
            let fut = ios_impl::pick_now(false);
            return async move {
                fut.await
                    .into_iter()
                    .next()
                    .map(FileHandle::from_path)
            };
        }
        #[cfg(not(target_os = "ios"))]
        {
            async move { None }
        }
    }

    pub fn pick_files(self) -> impl Future<Output = Option<Vec<FileHandle>>> {
        // NOTE: same as `pick_file`: present now, await later.
        #[cfg(target_os = "ios")]
        {
            let fut = ios_impl::pick_now(true);
            return async move {
                let paths = fut.await;
                if paths.is_empty() {
                    None
                } else {
                    Some(paths.into_iter().map(FileHandle::from_path).collect())
                }
            };
        }
        #[cfg(not(target_os = "ios"))]
        {
            async move { None }
        }
    }

    pub fn pick_folder(self) -> impl Future<Output = Option<FileHandle>> {
        // NOTE: presents now (UI thread), like `pick_file`.
        #[cfg(target_os = "ios")]
        {
            let fut = ios_impl::folder_now(false);
            return async move {
                fut.await
                    .into_iter()
                    .next()
                    .map(FileHandle::from_path)
            };
        }
        #[cfg(not(target_os = "ios"))]
        {
            async move { sandbox_fallback_dir().map(FileHandle::from_path) }
        }
    }

    pub fn pick_folders(self) -> impl Future<Output = Option<Vec<FileHandle>>> {
        // NOTE: presents now (UI thread), like `pick_file`.
        #[cfg(target_os = "ios")]
        {
            let fut = ios_impl::folder_now(true);
            return async move {
                let paths = fut.await;
                if paths.is_empty() {
                    None
                } else {
                    Some(paths.into_iter().map(FileHandle::from_path).collect())
                }
            };
        }
        #[cfg(not(target_os = "ios"))]
        {
            async move { sandbox_fallback_dir().map(|p| vec![FileHandle::from_path(p)]) }
        }
    }

    pub fn pick_file_or_folder(self) -> impl Future<Output = Option<FileHandle>> {
        self.pick_file()
    }

    pub fn pick_files_or_folders(
        self,
    ) -> impl Future<Output = Option<Vec<FileHandle>>> {
        self.pick_files()
    }

    pub fn save_file(self) -> impl Future<Output = Option<FileHandle>> {
        // NOTE: presents now (UI thread): folder picker, file name appended
        // on resolve. Falls back to sandboxed Documents off-iOS.
        #[cfg(target_os = "ios")]
        {
            let name = take_name().unwrap_or_else(|| "document.pdf".to_string());
            let fut = ios_impl::save_now(name);
            return async move {
                fut.await
                    .into_iter()
                    .next()
                    .map(FileHandle::from_path)
            };
        }
        #[cfg(not(target_os = "ios"))]
        {
            async move {
                let name = take_name().unwrap_or_else(|| "document.pdf".to_string());
                let mut path = sandbox_fallback_dir()?;
                path.push(uniquify(&path, &name));
                Some(FileHandle::from_path(path))
            }
        }
    }
}

/// Handle to a picked file.
#[derive(Debug, Clone, Default)]
pub struct FileHandle {
    path: PathBuf,
}

impl FileHandle {
    fn from_path(path: PathBuf) -> Self {
        Self { path }
    }

    pub fn file_name(&self) -> String {
        self.path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default()
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub async fn read(&self) -> Vec<u8> {
        std::fs::read(&self.path).unwrap_or_default()
    }

    pub async fn write(&self, data: &[u8]) -> std::io::Result<()> {
        std::fs::write(&self.path, data)
    }

    pub fn inner(&self) -> &Path {
        &self.path
    }
}

static SAVE_NAME: Mutex<Option<String>> = Mutex::new(None);

fn take_name() -> Option<String> {
    SAVE_NAME.lock().unwrap().take()
}

/// Uniquified file name inside `dir` so saves never silently overwrite.
fn uniquify(dir: &Path, name: &str) -> String {
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
fn sandbox_fallback_dir() -> Option<PathBuf> {
    let home = std::env::var("HOME").ok()?;
    let docs = PathBuf::from(home).join("Documents");
    std::fs::create_dir_all(&docs).ok()?;
    Some(docs)
}

/// Shared state between the eagerly-presented picker and the future polled
/// later on a worker thread.
struct WaitShared {
    done: Mutex<Option<Vec<PathBuf>>>,
    waker: Mutex<Option<Waker>>,
}

#[cfg(target_os = "ios")]
mod ios_impl;
