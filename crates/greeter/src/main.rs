use anyhow::Result;

fn main() -> Result<()> {
    // TODO: UI, greetd_ipc login flow over $GREETD_SOCK, and a "Request more time"
    // action that goes through the client daemon on screentime_common::CLIENT_SOCKET.
    println!("screentime greeter (stub)");
    Ok(())
}
