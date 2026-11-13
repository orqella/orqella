(() => {
  if (window.__orqellaTransportWebSocket) return;
  window.__orqellaTransportWebSocket = true;
  const NativeWebSocket = window.WebSocket;
  const sockets = new Map();
  let nextId = 1;

  class TransportWebSocket extends EventTarget {
    static CONNECTING = 0;
    static OPEN = 1;
    static CLOSING = 2;
    static CLOSED = 3;

    constructor(url, protocols) {
      super();
      this.url = String(url);
      this.protocol = '';
      this.extensions = '';
      this.binaryType = 'blob';
      this.readyState = TransportWebSocket.CONNECTING;
      this.onopen = null;
      this.onmessage = null;
      this.onerror = null;
      this.onclose = null;
      let parsed;
      try { parsed = new URL(this.url, location.href); } catch (_) { parsed = null; }
      const appMount = window.__orqellaAppMount || '';
      const appHost = parsed && (parsed.hostname.endsWith('.orqella.localhost') || parsed.hostname.endsWith('.localhost'));
      if (!parsed || (!appHost && parsed.host !== location.host) || !(appMount || parsed.pathname.startsWith('/m/') || parsed.pathname.startsWith('/api/') || parsed.pathname.startsWith('/p/'))) {
        this._native = new NativeWebSocket(url, protocols);
        this._native.binaryType = this.binaryType;
        this._native.onopen = (event) => this._emit('open', event);
        this._native.onmessage = (event) => this._emit('message', event);
        this._native.onerror = (event) => this._emit('error', event);
        this._native.onclose = (event) => this._emit('close', event);
        return;
      }
      this._id = `ws-${nextId++}`;
      sockets.set(this._id, this);
      window.addEventListener('message', this._onMessage = (event) => {
        const data = event.data;
        if (!data || data.id !== this._id) return;
        if (data.type === 'orqella-ws-open-ok') {
          this.readyState = TransportWebSocket.OPEN;
          this._emit('open', { type: 'open' });
        } else if (data.type === 'orqella-ws-message') {
          const bytes = Uint8Array.from(atob(data.data || ''), (c) => c.charCodeAt(0));
          const payload = data.kind === 'text' ? new TextDecoder().decode(bytes) : (this.binaryType === 'arraybuffer' ? bytes.buffer : new Blob([bytes]));
          this._emit('message', { type: 'message', data: payload });
        } else if (data.type === 'orqella-ws-error') {
          this._emit('error', { type: 'error', message: data.error });
          this._closeLocal(1006, data.error || 'WebSocket error');
        } else if (data.type === 'orqella-ws-close') {
          this._closeLocal(1000, 'closed');
        }
      });
      const isSseStream = parsed.pathname === '/__orqella_sse';
      window.top.postMessage({
        type: 'orqella-ws-open',
        id: this._id,
        path: isSseStream
          ? parsed.pathname + parsed.search
          : appMount ? `/p/${encodeURIComponent(appMount)}${parsed.pathname}` + parsed.search : parsed.pathname + parsed.search,
        headers: {},
      }, '*');
    }

    _emit(type, event) {
      const handler = this[`on${type}`];
      if (typeof handler === 'function') handler.call(this, event);
      this.dispatchEvent(new Event(type));
    }

    _closeLocal(code, reason) {
      if (this.readyState === TransportWebSocket.CLOSED) return;
      this.readyState = TransportWebSocket.CLOSED;
      sockets.delete(this._id);
      this._emit('close', { type: 'close', code, reason, wasClean: code === 1000 });
    }

    send(data) {
      if (this._native) return this._native.send(data);
      if (this.readyState !== TransportWebSocket.OPEN) throw new DOMException('WebSocket is not open', 'InvalidStateError');
      let bytes;
      let kind = 'binary';
      if (typeof data === 'string') {
        bytes = new TextEncoder().encode(data);
        kind = 'text';
      } else if (data instanceof ArrayBuffer) {
        bytes = new Uint8Array(data);
      } else if (ArrayBuffer.isView(data)) {
        bytes = new Uint8Array(data.buffer, data.byteOffset, data.byteLength);
      } else {
        throw new TypeError('Unsupported WebSocket payload');
      }
      let text = '';
      for (let i = 0; i < bytes.length; i += 0x8000) {
        text += String.fromCharCode(...bytes.subarray(i, i + 0x8000));
      }
      window.top.postMessage({ type: 'orqella-ws-send', id: this._id, kind, data: btoa(text) }, '*');
    }

    close(code = 1000, reason = '') {
      if (this._native) return this._native.close(code, reason);
      if (this.readyState >= TransportWebSocket.CLOSING) return;
      this.readyState = TransportWebSocket.CLOSING;
      window.top.postMessage({ type: 'orqella-ws-close', id: this._id }, '*');
      this._closeLocal(code, reason);
    }
  }

  Object.assign(window, { WebSocket: TransportWebSocket });
})();
