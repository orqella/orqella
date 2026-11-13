# Notifications

## Permission and settings

The notification plugin is initialized at startup. macOS/Windows permission
prompts are handled by the plugin; the Settings window has **Send test
notification** to verify OS delivery.

## Module and dashboard routing

The shell shim exposes:

```js
await window.__ORQELLA_NOTIFY("Build finished", "The module is ready.");
```

The dashboard and same-origin module frames can call it (module frames use
`parent.__ORQELLA_NOTIFY`). The shim posts to
`POST /__orqella/native/notify` with the per-run native token. The gateway
validates:

- native token (403 otherwise);
- non-empty title, at most 120 characters;
- body at most 500 characters.

The Tauri shell registers a notification sender when the gateway starts, so the
message is shown through the platform notification plugin. Isolated app origins
(`127.0.0.2+`) do not expose native routes and cannot raise notifications.

## Verification

Gateway test `routes_authenticated_native_notifications`:

- rejects a request without the native token (`403`);
- delivers `Build finished` / `The module is ready.` to the registered sender;
- rejects an empty title (`400`).

Linux delivery was also verified on D-Bus: **Send test notification** emitted
`org.freedesktop.Notifications.Notify` with app name `orqella-desktop`, title
`Orqella`, and body `Desktop notifications are working.` The Windows installed
client launches with the same plugin; OS delivery there depends on the user
granting notification permission.
