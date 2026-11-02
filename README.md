# acceptrequest

Screen time controls for Linux, in the spirit of Windows Family Safety: daily allowances,
downtime schedules, and kids asking for more time with a notification on your phone.

![Today view on desktop](docs/screenshots/desktop-today.jpg)

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

`gen-cert` makes a certificate for the names and addresses you list and prints its fingerprint.
Computers trust the server by that fingerprint, so there is nothing to install on them:

```nix
services.acceptrequest.client.serverFingerprint = "1F:E7:E4:...:3E:6A";
```

The fingerprint changes each time you run `gen-cert`; the server also logs it at startup, and
`acceptrequest-server fingerprint cert.pem` prints it.

**Phones are stricter.** Browsers only allow notifications from an HTTPS site they trust, and a
fingerprint can't be given to a phone browser. With a self-signed certificate the web UI works
after tapping through the warning, but notifications won't turn on. For notifications without
installing anything, use a certificate the phone already trusts:

- **Tailscale:** `tailscale cert yourserver.<tailnet>.ts.net`, then point `tls.certFile`/`keyFile` at it.
- **Your own domain:** Let's Encrypt with a DNS challenge through NixOS `security.acme`; the server
  doesn't need to be reachable from the internet.

Or install `ca.pem` from the `gen-cert` directory on the phone once (iPhone: install the profile,
then enable it under Settings > General > About > Certificate Trust Settings; Android: Settings >
Security > Install a certificate > CA certificate).

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
