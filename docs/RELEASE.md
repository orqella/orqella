# Release checklist

## Automated

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo build --release

# Optional Windows cross-check (requires cargo-xwin and the MSVC target):
cargo xwin clippy --target x86_64-pc-windows-msvc --all-targets -- -D warnings
cargo xwin build --release --target x86_64-pc-windows-msvc
```

Public CI runs the core and desktop checks on push and pull requests. macOS
and Windows run workspace checks; platform-specific service behavior still
must be exercised on real machines before a release. Mobile release workflows
live in the private mobile repository.

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
5. Install `examples/sidecar-module`, call it, and remove it.
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
14. Run the platform sidecar policy with a test sidecar (Linux bubblewrap,
    macOS sandbox-exec, Windows Job Object).
15. On Windows with Python 3.9+ and PowerShell, install the System module and
    verify Network Universe reads TCP connections and System services lists
    Windows services. On a host without Python, verify metrics stay live and
    the advanced sections stay hidden. On macOS, verify Network Universe reads
    `netstat` connections and System services lists user launchd jobs.
16. On macOS, run `orqella hosts install`, then open the shell, a configured
    app host, and a Link machine app host in Safari. Verify `orqella hosts print`
    includes all three and `orqella hosts remove` leaves unrelated entries.
17. Install a module from a git URL, edit it, run `module checkpoint`, and
    verify Settings → Modules shows the commit and a clean tree.
18. Push a new commit to the module origin, run `module update`, and verify the
    version changes while `data/` survives.
19. Connect a GitHub App through the publisher bridge, publish a module, and
    verify the repository was created and the pushed commit matches the local
    HEAD. Verify **Disconnect** revokes the bridge token.
20. Verify `module update` refuses a dirty checkout and that private GitHub
    module pulls use a short-lived installation token.

## Security review

- [ ] No module assets expose `module.toml`, `sidecar.log`, dotfiles, `.git`,
  or runtime `data/` files.
- [ ] Publish tokens are never written to config, git remotes, module files, or logs.
- [ ] The GitHub App private key exists only on the publisher bridge.
- [ ] Disconnecting GitHub revokes the bridge token.
- [ ] Proxy traversal (`..`, encoded dots, backslashes) is rejected.
- [ ] Only configured app hosts pass the Host allowlist.
- [ ] Sidecar sandbox policy is daemon-owned.
- [ ] Sidecars and module WebSockets do not start or proxy without `backend`.
- [ ] Host-key pinning is mandatory in the native client tunnel core.
- [ ] Peer tokens never reach module pages or WebViews.

## Deferred

- Signed release/update stream with rollback.
- Signed module registry and discovery UI.
- Production Link control-plane deployment and signing-key pinning.
- Production GitHub App/bridge deployment and token revocation UI.
- Full typed API error taxonomy.
- macOS/Windows sidecar containment verification; Windows filesystem isolation.
- Windows 11 module validation.
- Native mobile shells are maintained in a separate private repository.
