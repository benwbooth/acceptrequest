//! The Linux daemon: logind for sessions, PAM for "My parent is here", a Unix socket for
//! the PAM module, greeter and CLI.

#[cfg(target_os = "linux")]
mod linux {
    use screentime_client::agent::{Agent, Platform, Session};
    use screentime_client::config::Config;
    use screentime_client::logind::Logind;
    use screentime_client::parent;
    use anyhow::{Context, Result};
    use std::os::unix::fs::PermissionsExt;
    use std::sync::Arc;
    use tokio::net::UnixListener;

    struct Linux {
        logind: Logind,
        admin_group: String,
    }

    impl Platform for Linux {
        async fn sessions(&self) -> Result<Vec<Session>> {
            Ok(self
                .logind
                .sessions()
                .await?
                .into_iter()
                .filter(|s| s.class == "user")
                .map(|s| Session { in_use: s.counts_as_use(), locked: s.locked, id: s.id, user: s.user })
                .collect())
        }

        async fn lock(&self, session: &Session) -> Result<()> {
            self.logind.lock_session(&session.id).await
        }

        async fn end_sessions(&self, user: &str, _sessions: &[Session]) -> Result<()> {
            let uid = self
                .logind
                .sessions()
                .await?
                .into_iter()
                .find(|s| s.user == user)
                .map(|s| s.uid)
                .context("no session left to end")?;
            self.logind.terminate_user(uid).await
        }

        fn local_accounts(&self) -> Vec<screentime_common::LocalAccount> {
            let admins = parent::group_members(&self.admin_group);
            parent::human_accounts()
                .into_iter()
                .map(|name| screentime_common::LocalAccount {
                    admin: admins.contains(&name),
                    name,
                    configured_kid: None,
                })
                .collect()
        }

        fn admins(&self) -> Vec<String> {
            parent::group_members(&self.admin_group).into_iter().filter(|u| u != "root").collect()
        }
    }

    fn bind_socket(path: &std::path::Path) -> Result<UnixListener> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let _ = std::fs::remove_file(path);
        let listener = UnixListener::bind(path).with_context(|| format!("binding {}", path.display()))?;
        // PAM runs as root, but the greeter and the in-session overlay run as ordinary users.
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o666))?;
        Ok(listener)
    }

    pub async fn main() -> Result<()> {
        let config = Config::from_env()?;
        let platform = Linux {
            logind: Logind::connect().await?,
            admin_group: config.admin_group.clone(),
        };
        let listener = bind_socket(&config.socket)?;
        let agent = Agent::new(config, platform)?;

        let server = agent.clone();
        tokio::spawn(async move {
            loop {
                match listener.accept().await {
                    Ok((stream, _)) => {
                        let agent = server.clone();
                        tokio::spawn(async move {
                            let (r, w) = stream.into_split();
                            if let Err(e) = agent.handle_connection(r, w).await {
                                tracing::debug!("socket connection ended: {e:#}");
                            }
                        });
                    }
                    Err(e) => tracing::error!("accept failed: {e:#}"),
                }
            }
        });
        Arc::clone(&agent).run().await;
        Ok(())
    }
}

#[cfg(target_os = "linux")]
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();
    linux::main().await
}

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("screentime-client is the Linux daemon; on Windows use screentime-service.");
    std::process::exit(1);
}
