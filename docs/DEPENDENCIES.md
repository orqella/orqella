# Dependency policy

Orqella keeps its dependency surface small, but the tree includes iroh/russh and
platform integration crates that move quickly. This policy keeps the audit
output intentional.

## Baseline

Run before a release:

```bash
cargo audit
cargo deny check --hide-inclusion-graph
cargo tree -d
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
```

`cargo audit` currently has one documented advisory ignore:

- `RUSTSEC-2023-0071` (RSA timing side channel). `russh` pins the pre-release
  `rsa 0.10.0-rc.18`; Orqella uses Ed25519 host/device keys and does not use the
  vulnerable RSA decryption path. Remove the ignore when `russh` moves to a
  fixed release.

Unmaintained-crate warnings are allowed only when the crate is transitive and no
maintained alternative is available. Each allowed warning must have a comment in
the CI/audit command.

## Rules

- Do not add an advisory ignore without a linked upstream issue and an owner.
- Do not add a second crate for a problem an existing dependency already solves.
- Prefer the workspace dependency version; if two major versions are required
  (for example the `rand 0.8`/`0.10` split between ed25519-dalek and russh),
  document why in `Cargo.toml`.
- Run `cargo tree -d` on dependency updates and remove duplicates when upstream
  allows.
- Review the audit output quarterly and before every public release.
- A new unmaintained dependency warning blocks a release unless the release
  notes call it out.

## License and duplicate-version policy

`deny.toml` at the repository root is enforced in CI for the core and desktop
workspaces. It covers advisory ignores, license allow-listing, duplicate
versions, and registry/source policy. Run it locally with:

```bash
cargo deny check --hide-inclusion-graph
```

For the desktop workspace from `client/`, pass the shared config explicitly:

```bash
cargo deny --config ../deny.toml check --hide-inclusion-graph
```

Keep the `RUSTSEC-2023-0071` ignore in `.cargo/audit.toml` and `deny.toml` in
sync, and remove both when `russh` moves to a fixed `rsa` release.
