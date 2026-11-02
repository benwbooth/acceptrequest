use anyhow::{Context, Result, bail};
use std::path::PathBuf;
use std::time::Duration;

pub struct Config {
    /// Base URL of the acceptrequest server. Without one the daemon runs on its cached policy.
    pub server: Option<String>,
    pub token: Option<String>,
    /// Extra CA certificate (PEM) to trust for the server, e.g. for a self-signed one.
    pub ca_file: Option<PathBuf>,
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
        if users.iter().any(|u| u == "root") {
            bail!("root cannot be a managed user");
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
            users,
            state_dir: var("ACCEPTREQUEST_STATE_DIR")
                .unwrap_or_else(|| "/var/lib/acceptrequest".into())
                .into(),
            socket: var("ACCEPTREQUEST_SOCKET")
                .unwrap_or_else(|| acceptrequest_common::CLIENT_SOCKET.into())
                .into(),
            grace: Duration::from_secs(grace_secs),
            host: std::fs::read_to_string("/proc/sys/kernel/hostname")
                .map(|h| h.trim().to_owned())
                .unwrap_or_else(|_| "unknown".into()),
            admin_group: var("ACCEPTREQUEST_ADMIN_GROUP").unwrap_or_else(|| "wheel".into()),
            pam_service: var("ACCEPTREQUEST_PAM_SERVICE").unwrap_or_else(|| "acceptrequest".into()),
            pam_confdir: var("ACCEPTREQUEST_PAM_CONFDIR"),
        })
    }

    pub fn is_managed(&self, user: &str) -> bool {
        self.users.iter().any(|u| u == user)
    }
}
