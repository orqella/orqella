# Release checklist

## Automated

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo audit
cargo deny check --hide-inclusion-graph
cargo build --release

# Optional Windows cross-check (requires cargo-xwin and the MSVC target):
cargo xwin clippy --target x86_64-pc-windows-msvc --all-targets -- -D warnings
cargo xwin build --release --target x86_64-pc-windows-msvc
```

Public CI runs the core and desktop checks on push and pull requests. macOS
and Windows run workspace checks; platform-specific service behavior still
must be exercised on real machines before a release. Mobile release workflows
live in the private mobile repository.

## Signed daemon updates

Daemon updates refuse to execute a checkout unless `ORQELLA_UPDATE.json` is
signed by the release minisign key. The manifest names the release commit; only
the manifest and its `.minisig` file may be committed after that commit.

```bash
# One-time, offline. Back up the secret key and commit/publish only the .pub.
minisign -G -p orqella-update.pub -s orqella-update.key

# After committing the release code, before committing the manifest:
VERSION=0.4.0 scripts/release-sign.sh orqella-update.key

git add ORQELLA_UPDATE.json ORQELLA_UPDATE.json.minisig
git commit -m "release: v0.4.0"
git tag v0.4.0

# Anyone can verify before publishing:
ORQELLA_UPDATE_PUBLIC_KEY=orqella-update.pub scripts/release-verify.sh
```

Official builds embed the public key with
`ORQELLA_UPDATE_PUBLIC_KEY=<base64>` at build time. A source build can instead
set `[update] public_key` in its config. If no key is configured, updates fail
closed. `[update] allow_unsigned = true` is a local-development escape hatch;
remote/peer updates always require a signature.

The daemon records the highest applied release in
`<data>/update-state.json` and refuses a manifest that is not newer than that
record or the running version. Never reuse a version number for a different
commit.

## Desktop client

```bash
cd client
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo build -p orqella-desktop
```

Installers are built from `client/desktop/src-tauri` with `cargo tauri build`
after installing the Tauri CLI and platform packaging dependencies. Signed
release and updater keys stay outside the repository.

## Manual smoke test

1. Start with an empty config/data directory.
2. Confirm onboarding appears and persists after completion.
3. Open System and verify live metrics.
4. Open a configured app through its dedicated host.
5. Install `examples/sidecar-module`, grant its `backend` permission, call it,
   and remove it.
6. Update a test module with a new permission; verify RPC is blocked until the
   permission sheet is saved.
7. Sign in to Link on a second daemon; select it and read metrics.
8. Export/import a module and push one to the Link machine.
9. Run `orqella backup` and `orqella support`; inspect the zip contents.
10. Run `orqella service status` on the target platform.
11. Developer-only: run `orqella-tunnel` from `crates/orqella-tunnel-core`
    against a real SSH host and open the dashboard through the loopback URL.
    This harness is not shipped in the release artifact.
12. Export a module, verify its signature, import it on a second daemon, and
    confirm an untrusted publisher is rejected until `module trust` is used.
13. Run the platform sidecar policy with a test sidecar (Linux bubblewrap,
    macOS sandbox-exec, Windows Job Object).
14. On Windows with Python 3.9+ and PowerShell, install the System module and
    verify Network Universe reads TCP connections and System services lists
    Windows services. On a host without Python, verify metrics stay live and
    the advanced sections stay hidden. On macOS, verify Network Universe reads
    `netstat` connections and System services lists user launchd jobs.
15. On macOS, verify the shell at `http://127.0.0.1:9477/` and a configured
    app host in Chrome/Firefox with no `/etc/hosts` entries. Optionally run
    `orqella hosts install`, then open the canonical shell, a configured app
    host, and a Link machine app host in Safari. Verify `orqella hosts print`
    includes all three and `orqella hosts remove` leaves unrelated entries.
16. Install a signed module from a git URL (trust its publisher first), edit it,
    run `module checkpoint`, and verify Settings → Modules shows the commit and
    a clean tree.
17. Push a new commit to the module origin, run `module update`, and verify the
    version changes while `data/` survives. Verify an unsigned update requires
    `--allow-unsigned` or the dashboard confirmation.
18. Verify `module update` refuses a dirty checkout and fast-forwards a clean
    checkout from its configured origin.

## Security review

- [ ] No module assets expose `module.toml`, `sidecar.log`, dotfiles, `.git`,
  or runtime `data/` files.
- [ ] Proxy traversal (`..`, encoded dots, backslashes) is rejected.
- [ ] Only configured app hosts pass the Host allowlist.
- [ ] Sidecar sandbox policy is daemon-owned.
- [ ] Sidecars and module WebSockets do not start or proxy without `backend`.
- [ ] Host-key pinning is mandatory in the native client tunnel core.
- [ ] Peer tokens never reach module pages or WebViews.

## Deferred

- Signed module registry and discovery UI.
- Production Link control-plane deployment and signing-key pinning.
- Full typed API error taxonomy.
- macOS/Windows sidecar containment verification; Windows filesystem isolation.
- Windows 11 module validation.
- Native mobile shells are maintained in a separate private repository.
