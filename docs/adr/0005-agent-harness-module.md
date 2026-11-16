# ADR 0005: Generalize harness integration around the Agent Harness module

Status: accepted.

## Context

Orqella had a single OpenCode module that combined two things: an OpenCode
service/pairing integration and a workspace launcher. The daemon separately had
a headless agent abstraction (`[agent]`, `agent.prompt`, `conversation.open`)
with an OpenCode-specific viewer fallback. Users want the same interactive
integration for other coding harnesses (Claude Code, Codex, Gemini), but only
OpenCode ships a local web UI; the others expose structured protocols (ACP,
stream-json, app-server) with no app to embed.

## Decision

1. Ship one `agent-harness` module that owns a harness registry, a session
   index, transcripts, a conversation UI, and drivers. Every harness, including
   OpenCode, renders through the same ACP v1 conversation UI. OpenCode also
   keeps an explicit native-workspace action for features the module does not
   mirror. Arbitrary commands use a one-shot `exec` driver.
2. The daemon's preset viewer defaults point at the module:
   `/?harness=<preset>&session={session}&cwd={root}` for OpenCode, Codex,
   Claude, and Gemini; `/?session={session}&cwd={root}` for custom. An explicit
   `[agent.viewer]` still wins. The module resolves the session and renders it;
   an OpenCode session id that is not in the module index opens the native
   workspace instead.
3. Add two host actions:
   - `module.open` (no permission): a module opens its own page at a
     root-relative path. The shell validates the path and targets only the
     calling module.
   - `system.pick_directory` (`fs.pick`): the trusted shell browses directories
     (directories only, privileged, node-aware) and returns the chosen path.
     Modules never receive listings or file contents.
4. Filesystem and terminal delegation are opt-in per harness, confined to the
   session root set, audited, and visible in the conversation.

## Consequences

- Installing one module makes a machine's harnesses usable; adding a harness is
  registry configuration, not a new module.
- The daemon remains ignorant of the harness registry. Its only new coupling is
  the default viewer path and the two host actions.
- The old `opencode` module is not shipped; this module supersedes it. Because
  the module has not been published, no migration path is provided.
- `fs.pick` is a real implemented permission rather than a reserved `fs.*`
  namespace. It grants a picker, not filesystem access.
- The module runs unsandboxed because it spawns harnesses and reads their local
  configuration. The security boundary is the module permission review plus the
  per-harness delegation flags, not a filesystem jail.
- OpenCode is no longer a privileged path in the workspace UI. Its native web
  app is an escape hatch, not the primary experience; if its ACP surface lags,
  the button still reaches the native app.
- If another harness ships a genuinely embeddable web UI later, it can add a
  native action without changing the conversation UI or session contracts.

## Alternatives considered

- **Keep the module OpenCode-only and add one module per harness.** Rejected:
  duplicated UI, no single harness switcher, and `conversation.open` would need
  a per-preset module anyway.
- **Route everything through ACP, including OpenCode, with no native path.**
  Rejected: OpenCode's native workspace evolves faster than this module, so a
  native action is kept as an explicit escape hatch even though the primary
  experience is ACP.
- **Daemon-owned harness registry.** Rejected: it would make every harness
  change a core release and duplicate configuration that modules can own.
- **Let modules list directories directly through a host API.** Rejected:
  directory contents leak more than a single user-chosen path, and the shell can
  provide the picker without exposing listings to module iframes.
