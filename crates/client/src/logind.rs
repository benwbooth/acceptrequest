//! The parts of systemd-logind's D-Bus API the daemon needs.

use anyhow::Result;
use zbus::proxy::CacheProperties;
use zbus::{Connection, proxy, zvariant::OwnedObjectPath};

#[proxy(
    interface = "org.freedesktop.login1.Manager",
    default_service = "org.freedesktop.login1",
    default_path = "/org/freedesktop/login1"
)]
trait Manager {
    fn list_sessions(&self) -> zbus::Result<Vec<(String, u32, String, String, OwnedObjectPath)>>;
    fn lock_session(&self, session_id: &str) -> zbus::Result<()>;
    fn terminate_user(&self, uid: u32) -> zbus::Result<()>;
}

#[proxy(interface = "org.freedesktop.login1.Session", default_service = "org.freedesktop.login1")]
trait Session {
    #[zbus(property)]
    fn active(&self) -> zbus::Result<bool>;
    #[zbus(property)]
    fn remote(&self) -> zbus::Result<bool>;
    #[zbus(property)]
    fn locked_hint(&self) -> zbus::Result<bool>;
    #[zbus(property)]
    fn class(&self) -> zbus::Result<String>;
}

#[derive(Debug, Clone)]
pub struct SessionInfo {
    pub id: String,
    pub uid: u32,
    pub user: String,
    pub active: bool,
    pub remote: bool,
    pub locked: bool,
    pub class: String,
}

impl SessionInfo {
    /// Whether this session is being used right now and should consume screen time.
    pub fn counts_as_use(&self) -> bool {
        self.class == "user" && !self.locked && (self.active || self.remote)
    }
}

pub struct Logind {
    conn: Connection,
}

impl Logind {
    pub async fn connect() -> Result<Logind> {
        Ok(Logind { conn: Connection::system().await? })
    }

    async fn manager(&self) -> zbus::Result<ManagerProxy<'_>> {
        ManagerProxy::new(&self.conn).await
    }

    pub async fn sessions(&self) -> Result<Vec<SessionInfo>> {
        let mut out = Vec::new();
        for (id, uid, user, _seat, path) in self.manager().await?.list_sessions().await? {
            let s = SessionProxy::builder(&self.conn)
                .path(path)?
                .cache_properties(CacheProperties::No)
                .build()
                .await?;
            // A session can vanish between listing and querying it.
            let Ok(class) = s.class().await else { continue };
            out.push(SessionInfo {
                id,
                uid,
                user,
                active: s.active().await.unwrap_or(false),
                remote: s.remote().await.unwrap_or(false),
                locked: s.locked_hint().await.unwrap_or(false),
                class,
            });
        }
        Ok(out)
    }

    pub async fn lock_session(&self, id: &str) -> Result<()> {
        Ok(self.manager().await?.lock_session(id).await?)
    }

    pub async fn terminate_user(&self, uid: u32) -> Result<()> {
        Ok(self.manager().await?.terminate_user(uid).await?)
    }
}
