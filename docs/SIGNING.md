# Module signing and trust

## The short answer

Signing does **not** gate local module creation.

- `orqella module new`, `orqella module dev`, the AI builder, and local folder
  installs work immediately. They are local authoring actions.
- Installing or updating from a git URL verifies the checkout against a trusted
  publisher before it is accepted. The repository therefore needs a trusted
  `ORQELLA_SIGNATURE.json`, or the user must explicitly acknowledge an unsigned
  checkout with `--allow-unsigned`.
- Signing also applies when a module becomes a **portable package**: export,
  import, peer sync, or sharing with another person.
- A daemon automatically trusts its own signing key. Another machine must
  explicitly trust the publisher before accepting code signed with that key.

This keeps the "ask AI, get a page" loop instant while making code that moves
between machines attributable and tamper-evident.

Modules normally move as git repositories (see
[`MODULE_REPOS.md`](MODULE_REPOS.md)). The same trusted-signature check covers
git installs, git updates, and portable zip packages (export/import and peer
sync). Local folder installs are an authoring/development action and do not
require a package signature.

## Git installs and updates

A git URL install clones the repository, then verifies its tree before the
module is installed. An update performs the same check after the fast-forward
pull. If the checkout is unsigned or signed by an untrusted key, the install or
update fails.

For local development against a repository that is not signed yet, pass
`--allow-unsigned`:

```bash
orqella module install https://github.com/acme/orqella-weather --allow-unsigned
orqella module update weather --allow-unsigned
```

The dashboard asks for confirmation before retrying an unsigned install or
update. Use this only for code you trust; it bypasses publisher trust for that
operation.

## Keys

Each daemon creates `module-signing.key` under its data directory on first use.
The private key is user-only on Unix. Show the public identity:

```bash
orqella module key
# public-key <64 hex chars>
# fingerprint ed25519:<sha256 hex>
```

Trust a publisher on the receiving machine:

```bash
orqella module trust <public-key-hex> --label "Joe's laptop"
```

`module trust` calls the running daemon and falls back to config editing when the
daemon is not running.

## Export and import

```bash
# Exports a signed zip using this daemon's key.
orqella module export system ./system.zip

# Verifies signature and trust without installing.
orqella module verify ./system.zip

# Imports only if the signature is valid and the publisher is trusted.
orqella module import ./system.zip
```

An unsigned package is rejected by import. A signed package from an unknown
publisher is rejected until that public key is trusted. The strict default is
intentional: import is a code-execution path.

Local folder installs are different:

```bash
orqella module install ./my-module   # local authoring path; no package signature
```

## Signing an existing directory

```bash
orqella module sign ./my-module
```

This writes `ORQELLA_SIGNATURE.json` into the module directory. The export path
also signs at export time, so a directory does not have to be pre-signed.

## What is signed

The signature covers a canonical digest of sorted module files (path length,
path bytes, file length, file bytes), excluding:

- `.git`
- `data/` (top-level runtime directory)
- `.config`
- `.cache`
- `.local`
- `target`
- `node_modules`
- `.venv`
- `__pycache__`
- `sidecar.log`
- `ORQELLA_SIGNATURE.json`

The signature format is Ed25519 over that SHA-256 digest. The package carries
the public key, fingerprint, digest, and signature.

## Peer sync

Peer sync exports a signed package and the receiving daemon verifies it. The
receiving daemon must trust the sending daemon's public key. This is deliberate:
peer transport authenticity is not the same thing as module publisher trust.

## Limits

Package import also enforces:

- 2,000 entries maximum;
- 64 MiB per uncompressed entry;
- 128 MiB total uncompressed expansion.

