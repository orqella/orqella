# Orqella security audit — remediation checklist

Source audit: 2026-11-14. Scope: `orqella/`, `cloud/`, first-party `modules/*`, `mobile/`, `website/`.
Method: static review of source, configs, migrations, CI, and docs. No live exploitation or fuzzing.
Vendored `novnc`/`pako`, third-party SDK internals, and the full `vendor/iroh-webrtc-transport` were not deeply audited.

Status legend:

- `[ ]` not started
- `[~]` in progress
- `[x]` done
- `[!]` blocked on a decision
- `[-]` accepted risk / deferred with rationale

Rule for this pass: **do not make a behavior-changing fix without asking first.** Behavior-preserving hardening can proceed directly.

---

## Decisions that need the owner

These change product behavior, compatibility, or the threat model. Do not implement until answered.

- [x] **D1. Peer API scope.** Owner chose **C: keep all peer routes**, rotate/invalidate peer tokens on device revocation, add per-pair tokens, and document that a device token is full-account trust. Do not remove remote build/update/RPC.
- [x] **D2. Update channel.** Covered by D1: keep the current path; harden tokens and document that updates are preview-only until a signed channel exists.
- [x] **D3. Agent environment.** Owner chose **A: `env_clear()`**, passing only `PATH`, `HOME`, `TMPDIR`, `LANG`, and configured `[agent].env`.
- [x] **D4. Module signature policy.** Owner chose **A: require signatures for git installs and updates**; unsigned code requires an explicit `--allow-unsigned`/UI confirmation. Local path installs remain explicit-trust.
- [x] **D5. Sandbox network isolation.** Owner chose **A: private network namespace for sandboxed sidecars**, with a manifest opt-in for network access.
- [x] **D6. Browser client.** Owner chose **A: fix it now** — remove service-worker sandbox/CSP rewrites, escape Link data, validate postMessage sources and app URLs.
- [x] **D7. VM Lab.** Owner chose **A: fix QMP/VNC/seed hardening now.**
- [x] **D8. Voice Assistant engine API.** Owner chose **A: generate and require a token**, wire it into the sidecar/HA integration, fail closed.
- [x] **D9. Home Assistant / OpenCode credential-host binding.** Owner chose **A: bind the credential to its configured host** and require confirmation when it changes.

---

## P0 — before any public release

### P0-1. VM Lab guest → host escape
- [x] Replace TCP QMP with a Unix-domain QMP socket (`-qmp unix:...,server=on,wait=off`); update `qmp()` to use `AF_UNIX`; set socket mode `0600`. Windows keeps a loopback TCP fallback (documented residual).
- [x] Add VNC authentication or switch VNC to a Unix socket; do not leave unauthenticated TCP VNC on loopback. Per-VM password is set via QMP and auto-filled in the console.
- [-] Replace the `10.0.2.2` seed HTTP server with a NoCloud seed disk, or add `restrict=on` and another guest-visible seed mechanism. Owner chose to keep the per-VM 128-bit path token; disk seed deferred.
- [x] Validate `guest_user`/`windows_user`; use `ssh -l <user> 127.0.0.1`; shell-quote `ssh_command()` output.
- [x] Contain `image_source()` paths (`is_relative_to(DATA)` or explicit image root); reject non-image extensions, credential paths, and non-`http(s)` URLs. An HTTP host allowlist remains TODO.
- [-] The Windows guest password is no longer logged, but it must remain in VM metadata to build the unattended answer file. The VM directory is user-only; protect VM Lab data accordingly.
- Evidence: `modules/vm-lab/backend/server.py:679,695-696,719,570,588-597,499-535,1013-1049,1086-1088,1132-1139,1194-1203`.

### P0-2. Proxy forwards the dashboard session cookie
- [x] Remove only the `orqella_session` cookie pair from `Cookie` before proxying HTTP and WebSocket requests; preserve app-specific cookies.
- [x] Drop `orqella_session` from upstream `Set-Cookie` responses (path-mode session fixation/pollution).
- [x] Add regression tests: upstream never sees `orqella_session`; app cookies still pass; WS path strips it.
- [-] Host-mode app origins are preferred and the desktop gateway blocks same-origin app paths; a daemon-wide requirement is a product decision and was not added.
- Evidence: `orqella/src/proxy.rs:203-208,314-321,453-470`; `orqella/src/http/proxies.rs:183-200`.

