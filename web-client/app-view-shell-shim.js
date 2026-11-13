(() => {
  if (window.__orqellaAppViewShellShim) return;
  window.__orqellaAppViewShellShim = true;

  function mountForHost(hostname) {
    if (!hostname || (!hostname.endsWith('.orqella.localhost') && !hostname.endsWith('.localhost'))) {
      return '';
    }
    const first = hostname.split('.')[0];
    return first && first !== 'orqella' ? first : '';
  }

  const APP_PORTS = { opencode: '8811', homeassistant: '8810' };

  function appOrigin(mount) {
    try {
      const configured = window.top?.__ORQELLA_APP_ORIGINS || window.__ORQELLA_APP_ORIGINS;
      if (configured?.[mount]) return new URL(configured[mount], location.href).origin;
    } catch (_) {}
    const suffix = window.top?.__ORQELLA_APP_ORIGIN_SUFFIX || window.__ORQELLA_APP_ORIGIN_SUFFIX;
    if (suffix) return `${location.protocol}//${mount}${suffix}`;
    const port = APP_PORTS[mount];
    return port ? `${location.protocol}//${location.hostname}:${port}` : '';
  }

  function mountForUrl(url) {
    const hostMount = mountForHost(url.hostname);
    if (hostMount) return hostMount;
    const parts = url.pathname.split('/').filter(Boolean);
    if (parts[0] === 'p' && parts[1]) {
      try { return decodeURIComponent(parts[1]); } catch (_) { return parts[1]; }
    }
    if (parts[0] === 'n' && parts[2] === 'p' && parts[3]) {
      try { return decodeURIComponent(parts[3]); } catch (_) { return parts[3]; }
    }
    return '';
  }

  function targetFor(url, mount) {
    const encoded = encodeURIComponent(mount);
    let path = url.pathname;
    const remote = new RegExp(`^/n/[^/]+/p/${encoded.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')}(?=/|$)`);
    const remoteMatch = path.match(remote);
    if (remoteMatch) {
      path = path.slice(remoteMatch[0].length) || '/';
    } else if (path === `/p/${encoded}`) {
      path = '/';
    } else if (path.startsWith(`/p/${encoded}/`)) {
      path = path.slice(`/p/${encoded}`.length);
    }
    return `${path}${url.search}`;
  }

  function rewrite(value) {
    try {
      const url = new URL(String(value), location.href);
      const mount = mountForUrl(url);
      const origin = appOrigin(mount);
      if (!mount || !origin) return String(value);
      const target = targetFor(url, mount);
      return `${origin}/app-client/?mount=${encodeURIComponent(mount)}&target=${encodeURIComponent(target)}`;
    } catch (_) {
      return String(value);
    }
  }

  const descriptor = Object.getOwnPropertyDescriptor(HTMLIFrameElement.prototype, 'src');
  if (descriptor?.set) {
    Object.defineProperty(HTMLIFrameElement.prototype, 'src', {
      configurable: true,
      get: descriptor.get,
      set(value) {
        if (this.id === 'appFrame') value = rewrite(value);
        return descriptor.set.call(this, value);
      },
    });
  }

  const setAttribute = Element.prototype.setAttribute;
  Element.prototype.setAttribute = function (name, value) {
    if (this instanceof HTMLIFrameElement && this.id === 'appFrame' && String(name).toLowerCase() === 'src') {
      value = rewrite(value);
    }
    return setAttribute.call(this, name, value);
  };
})();
