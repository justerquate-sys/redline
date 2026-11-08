#![cfg_attr(test, allow(clippy::unwrap_used))]
pub mod api;
pub mod install;
pub mod native;
pub mod service;

pub const HOST_NAME: &str = "com.fulltranscript.helper";
pub const HELPER_URL: &str = "http://127.0.0.1:8765";
