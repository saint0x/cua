//! Windows backend crate for Graphics Capture and SendInput.

pub const BACKEND_NAME: &str = "windows";

pub fn support_status() -> &'static str {
    "unsupported_until_native_backend_is_enabled"
}
