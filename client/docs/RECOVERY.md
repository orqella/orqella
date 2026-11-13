# Network, sleep, and reconnect recovery

The desktop health monitor probes the active hub every 10 seconds through the
local gateway.

Failure handling:

- Three consecutive probe failures mark the session `reconnecting`.
- Local sessions reconnect to `127.0.0.1:9477`.
- Remote sessions re-fetch the Link ticket and reconnect the direct-only tunnel.
- Reconnect attempts use bounded exponential backoff and never duplicate a
  tunnel; the previous tunnel is stopped first.
- After three failed attempts the session is marked `disconnected`.

Change detection:

- Every health tick compares the sorted set of local interface addresses. An
  address change triggers an immediate probe instead of waiting for three
  missed checks.
- A tick delayed by more than 30 seconds is treated as a resume from sleep and
  also triggers an immediate probe.
- If the hub is healthy after a wake or address change, the existing session is
  kept; otherwise reconnect starts on that tick.

The comparison and resume threshold are covered by
`desktop/src-tauri/src/lib.rs` unit tests. Physical network switching and
suspend/resume still need the platform matrix described in the public roadmap.
