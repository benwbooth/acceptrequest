use anyhow::Result;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    // TODO: sync policy from server and cache it on disk, watch logind sessions,
    // count active time, warn at 10/5/1 minutes, lock then terminate, and answer
    // PAM/greeter queries on acceptrequest_common::CLIENT_SOCKET.
    let _system = zbus::Connection::system().await?;
    tracing::info!("acceptrequest client started");
    Ok(())
}
