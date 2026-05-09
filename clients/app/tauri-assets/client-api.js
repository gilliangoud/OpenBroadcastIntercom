(() => {
  const invoke = window.__TAURI__?.core?.invoke;

  async function request(path, opts = {}) {
    if (!invoke) {
      throw new Error('Client controls require the Tauri app runtime.');
    }

    const method = opts.method || 'GET';
    const body = opts.body ? JSON.parse(opts.body) : {};

    if (method === 'GET' && path === '/state') return invoke('client_state');
    if (method === 'PUT' && path === '/config') return invoke('client_config', { request: body });
    if (method === 'POST' && path === '/talk-mode') return invoke('client_talk_mode', { mode: body.mode });
    if (method === 'POST' && path === '/mute') return invoke('client_mute');
    if (method === 'POST' && path === '/unmute') return invoke('client_unmute');
    if (method === 'POST' && path === '/talk/down') return invoke('client_talk_down');
    if (method === 'POST' && path === '/talk/up') return invoke('client_talk_up');
    if (method === 'POST' && path === '/talk/toggle') return invoke('client_talk_toggle');
    if (method === 'POST' && path === '/codec') return invoke('client_codec', { codec: body.codec });
    if (method === 'POST' && path === '/gain') return invoke('client_gain', { request: body });

    let match = path.match(/^\/buttons\/(.+)\/(down|up|toggle)$/);
    if (method === 'POST' && match) {
      return invoke(`client_button_${match[2]}`, { id: decodeURIComponent(match[1]) });
    }

    match = path.match(/^\/calls\/(\d+)\/(down|up|toggle)$/);
    if (method === 'POST' && match) {
      return invoke(`client_call_${match[2]}`, { id: Number(match[1]) });
    }

    if (method === 'POST' && path === '/reply/down') return invoke('client_reply_down');
    if (method === 'POST' && path === '/reply/up') return invoke('client_reply_up');
    if (method === 'POST' && path === '/reply/toggle') return invoke('client_reply_toggle');
    if (method === 'POST' && path === '/alerts') return invoke('client_send_alert', { request: body });

    match = path.match(/^\/alerts\/(\d+)\/ack$/);
    if (method === 'POST' && match) return invoke('client_ack_alert', { id: Number(match[1]) });

    match = path.match(/^\/alerts\/(\d+)\/cancel$/);
    if (method === 'POST' && match) return invoke('client_cancel_alert', { id: Number(match[1]) });

    throw new Error(`Unsupported local client command ${method} ${path}`);
  }

  function mobileShell() {
    try {
      const params = new URLSearchParams(window.location.search);
      return params.get('mobile') === '1' || sessionStorage.getItem('intercom-mobile-shell') === '1';
    } catch (_) {
      return false;
    }
  }

  function setup() {
    if (mobileShell()) {
      window.location.href = 'mobile.html';
      return;
    }
    return invoke('open_native_settings');
  }

  const setupAvailable = mobileShell() || !!invoke;

  window.intercomClientApi = {
    name: 'tauri',
    capabilities: {
      setup: setupAvailable,
      runtimeSettings: false,
      gain: true,
      macosMicrophoneModes: false
    },
    request,
    setup
  };
})();
