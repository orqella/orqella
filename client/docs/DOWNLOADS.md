# Downloads

The desktop client handles file downloads from the dashboard.

## Behavior

- Default destination: the OS Downloads directory.
- Filenames are sanitized, and collisions get a numeric suffix.
- The WebView download is allowed to proceed normally, preserving cookies and
  authentication.
- The native handler records the URL, path, byte count, and state in the desktop
  app state.
- A polling task emits `orqella-download` events as the file grows. WebKitGTK
  does not always deliver a completion event, so a stable non-empty file is
  marked finished after two seconds.
- Download records are available through the desktop IPC `list_downloads`
  command. The Settings list is hidden for now.

## Verification

A debug-only gateway route served a 491,520-byte attachment. The WebView opened
the URL and the file was saved as `~/Downloads/test-download-1`. The download
record reported:

```text
test-download-1
finished · 491520 bytes
```

The debug route is not compiled into release builds.
