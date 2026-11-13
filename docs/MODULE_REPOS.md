# Module repositories

Orqella modules are independent git repositories. A module is never stored
inside the main Orqella checkout. Git must be installed and on `PATH`; install
and update fail with a clear error when it is missing. The daemon keeps an installed checkout under
its data directory:

```text
~/.local/share/orqella/modules/<module-id>/   # Linux
~/Library/Application Support/orqella/modules/<module-id>/
%LOCALAPPDATA%\orqella\modules\<module-id>%   # Windows
```

Those directories are real git working trees. Runtime state lives in `data/`
and is ignored by the repository. This gives each module its own history,
upstream, and release cadence while keeping the daemon source tree small.

## Install

```bash
orqella module install /path/to/module
orqella module install https://github.com/acme/orqella-weather
orqella module install git@github.com:acme/orqella-weather.git --branch main
```

The daemon accepts a local folder or a git URL through
`POST /api/v1/modules/install`:

```json
{ "path": "/path/to/module" }
{ "url": "https://github.com/acme/weather", "branch": "main" }
```

A local folder gets a fresh repository on install when it is not already a
checkout. URL installs clone the repository and keep their origin. Installing an update
preserves the module's `data/` directory and existing permission grants.

## Develop

```bash
orqella module new ~/Projects/orqella/modules/weather
cd ~/Projects/orqella/modules/weather
# edit module.toml and ui/
orqella module dev .
```

`module new` initializes a repository and commits the scaffold when git is
available. The AI Build and Edit flows also work on a git workspace: a
successful edit is committed before it is validated and installed.

## Update

```bash
orqella module update weather
```

An update is a fast-forward pull from `origin`. The daemon refuses to update a
checkout with uncommitted changes; commit or publish first. The module data
directory is not tracked.

Settings → Modules shows the selected module's remote, branch, short commit, and dirty state.
It exposes **Update from git**, **Checkpoint**, and **Publish to GitHub**. The
daemon routes are:

- `POST /api/v1/modules/{id}/update`
- `POST /api/v1/modules/{id}/checkpoint` with `{ "message": "..." }`
- `POST /api/v1/github/publish` with
  `{ "module_id", "repository", "confirm_repository", "private", "fork", "message" }`
- `GET /api/v1/github`, `POST /api/v1/github/connect`,
  `POST /api/v1/github/connect/poll`, `POST /api/v1/github/config`,
  `POST /api/v1/github/disconnect`

## Checkpoint and publish

A checkpoint commits the current module tree to its own repository without
pushing:

```bash
orqella module checkpoint weather "Add hourly forecast"
```

Publishing creates or reuses a GitHub repository, sets `origin`, commits any
remaining changes, and pushes the current branch:

```bash
orqella module publish weather                     # create a private repo
orqella module publish weather --public            # create a public repo
orqella module publish weather --repo acme/weather --message "Weather 0.2"
orqella module publish weather --fork              # fork the existing remote
```

The first publish creates `orqella-<id>` when no repository is supplied.
The publish dialog in module Settings lets you choose **Private** or **Public**
for a new repository. Existing repositories keep the visibility configured on
GitHub.

Publishing an already-published module pushes to its existing remote. A module
can also keep a non-GitHub git remote; use `--repo owner/name` to publish an
additional GitHub copy. When the requested repository differs from the source
recorded at install time, the browser dialog requires you to retype
`owner/name`; the CLI treats an explicit `--repo` as that confirmation. A
rewritten `.git/config` alone cannot redirect a publish.

When the module's existing GitHub remote belongs to another account, the
publish dialog offers **Publish as a fork**. The bridge creates a fork in the
connected GitHub account and the daemon pushes the module branch to that fork.
Fork publishing requires an existing GitHub remote and does not accept a
repository override. Forks keep the upstream repository's visibility rules.

## GitHub App

The GitHub App private key never lives on user machines. The publisher bridge
runs as a hosted service outside this repository, configured with the app ID,
client ID, client secret, slug, and private key.
The bridge:

- owns the GitHub App credentials;
- pairs a GitHub App installation to a daemon with a short session;
- stores only hashed daemon tokens;
- mints one-hour installation tokens on demand.

The daemon stores its bridge token in
`<data>/github/connection.json` with mode `0600`. It requests an installation
token only for the duration of a publish. The token is supplied to `git`
through a temporary `GIT_ASKPASS` helper and is never written to the remote URL,
module files, or logs.

The bridge needs these GitHub App repository permissions:

- **Contents**: read and write
- **Administration**: read and write (create repositories)
- **Metadata**: read

Set the GitHub App callback URL to `https://<bridge>/github/callback` and the
setup URL to `https://<bridge>/github/setup`.

Configure the daemon in **Settings → General → GitHub publishing** or in
`config.toml`:

```toml
[github]
bridge_url = "https://github.example.com"
```

The default bridge URL is `http://127.0.0.1:9490` for local development. Use
HTTPS in production.

## First-party modules

First-party modules live in sibling repositories, for example:

```text
~/Projects/orqella/modules/system
~/Projects/orqella/modules/opencode
~/Projects/orqella/modules/vm-lab
~/Projects/orqella/modules/voice-assistant
~/Projects/orqella/modules/terminal
~/Projects/orqella/modules/home-assistant
~/Projects/orqella/modules/workspace-picker
```

They are installed like any other module:

```bash
orqella module install ~/Projects/orqella/modules/system
```

The main Orqella repository no longer tracks a `modules/` directory. The
`/modules/` path is ignored so a local checkout cannot silently reintroduce
first-party module source into the daemon repository.

## Migration from a zip or an older install

Old installs that are plain folders are imported into a new repository the first
time the daemon scans them. Existing `data/` is ignored, not deleted. Importing
a signed `.orqella-module.zip` still verifies the publisher signature before the
files are installed into a repository.

## Security notes

- Installing a module is code execution; the sidecar runs as the logged-in
  user. Review permissions before enabling a backend.
- Git hooks from a remote clone are not copied into an install.
- Publish tokens are short-lived and never persisted by the daemon.
- A bridge token can mint installation tokens until it is revoked.
  **Disconnect** calls the bridge revoke endpoint before removing the local
  credential.
- A module repository can be public or private; publishing to a private
  repository does not make the module installable by someone else until they
  have access to that repository.
