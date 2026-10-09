# iOS spike (PdfCraft-as-IPA experiment)

`cargo check -p pdfcraft --target aarch64-apple-ios` fails only inside `rfd`
0.17, which has no iOS dialog backend. `rfd-stub/` is an API-compatible stub
swapped in via `[patch.crates-io]` (root `Cargo.toml`) so the rest of the
workspace can be checked for iOS. Every dialog resolves to "picked nothing".

This is spike-branch-only scaffolding: a real port would back file picking
with `UIDocumentPickerViewController`, wire the eframe iOS app lifecycle, add
an Xcode harness, and sign the IPA. Desktop builds from this branch also get
the stub (dialogs no-op) — do not merge to `main`.
