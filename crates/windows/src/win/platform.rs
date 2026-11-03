//! Sessions through the WTS API, locking by disconnecting, logging off, Windows logon hours,
//! the admin password check, and keeping the tray app running in the kid's session.

use super::{from_wide, pcwstr, wide};
use crate::logon_hours;
use acceptrequest_client::agent::{Platform, Session};
use acceptrequest_client::state::State;
use acceptrequest_common::Decision;
use anyhow::{Context, Result, anyhow};
use chrono::{DateTime, Local, Utc};
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};
use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::NetworkManagement::NetManagement::{
    NetApiBufferFree, NetUserGetInfo, NetUserSetInfo, USER_INFO_1, USER_INFO_1020, USER_PRIV_ADMIN,
};
use windows::Win32::Security::{
    EqualSid, GetTokenInformation, LOGON32_LOGON_NETWORK, LOGON32_PROVIDER_DEFAULT, LogonUserW, PSID,
    SECURITY_MAX_SID_SIZE, TOKEN_GROUPS, TokenGroups, WinBuiltinAdministratorsSid, CreateWellKnownSid,
};
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
};
use windows::Win32::System::Environment::{CreateEnvironmentBlock, DestroyEnvironmentBlock};
use windows::Win32::System::RemoteDesktop::{
    WTS_SESSION_INFOW, WTSActive, WTSDisconnectSession, WTSDisconnected, WTSEnumerateSessionsW,
    WTSFreeMemory, WTSINFOEXW, WTSLogoffSession, WTSQuerySessionInformationW, WTSQueryUserToken,
    WTSSessionInfoEx, WTSUserName, WTS_SESSIONSTATE_LOCK, ProcessIdToSessionId,
};
use windows::Win32::System::Threading::{
    CREATE_UNICODE_ENVIRONMENT, CreateProcessAsUserW, PROCESS_INFORMATION, STARTUPINFOW,
};
use windows::core::PWSTR;

/// Hours in Windows' weekly logon-hours map.
const UNITS_PER_WEEK: u32 = 168;
const TRAY_EXE: &str = "acceptrequest-tray.exe";
/// Don't try to start the tray in a session more often than this.
const TRAY_RETRY: Duration = Duration::from_secs(60);

pub struct Windows {
    users: Vec<String>,
    /// Mirror the schedule into Windows logon hours (on unless ACCEPTREQUEST_LOGON_HOURS=off).
    use_logon_hours: bool,
    /// Last logon-hours map written for each user, so Windows is only told about changes.
    logon_hours: Mutex<HashMap<String, [u8; logon_hours::BYTES]>>,
    tray_started: Mutex<HashMap<u32, Instant>>,
}

impl Windows {
    pub fn new(users: Vec<String>) -> Windows {
        for user in &users {
            if is_admin_account(user) {
                tracing::error!(user, "this account is an administrator; it can't be limited, so it will be ignored");
            }
        }
        let use_logon_hours = !std::env::var("ACCEPTREQUEST_LOGON_HOURS").is_ok_and(|v| v.eq_ignore_ascii_case("off"));
        if !use_logon_hours {
            tracing::warn!("logon hours are off: sign-in is only blocked while the service is running");
        }
        Windows { users, use_logon_hours, logon_hours: Mutex::default(), tray_started: Mutex::default() }
    }

    fn is_managed(&self, user: &str) -> bool {
        self.users.iter().any(|u| u.eq_ignore_ascii_case(user))
    }

    fn ensure_tray(&self, session_id: u32) {
        if tray_running_in(session_id) {
            return;
        }
        let mut started = self.tray_started.lock().unwrap();
        if started.get(&session_id).is_some_and(|at| at.elapsed() < TRAY_RETRY) {
            return;
        }
        started.insert(session_id, Instant::now());
        match start_in_session(session_id) {
            Ok(()) => tracing::info!(session_id, "started the tray app"),
            Err(e) => tracing::warn!(session_id, "couldn't start the tray app: {e:#}"),
        }
    }
}

impl Platform for Windows {
    async fn sessions(&self) -> Result<Vec<Session>> {
        let sessions = list_sessions()?;
        for s in &sessions {
            if s.in_use && self.is_managed(&s.user) {
                self.ensure_tray(s.id.parse().unwrap_or(0));
            }
        }
        Ok(sessions)
    }

    async fn lock(&self, session: &Session) -> Result<()> {
        // Disconnecting the console session drops it to the sign-in screen with everything still
        // open, which is what a lock is; a service can't call LockWorkStation for another session.
        let id: u32 = session.id.parse()?;
        unsafe { WTSDisconnectSession(None, id, false) }.context("WTSDisconnectSession")
    }

