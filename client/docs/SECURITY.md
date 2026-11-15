# Desktop client security notes

## Local gateway

- Binds only to `127.0.0.0/8` and refuses a non-loopback listener.
- Serves the shell only on the address it advertises (plus a `localhost`
  alias); app listeners answer only for their own loopback address or browser
  host.
- Rejects unknown `Host` headers and treats `127.*` as a loopback address only
  when the complete host parses as a loopback IP.
- Rejects path-based `/p/` and `/n/<node>/p/` app surfaces on the shell
  listener.
- Has no CONNECT or TRACE handling and no arbitrary URL proxy.
- Strips hop-by-hop, forwarded, and proxy headers, including headers named by
  `Connection`.
- Rewrites the upstream Host and app cookie domains. Origin/Referer are rewritten
  only for requests from the local app origin; cross-site requests keep their
  real origin.
- Confines app framing to the shell origin with `frame-ancestors`, and marks
  app responses `Origin-Agent-Cluster: ?1` so sibling app subdomains cannot
  relax to one another with `document.domain`.
- The app iframe delegates microphone, camera, display capture, fullscreen, and
  clipboard permissions; the Tauri permission handler accepts all loopback and
  app-origin hosts.
- Serves the shell shim, including the native token, only at a per-run
  unguessable path and only for the shell document. Module/app documents do not
  receive it.
- Native endpoints require the native token, a shell-matching `Origin` when the
  browser sends one, and a non-cross-site `Sec-Fetch-Site`.
- Does not expose the Link account token to the dashboard or app origins.

## Native shell

- The dashboard is external content (`http://127.0.0.1:<port>`); Tauri IPC is
  not granted to it.
- Custom commands are listed in the Tauri app-command manifest. The `settings`
  window has its own capability; `main` has none of the privileged commands.
- The settings window uses only local styling and never loads hub CSS.
- Navigation is allowlisted to parsed loopback hosts, the canonical Orqella
  host, and active app-origin names; other HTTP(S) links open in the system
  browser. External URLs are validated as HTTP(S) without credentials; Windows
  uses `ShellExecuteW`, never `cmd.exe`.
- Deep links are parsed and validated; they never carry tokens.
- App surfaces are isolated by loopback origin, not by path prefix. Leases are
  persisted per hub and mount so an origin is not reused for a different app.
- Module iframes do not receive microphone permission; microphone use goes
  through the shell `media.record` bridge. Remote-hub microphone consent and
  macOS same-site cookie isolation remain product decisions.

## Credentials

- Link account tokens, SSH private keys, and iroh identity secrets live in the
  OS keychain.
- No plaintext fallback is implemented.
- Sign-out clears the account token and dashboard browsing data; the device
  identity stays in the keychain so signing back in does not create a new device.
- Tokens are omitted from logs and diagnostics. Log files and the profile are
  created with owner-only permissions (`0600`/`0700` on Unix); console and file
  writers both redact credentials, including multiline private keys.

## Transport

- Remote links use the core direct-only SSH/iroh path.
- Host-key pinning is mandatory; the client refuses a target without an
  advertised host-key fingerprint.
- A relay-only SSH path must fail before application bytes are opened.
- Managed relay credentials are control-plane/bootstrap only.
