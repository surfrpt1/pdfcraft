//! SPIKE-ONLY stub of the `rfd` 0.17 API surface used by pdfcraft, for targets
//! without a native dialog backend (iOS).
//!
//! Upstream `rfd` has no iOS backend, so `cargo check --target aarch64-apple-ios`
//! fails inside `rfd` itself. This crate is swapped in via `[patch.crates-io]`
//! on the spike branch only. Every dialog immediately resolves to "picked
//! nothing", matching a user dismissing the panel. A real iOS port would back
//! this with `UIDocumentPickerViewController` instead.

use std::path::{Path, PathBuf};

/// Builder for an async file dialog. All settings are accepted and ignored.
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

    pub fn set_file_name(self, _file_name: impl Into<String>) -> Self {
        self
    }

    pub fn set_title(self, _title: impl Into<String>) -> Self {
        self
    }

    pub fn set_can_create_directories(self, _can: bool) -> Self {
        self
    }

    /// iOS has no panel yet: behaves as an immediately dismissed dialog.
    pub async fn pick_file(self) -> Option<FileHandle> {
        None
    }

    pub async fn pick_files(self) -> Option<Vec<FileHandle>> {
        None
    }

    pub async fn pick_folder(self) -> Option<FileHandle> {
        None
    }

    pub async fn pick_folders(self) -> Option<Vec<FileHandle>> {
        None
    }

    pub async fn pick_file_or_folder(self) -> Option<FileHandle> {
        None
    }

    pub async fn pick_files_or_folders(self) -> Option<Vec<FileHandle>> {
        None
    }

    pub async fn save_file(self) -> Option<FileHandle> {
        None
    }
}

/// Handle to a picked file. Unreachable on iOS (pickers return `None`), but
/// the type must exist with the same API for call sites to compile.
#[derive(Debug, Clone, Default)]
pub struct FileHandle {
    path: PathBuf,
}

impl FileHandle {
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
        Vec::new()
    }

    pub async fn write(&self, _data: &[u8]) -> std::io::Result<()> {
        Ok(())
    }

    pub fn inner(&self) -> &Path {
        &self.path
    }
}
