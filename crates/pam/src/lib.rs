//! `pam_acceptrequest.so`: account-phase module. Asks the local client daemon
//! whether the user may log in now, so login stays fast and works offline.

use std::ffi::{c_char, c_int, c_void};

const PAM_SUCCESS: c_int = 0;
const PAM_IGNORE: c_int = 25;

#[unsafe(no_mangle)]
pub extern "C" fn pam_sm_acct_mgmt(
    _pamh: *mut c_void,
    _flags: c_int,
    _argc: c_int,
    _argv: *const *const c_char,
) -> c_int {
    // TODO: pam_get_user, query acceptrequest_common::CLIENT_SOCKET, return PAM_PERM_DENIED when out of time.
    PAM_SUCCESS
}

#[unsafe(no_mangle)]
pub extern "C" fn pam_sm_open_session(
    _pamh: *mut c_void,
    _flags: c_int,
    _argc: c_int,
    _argv: *const *const c_char,
) -> c_int {
    PAM_IGNORE
}

#[unsafe(no_mangle)]
pub extern "C" fn pam_sm_close_session(
    _pamh: *mut c_void,
    _flags: c_int,
    _argc: c_int,
    _argv: *const *const c_char,
) -> c_int {
    PAM_IGNORE
}
