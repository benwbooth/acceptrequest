//! "My parent is here": checking an administrator's password through PAM.

use std::ffi::{CStr, CString, c_char, c_int, c_void};

const PAM_SUCCESS: c_int = 0;
const PAM_BUF_ERR: c_int = 5;
const PAM_CONV_ERR: c_int = 19;
const PAM_PROMPT_ECHO_OFF: c_int = 1;
const PAM_PROMPT_ECHO_ON: c_int = 2;

#[repr(C)]
struct PamMessage {
    msg_style: c_int,
    msg: *const c_char,
}

#[repr(C)]
struct PamResponse {
    resp: *mut c_char,
    resp_retcode: c_int,
}

type ConvFn =
    unsafe extern "C" fn(c_int, *mut *const PamMessage, *mut *mut PamResponse, *mut c_void) -> c_int;

#[repr(C)]
struct PamConv {
    conv: Option<ConvFn>,
    appdata_ptr: *mut c_void,
}

#[link(name = "pam")]
unsafe extern "C" {
    fn pam_start(
        service: *const c_char,
        user: *const c_char,
        conv: *const PamConv,
        pamh: *mut *mut c_void,
    ) -> c_int;
    fn pam_start_confdir(
        service: *const c_char,
        user: *const c_char,
        conv: *const PamConv,
        confdir: *const c_char,
        pamh: *mut *mut c_void,
    ) -> c_int;
    fn pam_authenticate(pamh: *mut c_void, flags: c_int) -> c_int;
    fn pam_acct_mgmt(pamh: *mut c_void, flags: c_int) -> c_int;
    fn pam_end(pamh: *mut c_void, status: c_int) -> c_int;
    fn pam_strerror(pamh: *mut c_void, errnum: c_int) -> *const c_char;
}

/// Answers every prompt with the password in `appdata`; libpam frees the responses.
unsafe extern "C" fn answer_with_password(
    num_msg: c_int,
    msg: *mut *const PamMessage,
    resp: *mut *mut PamResponse,
    appdata: *mut c_void,
) -> c_int {
    if num_msg <= 0 || msg.is_null() || resp.is_null() {
        return PAM_CONV_ERR;
    }
    let password = appdata as *const c_char;
    let n = num_msg as usize;
    let replies = unsafe { libc::calloc(n, size_of::<PamResponse>()) } as *mut PamResponse;
    if replies.is_null() {
        return PAM_BUF_ERR;
    }
    for i in 0..n {
        let style = unsafe { (**msg.add(i)).msg_style };
        if style == PAM_PROMPT_ECHO_OFF || style == PAM_PROMPT_ECHO_ON {
            unsafe { (*replies.add(i)).resp = libc::strdup(password) };
        }
    }
    unsafe { *resp = replies };
    PAM_SUCCESS
}

/// Check `user`'s password with the PAM `service`. `confdir` replaces /etc/pam.d, for tests.
pub fn authenticate(service: &str, confdir: Option<&str>, user: &str, password: &str) -> Result<(), String> {
    let service = CString::new(service).map_err(|_| "bad service name")?;
    let user = CString::new(user).map_err(|_| "bad user name")?;
    let password = CString::new(password).map_err(|_| "bad password")?;
    let confdir = confdir.map(CString::new).transpose().map_err(|_| "bad confdir")?;
    let conv = PamConv {
        conv: Some(answer_with_password),
        appdata_ptr: password.as_ptr() as *mut c_void,
    };

    let mut pamh: *mut c_void = std::ptr::null_mut();
    let rc = unsafe {
        match &confdir {
            Some(dir) => pam_start_confdir(service.as_ptr(), user.as_ptr(), &conv, dir.as_ptr(), &mut pamh),
            None => pam_start(service.as_ptr(), user.as_ptr(), &conv, &mut pamh),
        }
    };
    if rc != PAM_SUCCESS {
        return Err(format!("pam_start failed ({rc})"));
    }
    let mut rc = unsafe { pam_authenticate(pamh, 0) };
    if rc == PAM_SUCCESS {
        rc = unsafe { pam_acct_mgmt(pamh, 0) };
    }
    let result = if rc == PAM_SUCCESS {
        Ok(())
    } else {
        Err(unsafe { CStr::from_ptr(pam_strerror(pamh, rc)) }.to_string_lossy().into_owned())
    };
    unsafe { pam_end(pamh, rc) };
    result
}

/// The members listed for `group` in the group database (not users who only have it as their
/// primary group).
pub fn group_members(group: &str) -> Vec<String> {
    let Ok(group_c) = CString::new(group) else { return Vec::new() };
    let mut buf = vec![0 as c_char; 64 * 1024];
    let mut gr: libc::group = unsafe { std::mem::zeroed() };
    let mut out: *mut libc::group = std::ptr::null_mut();
    let rc = unsafe { libc::getgrnam_r(group_c.as_ptr(), &mut gr, buf.as_mut_ptr(), buf.len(), &mut out) };
    if rc != 0 || out.is_null() || gr.gr_mem.is_null() {
        return Vec::new();
    }
    let mut members = Vec::new();
    let mut p = gr.gr_mem;
    unsafe {
        while !(*p).is_null() {
            members.push(CStr::from_ptr(*p).to_string_lossy().into_owned());
            p = p.add(1);
        }
    }
    members
}

/// Whether `user` belongs to `group`, as a primary or supplementary group.
pub fn in_group(user: &str, group: &str) -> bool {
    let (Ok(user_c), Ok(group_c)) = (CString::new(user), CString::new(group)) else {
        return false;
    };
    let mut buf = vec![0 as c_char; 16 * 1024];

    let mut pw: libc::passwd = unsafe { std::mem::zeroed() };
    let mut pw_out: *mut libc::passwd = std::ptr::null_mut();
    let rc = unsafe { libc::getpwnam_r(user_c.as_ptr(), &mut pw, buf.as_mut_ptr(), buf.len(), &mut pw_out) };
    if rc != 0 || pw_out.is_null() {
        return false;
    }
    let primary = pw.pw_gid;

    let mut gr: libc::group = unsafe { std::mem::zeroed() };
    let mut gr_out: *mut libc::group = std::ptr::null_mut();
    let rc = unsafe { libc::getgrnam_r(group_c.as_ptr(), &mut gr, buf.as_mut_ptr(), buf.len(), &mut gr_out) };
    if rc != 0 || gr_out.is_null() {
        return false;
    }
    let wanted = gr.gr_gid;

    let mut groups = vec![0 as libc::gid_t; 256];
    let mut n = groups.len() as c_int;
    if unsafe { libc::getgrouplist(user_c.as_ptr(), primary, groups.as_mut_ptr(), &mut n) } < 0 {
        groups.resize(n.max(0) as usize, 0);
        if unsafe { libc::getgrouplist(user_c.as_ptr(), primary, groups.as_mut_ptr(), &mut n) } < 0 {
            return false;
        }
    }
    groups[..n.max(0) as usize].contains(&wanted)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn group_lookup() {
        assert!(in_group("root", "root"));
        assert!(!in_group("root", "no-such-group-screentime"));
        assert!(!in_group("no-such-user-screentime", "root"));
    }
}
