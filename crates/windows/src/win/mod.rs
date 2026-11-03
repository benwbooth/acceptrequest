//! Windows platform pieces. Everything here is `unsafe` FFI kept behind small safe functions.

pub mod pipe;
pub mod platform;

use windows::core::PCWSTR;

/// A NUL-terminated UTF-16 copy of `s`, for passing to Win32.
pub fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

pub fn pcwstr(w: &[u16]) -> PCWSTR {
    PCWSTR(w.as_ptr())
}

/// A UTF-16 buffer up to its first NUL.
pub fn from_wide(w: &[u16]) -> String {
    let end = w.iter().position(|c| *c == 0).unwrap_or(w.len());
    String::from_utf16_lossy(&w[..end])
}

/// Where the service keeps its configuration, state and log.
pub const DATA_DIR: &str = r"C:\ProgramData\acceptrequest";
