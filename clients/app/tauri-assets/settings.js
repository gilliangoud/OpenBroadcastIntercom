const invoke = window.__TAURI__?.core?.invoke;
const $ = id => document.getElementById(id);
const CONTROLS_PAGE = 'client-controls.html';
const DEFAULT_HOST = '127.0.0.1';
const AUDIO_PORT = 40000;
const CONTROL_PORT = 40001;
const ADMIN_PORT = 40002;
const MANUAL_SERVER_VALUE = '__manual__';

let current = null;
let currentLocalUiUrl = null;
let runtimeRunning = false;
let serverProfiles = [];

function setMessage(text, kind = '') {
  const message = $('message');
  message.textContent = text || '';
  message.className = kind ? `hint ${kind}` : 'hint';
}

function setStatus(text, kind = 'offline') {
  const status = $('status');
  status.textContent = text;
  status.className = `tag ${kind}`;
}

function setControlsUrl(url) {
  currentLocalUiUrl = url || null;
  $('close-config').disabled = !currentLocalUiUrl;
}

function setRuntimeRunning(running) {
  runtimeRunning = !!running;
  const button = $('start');
  button.textContent = runtimeRunning ? 'Disconnect' : 'Connect';
  button.classList.toggle('talk-button-main', !runtimeRunning);
  button.classList.toggle('disconnect-button', runtimeRunning);
  button.setAttribute('aria-pressed', runtimeRunning ? 'true' : 'false');
}

function serverProfileLabel(profile) {
  const badges = [];
  if (profile.discovered) badges.push('LAN');
  if (profile.last_connected_ms) badges.push('Recent');
  if (profile.auth === 'required') badges.push('Auth');
  return `${profile.name || profile.server_host || profile.control} - ${profile.server_host || profile.server}${badges.length ? ` (${badges.join(', ')})` : ''}`;
}

function selectedServerProfile() {
  const value = $('server-picker').value;
  if (!value || value === MANUAL_SERVER_VALUE) return null;
  return serverProfiles.find(profile => profile.id === value) || null;
}

function selectedServerHost() {
  const profile = selectedServerProfile();
  return normalizeHost(profile?.server_host) || normalizeHost($('server_host').value) || DEFAULT_HOST;
}

function setManualServerVisible(visible) {
  $('manual-server-row').hidden = !visible;
}

function syncServerSelection() {
  const picker = $('server-picker');
  const manual = picker.value === MANUAL_SERVER_VALUE || !selectedServerProfile();
  setManualServerVisible(manual);
  if (!manual) {
    const profile = selectedServerProfile();
    const host = normalizeHost(profile?.server_host);
    if (host) $('server_host').value = host;
    $('server-list-status').textContent = `Selected ${profile.name || host || profile.control}.`;
  } else {
    $('server-list-status').textContent = 'Manual server host will be used when you connect.';
  }
}

function setServerProfiles(profiles = [], opts = {}) {
  const byId = new Map();
  for (const profile of profiles) {
    if (!profile || !profile.id) continue;
    byId.set(profile.id, profile);
  }
  serverProfiles = Array.from(byId.values());
  const picker = $('server-picker');
  picker.innerHTML = '';

  picker.disabled = false;
  for (const profile of serverProfiles) {
    const option = document.createElement('option');
    option.value = profile.id;
    option.textContent = serverProfileLabel(profile);
    picker.appendChild(option);
  }

  const manual = document.createElement('option');
  manual.value = MANUAL_SERVER_VALUE;
  manual.textContent = 'Manual';
  picker.appendChild(manual);

  const currentHost = normalizeHost($('server_host').value || current?.server_host || DEFAULT_HOST);
  const currentControl = current?.control || controlForHost(currentHost);
  const currentProfile = serverProfiles.find(profile =>
    profile.control === currentControl || normalizeHost(profile.server_host) === currentHost
  );
  if (currentProfile) {
    picker.value = currentProfile.id;
  } else if (opts.preferFirst && serverProfiles.length) {
    picker.value = serverProfiles[0].id;
  } else {
    picker.value = MANUAL_SERVER_VALUE;
  }
  syncServerSelection();
}

async function openControls() {
  if (!currentLocalUiUrl) return;
  if (invoke) {
    try {
      await invoke('native_open_controls');
    } catch (err) {
      setMessage(`Could not open controls. ${err}`, 'error');
      return;
    }
  }
  sessionStorage.setItem('intercom-mobile-shell', '1');
  window.location.href = currentLocalUiUrl;
}

function showGainValues() {
  if ($('mic_gain_value') && $('mic_gain')) {
    $('mic_gain_value').textContent = Number($('mic_gain').value || 1).toFixed(2);
  }
  if ($('speaker_gain_value') && $('speaker_gain')) {
    $('speaker_gain_value').textContent = Number($('speaker_gain').value || 1).toFixed(2);
  }
}

function normalizeHost(host) {
  return String(host || '').trim().replace(/^\[(.*)\]$/, '$1');
}

function hostForUrl(host) {
  const normalized = normalizeHost(host);
  return normalized.includes(':') ? `[${normalized}]` : normalized;
}

