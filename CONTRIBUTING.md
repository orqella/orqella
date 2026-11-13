# Contributing to Orqella

Thanks for helping improve Orqella. The core is MIT-licensed and contributions
are accepted under the same license.

## Before you start

- Read [docs/PUBLIC_SCOPE.md](docs/PUBLIC_SCOPE.md) so changes stay within the
  open-source boundary.
- For a large feature or a new module surface, open an issue first so the
  direction can be discussed.
- Do not include secrets, private infrastructure, customer data, or internal
  runbooks in a pull request.
- Report security issues through the private advisory flow in
  [SECURITY.md](SECURITY.md), not a public issue.

## Development setup

Requirements:

- Rust stable
- Git
- Python 3.11+ for modules with Python sidecars
- Node.js for the browser client and dashboard tests
- Platform desktop dependencies for the client:
  - Linux: WebKitGTK 4.1, GTK 3, libayatana-appindicator, `patchelf`
  - macOS: Xcode command-line tools
  - Windows: WebView2

Build and run the daemon:

```bash
cargo build --release
./target/release/orqella serve
```

Open `http://orqella.localhost:9477/`.

Build the desktop client:

```bash
cd client
cargo build -p orqella-desktop
```

## Checks

Run the core checks from the repository root:

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
node --test web/tests/*.test.mjs web-client/tests/*.test.mjs
```

Run the desktop client checks from `client/`:

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

If your change touches module permissions, the host API, the theme contract, or
the app-surface model, update the corresponding document in `docs/` in the same
change.

## Modules

Modules are independent git repositories. Do not add a `modules/` directory to
this checkout. Start with [docs/MODULE_SPEC.md](docs/MODULE_SPEC.md) and
[docs/MODULE_AGENT.md](docs/MODULE_AGENT.md). New permissions require an
explicit review flow; do not silently expand a module's access.

## Sign-off

This project uses the Developer Certificate of Origin. Add a sign-off line to
each commit:

```text
Signed-off-by: Your Name <you@example.com>
```

By signing off you certify that you have the right to submit the contribution
under this project's license. See [DCO](DCO) for the full text.