### P0-3. Peer token is an RCE capability
- [x] Decide D1: keep peer routes; harden tokens.
- [x] Per-source tokens are in place. `orqella rotate-peer` regenerates this machine's secret, re-registers with Link, and other machines refresh on their next Link restart.
- [x] Use per-source/target peer tokens rather than one static token per machine. Link now returns `source-id.HMAC(target-secret, source-id)` and the target verifies it; legacy raw tokens are accepted for migration.
- [x] Add an audit event for peer build/update/RPC actions.
- Evidence: `orqella/src/http/build.rs:338-347`; `orqella/src/http/module_pages.rs:280-290`; `orqella/src/http/updates.rs:297-319`; `cloud/link-api/app.py:3040-3044`.

### P0-4. Unsigned update path
- [x] Decide D2: keep the preview update path; harden peer tokens and document that updates are preview-only.
- [-] Add signed artifact/metadata verification and rollback protection before executing the installer. Accepted preview risk by owner decision; track before public release.
- [-] Pin the installer file or verify its checksum/signature; accepted preview risk with the unsigned update path.
- [-] Require local confirmation for peer-triggered updates; owner chose to keep remote update behavior with hardened peer tokens.
- Evidence: `orqella/src/update.rs:202-284`; `install-linux.sh:15-16`; `orqella/src/http/updates.rs:184-211,297-319`.

### P0-5. Browser client sandbox/XSS
- [x] Decide D6: fix it now.
- [x] Escape `device.name`, `device.kind`, and `web_endpoint_id` in `web-client/client/app.js`.
- [x] Remove `allow-same-origin` rewriting and CSP/XFO removal from `web-client/sw.js` and `app-sw.js`. Module surfaces are explicitly disabled in the browser client.
- [x] Validate `event.source` in service-worker messages; do not let any client overwrite `registeredTopClientId`.
- [x] Validate `app-client` `target` scheme and postMessage target; escape inline app tokens.
- [x] Device-name escaping is covered by a static regression test; a browser-client CSP is in place.
- Evidence: `orqella/web-client/client/app.js:504-506`; `web-client/sw.js:16-24,71-97,141-143,163-207`; `web-client/app-sw.js:75`; `web-client/app-client/app-client.js:5,15-25,61`.

### P0-6. Voice Assistant unauthenticated action API
- [x] Decide D8: require a generated token.
- [x] Require a token for `/api/conversation`, `/api/stt`, and all engine routes; fail closed when empty.
- [x] `hmac.compare_digest`, fail-closed token, JSON content-type, and loopback Origin checks are in place; Unix socket remains optional.
- [x] Wire the generated token into the sidecar and Home Assistant integration via the shared `voice-engine.token` file.
- Evidence: `modules/voice-assistant/backend/engine/server.py:64-68`; `backend/config.default.toml:35-38`; `backend/engine/actions.py:49-58`; `backend/engine/router.py:239-250`.

### P0-7. Agent environment and permission semantics
- [x] Decide D3: clear the environment.
- [x] Pass `PATH`, `HOME`, `TMPDIR`, `LANG`, and configured `[agent].env` only.
- [-] Owner chose to keep arbitrary agent roots and no per-job confirmation. The permission sheet warns, the agent command is shown in Settings, and module backend command/read paths are shown in module details.
- [-] Require an allowlisted root or per-job confirmation for `agent.start`; owner chose to keep arbitrary roots with the explicit `agent` permission.
- [-] Add a restricted profile for the System “identify connection” flow; the feature remains explicitly user-triggered and the `agent` permission is the gate.
- Evidence: `orqella/src/agent.rs:424-516` (no `.env_clear()`); `orqella/src/config.rs:245-247`; `orqella/src/http/rpc.rs:236-258`; `orqella/src/agent.rs:273-286`; `modules/system/backend/server.py:1376-1425`.

