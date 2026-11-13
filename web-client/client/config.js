// Optional full-origin overrides for the isolated application clients.
// Leave empty for local development; the shell then uses ports 8810/8811 on
// the client hostname.
//
// Example staging/production configuration:
// window.__ORQELLA_APP_ORIGINS = {
//   homeassistant: 'https://homeassistant-web.example.com',
//   opencode: 'https://opencode-web.example.com',
// };
window.__ORQELLA_APP_ORIGINS = window.__ORQELLA_APP_ORIGINS || {};

// Optional wildcard app-origin suffix. A module mount "immich" then uses
// https://immich.apps.example automatically, provided the app origin serves
// the same /app-client files. Explicit entries above override this.
window.__ORQELLA_APP_ORIGIN_SUFFIX = window.__ORQELLA_APP_ORIGIN_SUFFIX || '';

// The browser client uses managed relay credentials only for WebRTC
// bootstrap/signaling. Application data is WebRTC-only and the hub rejects
// relayed application connections.