    async fn end_sessions(&self, _user: &str, sessions: &[Session]) -> Result<()> {
        for s in sessions {
            let id: u32 = s.id.parse()?;
            unsafe { WTSLogoffSession(None, id, false) }.context("WTSLogoffSession")?;
        }
        Ok(())
    }

    fn verify_admin(&self, admin: &str, password: &str) -> Result<(), String> {
        verify_admin(admin, password)
    }

    fn sync_os_restrictions(&self, user: &str, state: &State, now: DateTime<Utc>) {
        if !self.use_logon_hours || is_admin_account(user) {
            return;
        }
        let map = logon_hours::weekly_map(
            now,
            |t| t.with_timezone(&Local).naive_local(),
            |t| matches!(state.decide(user, t).0, Decision::Allowed { .. }),
        );
        let mut written = self.logon_hours.lock().unwrap();
        if written.get(user) == Some(&map) {
            return;
        }
        match set_logon_hours(user, map) {
            Ok(()) => {
                tracing::info!(user, "updated Windows logon hours");
                written.insert(user.to_owned(), map);
            }
            Err(e) => tracing::error!(user, "setting logon hours failed: {e:#}"),
        }
    }
}

fn query_string(session_id: u32, class: windows::Win32::System::RemoteDesktop::WTS_INFO_CLASS) -> Option<String> {
    let mut buf = PWSTR::null();
    let mut len = 0u32;
    unsafe {
        WTSQuerySessionInformationW(None, session_id, class, &mut buf, &mut len).ok()?;
        let s = buf.to_string().ok();
        WTSFreeMemory(buf.0 as _);
        s
    }
}

fn is_locked(session_id: u32) -> bool {
    let mut buf = PWSTR::null();
    let mut len = 0u32;
    unsafe {
        if WTSQuerySessionInformationW(None, session_id, WTSSessionInfoEx, &mut buf, &mut len).is_err() {
            return false;
        }
        let info = &*(buf.0 as *const WTSINFOEXW);
        let flags = info.Data.WTSInfoExLevel1.SessionFlags as u32;
        WTSFreeMemory(buf.0 as _);
        flags == WTS_SESSIONSTATE_LOCK
    }
}

/// Signed-in user sessions: the console and any remote ones, connected or not.
fn list_sessions() -> Result<Vec<Session>> {
    let mut info: *mut WTS_SESSION_INFOW = std::ptr::null_mut();
    let mut count = 0u32;
    unsafe { WTSEnumerateSessionsW(None, 0, 1, &mut info, &mut count) }.context("WTSEnumerateSessionsW")?;
    let mut out = Vec::new();
    for i in 0..count as usize {
        let s = unsafe { &*info.add(i) };
        if s.SessionId == 0 || (s.State != WTSActive && s.State != WTSDisconnected) {
            continue;
        }
        let Some(user) = query_string(s.SessionId, WTSUserName).filter(|u| !u.is_empty()) else { continue };
        let locked = s.State == WTSDisconnected || is_locked(s.SessionId);
        out.push(Session { id: s.SessionId.to_string(), user, in_use: !locked, locked });
    }
    unsafe { WTSFreeMemory(info as _) };
    Ok(out)
}

/// Whether a local account has administrator rights (USER_PRIV_ADMIN).
pub fn is_admin_account(user: &str) -> bool {
    let name = wide(user);
    let mut buf: *mut u8 = std::ptr::null_mut();
    unsafe {
        if NetUserGetInfo(None, pcwstr(&name), 1, &mut buf) != 0 || buf.is_null() {
            return false;
        }
        let admin = (*(buf as *const USER_INFO_1)).usri1_priv == USER_PRIV_ADMIN;
        NetApiBufferFree(Some(buf as _));
        admin
    }
}

fn set_logon_hours(user: &str, mut map: [u8; logon_hours::BYTES]) -> Result<()> {
    let name = wide(user);
    let info = USER_INFO_1020 { usri1020_units_per_week: UNITS_PER_WEEK, usri1020_logon_hours: map.as_mut_ptr() };
    let rc = unsafe { NetUserSetInfo(None, pcwstr(&name), 1020, &info as *const _ as *const u8, None) };
    if rc == 0 { Ok(()) } else { Err(anyhow!("NetUserSetInfo returned {rc}")) }
}

