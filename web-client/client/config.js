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

// Optional Link account origin override. Leave empty to use production, or
// let the staging hostname select the staging Link account service.
window.__ORQELLA_LINK_ACCOUNT_URL = window.__ORQELLA_LINK_ACCOUNT_URL || '';

// Optional wildcard app-origin suffix. A module mount "immich" then uses
// https://immich.apps.example automatically, provided the app origin serves
// the same /app-client files. Explicit entries above override this.
window.__ORQELLA_APP_ORIGIN_SUFFIX = window.__ORQELLA_APP_ORIGIN_SUFFIX || '';

// Per-module browser origins are required by this client. Each module gets a
// dedicated host, so the shell never has to fall back to an opaque frame.
//
// Production: m-<hash>.modules.orqella.io / <mount>.apps.orqella.io
// Staging:    m-<hash>.modules.staging.orqella.io / <mount>.apps.staging.orqella.io
// Local:      m-<hash>.localhost:<port> with app ports 8810/8811
//
// moduleOrigins=0 is a fail-closed kill switch for module surfaces.
const moduleOriginParams = new URLSearchParams(location.search);
const moduleOriginsDisabled = moduleOriginParams.get('moduleOrigins') === '0';
const moduleOriginHost = location.hostname;
window.__ORQELLA_REQUIRE_MODULE_ORIGINS = true;

if (moduleOriginsDisabled) {
  window.__ORQELLA_MODULE_ORIGIN_SUFFIX = '';
} else if (!window.__ORQELLA_MODULE_ORIGIN_SUFFIX) {
  if (moduleOriginHost === 'orqella-web.fly.dev') {
    window.__ORQELLA_MODULE_ORIGIN_SUFFIX = '.modules.orqella.io';
    window.__ORQELLA_APP_ORIGIN_SUFFIX = window.__ORQELLA_APP_ORIGIN_SUFFIX || '.apps.orqella.io';
  } else if (moduleOriginHost === 'orqella-web-staging.fly.dev') {
    window.__ORQELLA_MODULE_ORIGIN_SUFFIX = '.modules.staging.orqella.io';
    window.__ORQELLA_APP_ORIGIN_SUFFIX = window.__ORQELLA_APP_ORIGIN_SUFFIX || '.apps.staging.orqella.io';
  } else if (moduleOriginHost === 'localhost'
    || moduleOriginHost === '127.0.0.1'
    || moduleOriginHost.endsWith('.localhost')) {
    window.__ORQELLA_MODULE_ORIGIN_SUFFIX = '.localhost';
  }
}

// The browser client uses managed relay credentials only for WebRTC
// bootstrap/signaling. Application data is WebRTC-only and the hub rejects
// relayed application connections.
