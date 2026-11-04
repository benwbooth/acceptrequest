//! `pam_screentime.so`: account-phase module. Asks the local client daemon
//! whether the user may log in now, so login stays fast and works offline.
//!
//! Module arguments:
//! - `users=a,b`: the managed users. Everyone else is ignored without contacting the daemon,
//!   so a broken daemon can never lock out a parent or root.
//! - `socket=/path`: the daemon socket, defaulting to `CLIENT_SOCKET`.
//!
//! For a managed user, anything other than an explicit "allowed" from the daemon denies the
//! login: no daemon, a timeout, a garbled reply, or a panic in this module.

use screentime_common::{CLIENT_SOCKET, Decision, SocketRequest, SocketResponse};
use std::ffi::{CStr, CString, c_char, c_int, c_void};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::time::Duration;

const PAM_SUCCESS: c_int = 0;
const PAM_PERM_DENIED: c_int = 6;
const PAM_IGNORE: c_int = 25;
const PAM_CONV: c_int = 5;
const PAM_ERROR_MSG: c_int = 3;

const DAEMON_TIMEOUT: Duration = Duration::from_secs(3);

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

#[repr(C)]
struct PamConv {
    conv: Option<
        unsafe extern "C" fn(c_int, *mut *const PamMessage, *mut *mut PamResponse, *mut c_void) -> c_int,
    >,
    appdata_ptr: *mut c_void,
}

#[link(name = "pam")]
unsafe extern "C" {
    fn pam_get_user(pamh: *mut c_void, user: *mut *const c_char, prompt: *const c_char) -> c_int;
    fn pam_get_item(pamh: *const c_void, item_type: c_int, item: *mut *const c_void) -> c_int;
}

#[derive(Debug, PartialEq, Eq)]
pub struct Args {
    pub users: Vec<String>,
    pub socket: String,
}

pub fn parse_args<'a>(args: impl IntoIterator<Item = &'a str>) -> Args {
    let mut parsed = Args { users: Vec::new(), socket: CLIENT_SOCKET.to_owned() };
    for arg in args {
        if let Some(users) = arg.strip_prefix("users=") {
            parsed.users.extend(users.split(',').filter(|u| !u.is_empty()).map(str::to_owned));
        } else if let Some(socket) = arg.strip_prefix("socket=") {
            parsed.socket = socket.to_owned();
        }
    }
    parsed
}

#[derive(Debug, PartialEq, Eq)]
pub enum Verdict {
    Ignore,
    Allow,
    Deny(String),
}

/// Decide for `user`, consulting the daemon at `args.socket` only for managed users.
pub fn check(args: &Args, user: &str) -> Verdict {
    if !args.users.iter().any(|u| u == user) {
        return Verdict::Ignore;
    }
    match ask_daemon(&args.socket, user) {
        Ok(SocketResponse::Decision { decision: Decision::Allowed { .. }, .. }) => Verdict::Allow,
        Ok(SocketResponse::Decision { decision: Decision::Denied { reason }, .. }) => {
            Verdict::Deny(reason.to_string())
        }
        Ok(other) => Verdict::Deny(format!("Screen time service gave an unexpected answer: {other:?}")),
        Err(e) => Verdict::Deny(format!("Screen time service is not available ({e}).")),
    }
}

fn ask_daemon(socket: &str, user: &str) -> std::io::Result<SocketResponse> {
    let mut stream = UnixStream::connect(socket)?;
    stream.set_read_timeout(Some(DAEMON_TIMEOUT))?;
    stream.set_write_timeout(Some(DAEMON_TIMEOUT))?;
    let mut line = serde_json::to_vec(&SocketRequest::CheckLogin { username: user.to_owned() })?;
    line.push(b'\n');
    stream.write_all(&line)?;
    let mut reply = String::new();
    BufReader::new(stream).read_line(&mut reply)?;
    Ok(serde_json::from_str(&reply)?)
}

unsafe fn c_args<'a>(argc: c_int, argv: *const *const c_char) -> Vec<&'a str> {
    if argv.is_null() {
        return Vec::new();
    }
    (0..argc.max(0) as usize)
        .filter_map(|i| unsafe { (*argv.add(i)).as_ref() })
        .filter_map(|p| unsafe { CStr::from_ptr(p) }.to_str().ok())
        .collect()
}

