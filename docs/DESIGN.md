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
   The module asserts the editor setting.

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
