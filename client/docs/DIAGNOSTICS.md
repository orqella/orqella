# Diagnostics and logs

## Log files

The client writes structured tracing logs to a platform log directory:

| Platform | Path |
| --- | --- |
| Linux | `$XDG_STATE_HOME/io.orqella.desktop/logs/orqella.log` (fallback `~/.local/state/...`) |
| macOS | `~/Library/Logs/io.orqella.desktop/orqella.log` |
| Windows | `%LOCALAPPDATA%\io.orqella.desktop\logs\orqella.log` |

The file is appended to across runs. At startup, a file larger than 5 MiB is
rotated once to `orqella.log.1`; the previous rotation is replaced.

Log lines are redacted **before they are written** to both the file and the
console layer:

- `Authorization`, `Cookie`, `Set-Cookie`, `token`/`access_token`/
  `refresh_token`/`ws_token`/`device_code`/`user_code`, `password`, `secret`,
  and `api_key` values.
- Bare `Bearer` tokens.
- PEM private-key blocks, including multiline blocks held in the writer state.
- Multi-value cookie headers are redacted as a whole line.

Log files are created with owner-only permissions (`0600`) and the log
directory uses `0700` on Unix. The desktop profile is written with `0600`.

Redaction is repeated when a diagnostics bundle is exported, so a log file
modified outside the client still cannot leak these patterns through the export.

## Diagnostics summary

The desktop IPC still exposes `diagnostics` (JSON summary) and
`export_diagnostics` (writes
`orqella-diagnostics-<unix-time>.txt` to the platform Downloads directory).
The Settings → Desktop integration section is hidden for now.

The summary includes:

- client version, OS/architecture, and WebView user agent;
- connection mode, shell origin, hub version;
- identity/account-token presence (booleans only, never values);
- the log-file path.

The exported bundle also contains the most recent 5,000 log lines, redacted.

## Verification

On Linux the export command produced a bundle containing a deliberately logged
credential:

```text
"hub_version": "0.1.0",
"webview": "Mozilla/5.0 (X11; Linux x86_64) ...",
"log_file": "/home/user/.local/state/io.orqella.desktop/logs/orqella.log"
...
diagnostic smoke: token=[REDACTED] Authorization: [REDACTED]
```

The raw secret values were absent from both the bundle and the log lines written
by the client. Debug builds also honor `ORQELLA_DIAGNOSTICS_EXPORT_PATH` to make
automated export verification deterministic.
