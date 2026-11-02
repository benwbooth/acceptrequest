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

Scaffold only. Every binary is a stub.
