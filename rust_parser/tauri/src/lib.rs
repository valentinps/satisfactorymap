//! Tauri-free core of the desktop shell: the native `AppSession` that wraps
//! `sav_core` (load / edit / export / queries). The binary (`main.rs`) is the
//! thin Tauri command + IPC layer over this; keeping it in a lib lets the
//! orchestration be integration-tested without spinning up a webview.

pub mod server_api;
pub mod session;
