(() => {
  const params = new URLSearchParams(location.search);
  const mount = params.get('mount') || '';
  const target = decodeURIComponent(params.get('target') || '/');
  let authToken = '';
  try { authToken = new URL(target, location.origin).searchParams.get('auth_token') || ''; } catch (_) {}
  const frame = document.getElementById('app');
  const status = document.getElementById('status');

  function showError(error) {
    status.hidden = false;
    status.textContent = String(error?.stack || error);
  }

  function forwardToTransport(request) {
    return new Promise((resolve, reject) => {
      const channel = new MessageChannel();
      const timer = setTimeout(() => reject(new Error('transport request timed out')), 30000);
      channel.port1.onmessage = (event) => {
        clearTimeout(timer);
        resolve(event.data);
      };
      window.top.postMessage({ type: 'orqella-app-http', request }, '*', [channel.port2]);
    });
  }

  async function main() {
    if (!mount) throw new Error('missing app mount');
    const registration = await navigator.serviceWorker.register('/app-sw.js', {
      scope: '/',
      updateViaCache: 'none',
    });
    await registration.update().catch(() => {});
    await navigator.serviceWorker.ready;
    if (!navigator.serviceWorker.controller) {
      await new Promise((resolve) => {
        const done = () => resolve();
        navigator.serviceWorker.addEventListener('controllerchange', done, { once: true });
        setTimeout(done, 1500);
      });
    }
    const register = () => {
      navigator.serviceWorker.controller?.postMessage({ type: 'orqella-app-register', mount, authToken });
    };
    register();
    navigator.serviceWorker.addEventListener('controllerchange', register);
    navigator.serviceWorker.addEventListener('message', async (event) => {
      if (event.data?.type !== 'orqella-app-request') return;
      const port = event.ports?.[0];
      if (!port) return;
      try {
        port.postMessage(await forwardToTransport(event.data.request));
      } catch (error) {
        port.postMessage({ error: String(error?.stack || error) });
      }
    });
    frame.onload = () => {
      status.hidden = true;
      frame.hidden = false;
    };
    frame.src = target;
  }

  main().catch(showError);
})();
