# Design notes

## No bypass

The greeter is not the security boundary. A managed user who skips it (switching to a TTY,
SSH, another display manager, `su`) must hit the same wall, so enforcement is layered:

1. **PAM on every login path.** The client NixOS module adds `pam_acceptrequest.so` as a
   `required` account rule to `login`, `greetd`, `sshd`, `su`, `sudo`, `systemd-user` and any
   other service that can start a session for a managed user. Outside allowed time the module
   denies the login whichever front end asked.
2. **Daemon enforcement of running sessions.** The root daemon watches logind for every session
   that belongs to a managed user, whatever started it. When time runs out it locks the session,
   then calls `TerminateUser` after a short grace period. A session that started somehow without
   PAM still gets killed.
3. **Fail closed.** If the daemon is unreachable the PAM module denies managed users. If the
   server is unreachable the daemon uses its cached policy. A missing cache means downtime.
4. **Clock tampering.** Usage is counted with the monotonic clock. Wall time comes from the
   server on every sync, and a local clock that jumps is treated as suspect.
5. **The managed user can't touch the enforcer.** No `wheel` or sudo, no write access to
   `/etc` or `/nix`, and the daemon runs as root with a systemd unit the user can't stop.
6. **Physical access (outside the software).** A firmware password, Secure Boot or a locked boot
   order, and `boot.loader.systemd-boot.editor = false` so nobody can boot with
   `init=/bin/sh`. Without these anyone with the keyboard can get around any OS-level control.
   The client module will assert the editor setting.

## Warnings that don't interrupt

Warnings at 10, 5 and 1 minutes must not steal focus from a fullscreen game.

- They are drawn as a small toast in the lower-right corner on a `wlr-layer-shell` overlay
  surface (KWin supports it) with an empty input region and keyboard interactivity `none`. It
  can't take focus or receive clicks, and it appears above fullscreen windows.
- On X11 the fallback is an override-redirect, input-transparent window that is never mapped
  with focus.
- They are not routed through `org.freedesktop.Notifications`, because KDE and GNOME can
  suppress notifications in fullscreen or Do Not Disturb, and some setups raise the window.
- The toast fades after a few seconds and never makes a sound by default. A small "Request
  more time" hint can be triggered with a global shortcut, so the child never has to alt-tab
  out of the game.
- The in-session part runs as a per-user systemd service started by the daemon's policy, not
  as something the child can disable. If the child kills it the warnings stop, but enforcement
  in the root daemon doesn't.

## "My parent is here"

When a parent is physically present they can add time without the phone or the server.

- The greeter, the in-session overlay, or the `acceptrequest parent USER MINUTES` command sends the
  parent's account name and password to the root daemon over its local socket.
- The daemon checks that the account is in the admin group (`wheel` by default), is not a managed
  account and is not root, then verifies the password through the `acceptrequest` PAM service
  (plain unix password, no fingerprint, because the daemon answers the prompts itself).
- A grant adds its minutes to today's allowance and also lets the user stay on through downtime
  for that long. Grants made back to back stack. It works even before the machine has ever
  reached the server.
- Guessing is limited: each wrong answer takes 3 seconds, and after 5 wrong answers in 10 minutes
  the check is refused. Attempts are counted before the password is checked, so firing many guesses
  at once doesn't get around the limit.
- Grants are logged with the parent's name and reported to the server with the usage figures.

## Windows

The Windows service runs the same agent as the Linux daemon (`acceptrequest-client`'s `agent`
module) behind a `Platform` implementation:

- Sessions come from WTS (`WTSEnumerateSessionsW`); a session counts as in use when it is
  connected and unlocked (`WTSSessionInfoEx`).
- Locking is `WTSDisconnectSession` (a service can't call `LockWorkStation` for another session);
  ending is `WTSLogoffSession`.
- Windows logon hours (`NetUserSetInfo` level 1020) mirror the schedule for the next week, at hour
  granularity: an hour is open if any quarter of it is allowed. This is the fail-safe: sign-in is
  refused by Windows even if the service is stopped. The service refuses to touch administrator
  accounts.
- "My parent is here" uses `LogonUserW` and checks the token for BUILTIN\Administrators (present,
  deny-only, even in a UAC-filtered token), so a local or Microsoft-account administrator works.
- The named pipe `\\.\pipe\acceptrequest` speaks the same JSON lines as the Unix socket and is
  open to signed-in users only.
- Tamper resistance relies on the kid being a standard user: the service, its files and its data
  directory are writable only by SYSTEM and administrators, it restarts on failure and is
  registered for Safe Mode. A BIOS password is still advised.
