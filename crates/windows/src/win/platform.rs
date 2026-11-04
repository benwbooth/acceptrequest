//! Sessions through the WTS API, locking by disconnecting, logging off, Windows logon hours,
//! the admin password check, and keeping the tray app running in the kid's session.

use super::{from_wide, pcwstr, wide};
use crate::logon_hours;
use screentime_client::agent::{Platform, Session};
use screentime_client::state::State;
use screentime_common::Decision;
use anyhow::{Context, Result, anyhow};
use chrono::{DateTime, Local, Utc};
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};
use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::NetworkManagement::NetManagement::{
    LOCALGROUP_MEMBERS_INFO_2, NetApiBufferFree, NetLocalGroupGetMembers, NetUserGetInfo, NetUserSetInfo,
    UF_ACCOUNTDISABLE, USER_INFO_1, USER_INFO_24, USER_INFO_1020, USER_PRIV_ADMIN,
};
use windows::Win32::Security::{
    EqualSid, GetTokenInformation, LOGON32_LOGON_NETWORK, LOGON32_PROVIDER_DEFAULT, LogonUserW,
    LookupAccountSidW, PSID, SID_NAME_USE, SidTypeUser,
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
use windows::core::{PCWSTR, PWSTR};

/// Hours in Windows' weekly logon-hours map.
const UNITS_PER_WEEK: u32 = 168;
const TRAY_EXE: &str = "screentime-tray.exe";
/// Don't try to start the tray in a session more often than this.
const TRAY_RETRY: Duration = Duration::from_secs(60);

pub struct Windows {
    users: Vec<String>,
    /// Mirror the schedule into Windows logon hours (on unless SCREENTIME_LOGON_HOURS=off).
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
        let use_logon_hours = !std::env::var("SCREENTIME_LOGON_HOURS").is_ok_and(|v| v.eq_ignore_ascii_case("off"));
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
        list_sessions()
    }

    fn on_allowed_session(&self, session: &Session) {
        // Only into sessions that are staying: starting it in one being shut out crashed it.
        if self.is_managed(&session.user) {
            self.ensure_tray(session.id.parse().unwrap_or(0));
        }
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

    fn admins(&self) -> Vec<String> {
        local_admins()
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

/// Enabled local user accounts in BUILTIN\Administrators (looked up by SID, so it works
/// whatever the group is called in this Windows language).
pub fn local_admins() -> Vec<String> {
    let mut sid = [0u8; SECURITY_MAX_SID_SIZE as usize];
    let mut size = sid.len() as u32;
    let psid = PSID(sid.as_mut_ptr() as _);
    if unsafe { CreateWellKnownSid(WinBuiltinAdministratorsSid, None, Some(psid), &mut size) }.is_err() {
        return Vec::new();
    }
    let (mut name, mut domain) = ([0u16; 256], [0u16; 256]);
    let (mut name_len, mut domain_len) = (name.len() as u32, domain.len() as u32);
    let mut use_ = SID_NAME_USE::default();
    if unsafe {
        LookupAccountSidW(
            None,
            psid,
            Some(PWSTR(name.as_mut_ptr())),
            &mut name_len,
            Some(PWSTR(domain.as_mut_ptr())),
            &mut domain_len,
            &mut use_,
        )
    }
    .is_err()
    {
        return Vec::new();
    }
    let mut buf: *mut u8 = std::ptr::null_mut();
    let (mut read, mut total) = (0u32, 0u32);
    let rc = unsafe {
        NetLocalGroupGetMembers(None, PCWSTR(name.as_ptr()), 2, &mut buf, u32::MAX, &mut read, &mut total, None)
    };
    if rc != 0 || buf.is_null() {
        return Vec::new();
    }
    let members = unsafe { std::slice::from_raw_parts(buf as *const LOCALGROUP_MEMBERS_INFO_2, read as usize) };
    let computer = std::env::var("COMPUTERNAME").unwrap_or_default();
    let mut out = Vec::new();
    for m in members {
        if m.lgrmi2_sidusage != SidTypeUser {
            continue;
        }
        let full = unsafe { m.lgrmi2_domainandname.to_string() }.unwrap_or_default();
        let Some((domain, user)) = full.split_once('\\') else { continue };
        if domain.eq_ignore_ascii_case(&computer) && !is_disabled(user) {
            out.push(user.to_owned());
        }
    }
    unsafe { NetApiBufferFree(Some(buf as _)) };
    out
}

fn is_disabled(user: &str) -> bool {
    let name = wide(user);
    let mut buf: *mut u8 = std::ptr::null_mut();
    unsafe {
        if NetUserGetInfo(None, pcwstr(&name), 1, &mut buf) != 0 || buf.is_null() {
            return true;
        }
        let disabled = (*(buf as *const USER_INFO_1)).usri1_flags & UF_ACCOUNTDISABLE == UF_ACCOUNTDISABLE;
        NetApiBufferFree(Some(buf as _));
        disabled
    }
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
/// Accepts `name`, `COMPUTER\name`, `MicrosoftAccount\email` or an email. A local account
/// linked to a Microsoft account signs in with the Microsoft account's password, which only
/// works under its Microsoft name, so that is tried too.
pub fn verify_admin(admin: &str, password: &str) -> Result<(), String> {
    let (domain, user) = match admin.split_once('\\') {
        Some((d, u)) => (d.to_owned(), u.to_owned()),
        None if admin.contains('@') => ("MicrosoftAccount".to_owned(), admin.to_owned()),
        None => (".".to_owned(), admin.to_owned()),
    };
    let token = match logon(&domain, &user, password) {
        Ok(t) => t,
        Err(local_err) => match (domain.as_str(), microsoft_account_of(&user)) {
            (".", Some(email)) => logon("MicrosoftAccount", &email, password).map_err(|e| format!("sign-in failed: {e}"))?,
            _ => return Err(format!("sign-in failed: {local_err}")),
        },
    };
    let admin_ok = token_is_admin(token);
    unsafe {
        let _ = CloseHandle(token);
    }
    if admin_ok? { Ok(()) } else { Err(format!("{user} is not an administrator")) }
}

fn logon(domain: &str, user: &str, password: &str) -> windows::core::Result<HANDLE> {
    let (user_w, domain_w, pass_w) = (wide(user), wide(domain), wide(password));
    let mut token = HANDLE::default();
    unsafe {
        LogonUserW(
            pcwstr(&user_w),
            pcwstr(&domain_w),
            pcwstr(&pass_w),
            LOGON32_LOGON_NETWORK,
            LOGON32_PROVIDER_DEFAULT,
            &mut token,
        )
    }?;
    Ok(token)
}

/// The Microsoft account (email) a local account is linked to, if any.
fn microsoft_account_of(user: &str) -> Option<String> {
    let name = wide(user);
    let mut buf: *mut u8 = std::ptr::null_mut();
    unsafe {
        if NetUserGetInfo(None, pcwstr(&name), 24, &mut buf) != 0 || buf.is_null() {
            return None;
        }
        let info = &*(buf as *const USER_INFO_24);
        let email = (info.usri24_internet_identity.as_bool() && !info.usri24_internet_principal_name.is_null())
            .then(|| info.usri24_internet_principal_name.to_string().ok())
            .flatten();
        NetApiBufferFree(Some(buf as _));
        email.filter(|e| !e.is_empty())
    }
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

