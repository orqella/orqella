# Module repositories

Orqella modules are independent git repositories. A module is never stored
inside the main Orqella checkout. Git must be installed and on `PATH`; install
and update fail with a clear error when it is missing. The daemon keeps an
installed checkout under its data directory:

```text
~/.local/share/orqella/modules/<module-id>/   # Linux
~/Library/Application Support/orqella/modules/<module-id>/
%LOCALAPPDATA%\orqella\modules\<module-id>    # Windows
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

Git URL installs verify the module's publisher signature before the checkout is
accepted. An unsigned repository is rejected unless you pass
`--allow-unsigned` or confirm the dashboard's unsigned-install prompt. Local
folder installs are not signature-gated.

The daemon accepts a local folder or a git URL through
`POST /api/v1/modules/install`:

```json
{ "path": "/path/to/module" }
{ "url": "https://github.com/acme/weather", "branch": "main", "allow_unsigned": false }
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
checkout with uncommitted changes; commit first. The module data directory is
not tracked. The updated tree must also pass the publisher-signature check;
unsigned updates require `--allow-unsigned` or the dashboard confirmation.

Settings → Modules shows the selected module's remote, branch, short commit, and dirty state.
It exposes **Update from git** and **Checkpoint**. The daemon routes are:

- `POST /api/v1/modules/{id}/update`
- `POST /api/v1/modules/{id}/checkpoint` with `{ "message": "..." }`

## Checkpoint

A checkpoint commits the current module tree to its own repository without
pushing:

```bash
orqella module checkpoint weather "Add hourly forecast"
```

Push the repository with ordinary git tooling when you want to share it. The
daemon does not manage GitHub accounts, repositories, or publish tokens.

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
- A module repository can be public or private. Private repositories need
  credentials available to git itself, such as an SSH agent or a URL that
  already carries the token; the daemon disables credential helpers and
  terminal prompts.
