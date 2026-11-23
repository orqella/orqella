# Release checklist

## Release workflows

The repository has two tag-driven release workflows:

| Product | Tag | Workflow | Assets |
| --- | --- | --- | --- |
| Daemon / CLI | `v0.6.0` | `.github/workflows/release.yml` | Linux x86_64, macOS Intel/Apple Silicon, Windows x86_64, Windows combined setup, `install.sh` |
| Desktop | `desktop-v0.2.1` | `.github/workflows/release-desktop.yml` | Tauri bundles for Linux, macOS, Windows |

Both workflows can also be dispatched manually with an existing tag. The version
in `Cargo.toml` (daemon) or `client/Cargo.toml` (desktop) must match the tag.
The daemon workflow publishes `SHA256SUMS.txt`; the desktop workflow adds
`SHA256SUMS.txt` after all matrix jobs finish. If `ORQELLA_UPDATE.json` exists
in the tagged revision, the daemon workflow verifies and attaches it; otherwise
it publishes binary assets only.

Desktop releases are currently **unsigned**: the workflow intentionally omits
the signing environment variables because passing an empty
`APPLE_CERTIFICATE` makes Tauri attempt a PKCS#12 import and fail the bundle.
To enable signing, add the Apple/Windows secrets and pass them to the
`tauri-action` step. The expected secrets are:

- macOS: `APPLE_CERTIFICATE`, `APPLE_CERTIFICATE_PASSWORD`,
  `APPLE_SIGNING_IDENTITY`, `APPLE_ID`, `APPLE_PASSWORD`, `APPLE_TEAM_ID`
- Windows: `WINDOWS_CERTIFICATE`, `WINDOWS_CERTIFICATE_PASSWORD`
- Tauri updater (when enabled): `TAURI_SIGNING_PRIVATE_KEY`,
  `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`

The update signing key stays offline. CI verifies a signed update manifest; it
never receives the minisign secret key.

## Local checks

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

macOS and Windows run workspace checks; platform-specific service behavior still
must be exercised on real machines before a release. Mobile release workflows
live in the private mobile repository.

## Cutting a release

```bash
# Daemon / CLI
git tag v0.6.0
git push origin v0.6.0

# Desktop
git tag desktop-v0.2.1
git push origin desktop-v0.2.1
```

The website reads published GitHub releases through
`scripts/fetch_releases.py`; no version is hard-coded in the marketing site.

## Daemon release updates

Installed daemons update from GitHub Releases. `orqella update check` queries
`[update] repository` (default `orqella/orqella`) for the newest published
non-prerelease `v<version>` release, selects the archive for the current
platform, and compares its version to the running binary.

`orqella update apply` downloads that archive, verifies the matching entry in
`SHA256SUMS.txt`, extracts the binary, replaces the running executable, and
restarts the per-user service. When no managed service is installed it starts
the updated daemon. It does not use git, Cargo, a checkout, or an installer
script.

If `[update] public_key` is set (or `ORQELLA_UPDATE_PUBLIC_KEY` was baked into
the build), the release must also include a valid minisign-signed
`ORQELLA_UPDATE.json` and `ORQELLA_UPDATE.json.minisig`. The manifest version
must match the release tag. Without a configured key, the updater verifies the
SHA-256 checksum only.

Set `ORQELLA_GITHUB_TOKEN` in the daemon environment when GitHub API rate
limits are a concern.

Required release assets:

```text
orqella-<version>-linux-x86_64.tar.gz
orqella-<version>-macos-x86_64.tar.gz
orqella-<version>-macos-arm64.tar.gz
orqella-<version>-windows-x86_64.zip
Orqella-Setup-<version>-windows-x86_64.exe
install.sh
SHA256SUMS.txt
```

The Windows setup is a per-user NSIS installer that bundles the daemon with the
latest published Orqella Desktop NSIS setup. It installs both apps, adds the
CLI to the user PATH, and registers the startup entry. If no published desktop
release is available, the workflow does not attach a combined setup and the
Windows archive remains the fallback.

