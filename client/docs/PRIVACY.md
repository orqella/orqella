# Privacy and diagnostics

- The desktop client has **no telemetry** and no analytics SDK.
- Network requests go to the Orqella Link service, the selected Orqella hub,
  and the app upstreams configured by that hub.
- Account tokens, SSH private keys, iroh secrets, cookies, and app data are not
  sent to this repository or to any Orqella-hosted logging service.
- Diagnostics are generated locally and copied only when the user requests it.
- Diagnostic output must not include account tokens, private keys, bearer
  headers, cookies, or pairing/device codes.
- Application data is direct-only; managed relay is used only for
  control-plane/signaling bootstrap.
- Logs are local. A future log-export feature must apply the same redaction
  rules and require an explicit user action.