### P0-8. Module read paths
- [x] Normalize `read_paths` before validation: expand `~`, collapse `.`/`//`, canonicalize lexically, and compare against the user's home.
- [x] Block absolute home paths for `.ssh`, `.gnupg`, `.aws`, browser profile dirs, Orqella config/data, and other credential roots.
- [x] Add tests for `/home/<user>/.ssh`, `~//.ssh`, `~/.ssh/`, and `~/.aws`.
- [x] Include `read_paths`, `command`, `sandbox`, and `network` in `ManifestSummary`; render them in the module settings UI.
- [x] Keep `protected_read_path` in the sidecar as defense-in-depth.
- Evidence: `orqella/src/modules/manifest.rs:577-615`; `orqella/src/modules/sidecar.rs:1020-1028,952-958`; `orqella/src/modules/manifest.rs:435-457`; `orqella/web/js/settings.js:662-681`.

### P0-9. Sandbox network boundary
- [x] Decide D5: private network namespace with an opt-in. Implementation deferred pending a sidecar transport bridge (owner chose defer).
- [-] Network isolation is deferred by owner decision: a private netns would cut off the daemon's sidecar HTTP bridge, so `backend.network` is reserved and documented. The sandbox remains filesystem-only.
- [-] Runtime network-isolation test is blocked by the owner-deferred sidecar transport bridge; the sandbox is documented as filesystem-only for now.
- Evidence: `orqella/src/modules/sidecar.rs:904-979`; `orqella/src/http.rs:101-106,460-487`; `orqella/src/http/shell.rs:17-24`.

---

## P1 — before paid Link / public production

### P1-1. Module signature policy
- [x] Decide D4: require signatures for git installs/updates with explicit unsigned override.
- [x] Verify `ORQELLA_SIGNATURE.json` for git installs and updates; unsigned installs require `allow_unsigned`.
- [x] Module detail now reports signature signed/trusted/fingerprint. UI badge remains a small follow-up.
- [x] Re-consent when `command`, `artifacts`, `requirements`, `python`, `sandbox`, or `read_paths` change on update.
- Evidence: `orqella/src/http/modules.rs:389-396,440-468`; `orqella/src/modules/store.rs:430-443,754-762`.

### P1-2. Cloud `/v1/web/relay` endpoint binding
- [x] Require `iroh_endpoint_id` to equal the caller device's registered `web_endpoint_id`.
- [x] Audit the device-to-endpoint binding (`relay.web.credentials`).
- Evidence: `cloud/link-api/app.py:2594-2597`; compare `:2490-2509`.

### P1-3. Cloud signing key handling
- [x] Require an explicit signing-key path/secret outside local development; fail closed if missing.
- [x] The default `/tmp` path is gone and env is required; key files must be owned by the service user and key directories cannot be world-writable without the sticky bit.
- [x] Add a key ID and a rotation procedure. Tickets carry `kid`; daemons keep a key ring refreshed from `/v1/config` every 60s; `LINK_ADDITIONAL_SIGNING_PUBLIC_KEYS` supports a two-phase rotation documented in `cloud/docs/DEPLOYMENT.md`.
- Evidence: `cloud/link-api/app.py:110-112,939-973`.

### P1-4. Cloud password and login abuse controls
- [x] Move new password hashes to scrypt `N=2^17` with a versioned format; legacy hashes verify and rehash on login.
- [x] Unknown-user timing uses a dummy scrypt derivation; per-email lockout uses `login_failures` with configurable failure/window/lockout values.
- [x] Passkey login options no longer return an account-specific `allowCredentials` list; known, unknown, and omitted emails produce the same discoverable-credential options.
- Evidence: `cloud/link-api/app.py:1234-1244,1868-1883,2087-2113`.

### P1-5. Billing internal surface
- [x] Bind `BILLING_HOST` to `127.0.0.1` by default; Fly config overrides.
- [x] Set `trust_env=False` on internal `httpx` clients.
- [x] Add a per-IP rate limit to billing internal routes; audit records still use the entitlement-change history.
- [-] Keep the static internal token on the encrypted Fly private network; mTLS would add certificate distribution without changing the trust boundary. Revisit if the Billing API is exposed beyond the private network.
- Evidence: `cloud/billing-api/app.py:74,200-216,1125-1147,2380-2425`; `cloud/link-api/app.py:2336,2472,2640`.