function audioForHost(host) {
  const normalized = normalizeHost(host) || DEFAULT_HOST;
  return `${hostForUrl(normalized)}:${AUDIO_PORT}`;
}

function controlForHost(host) {
  const normalized = normalizeHost(host) || DEFAULT_HOST;
  return `ws://${hostForUrl(normalized)}:${CONTROL_PORT}`;
}

function adminForHost(host) {
  const normalized = normalizeHost(host) || DEFAULT_HOST;
  return `http://${hostForUrl(normalized)}:${ADMIN_PORT}`;
}

function fill(settings) {
  current = settings;
  $('server_host').value = settings.server_host || DEFAULT_HOST;
  $('opus_profile').value = settings.opus_profile || 'speech_24_standard';
  $('mic_gain').value = settings.mic_gain ?? 1;
  $('speaker_gain').value = settings.speaker_gain ?? 1;
  $('button_count').value = settings.button_count ?? 6;
  setServerProfiles(settings.server_profiles || serverProfiles);
  showGainValues();
}

function collect() {
  const host = selectedServerHost();
  return {
    ...(current || {}),
    app_title: 'RedLine',
    server_host: host,
    server: audioForHost(host),
    control: controlForHost(host),
    admin: adminForHost(host),
    advanced_endpoints: false,
    user_id: null,
    codec: 'opus',
    opus_profile: $('opus_profile').value,
    listen_channel: Number(current?.listen_channel ?? 0),
    tx_channel: Number(current?.tx_channel ?? 0),
    mic_gain: Number($('mic_gain').value || 1),
    speaker_gain: Number($('speaker_gain').value || 1),
    button_count: Number($('button_count').value || 6),
    buttons: [],
    button_keys: [],
    server_profiles: serverProfiles,
    disable_local_ui: false,
    window_mode: 'native',
  };
}

async function load() {
  if (!invoke) {
    setMessage('This page must be opened inside the RedLine macOS app.', 'error');
    return;
  }
  try {
    const settings = await invoke('load_native_settings');
    fill(settings);
    const status = await invoke('native_status');

    setControlsUrl(status.local_ui_url);
    const phase = status.phase || (status.running ? 'running' : 'stopped');
    setRuntimeRunning(status.running);
    setStatus(phase, phase === 'running' ? 'talk' : phase === 'failed' ? 'error' : phase === 'starting' ? 'starting' : 'offline');
    setMessage(status.last_error || (status.running ? 'Client connected. Close to return to controls.' : 'Choose a server, configure audio, and connect.'), status.last_error ? 'error' : status.running ? 'running' : '');
  } catch (err) {
    try {
      fill(await invoke('default_native_settings'));
    } catch (_) {}
    setRuntimeRunning(false);
    setStatus('error', 'error');
    setMessage(String(err), 'error');
  }
}

$('server_host').addEventListener('input', () => {
  $('server-picker').value = MANUAL_SERVER_VALUE;
  syncServerSelection();
});
$('mic_gain').addEventListener('input', showGainValues);
$('speaker_gain').addEventListener('input', showGainValues);

$('scan-servers').addEventListener('click', async event => {
  event.preventDefault();
  if (!invoke) return;
  $('scan-servers').disabled = true;
  $('server-list-status').textContent = 'Scanning local network...';
  try {
    const profiles = await invoke('native_discover_servers');
    setServerProfiles(profiles, { preferFirst: true });
    setMessage(profiles.length ? 'Scan refreshed servers. Select one or choose Manual.' : 'No RedLine servers found. Manual entry is still available.', profiles.length ? 'running' : '');
  } catch (err) {
    setMessage(String(err), 'error');
    $('server-list-status').textContent = 'Server scan failed.';
  } finally {
    $('scan-servers').disabled = false;
  }
});

$('server-picker').addEventListener('change', event => {
  event.preventDefault();
  syncServerSelection();
});

async function disconnectClient() {
  if (!invoke) return;
  try {
    $('start').disabled = true;
    await invoke('native_stop_client');
    setControlsUrl(null);
    setRuntimeRunning(false);
    setStatus('stopped', 'offline');
    setMessage('Client disconnected.');
  } catch (err) {
    setStatus('error', 'error');
    setMessage(String(err), 'error');
  } finally {
    $('start').disabled = false;
  }
}

$('close-config').addEventListener('click', event => {
  event.preventDefault();
  openControls();
});

$('mobile-form').addEventListener('submit', async event => {
  event.preventDefault();
  if (!invoke) return;
  if (runtimeRunning) {
    await disconnectClient();
    return;
  }
  try {
    $('start').disabled = true;
    setControlsUrl(null);
    setStatus('starting', 'starting');
    setMessage('Connecting audio client...');
    const response = await invoke('native_start_client', { settings: collect() });
    setControlsUrl(response.local_ui_url);
    setRuntimeRunning(response.running);
    setStatus(response.phase || 'running', 'talk');
    setMessage(response.last_error || 'Connected. Opening controls.', response.last_error ? 'error' : 'running');
    await openControls();
  } catch (err) {
    setRuntimeRunning(false);
    setStatus('error', 'error');
    setMessage(String(err), 'error');
  } finally {
    $('start').disabled = false;
  }
});

load();