/// Check `admin`'s password and that the account is an administrator.
/// Accepts `name`, `COMPUTER\name`, `MicrosoftAccount\email` or an email.
pub fn verify_admin(admin: &str, password: &str) -> Result<(), String> {
    let (domain, user) = match admin.split_once('\\') {
        Some((d, u)) => (Some(d), u),
        None if admin.contains('@') => (Some("MicrosoftAccount"), admin),
        None => (Some("."), admin),
    };
    let (user_w, pass_w) = (wide(user), wide(password));
    let domain_w = domain.map(wide);
    let mut token = HANDLE::default();
    unsafe {
        LogonUserW(
            pcwstr(&user_w),
            domain_w.as_deref().map(pcwstr).unwrap_or(windows::core::PCWSTR::null()),
            pcwstr(&pass_w),
            LOGON32_LOGON_NETWORK,
            LOGON32_PROVIDER_DEFAULT,
            &mut token,
        )
    }
    .map_err(|e| format!("sign-in failed: {e}"))?;
    let admin = token_is_admin(token);
    unsafe {
        let _ = CloseHandle(token);
    }
    if admin? { Ok(()) } else { Err(format!("{admin} is not an administrator", admin = user)) }
}

/// Whether the token's groups include BUILTIN\Administrators. With UAC the group is marked
/// deny-only in a filtered token, but its presence still means the account is an administrator.
fn token_is_admin(token: HANDLE) -> Result<bool, String> {
    let mut admins = [0u8; SECURITY_MAX_SID_SIZE as usize];
    let mut size = admins.len() as u32;
    let admins_sid = PSID(admins.as_mut_ptr() as _);
    unsafe { CreateWellKnownSid(WinBuiltinAdministratorsSid, None, Some(admins_sid), &mut size) }
        .map_err(|e| e.to_string())?;

    let mut needed = 0u32;
    let _ = unsafe { GetTokenInformation(token, TokenGroups, None, 0, &mut needed) };
    let mut buf = vec![0u8; needed as usize];
    unsafe { GetTokenInformation(token, TokenGroups, Some(buf.as_mut_ptr() as _), needed, &mut needed) }
        .map_err(|e| e.to_string())?;
    let groups = unsafe { &*(buf.as_ptr() as *const TOKEN_GROUPS) };
    let entries = unsafe { std::slice::from_raw_parts(groups.Groups.as_ptr(), groups.GroupCount as usize) };
    Ok(entries.iter().any(|g| unsafe { EqualSid(g.Sid, admins_sid) }.is_ok()))
}

fn tray_running_in(session_id: u32) -> bool {
    let Ok(snap) = (unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) }) else { return false };
    let mut entry = PROCESSENTRY32W { dwSize: size_of::<PROCESSENTRY32W>() as u32, ..Default::default() };
    let mut found = false;
    let mut ok = unsafe { Process32FirstW(snap, &mut entry) }.is_ok();
    while ok {
        if from_wide(&entry.szExeFile).eq_ignore_ascii_case(TRAY_EXE) {
            let mut sid = 0u32;
            if unsafe { ProcessIdToSessionId(entry.th32ProcessID, &mut sid) }.is_ok() && sid == session_id {
                found = true;
                break;
            }
        }
        ok = unsafe { Process32NextW(snap, &mut entry) }.is_ok();
    }
    unsafe {
        let _ = CloseHandle(snap);
    }
    found
}

/// Start the tray app as the session's user, next to this executable.
fn start_in_session(session_id: u32) -> Result<()> {
    let exe = std::env::current_exe()?.with_file_name(TRAY_EXE);
    let mut cmd = wide(&format!("\"{}\"", exe.display()));
    let mut desktop = wide(r"winsta0\default");
    let mut token = HANDLE::default();
    unsafe {
        WTSQueryUserToken(session_id, &mut token).context("WTSQueryUserToken")?;
        let mut env: *mut core::ffi::c_void = std::ptr::null_mut();
        let env_ok = CreateEnvironmentBlock(&mut env, Some(token), false).is_ok();
        let startup = STARTUPINFOW {
            cb: size_of::<STARTUPINFOW>() as u32,
            lpDesktop: PWSTR(desktop.as_mut_ptr()),
            ..Default::default()
        };
        let mut process = PROCESS_INFORMATION::default();
        let result = CreateProcessAsUserW(
            Some(token),
            windows::core::PCWSTR::null(),
            Some(PWSTR(cmd.as_mut_ptr())),
            None,
            None,
            false,
            CREATE_UNICODE_ENVIRONMENT,
            env_ok.then_some(env as *const _),
            windows::core::PCWSTR::null(),
            &startup,
            &mut process,
        );
        if env_ok {
            let _ = DestroyEnvironmentBlock(env);
        }
        let _ = CloseHandle(token);
        result.context("CreateProcessAsUserW")?;
        let _ = CloseHandle(process.hThread);
        let _ = CloseHandle(process.hProcess);
    }
    Ok(())
}