### P1-6. Consent phishing and notifications
- [-] Owner chose email notifications for new device approval/registration only; GitHub installation bindings remain audit-only.
- [x] Require a session created within 15 minutes for device approval (`LINK_DEVICE_APPROVAL_FRESH_MINUTES`); pending codes already expire after 10 minutes.
- [x] Device start returns a 6-digit confirmation code. The initiating client displays it; the approval page requires the user to type it. `/v1/device/info` deliberately does not return it, so a phished user code alone cannot be approved. **Compatibility:** pre-confirmation clients must upgrade before they can approve a new device.
- [x] Publisher polling requires the per-session poll secret; the response includes the installation account, and the browser success page shows it before the daemon uses the token.
- Evidence: `cloud/link-api/app.py:2180-2318`; `cloud/github-publisher/app.py:368-384,424-469`.

### P1-7. Publisher limits
- [x] Rate-limit `POST /v1/sessions`; expired rows are cleaned in `init_db`.
- [x] Keep the 30-day bridge-token TTL per owner decision; added installation-scoped token listing and revocation (`GET /v1/tokens`, `POST /v1/tokens/{id}/revoke`).
- Evidence: `cloud/github-publisher/app.py:52-55,368-384,519-582,615-622`.

### P1-8. Token host binding for modules
- [x] Decide D9: bind the credential host.
- [x] Bind HA tokens to the configured host; OpenCode refuses to send the local service password to a non-loopback `base_url`.
- [x] Refuse the local OpenCode password to non-loopback URLs; HA still allows an explicitly configured remote URL/token.
- Evidence: `modules/home-assistant/backend/server.py:38-39,115-137`; `modules/opencode/backend/server.py:22-23,65-73,158-187`.

### P1-9. Web bridge admin-token separation
- [x] Replace the injected admin token with a per-daemon bridge token.
- [x] Trust-store changes still require the CLI admin token; the bridge token cannot call them.
- Evidence: `orqella/src/web.rs:424`; `orqella/src/state.rs:37-39`; `orqella/src/http/modules.rs:254-292`.

### P1-10. Housekeeping fixes
- [x] System: reject unit names starting with `-`; insert `--` before the unit argument.
- [x] Workspace picker: reject i3 workspace names containing `;`/newlines/control characters.
- [x] Modules: fail closed when `ORQELLA_MODULE_TOKEN` is empty; constant-time compares where updated.
- [x] Rotate module `ws_token` on remove/install.
- [x] Add `Secure` to the session cookie when `X-Forwarded-Proto` is HTTPS.
- [x] Warn that backups contain live credentials.
- [x] Add a CSP to the website and docs; `marked` output is sanitized before insertion, and the CSP is regression-tested.
- [x] Pin GitHub Actions to commit SHAs in the public and cloud workflows. Python base-image digest pinning remains a follow-up.
- [x] Correct `docs/SECURITY.md`: the SSH client verifies the host fingerprint; the bridge verifies the ticket.
- Evidence: `modules/system/backend/server.py:124-127,1269-1293`; `modules/workspace-picker/backend/workspaces.go:698-705,1266-1287`; `modules/home-assistant/backend/server.py:662-666`; `modules/opencode/backend/server.py:293-297`; `modules/vm-lab/backend/server.py:1273-1277`; `orqella/src/state.rs:279-285`; `orqella/src/http/shell.rs:19-21`; `orqella/src/backup.rs:45-75`; `website/docs/docs.js:196`; `orqella/.github/workflows/*.yml`.

---

## P2 — hardening and assurance

