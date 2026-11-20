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

// Per-module browser origins. This stays opt-in until the wildcard module
// host and certificate exist. Add ?moduleOrigins=1 to the client URL to use
// the matching suffix for the current deployment.
//
// Production: m-<hash>.modules.orqella.io
// Staging:    m-<hash>.modules.staging.orqella.io
// Local:      m-<hash>.localhost:<port>
const moduleOriginParams = new URLSearchParams(location.search);
// The browser client must never fall back to an opaque module frame: opacity
// alone leaves module assets outside the transport and defeats the broker.
window.__ORQELLA_REQUIRE_MODULE_ORIGINS = true;
window.__ORQELLA_MODULE_ORIGIN_SUFFIX = window.__ORQELLA_MODULE_ORIGIN_SUFFIX || (() => {
  if (moduleOriginParams.get('moduleOrigins') !== '1') return '';
  const host = location.hostname;
  if (host === 'orqella-web.fly.dev') return '.modules.orqella.io';
  if (host === 'orqella-web-staging.fly.dev') return '.modules.staging.orqella.io';
  if (host === 'localhost' || host === '127.0.0.1' || host.endsWith('.localhost')) return '.localhost';
  return '';
})();
// Full-app surfaces need their own wildcard origins too. Local development
// keeps the 8810/8811 ports; deployments use the matching wildcard suffix.
if (moduleOriginParams.get('moduleOrigins') === '1' && !window.__ORQELLA_APP_ORIGIN_SUFFIX) {
  if (location.hostname === 'orqella-web.fly.dev') window.__ORQELLA_APP_ORIGIN_SUFFIX = '.apps.orqella.io';
  if (location.hostname === 'orqella-web-staging.fly.dev') window.__ORQELLA_APP_ORIGIN_SUFFIX = '.apps.staging.orqella.io';
}

// The browser client uses managed relay credentials only for WebRTC
// bootstrap/signaling. Application data is WebRTC-only and the hub rejects
// relayed application connections.
