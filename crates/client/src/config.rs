use anyhow::{Context, Result, bail};
use std::path::PathBuf;
use std::time::Duration;

pub struct Config {
    /// Base URL of the acceptrequest server. Without one the daemon runs on its cached policy.
    pub server: Option<String>,
    pub token: Option<String>,
    /// Extra CA certificate (PEM) to trust for the server, e.g. for a self-signed one.
    pub ca_file: Option<PathBuf>,
    /// SHA-256 of the server's certificate. When set, only that certificate is accepted,
    /// which lets a self-signed server work without installing a CA.
    pub server_fingerprint: Option<[u8; 32]>,
    pub users: Vec<String>,
    pub state_dir: PathBuf,
    pub socket: PathBuf,
    /// How long a session stays locked after time runs out before it is terminated.
    pub grace: Duration,
    pub host: String,
    /// Members of this group can approve time on the spot with "My parent is here".
    pub admin_group: String,
    pub pam_service: String,
    /// Replaces /etc/pam.d for the parent check; only for tests.
    pub pam_confdir: Option<String>,
}

impl Config {
    pub fn from_env() -> Result<Config> {
        let var = |name: &str| std::env::var(name).ok().filter(|v| !v.is_empty());

        let users: Vec<String> = var("ACCEPTREQUEST_USERS")
            .unwrap_or_default()
            .split(',')
            .map(|u| u.trim().to_owned())
            .filter(|u| !u.is_empty())
            .collect();
        if users.iter().any(|u| u == "root" || u.eq_ignore_ascii_case("administrator")) {
            bail!("an administrator account cannot be a managed user");
        }

        let token = match var("ACCEPTREQUEST_TOKEN_FILE") {
            Some(path) => Some(
                std::fs::read_to_string(&path)
                    .with_context(|| format!("reading token file {path}"))?
                    .trim()
                    .to_owned(),
            ),
            None => None,
        };

        let grace_secs = match var("ACCEPTREQUEST_GRACE_SECS") {
            Some(s) => s.parse().context("ACCEPTREQUEST_GRACE_SECS")?,
            None => 120,
        };

        Ok(Config {
            server: var("ACCEPTREQUEST_SERVER").map(|s| s.trim_end_matches('/').to_owned()),
            token,
            ca_file: var("ACCEPTREQUEST_CA_FILE").map(PathBuf::from),
            server_fingerprint: match var("ACCEPTREQUEST_SERVER_FINGERPRINT") {
                Some(f) => Some(
                    crate::pinning::parse_fingerprint(&f)
                        .context("ACCEPTREQUEST_SERVER_FINGERPRINT must be a SHA-256 fingerprint")?,
                ),
                None => None,
            },
            users,
            state_dir: var("ACCEPTREQUEST_STATE_DIR").unwrap_or_else(|| DEFAULT_STATE_DIR.into()).into(),
            socket: var("ACCEPTREQUEST_SOCKET").unwrap_or_else(|| DEFAULT_SOCKET.into()).into(),
            grace: Duration::from_secs(grace_secs),
            host: hostname(),
            admin_group: var("ACCEPTREQUEST_ADMIN_GROUP").unwrap_or_else(|| DEFAULT_ADMIN_GROUP.into()),
            pam_service: var("ACCEPTREQUEST_PAM_SERVICE").unwrap_or_else(|| "acceptrequest".into()),
            pam_confdir: var("ACCEPTREQUEST_PAM_CONFDIR"),
        })
    }

    /// Windows user names are case-insensitive; Linux ones aren't.
    pub fn is_managed(&self, user: &str) -> bool {
        self.users.iter().any(|u| if cfg!(windows) { u.eq_ignore_ascii_case(user) } else { u == user })
    }

    /// The configured spelling of `user`, so state is keyed the same however Windows reports it.
    pub fn managed_name(&self, user: &str) -> Option<&str> {
        self.users
            .iter()
            .find(|u| if cfg!(windows) { u.eq_ignore_ascii_case(user) } else { *u == user })
            .map(String::as_str)
    }
}

#[cfg(unix)]
const DEFAULT_STATE_DIR: &str = "/var/lib/acceptrequest";
#[cfg(windows)]
const DEFAULT_STATE_DIR: &str = r"C:\ProgramData\acceptrequest";
#[cfg(unix)]
const DEFAULT_SOCKET: &str = acceptrequest_common::CLIENT_SOCKET;
#[cfg(windows)]
const DEFAULT_SOCKET: &str = acceptrequest_common::CLIENT_PIPE;
#[cfg(unix)]
const DEFAULT_ADMIN_GROUP: &str = "wheel";
#[cfg(windows)]
const DEFAULT_ADMIN_GROUP: &str = "Administrators";

fn hostname() -> String {
    #[cfg(unix)]
    let name = std::fs::read_to_string("/proc/sys/kernel/hostname").ok();
    #[cfg(windows)]
    let name = std::env::var("COMPUTERNAME").ok();
    name.map(|h| h.trim().to_owned()).filter(|h| !h.is_empty()).unwrap_or_else(|| "unknown".into())
}