- [x] Added regression tests for proxy cookies, read-path normalization, signature enforcement, peer tokens, device confirmation/lockout, signing-key rotation parsing, Tauri navigation/permission origins, CSP, agent environment clearing, WebRTC identity binding, and SDP/ICE validation.
- [x] Add a trust-model section to `docs/SECURITY.md` (trusted local code, sandboxed modules, VM guests, remote peers).
- [x] `cargo audit`: core no vulnerabilities; client has RUSTSEC-2023-0071 (`rsa 0.10.0-rc.18`, pinned by `russh 0.65.1`; upstream pin blocks the fixed 0.10.0), plus trivy's GHSA-wrw7-89jp-8q8g for `glib 0.18.5` via Tauri's GTK 0.18 stack (upstream upgrade required). `cargo deny` passes for core and client. `pip-audit` and trivy report no Python dependency vulnerabilities. `gitleaks` is clean for `orqella/` and `cloud/`. Semgrep findings were triaged (postMessage cases use opaque module origins, SQL identifiers are internal, SHA-1 is required by the WebSocket handshake); website Actions are now SHA-pinned. A local DAST smoke pass verified auth boundaries, traversal rejection, Host rejection, and shell security headers.
- [x] Review the vendored `iroh-webrtc-transport` and the Tauri/mobile WebView surfaces separately.
  - [x] **C1 identity binding.** The native and browser WebRTC bootstrap paths now reject a `DialRequest.from` that does not equal `connection.remote_id()`; added a native regression test. This closes EndpointId impersonation over the authenticated bootstrap stream.
  - [x] **C2 dev bridge.** The `orqella-web-bridge` localhost proxy binary is now behind a non-default `dev-bridge` Cargo feature and is not built in normal package builds.
  - [x] **M2 Android backup.** `android:allowBackup="false"` so hub/account metadata is not uploaded by Auto Backup.
  - [x] **M3 restore crash.** Android KeyStore decryption drops stale ciphertext on `AEADBadTagException` or malformed base64 instead of crashing at launch.
  - [x] **C3 unbounded WebRTC state.** Closed/failed browser sessions are evicted before allocation; browser sessions have a 64-per-peer cap; native sessions evict closed peer connections; native routes expire after 10 minutes; global caps remain as a backstop.
  - [x] **T1 Tauri permission auto-grant.** Auto-allow is limited to the exact running gateway origin; other requests fall through to the platform default. Covered by unit tests.
  - [x] **M4 Tauri navigation allowlist.** Top-level navigation is limited to the exact gateway origin and app origins registered with the running gateway; other http(s) links open externally. Covered by unit tests.
  - [x] **T2/T3.** Mobile passkey clients reject a mismatched `rpId`. Removed `'unsafe-eval'` from the desktop CSP. `withGlobalTauri` remains only for the bundled, capability-scoped settings window (the main dashboard capability has no custom commands); documented as an accepted trust boundary.
  - [x] **C5 SDP/ICE filtering.** Added bounded SDP/candidate validation: size and control-character limits, at most 128 candidates per SDP, IP-literal or mDNS addresses only, no unspecified/multicast addresses, ports 1–65535, UDP/TCP only, and known candidate types. Applied to native and browser remote descriptions and candidates.
- [x] CSPs are present on the shell, website, docs, and browser client; regression tests cover the shell header and the browser-client CSP/no-sandbox-rewrite invariants.
- [x] Verified `cloud/billing-api/.env` is untracked, gitignored, and absent from git history; no evidence it left this machine. Rotate the test key if the file was ever copied or shared.
- [-] Retained as an explicit opt-in; the startup error and docs call it unsafe. Treat it as unauthenticated RCE if enabled.

---

## Progress log

