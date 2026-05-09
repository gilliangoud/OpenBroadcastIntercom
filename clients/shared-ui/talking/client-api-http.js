(() => {
  async function request(path, opts = {}) {
    const res = await fetch(path, {
      headers: { 'content-type': 'application/json' },
      ...opts
    });
    if (!res.ok) {
      let message = res.statusText;
      try {
        message = (await res.json()).error || message;
      } catch {
        // Keep the HTTP status text if the response is not JSON.
      }
      throw new Error(message);
    }
    return res.json();
  }

  function mobileShell() {
    return new URLSearchParams(window.location.search).get('mobile') === '1';
  }

  function setup() {
    if (mobileShell() && history.length > 1) {
      history.back();
      return;
    }
    window.location.href = '/';
  }

  window.intercomClientApi = {
    name: 'http',
    capabilities: {
      setup: mobileShell(),
      runtimeSettings: !mobileShell(),
      gain: true,
      macosMicrophoneModes: true
    },
    request,
    setup
  };
})();
