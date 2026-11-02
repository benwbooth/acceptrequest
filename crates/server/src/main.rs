use acceptrequest_server::{AppState, config::Config, db, open_db, router};
use anyhow::{Context, Result};
use std::io::{BufRead, IsTerminal};

const USAGE: &str = "usage:
  acceptrequest-server [serve]          run the server (configured by ACCEPTREQUEST_* variables)
  acceptrequest-server hash-password    read a password and print its hash for ACCEPTREQUEST_ADMIN_PASSWORD_HASH
  acceptrequest-server gen-cert DIR NAME...  make a private CA and a server certificate for NAME (host names / IPs)
  acceptrequest-server fingerprint CERT.pem  print a certificate's fingerprint for clients' serverFingerprint
  acceptrequest-server add-client HOST  register a machine and print its token (replaces any old one)
  acceptrequest-server list-clients";

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.iter().map(String::as_str).collect::<Vec<_>>().as_slice() {
        [] | ["serve"] => serve().await,
        ["hash-password"] => {
            let password = if std::io::stdin().is_terminal() {
                let p = rpassword::prompt_password("Admin password: ")?;
                if p != rpassword::prompt_password("Again: ")? {
                    anyhow::bail!("passwords don't match");
                }
                p
            } else {
                std::io::stdin().lock().lines().next().context("no password on stdin")??
            };
            if password.chars().count() < 7 {
                anyhow::bail!("use at least 7 characters");
            }
            println!("{}", acceptrequest_server::auth::hash_password(&password)?);
            Ok(())
        }
        ["add-client", host] => {
            let config = Config::from_env()?;
            let pool = open_db(&config.db_url).await?;
            println!("{}", db::add_client(&pool, host).await?);
            Ok(())
        }
        ["gen-cert", dir, names @ ..] => {
            let names: Vec<String> = names.iter().map(|n| n.to_string()).collect();
            acceptrequest_server::certs::generate(std::path::Path::new(dir), &names)
        }
        ["fingerprint", cert] => {
            println!("{}", acceptrequest_server::certs::pem_fingerprint(std::path::Path::new(cert))?);
            Ok(())
        }
        ["list-clients"] => {
            let config = Config::from_env()?;
            let pool = open_db(&config.db_url).await?;
            for c in db::list_clients(&pool).await? {
                println!("{}\t{}", c.hostname, c.last_seen.as_deref().unwrap_or("never"));
            }
            Ok(())
        }
        _ => {
            eprintln!("{USAGE}");
            std::process::exit(2);
        }
    }
}

async fn serve() -> Result<()> {
    let config = Config::from_env()?;
    if config.admin_password_hash.is_none() {
        tracing::warn!("no admin password set; the web UI can't be signed into");
    }
    if config.ntfy_url.is_none() {
        tracing::warn!("no ACCEPTREQUEST_NTFY_URL set; time requests won't reach your phone");
    }
    let pool = open_db(&config.db_url).await?;
    let addr: std::net::SocketAddr = config.listen.parse().context("ACCEPTREQUEST_LISTEN")?;
    let tls = config.tls.clone();
    let state = AppState::new(pool, config);
    tokio::spawn(acceptrequest_server::notify::follow_replies(state.clone()));
    let app = router(state).into_make_service();

    match tls {
        Some((cert, key)) => {
            let _ = rustls::crypto::ring::default_provider().install_default();
            let tls = axum_server::tls_rustls::RustlsConfig::from_pem_file(&cert, &key)
                .await
                .with_context(|| format!("loading TLS cert {} and key {}", cert.display(), key.display()))?;
            match acceptrequest_server::certs::pem_fingerprint(&cert) {
                Ok(f) => tracing::info!("certificate fingerprint (for clients' serverFingerprint): {f}"),
                Err(e) => tracing::warn!("couldn't fingerprint the certificate: {e:#}"),
            }
            tracing::info!("listening on https://{addr}");
            axum_server::bind_rustls(addr, tls).serve(app).await?;
        }
        None => {
            tracing::warn!("no TLS certificate set; serving plain HTTP, which is only safe behind a TLS proxy");
            tracing::info!("listening on http://{addr}");
            axum_server::bind(addr).serve(app).await?;
        }
    }
    Ok(())
}
