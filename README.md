# acceptrequest

Screen time controls for Linux, in the spirit of Windows Family Safety: daily allowances,
downtime schedules, and kids asking for more time with a notification on your phone.

## Pieces

| Crate | What it does |
|---|---|
| `crates/server` | Axum + SQLite policy server over HTTPS, password protected. Hosts the web UI and sends ntfy notifications with one-tap grant buttons. |
| `crates/web` | Leptos admin UI with desktop and mobile layouts: downtime schedules, allowances, bonus time (+0/15/30/45/60/custom minutes), pending requests. |
| `crates/client` | Root daemon on each machine. Tracks active time through logind, warns at 10/5/1 minutes, locks then terminates sessions, caches policy for offline use. |
| `crates/pam` | `pam_acceptrequest.so`, an account-phase module that blocks login outside allowed time by asking the local daemon. |
| `crates/greeter` | greetd greeter with a "Request more time" button. |
| `crates/common` | Shared types. |

## NixOS

```nix
inputs.acceptrequest.url = "github:benwbooth/acceptrequest";

# on the server
imports = [ acceptrequest.nixosModules.server ];
services.acceptrequest.server.enable = true;

# on each client
imports = [ acceptrequest.nixosModules.client ];
services.acceptrequest.client = {
  enable = true;
  serverUrl = "https://screentime.example:8443";
  managedUsers = [ "kid" ];
  greeter.enable = true;
};
```

## Status

- **Client daemon:** working. Counts unlocked session time through logind, warns at 10/5/1 minutes
  to subscribers on its socket, locks sessions when time runs out and ends them after a grace period,
  caches policy in `/var/lib/acceptrequest/state.json`, syncs with the server, queues time requests.
- **"My parent is here":** working in the daemon and the `acceptrequest` command; a parent's
  admin password adds time on the spot, even during downtime or offline.
- **PAM module:** working. Fails closed for managed users and shows the reason at login.
- **NixOS client module:** working, with assertions against the boot menu editor and managed users in wheel.
- **Server:** working. SQLite storage, per-day allowances shared across machines, downtime,
  bonuses, time requests, per-machine tokens, admin login (argon2, HttpOnly SameSite=Strict cookie,
  throttled), ntfy notifications with one-tap +N buttons and a page offering 0/15/30/45/60/custom,
  HTTPS via rustls. The admin JSON API is ready for the web UI.
- **Web UI, greeter, warning overlay:** not started.

## Setting up the server

```sh
acceptrequest-server hash-password > admin.hash      # then point adminPasswordHashFile at it
acceptrequest-server add-client kids-pc              # prints the token for that machine's tokenFile
```

The certificate must be a real server certificate (Let's Encrypt, `tailscale cert`, or one issued
by your own CA, with that CA given to clients as `caFile`). A bare self-signed certificate is
rejected by the client.

For notifications, install the ntfy app on your phone and subscribe to a long random topic, e.g.
`https://ntfy.sh/acceptrequest-<random>`, then set `services.acceptrequest.server.ntfy.url` to it.
`publicUrl` must be reachable from the phone for the buttons to work.

```nix
services.acceptrequest.server = {
  enable = true;
  openFirewall = true;
  publicUrl = "https://screentime.example.net:8443";
  tls = { certFile = "/var/lib/acme/screentime/cert.pem"; keyFile = "/var/lib/acme/screentime/key.pem"; };
  adminPasswordHashFile = "/run/secrets/acceptrequest-admin-hash";
  ntfy.url = "https://ntfy.sh/acceptrequest-<random>";
};
```

See [docs/DESIGN.md](docs/DESIGN.md) for the anti-bypass and warning design.
