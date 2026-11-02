//! Command-line front end to the client daemon, for terminals and SSH.

use acceptrequest_common::{CLIENT_SOCKET, Decision, SocketRequest, SocketResponse};
use anyhow::{Context, Result, bail};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;

const USAGE: &str = "usage:
  acceptrequest status [USER]
  acceptrequest request [MINUTES] [REASON...]   ask a parent for more time
  acceptrequest parent USER MINUTES             my parent is here: approve time with an admin password";

fn send(request: &SocketRequest) -> Result<SocketResponse> {
    let socket = std::env::var("ACCEPTREQUEST_SOCKET").unwrap_or_else(|_| CLIENT_SOCKET.into());
    let mut stream = UnixStream::connect(&socket).with_context(|| format!("connecting to {socket}"))?;
    let mut line = serde_json::to_vec(request)?;
    line.push(b'\n');
    stream.write_all(&line)?;
    let mut reply = String::new();
    BufReader::new(stream).read_line(&mut reply)?;
    Ok(serde_json::from_str(&reply)?)
}

fn current_user() -> Result<String> {
    std::env::var("USER").context("USER is not set")
}

fn prompt(label: &str) -> Result<String> {
    print!("{label}");
    std::io::stdout().flush()?;
    let mut s = String::new();
    std::io::stdin().read_line(&mut s)?;
    Ok(s.trim().to_owned())
}

fn report(response: SocketResponse) -> Result<()> {
    match response {
        SocketResponse::Decision { decision, used_minutes } => {
            match decision {
                Decision::Allowed { remaining_minutes: Some(m) } => println!("{m} minutes left"),
                Decision::Allowed { remaining_minutes: None } => println!("No limit right now"),
                Decision::Denied { reason } => println!("{reason}"),
            }
            println!("{used_minutes} minutes used today");
        }
        SocketResponse::Unmanaged => println!("That account has no screen time limits."),
        SocketResponse::RequestSent => println!("Request sent."),
        SocketResponse::RequestQueued => println!("Request saved; it will be sent when the server is reachable."),
        SocketResponse::Granted { minutes } => println!("Added {minutes} minutes."),
        SocketResponse::Warning { remaining_minutes } => println!("{remaining_minutes} minutes left"),
        SocketResponse::Error { message } => bail!("{message}"),
    }
    Ok(())
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let request = match args.iter().map(String::as_str).collect::<Vec<_>>().as_slice() {
        ["status"] => SocketRequest::Status { username: current_user()? },
        ["status", user] => SocketRequest::Status { username: (*user).to_owned() },
        ["request", rest @ ..] => {
            let (minutes, reason) = match rest.split_first() {
                Some((m, reason)) if m.parse::<u32>().is_ok() => (m.parse().ok(), reason),
                _ => (None, rest),
            };
            SocketRequest::RequestTime {
                username: current_user()?,
                minutes,
                reason: (!reason.is_empty()).then(|| reason.join(" ")),
            }
        }
        ["parent", user, minutes] => {
            let minutes = minutes.parse().context("MINUTES must be a number")?;
            let admin = prompt("Parent's account name: ")?;
            let password = rpassword::prompt_password("Password: ")?;
            SocketRequest::ParentGrant { username: (*user).to_owned(), minutes, admin, password }
        }
        _ => {
            eprintln!("{USAGE}");
            std::process::exit(2);
        }
    };
    report(send(&request)?)
}
