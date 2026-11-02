use anyhow::{Context, Result};
use std::path::PathBuf;

#[derive(Debug, Clone, Default)]
pub struct Config {
    pub listen: String,
    pub db_url: String,
    /// TLS certificate and key in PEM. Without them the server speaks plain HTTP,
    /// which is only meant for running behind a TLS-terminating reverse proxy.
    pub tls: Option<(PathBuf, PathBuf)>,
    /// Argon2 PHC string for the admin password.
    pub admin_password_hash: Option<String>,
    /// Where links in notifications point, e.g. `https://screentime.example:8443`.
    pub public_url: String,
    /// Full ntfy topic URL, e.g. `https://ntfy.sh/some-long-random-topic`.
    pub ntfy_url: Option<String>,
    pub ntfy_token: Option<String>,
    /// Daily minutes for a user the server hasn't seen before.
    pub default_allowance_minutes: u32,
}

impl Config {
    pub fn from_env() -> Result<Config> {
        let var = |name: &str| std::env::var(name).ok().filter(|v| !v.is_empty());

        // Under systemd, secrets passed with LoadCredential= are picked up by name.
        let credential = |name: &str| {
            let path = PathBuf::from(std::env::var_os("CREDENTIALS_DIRECTORY")?).join(name);
            path.exists().then(|| path.to_string_lossy().into_owned())
        };
        let var_or_credential = |name: &str, cred: &str| var(name).or_else(|| credential(cred));
        let from_file = |name: &str, cred: &str| -> Result<Option<String>> {
            match var_or_credential(name, cred) {
                Some(path) => Ok(Some(
                    std::fs::read_to_string(&path)
                        .with_context(|| format!("reading {name} at {path}"))?
                        .trim()
                        .to_owned(),
                )),
                None => Ok(None),
            }
        };

        let tls = match (
            var_or_credential("ACCEPTREQUEST_TLS_CERT", "tls-cert"),
            var_or_credential("ACCEPTREQUEST_TLS_KEY", "tls-key"),
        ) {
            (Some(cert), Some(key)) => Some((cert.into(), key.into())),
            (None, None) => None,
            _ => anyhow::bail!("set both ACCEPTREQUEST_TLS_CERT and ACCEPTREQUEST_TLS_KEY, or neither"),
        };
        let listen = var("ACCEPTREQUEST_LISTEN").unwrap_or_else(|| "127.0.0.1:8443".into());
        let scheme = if tls.is_some() { "https" } else { "http" };

        Ok(Config {
            public_url: var("ACCEPTREQUEST_PUBLIC_URL")
                .unwrap_or_else(|| format!("{scheme}://{listen}"))
                .trim_end_matches('/')
                .to_owned(),
            listen,
            db_url: var("ACCEPTREQUEST_DB").unwrap_or_else(|| "sqlite://acceptrequest.db".into()),
            tls,
            admin_password_hash: var("ACCEPTREQUEST_ADMIN_PASSWORD_HASH")
                .or(from_file("ACCEPTREQUEST_ADMIN_PASSWORD_HASH_FILE", "admin-hash")?),
            ntfy_url: var("ACCEPTREQUEST_NTFY_URL"),
            ntfy_token: var("ACCEPTREQUEST_NTFY_TOKEN").or(from_file("ACCEPTREQUEST_NTFY_TOKEN_FILE", "ntfy-token")?),
            default_allowance_minutes: match var("ACCEPTREQUEST_DEFAULT_ALLOWANCE") {
                Some(m) => m.parse().context("ACCEPTREQUEST_DEFAULT_ALLOWANCE")?,
                None => 120,
            },
        })
    }
}