`install.sh` is the Linux/macOS one-command installer served at
`https://orqella.io/install.sh`. It resolves the latest daemon and desktop
releases, verifies both checksums, installs both apps for the current user,
runs `orqella setup` to install the Agent Harness module and connect an
existing coding agent, and installs the per-user service. Publish it as a
release asset and copy it into the website build with
`scripts/vendor_install.py`.

The signed manifest is optional, but required when a public key is configured.
The release signing key stays offline. The release workflow fails closed when
`ORQELLA_UPDATE_PUBLIC_KEY` is configured but the tag has no signed manifest.

```bash
# One-time, offline. Back up the secret key and commit/publish only the .pub.
minisign -G -p orqella-update.pub -s orqella-update.key

# After committing the release code, before committing the manifest:
VERSION=0.6.0 scripts/release-sign.sh orqella-update.key

git add ORQELLA_UPDATE.json ORQELLA_UPDATE.json.minisig
git commit -m "release: v0.6.0"
git tag v0.6.0
git push origin v0.6.0

# Anyone can verify before publishing:
ORQELLA_UPDATE_PUBLIC_KEY=orqella-update.pub scripts/release-verify.sh
```

To add a manifest to an already-published release (for example, to repair a
checksum-only release), use the offline key and upload the two assets without
changing the tag:

```bash
scripts/attach-update-manifest.sh v0.6.0 ~/.config/orqella/orqella-update.key
```

Official builds can embed the public key with
`ORQELLA_UPDATE_PUBLIC_KEY=<base64>` at build time. A source build can instead
set `[update] public_key` in its config. When a key is configured, a release
without a valid manifest is refused.

The daemon records the highest applied release version in
`<data>/update-state.json` and refuses a release that is not newer than that
record or the running version. Never reuse a version number.

## Desktop client

```bash
cd client
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo build -p orqella-desktop
```

Installers are built from `client/desktop/src-tauri` with `cargo tauri build`
after installing the Tauri CLI and platform packaging dependencies. The release
workflow uses `tauri-apps/tauri-action` and the tag version. Signed release and
updater keys stay outside the repository.

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
9. Run `orqella backup create`, `orqella support`, and
   `orqella backup restore` against a scratch `ORQELLA_HOME`; inspect the zip
   contents and confirm the restore round-trips.
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
19. Run `orqella update check` against a published release newer than the
    running build. Confirm it selects the platform archive and reports the
    version. Run `orqella update apply`, confirm the checksum verifies, the
    binary is replaced, the service restarts, and `/healthz` returns the new
    version. On a machine with `[update] public_key` set, also verify a release
    without a signed manifest is refused.
20. On a clean Linux or macOS VM, run `install.sh --dry-run`, then run the
    installer. Confirm it verifies both checksums, installs the daemon and
    desktop app, installs Agent Harness, detects or offers OpenCode, installs
    the per-user service, and reaches `/healthz`. Run it again over the
    installed copy and confirm the upgrade succeeds. Repeat with a `PATH` that
    has no Git to confirm the archive-backed Agent Harness install still
    works.
21. On a clean Windows VM, run `Orqella-Setup-<version>-windows-x86_64.exe`.
    Confirm it installs the daemon and desktop app, adds the CLI to the user
    PATH, installs Agent Harness, preselection of OpenCode matches the
    installed coding agents, creates the startup entry, starts the daemon, and
    uninstalls both apps and the startup entry.
22. On a machine with an existing OpenCode, Claude Code, Codex, or Gemini CLI,
    run `orqella setup` and confirm the registry probes it, the default
    conversation opens it, and the headless default agent works. On a machine
    with no coding agent, run `orqella setup --install-opencode` and confirm
    the free model is selected and a session answers a prompt.

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

- A signed module index and publisher key rotation. The current index pins
  immutable commits and tree digests over HTTPS; the index repository is the
  trust root.
- A signed desktop updater channel and hosted `latest.json`.
- Full typed API error taxonomy.
- macOS/Windows sidecar containment verification; Windows filesystem isolation.
- Windows 11 module validation.
- Native mobile shells are maintained in a separate private repository.
