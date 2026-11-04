//! Command-line front end to the client daemon, for terminals and SSH.

use screentime_common::{CLIENT_SOCKET, Decision, SocketRequest, SocketResponse};
use anyhow::{Context, Result, bail};
use std::io::{BufRead, BufReader, Write};

const USAGE: &str = "usage:
  screentime status [USER]
  screentime users                           every limited account on this computer
  screentime request [MINUTES] [REASON...]   ask a parent for more time
  screentime parent USER MINUTES             my parent is here: approve time with an admin password";

#[cfg(unix)]
fn connect(path: &str) -> std::io::Result<std::os::unix::net::UnixStream> {
    std::os::unix::net::UnixStream::connect(path)
}

/// On Windows the service listens on a named pipe, which opens like a file.
#[cfg(windows)]
fn connect(path: &str) -> std::io::Result<std::fs::File> {
    std::fs::OpenOptions::new().read(true).write(true).open(path)
}

fn send(request: &SocketRequest) -> Result<SocketResponse> {
    let default = if cfg!(windows) { screentime_common::CLIENT_PIPE } else { CLIENT_SOCKET };
    let socket = std::env::var("SCREENTIME_SOCKET").unwrap_or_else(|_| default.into());
    let mut stream = connect(&socket).with_context(|| format!("connecting to {socket}"))?;
    let mut line = serde_json::to_vec(request)?;
    line.push(b'\n');
    stream.write_all(&line)?;
    let mut reply = String::new();
    BufReader::new(stream).read_line(&mut reply)?;
    Ok(serde_json::from_str(&reply)?)
}

fn current_user() -> Result<String> {
    std::env::var(if cfg!(windows) { "USERNAME" } else { "USER" }).context("can't tell who is signed in")
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
        SocketResponse::Users { users } => {
            for u in users {
                println!("{}: {} minutes used, {:?}", u.username, u.used_minutes, u.decision);
            }
        }
        SocketResponse::Error { message } => bail!("{message}"),
    }
    Ok(())
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let request = match args.iter().map(String::as_str).collect::<Vec<_>>().as_slice() {
        ["status"] => SocketRequest::Status { username: current_user()? },
        ["users"] => SocketRequest::ListUsers,
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
