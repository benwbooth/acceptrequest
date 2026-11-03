//! The service's named pipe: the same JSON-lines protocol as the Linux Unix socket.

use super::{pcwstr, wide};
use acceptrequest_client::agent::{Agent, Platform};
use anyhow::{Context, Result};
use std::sync::Arc;
use tokio::net::windows::named_pipe::{NamedPipeServer, ServerOptions};
use windows::Win32::Security::Authorization::{
    ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
};
use windows::Win32::Security::{PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES};

/// Signed-in users may read and write; SYSTEM and administrators get full control.
const PIPE_SDDL: &str = "D:(A;;GRGW;;;AU)(A;;GA;;;SY)(A;;GA;;;BA)";

fn create(name: &str, first: bool) -> Result<NamedPipeServer> {
    let sddl = wide(PIPE_SDDL);
    let mut descriptor = PSECURITY_DESCRIPTOR::default();
    unsafe { ConvertStringSecurityDescriptorToSecurityDescriptorW(pcwstr(&sddl), SDDL_REVISION_1, &mut descriptor, None) }
        .context("building the pipe's security descriptor")?;
    let mut attrs = SECURITY_ATTRIBUTES {
        nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: descriptor.0,
        bInheritHandle: false.into(),
    };
    // The descriptor is small and created once per connection; leaking it is simpler than
    // tracking when Windows is done with it.
    let server = unsafe {
        ServerOptions::new()
            .first_pipe_instance(first)
            .create_with_security_attributes_raw(name, &mut attrs as *mut _ as *mut _)
    }
    .with_context(|| format!("creating pipe {name}"))?;
    Ok(server)
}

/// Accept connections forever, one task per client.
pub async fn serve<P: Platform>(agent: Arc<Agent<P>>, name: String) -> Result<()> {
    let mut server = create(&name, true)?;
    loop {
        server.connect().await?;
        let connected = server;
        server = create(&name, false)?;
        let agent = agent.clone();
        tokio::spawn(async move {
            let (r, w) = tokio::io::split(connected);
            if let Err(e) = agent.handle_connection(r, w).await {
                tracing::debug!("pipe connection ended: {e:#}");
            }
        });
    }
}
