# ADR 0004: Daemon-owned sidecar policy

Status: accepted.

Sidecar sandboxing and memory limits are daemon policy. The defaults are
configured under `[sidecar]`; a module may request an unsandboxed sidecar with
`[backend] sandbox = false`, but the permission review must show an explicit
warning and require the user's acknowledgment before granting `backend`.
Memory limits remain daemon-owned.

On Linux, `sandbox = true` uses bubblewrap with read-only system paths, a
writable module directory, private `/tmp`, and an address-space limit. If no
sandbox mechanism is available, startup fails rather than silently downgrading.
macOS now uses a sandbox-exec profile and Windows uses a Job Object for
lifecycle/memory containment (but not filesystem isolation). Memory limits also
apply when filesystem sandboxing is disabled on Linux and Windows. macOS has no
per-sidecar memory cap.

Consequences:

- Sidecars remain powerful code execution; the user reviews both the daemon
  policy and any per-module sandbox opt-out.
- Sidecar runtimes must be available inside the sandbox (system runtimes work;
  arbitrary home-directory runtimes may not).
- The sandbox is not a network namespace; network access remains a separate
  future hardening step.