/// Show `text` to the user through the application's conversation function, if it has one.
unsafe fn show_error(pamh: *mut c_void, text: &str) {
    let mut item: *const c_void = std::ptr::null();
    if unsafe { pam_get_item(pamh, PAM_CONV, &mut item) } != PAM_SUCCESS || item.is_null() {
        return;
    }
    let conv = unsafe { &*(item as *const PamConv) };
    let Some(f) = conv.conv else { return };
    let Ok(text) = CString::new(text) else { return };
    let msg = PamMessage { msg_style: PAM_ERROR_MSG, msg: text.as_ptr() };
    let mut msgs = [&msg as *const PamMessage];
    let mut resp: *mut PamResponse = std::ptr::null_mut();
    unsafe { f(1, msgs.as_mut_ptr(), &mut resp, conv.appdata_ptr) };
    if !resp.is_null() {
        unsafe {
            if !(*resp).resp.is_null() {
                libc::free((*resp).resp as *mut c_void);
            }
            libc::free(resp as *mut c_void);
        }
    }
}

unsafe fn acct_mgmt(pamh: *mut c_void, argc: c_int, argv: *const *const c_char) -> c_int {
    let args = parse_args(unsafe { c_args(argc, argv) });
    let mut user_ptr: *const c_char = std::ptr::null();
    if unsafe { pam_get_user(pamh, &mut user_ptr, std::ptr::null()) } != PAM_SUCCESS || user_ptr.is_null() {
        // Without a user we can't tell whether they're managed; deny only if anyone is.
        return if args.users.is_empty() { PAM_IGNORE } else { PAM_PERM_DENIED };
    }
    let Ok(user) = unsafe { CStr::from_ptr(user_ptr) }.to_str() else {
        return PAM_PERM_DENIED;
    };
    match check(&args, user) {
        Verdict::Ignore => PAM_IGNORE,
        Verdict::Allow => PAM_SUCCESS,
        Verdict::Deny(reason) => {
            unsafe { show_error(pamh, &reason) };
            PAM_PERM_DENIED
        }
    }
}

/// # Safety
/// Called by libpam with a valid handle and argument vector.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pam_sm_acct_mgmt(
    pamh: *mut c_void,
    _flags: c_int,
    argc: c_int,
    argv: *const *const c_char,
) -> c_int {
    std::panic::catch_unwind(|| unsafe { acct_mgmt(pamh, argc, argv) }).unwrap_or(PAM_PERM_DENIED)
}

#[cfg(test)]
mod tests {
    use super::*;
    use screentime_common::DenyReason;
    use std::os::unix::net::UnixListener;

    fn fake_daemon(reply: &'static str) -> (tempfile::TempDir, String) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.sock");
        let listener = UnixListener::bind(&path).unwrap();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let mut line = String::new();
                BufReader::new(&stream).read_line(&mut line).unwrap();
                (&stream).write_all(reply.as_bytes()).unwrap();
            }
        });
        (dir, path.to_string_lossy().into_owned())
    }

    fn args(socket: &str) -> Args {
        parse_args(["users=kid,kid2", &format!("socket={socket}")[..]])
    }

    #[test]
    fn parses_args() {
        assert_eq!(
            parse_args(["users=a,b", "socket=/x"]),
            Args { users: vec!["a".into(), "b".into()], socket: "/x".into() }
        );
        assert_eq!(parse_args([]).socket, CLIENT_SOCKET);
    }

    #[test]
    fn unmanaged_user_never_touches_daemon() {
        assert_eq!(check(&args("/nonexistent"), "dad"), Verdict::Ignore);
        assert_eq!(check(&args("/nonexistent"), "root"), Verdict::Ignore);
    }

    #[test]
    fn missing_daemon_denies_managed_user() {
        assert!(matches!(check(&args("/nonexistent"), "kid"), Verdict::Deny(_)));
    }

    #[test]
    fn follows_daemon_decision() {
        let (_d, s) = fake_daemon("{\"reply\":\"decision\",\"decision\":{\"decision\":\"allowed\",\"remaining_minutes\":30},\"used_minutes\":5}\n");
        assert_eq!(check(&args(&s), "kid"), Verdict::Allow);

        let (_d, s) = fake_daemon("{\"reply\":\"decision\",\"decision\":{\"decision\":\"denied\",\"reason\":{\"kind\":\"out_of_time\"}},\"used_minutes\":120}\n");
        assert_eq!(check(&args(&s), "kid"), Verdict::Deny(DenyReason::OutOfTime.to_string()));
    }

    #[test]
    fn garbage_or_unmanaged_reply_denies() {
        let (_d, s) = fake_daemon("not json\n");
        assert!(matches!(check(&args(&s), "kid"), Verdict::Deny(_)));
        let (_d, s) = fake_daemon("{\"reply\":\"unmanaged\"}\n");
        assert!(matches!(check(&args(&s), "kid"), Verdict::Deny(_)));
    }
}
