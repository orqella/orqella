# Accessibility checks

## Implemented

- Settings and startup pages have a language, labeled inputs, semantic headings,
  buttons, and a visible checkbox label.
- The in-window settings page closes with an X or Escape and keeps the same
  local theme as the startup page.
- Long-running status text uses `role="status"`/`aria-live="polite"`; errors use
  `role="alert"`/`aria-live="assertive"`.
- All interactive controls have a `:focus-visible` outline.
- Settings uses its local theme with a `prefers-color-scheme` fallback; light
  and dark palettes use the same semantic variables and maintain text contrast.
- All settings actions are native buttons.

## Automated audit

Run axe-core against the local pages (the settings page degrades gracefully
without Tauri IPC):

```bash
cd desktop/ui
python3 -m http.server 8099 --bind 127.0.0.1

npx @axe-core/cli http://127.0.0.1:8099/settings.html \
  --chromedriver-path /path/to/chromedriver --chrome-path /path/to/chromium --exit
```

Results on 2026-11-11 with axe-core 4.14.0 and Chromium 155:

```text
settings.html  0 violations found
index.html     0 violations found
```

Re-run the audit after changing the in-window settings/onboarding layout.

Axe reports only a subset of issues. A platform screen-reader pass (VoiceOver,
Narrator, Orca) and high-contrast/zoom checks remain part of the macOS/Windows
runtime matrix rather than this automated gate.
