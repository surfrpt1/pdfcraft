//! SPIKE-ONLY backing for the `rfd` 0.17 API surface used by pdfcraft.
//!
//! Upstream `rfd` has no iOS backend. This crate is swapped in via
//! `[patch.crates-io]` on the spike branch only.
//!
//! - Other targets: every dialog immediately resolves to "picked nothing"
//!   (previous spike behavior, keeps `cargo check` green everywhere).
//! - iOS: file picking goes through `UIDocumentPickerViewController` in
//!   Import mode (the system copies picks into the app sandbox, so no
//!   security-scope juggling). Folder/save panels fall back to the app's
//!   sandboxed Documents directory (visible in the Files app once the host
//!   sets `UIFileSharingEnabled`).

use std::path::{Path, PathBuf};

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
        Self::remember_name(file_name.into());
        self
    }

    pub fn set_title(self, _title: impl Into<String>) -> Self {
        self
    }

    pub fn set_can_create_directories(self, _can: bool) -> Self {
        self
    }

    pub async fn pick_file(self) -> Option<FileHandle> {
        pick(false).await.into_iter().next().map(FileHandle::from_path)
    }

    pub async fn pick_files(self) -> Option<Vec<FileHandle>> {
        let paths = pick(true).await;
        if paths.is_empty() {
            None
        } else {
            Some(paths.into_iter().map(FileHandle::from_path).collect())
        }
    }

    pub async fn pick_folder(self) -> Option<FileHandle> {
        sandbox_fallback_dir().map(FileHandle::from_path)
    }

    pub async fn pick_folders(self) -> Option<Vec<FileHandle>> {
        sandbox_fallback_dir().map(|p| vec![FileHandle::from_path(p)])
    }

    pub async fn pick_file_or_folder(self) -> Option<FileHandle> {
        self.pick_file().await
    }

    pub async fn pick_files_or_folders(self) -> Option<Vec<FileHandle>> {
        self.pick_files().await
    }

    pub async fn save_file(self) -> Option<FileHandle> {
        let name = take_name().unwrap_or_else(|| "document.pdf".to_string());
        let mut path = sandbox_fallback_dir()?;
        path.push(uniquify(&path, &name));
        Some(FileHandle::from_path(path))
    }

    #[allow(clippy::needless_pass_by_value)]
    fn remember_name(_name: String) {
        // v1: the iOS picker flow ignores the suggested name for opens;
        // saves read it back via `take_name`.
        #[cfg(target_os = "ios")]
        {
            *SAVE_NAME.lock().unwrap() = Some(_name);
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

// ---------------------------------------------------------------------------
// Non-iOS: dialogs resolve to "picked nothing" (check-only spike behavior).
// ---------------------------------------------------------------------------

#[cfg(not(target_os = "ios"))]
async fn pick(_multiple: bool) -> Vec<PathBuf> {
    Vec::new()
}

#[cfg(not(target_os = "ios"))]
fn sandbox_fallback_dir() -> Option<PathBuf> {
    None
}

#[cfg(not(target_os = "ios"))]
fn take_name() -> Option<String> {
    None
}

#[cfg(not(target_os = "ios"))]
fn uniquify(_dir: &Path, name: &str) -> String {
    name.to_string()
}

// ---------------------------------------------------------------------------
// iOS: real document picker + sandboxed Documents fallbacks.
// ---------------------------------------------------------------------------

#[cfg(target_os = "ios")]
mod ios_impl;

#[cfg(target_os = "ios")]
use ios_impl::{pick, sandbox_fallback_dir, take_name, uniquify, SAVE_NAME};
