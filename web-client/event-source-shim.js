(() => {
  if (window.__orqellaEventSourceShim) return;
  window.__orqellaEventSourceShim = true;

  const CONNECTING = 0;
  const OPEN = 1;
  const CLOSED = 2;
  const decoder = new TextDecoder('utf-8');

  class TransportEventSource extends EventTarget {
    static CONNECTING = CONNECTING;
    static OPEN = OPEN;
    static CLOSED = CLOSED;

    constructor(url, options = {}) {
      super();
      this.url = new URL(String(url), location.href).toString();
      this.withCredentials = !!options.withCredentials;
      this.readyState = CONNECTING;
      this.onopen = null;
      this.onmessage = null;
      this.onerror = null;
      this._closed = false;
      this._retry = 1000;
      this._lastEventId = '';
      this._buffer = '';
      this._ws = null;
      this._connect();
    }

    _connect() {
      if (this._closed) return;
      this.readyState = CONNECTING;
      const target = new URL(this.url);
      const token = window.__orqellaAppAuthToken;
      if (token && !target.searchParams.has('auth_token')) {
        target.searchParams.set('auth_token', token);
      }
      const socketUrl = new URL('/__orqella_sse', location.origin);
      socketUrl.protocol = location.protocol === 'https:' ? 'wss:' : 'ws:';
      socketUrl.searchParams.set('target', bridgeTarget(target));
      this._buffer = '';
      const ws = new WebSocket(socketUrl.toString());
      this._ws = ws;
      ws.binaryType = 'arraybuffer';
      ws.onopen = () => {
        this.readyState = OPEN;
        this._emit('open', new Event('open'));
      };
      ws.onmessage = (event) => {
        let text;
        if (typeof event.data === 'string') text = event.data;
        else text = decoder.decode(new Uint8Array(event.data), { stream: true });
        this._feed(text);
      };
      ws.onerror = () => {
        if (this._closed) return;
        this._emit('error', new Event('error'));
      };
      ws.onclose = () => {
        if (this._closed) return;
        this.readyState = CLOSED;
        this._emit('error', new Event('error'));
        setTimeout(() => this._connect(), this._retry);
      };
    }

    _feed(text) {
      this._buffer += text;
      let boundary;
      while ((boundary = this._buffer.indexOf('\n\n')) >= 0) {
        const block = this._buffer.slice(0, boundary);
        this._buffer = this._buffer.slice(boundary + 2);
        this._dispatch(block);
      }
    }

    _dispatch(block) {
      let type = 'message';
      let data = '';
      let hasData = false;
      for (const rawLine of block.split(/\r?\n/)) {
        if (!rawLine || rawLine.startsWith(':')) continue;
        const colon = rawLine.indexOf(':');
        const field = colon >= 0 ? rawLine.slice(0, colon) : rawLine;
        let value = colon >= 0 ? rawLine.slice(colon + 1) : '';
        if (value.startsWith(' ')) value = value.slice(1);
        if (field === 'event') type = value || 'message';
        else if (field === 'data') {
          data += (hasData ? '\n' : '') + value;
          hasData = true;
        } else if (field === 'id') this._lastEventId = value;
        else if (field === 'retry' && /^\d+$/.test(value)) this._retry = Number(value);
      }
      if (!hasData) return;
      const event = new MessageEvent(type, {
        data,
        origin: new URL(this.url).origin,
        lastEventId: this._lastEventId,
      });
      this._emit(type, event);
    }

    _emit(type, event) {
      const handler = this[`on${type}`];
      if (typeof handler === 'function') handler.call(this, event);
      this.dispatchEvent(event);
    }

    close() {
      this._closed = true;
      this.readyState = CLOSED;
      try { this._ws?.close(); } catch (_) {}
    }
  }


  function bridgeTarget(target) {
    const mount = window.__orqellaAppMount || '';
    return `${mount ? `/p/${encodeURIComponent(mount)}` : ''}${target.pathname}${target.search}`;
  }

  function sseSocketUrl(url) {
    const target = new URL(url, location.href);
    const token = window.__orqellaAppAuthToken;
    if (token && !target.searchParams.has('auth_token')) {
      target.searchParams.set('auth_token', token);
    }
    const socketUrl = new URL('/__orqella_sse', location.origin);
    socketUrl.protocol = location.protocol === 'https:' ? 'wss:' : 'ws:';
    socketUrl.searchParams.set('target', bridgeTarget(target));
    return socketUrl.toString();
  }

  function sseResponse(url, signal) {
    let ws;
    const stream = new ReadableStream({
      start(controller) {
        ws = new WebSocket(sseSocketUrl(url));
        ws.binaryType = 'arraybuffer';
        ws.onmessage = (event) => {
          try {
            controller.enqueue(event.data);
          } catch (_) {}
        };
        ws.onerror = () => {
          try { controller.close(); } catch (_) {}
        };
        ws.onclose = () => {
          try { controller.close(); } catch (_) {}
        };
        signal?.addEventListener('abort', () => {
          try { ws.close(); } catch (_) {}
          try { controller.close(); } catch (_) {}
        }, { once: true });
      },
      cancel() {
        try { ws?.close(); } catch (_) {}
      },
    });
    return new Response(stream, {
      status: 200,
      headers: {
        'content-type': 'text/event-stream',
        'cache-control': 'no-cache',
        'x-orqella-sse': '1',
      },
    });
  }

  const nativeFetch = window.fetch.bind(window);
  window.fetch = function (input, init = {}) {
    try {
      const url = new URL(typeof input === 'string' ? input : input.url, location.href);
      const method = String(init.method || (typeof input !== 'string' && input.method) || 'GET').toUpperCase();
      const headers = new Headers(init.headers || (typeof input !== 'string' && input.headers) || {});
      const accept = (headers.get('accept') || '').toLowerCase();
      const looksLikeEventStream = /(?:^|\/)(?:event|events|stream)(?:\/|$)/i.test(url.pathname);
      if (url.origin === location.origin && method === 'GET' && (accept.includes('text/event-stream') || looksLikeEventStream)) {
        return Promise.resolve(sseResponse(url.toString(), init.signal));
      }
    } catch (_) {}
    return nativeFetch(input, init);
  };

  window.EventSource = TransportEventSource;
})();