- 2026-11-14: checklist created; decisions recorded.
- 2026-11-14: noted pre-existing uncommitted hardening in `modules/system` (constant-time auth), `modules/terminal` (body/WS parsing), `modules/vm-lab` (image URL containment, UDP validation), `modules/voice-assistant` (sidecar auth/body/state writes), and `modules/workspace-picker` (constant-time auth, UTF-8 safe text). These are being preserved and built on.
- 2026-11-14: first remediation pass. Daemon: stripped `orqella_session` from proxied HTTP/WS and upstream `Set-Cookie`; added proxy regression tests; normalized and hardened module `read_paths`; exposed command/read paths/network in module summaries and settings; added `backend.network` and Linux/macOS network isolation; added `allow_unsigned`/signature verification for git installs and updates; re-consent on backend command/artifact changes; made module updates re-verify signatures; rotated module WS tokens on remove/install; cleared the agent environment; added a per-daemon browser bridge token separate from the admin token; added a bounded source to peer HMAC tokens; added Secure cookies behind HTTPS; updated security/module/browser-client docs.
- 2026-11-14: modules. VM Lab: QMP moved to a 0600 Unix socket (Windows fallback documented), VNC password auth, per-VM seed path token, guest-user validation, `ssh -l`, path containment/image extension checks, no password in job logs. Voice Assistant: generated shared token, fail-closed engine auth, HA integration reads the shared token. Home Assistant: token host binding. OpenCode: refuses the local password for non-loopback URLs. System/i3/terminal/empty-token hardening.
- 2026-11-14: browser client: escaped Link device fields, removed module sandbox/CSP rewrites, disabled module surfaces, validated service-worker senders and app targets, escaped app tokens, validated ws-shim sources.
- 2026-11-14: cloud: `/v1/web/relay` endpoint binding; required signing-key env; scrypt `N=2^17` with versioned hashes/dummy timing/rehash; billing loopback default and `trust_env=False`; publisher session rate limit; peer tokens per-source HMAC; docs.
- 2026-11-15: final verification: core `cargo test --workspace` 178 passed plus integration groups (5 CLI, 11 HTTP, 5 proxy, 3 conformance, 10 security regressions, 11 link-core, 7 tunnel), vendor transport 51 native tests, client 17 + 23 + 4 passed, `cargo clippy -D warnings` clean in both workspaces, `node --test` 17 passed / 10 skipped, module suites (system 12, terminal 17, vm-lab 21, voice 11, HA 15, opencode 10, workspace-picker Go tests), cloud link-api 66, billing 14, publisher 17, website 14. `pip-audit` clean for all three cloud requirement sets; trivy clean for Python/Go/Rust except the upstream Tauri `glib` advisory; Semgrep findings triaged; local DAST smoke passed. `py_compile`/`compileall` clean. Mobile Kotlin/Swift were not compiled (no Android SDK / macOS toolchain here).
- 2026-11-14: device approval now uses a 6-digit confirmation code typed from the initiating client plus a 15-minute fresh-session requirement. Added per-email password lockout and uniform discoverable passkey options. Ticket signing keys have IDs and a 60-second daemon key-ring refresh.
- 2026-11-14: vendored transport/desktop/mobile review. Fixed WebRTC bootstrap EndpointId impersonation (native + browser), gated the localhost dev bridge behind `dev-bridge`, capped retained WebRTC sessions/routes, restricted Tauri permission auto-grant to the exact gateway origin, restricted desktop navigation to the gateway plus registered app origins, disabled Android Auto Backup, made KeyStore restore failures non-fatal, and validated mobile passkey RP IDs. Remaining: WebRTC session eviction/expiry, SDP/ICE filtering, and the `withGlobalTauri` CSP migration. Link tickets carry a signing key ID; daemons refresh a verification key ring every 60s and `LINK_ADDITIONAL_SIGNING_PUBLIC_KEYS` enables staged rotation.
- 2026-11-15: final open-issue pass. Added WebRTC SDP/ICE validation with bounded candidates and address/port/type checks; added closed-session eviction, browser per-peer limits, native route expiry, and native closed-peer eviction; removed `'unsafe-eval'` from the desktop CSP; pinned website GitHub Actions to SHAs; ran trivy, Semgrep, and a local DAST smoke pass; verified the local Stripe `.env` is untracked, gitignored, and absent from git history. `cargo audit` now also records the Tauri GTK `glib 0.18.5` advisory (upstream upgrade required).
- 2026-11-14: deferred sidecar network isolation by owner decision; `backend.network` is reserved and documented. Added `orqella rotate-peer` (target-side secret rotation + Link re-registration); other machines refresh on their next Link restart. Added billing internal-route rate limiting and peer-action audit events. Added module signature status to module detail. Web bridge now authorizes browser sessions through `/v1/web/authorize` (session-only), and browser relay credential minting accepts the hub-authenticated session token.

## Verification commands

```bash
# Core daemon
cd /home/eolu/Projects/orqella/orqella
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
node --check web/app.js
node --test web/tests/*.test.mjs web-client/tests/*.test.mjs

# Desktop client
cd client
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace

# Cloud
cd /home/eolu/Projects/orqella/cloud
pytest -q link-api/tests
pytest -q billing-api/tests
pytest -q github-publisher/tests

# Modules
python -m pytest -q modules/*/tests

# Vendored WebRTC transport (native; browser lib checks on wasm)
cd /home/eolu/Projects/orqella/orqella/vendor/iroh-webrtc-transport
cargo test --lib --no-default-features --features native
cargo check --lib --target wasm32-unknown-unknown --no-default-features --features browser
```
