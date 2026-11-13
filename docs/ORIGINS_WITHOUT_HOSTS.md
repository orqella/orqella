# Origins without `/etc/hosts`

Status: design note, not implemented. Today `orqella hosts install` writes the
shell and app hostnames to `/etc/hosts` for Safari. Chrome and Firefox resolve
`*.localhost` themselves; Safari uses the system resolver and does not resolve
arbitrary subdomains of `localhost`.

The goal is to remove the sudo prompt and the stale-entry problem without
giving up per-app browser origins.

## Constraints

1. The shell keeps a stable origin (`orqella.localhost:9477` today).
2. Each app surface needs a distinct origin so cookies, storage, and CSP do not
   mix with the shell or another app.
3. The daemon serves everything from one loopback port.
4. Module iframes stay opaque-origin; app frames may be same-origin with their
   own app.
5. No public listener, no TLS certificate authority to manage on the LAN.

## Options

### A. Loopback IP per app

Use `http://127.0.0.2:<port>`, `127.0.0.3:<port>`, etc. for app surfaces.

- Works in Safari without resolution changes on all platforms.
- Distinct origins are preserved.
- Requires one port per app (or one IP per app on the same port), because the
  host header would otherwise be an IP and every app would share an origin.
- Breaks human-readable app URLs and the `*.orqella.localhost` contract.
- Linux and Windows already allow the entire `127.0.0.0/8` block; macOS does too.

### B. mDNS/Bonjour names (`*.orqella.local`)

Register each app host with `dns-sd -R` (macOS) or an mDNS responder on
Linux/Windows, and use `http://<app>.orqella.local:<port>`.

- No `/etc/hosts` edit and no root.
- Safari resolves `.local` names through Bonjour.
- Requires renaming the canonical host and migrating cookies/storage.
- mDNS registration must be kept alive and removed on app deletion.
- Some networks block multicast; loopback registration is local-only, but the
  resolver behavior still needs a per-platform test.
- `.local` can collide with a real LAN host of the same name; use a unique
  suffix such as `orqella.local` and check for conflicts.

### C. `localtest.me`-style public wildcard DNS

`*.localhost` is special-cased; a public wildcard such as `*.localtest.me`
resolves to `127.0.0.1` without local configuration.

- No sudo and no daemon-side resolver.
- Depends on an external DNS service and an internet lookup for local pages.
- Leaks app names to that DNS provider.
- Not acceptable as the only path for a local-first product.

### D. One port per app on `127.0.0.1`

Serve each app on its own loopback port and use `http://127.0.0.1:<port>`.

- Simple and universally supported.
- Distinct origins, but ugly URLs and a port per app.
- Conflicts with the one-user-facing-port design goal.

## Recommendation

Adopt **B for the shell and apps on macOS**, with **A as the fallback** when
mDNS registration fails, and keep the existing host-file path as an explicit
compatibility command during the transition. On Linux and Windows, keep
`*.localhost` (Chrome/Firefox) and use A only if a browser needs it.

Migration outline:

1. Add a canonical-host alias setting: `orqella.localhost` (current) and
   `orqella.local` (mDNS). Keep the current host valid for at least one release.
2. Add an origin registrar to the daemon that registers the shell and every
   configured app with `dns-sd -R` on macOS and removes registrations on app
   deletion/shutdown.
3. Add a fallback allocator that maps each app to the next free loopback IP and
   a stable port, and use it when registration or resolution fails.
4. Change `orqella doctor` to test resolution for the shell and one app and to
   print which mechanism is active.
5. Keep `orqella hosts install` as a compatibility command; stop calling it
   from the macOS installer once B passes the Safari matrix.
6. Acceptance test: fresh macOS user, no sudo, Safari opens the shell and two
   app hosts, each with a distinct `localStorage` and cookie jar; app deletion
   removes the registration; reboot restores it.
7. Then remove the host-file code, CLI variant, installer hook, release
   checklist item, and tests.

Do not delete `orqella hosts` before step 6 passes; it is the only Safari path
today.

## Why not just use path-based apps?

A path on the shell origin (`orqella.localhost:9477/p/app/`) shares cookies,
storage, and service workers with the shell. That defeats the app-origin
isolation the proxy model exists to provide. Path-only surfaces are already
rejected for full apps.
