//! Linux backend crate for X11 and portal-mediated Wayland support.

pub const BACKEND_NAME: &str = "linux";

pub fn support_status() -> &'static str {
    "unsupported_until_native_backend_is_enabled"
}
