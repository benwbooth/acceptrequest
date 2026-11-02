# acceptrequest

Screen time controls for Linux, in the spirit of Windows Family Safety: daily allowances,
downtime schedules, and kids asking for more time with a notification on your phone.

## Pieces

| Crate | What it does |
|---|---|
| `crates/server` | Axum + SQLite policy server over HTTPS, password protected. Hosts the web UI and sends Web Push (or ntfy) notifications with one-tap grant buttons. |
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
  throttled), HTTPS via rustls, and a private CA generator for self-signed setups.
- **Notifications:** built-in Web Push (no extra app or service) with one-tap +N buttons, plus a
  page offering 0/15/30/45/60/custom. ntfy is optional.
- **Web UI:** working. Leptos, phone layout (bottom tabs) and desktop layout (sidebar): today's
  status and requests, add time, per-day limits and downtime, computers, notifications.
- **Greeter, warning overlay:** not started.

## Setting up the server

```sh
acceptrequest-server hash-password > admin.hash      # then point adminPasswordHashFile at it
acceptrequest-server gen-cert /var/lib/acceptrequest-tls screentime.lan 192.168.1.10
acceptrequest-server add-client kids-pc              # prints the token for that machine's tokenFile
```

`gen-cert` makes a private CA (`ca.pem`, `ca.key`) and a server certificate signed by it
(`cert.pem`, `key.pem`) for the names and addresses you list. Running it again reuses the CA.
Copy `ca.pem` to each computer and set `services.acceptrequest.client.caFile`, and install it on
your phone once:

- **iPhone:** AirDrop or email `ca.pem` to yourself, install the profile in Settings, then turn it on
  under Settings > General > About > Certificate Trust Settings.
- **Android:** Settings > Security > Encryption & credentials > Install a certificate > CA certificate.

A certificate from Let's Encrypt or `tailscale cert` works too, with no `caFile` needed. A bare
self-signed certificate (one that is its own CA) is rejected by the client.

### Notifications

Open the web UI on your phone, go to Alerts and tap "Turn on notifications". On iPhone, first add
the page to your Home Screen (Share > Add to Home Screen) and open it from there; iOS doesn't show
the +15/+30 buttons on web notifications, so tapping opens the page with every choice instead.
`publicUrl` must be reachable from the phone. ntfy also works if you'd rather use its app: set
`services.acceptrequest.server.ntfy.url` to a long random topic.

```nix
services.acceptrequest.server = {
  enable = true;
  openFirewall = true;
  publicUrl = "https://screentime.example.net:8443";
  tls = { certFile = "/var/lib/acceptrequest-tls/cert.pem"; keyFile = "/var/lib/acceptrequest-tls/key.pem"; };
  adminPasswordHashFile = "/run/secrets/acceptrequest-admin-hash";
};
```

See [docs/DESIGN.md](docs/DESIGN.md) for the anti-bypass and warning design.
