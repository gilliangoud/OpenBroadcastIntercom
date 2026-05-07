const invoke = window.__TAURI__?.core?.invoke;
const $ = id => document.getElementById(id);
const CONTROLS_PAGE = 'client-controls.html';
const DEFAULT_HOST = '127.0.0.1';
const AUDIO_PORT = 40000;
const CONTROL_PORT = 40001;
const ADMIN_PORT = 40002;

let current = null;
let currentLocalUiUrl = null;
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
  $('open-controls').disabled = !currentLocalUiUrl;
  $('close-config').disabled = !currentLocalUiUrl;
}

function serverProfileLabel(profile) {
  const badges = [];
  if (profile.discovered) badges.push('LAN');
  if (profile.last_connected_ms) badges.push('Recent');
  if (profile.auth === 'required') badges.push('Auth');
  return `${profile.name || profile.server_host || profile.control} - ${profile.server_host || profile.server}${badges.length ? ` (${badges.join(', ')})` : ''}`;
}

function setServerProfiles(profiles = []) {
  const byId = new Map();
  for (const profile of profiles) {
    if (!profile || !profile.id) continue;
    byId.set(profile.id, profile);
  }
  serverProfiles = Array.from(byId.values());
  const picker = $('server-picker');
  picker.innerHTML = '';
  if (!serverProfiles.length) {
    const option = document.createElement('option');
    option.value = '';
    option.textContent = 'No saved or discovered servers';
    picker.appendChild(option);
    picker.disabled = true;
    $('forget-server').disabled = true;
    $('server-list-status').textContent = 'Use Scan or enter server addresses manually.';
    return;
  }
  picker.disabled = false;
  for (const profile of serverProfiles) {
    const option = document.createElement('option');
    option.value = profile.id;
    option.textContent = serverProfileLabel(profile);
    picker.appendChild(option);
  }
  const currentProfile = serverProfiles.find(profile => profile.control === $('control').value.trim() || profile.server_host === $('server_host').value.trim());
  if (currentProfile) picker.value = currentProfile.id;
  $('forget-server').disabled = !picker.value;
  $('server-list-status').textContent = `${serverProfiles.length} server${serverProfiles.length === 1 ? '' : 's'} available.`;
}

async function openControls() {
  if (!currentLocalUiUrl) return;
  if (invoke) {
    try {
      await invoke('mobile_open_controls');
    } catch (err) {
      setMessage(`Could not open controls. ${err}`, 'error');
      return;
    }
  }
  sessionStorage.setItem('intercom-mobile-shell', '1');
  window.location.href = currentLocalUiUrl;
}

function parseList(value) {
  return value.split(',').map(item => item.trim()).filter(Boolean);
}

function csv(value) {
  return Array.isArray(value) ? value.join(',') : '';
}

function numberOrNull(value) {
  const trimmed = String(value ?? '').trim();
  if (!trimmed) return null;
  const parsed = Number(trimmed);
  return Number.isFinite(parsed) ? parsed : null;
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

function syncEndpointFields() {
  const advanced = $('advanced_endpoints').checked;
  $('advanced-connection').open = advanced;
  for (const id of ['server', 'control', 'admin']) {
    $(id).disabled = !advanced;
  }
  if (!advanced) {
    const host = $('server_host').value.trim() || DEFAULT_HOST;
    $('server').value = audioForHost(host);
    $('control').value = controlForHost(host);
    $('admin').value = adminForHost(host);
  }
}

function fill(settings) {
  current = settings;
  $('server_host').value = settings.server_host || DEFAULT_HOST;
  $('server').value = settings.server || '127.0.0.1:40000';
  $('control').value = settings.control || 'ws://127.0.0.1:40001';
  $('admin').value = settings.admin || adminForHost(settings.server_host || DEFAULT_HOST);
  $('advanced_endpoints').checked = !!settings.advanced_endpoints;
  $('user_id').value = settings.user_id ?? '';
  $('codec').value = settings.codec || 'pcm16';
  $('opus_profile').value = settings.opus_profile || 'speech_24_standard';
  $('listen_channel').value = settings.listen_channel ?? 1;
  $('tx_channel').value = settings.tx_channel ?? 1;
  $('mic_gain').value = settings.mic_gain ?? 1;
  $('speaker_gain').value = settings.speaker_gain ?? 1;
  $('button_count').value = settings.button_count ?? 6;
  $('buttons').value = csv(settings.buttons);
  $('button_keys').value = csv(settings.button_keys);
  setServerProfiles(settings.server_profiles || serverProfiles);
  renderCodecFields();
  syncEndpointFields();
}

function collect() {
  syncEndpointFields();
  return {
    ...(current || {}),
    server_host: normalizeHost($('server_host').value) || DEFAULT_HOST,
    server: $('server').value.trim(),
    control: $('control').value.trim(),
    admin: $('admin').value.trim() || null,
    advanced_endpoints: $('advanced_endpoints').checked,
    user_id: numberOrNull($('user_id').value),
    codec: $('codec').value,
    opus_profile: $('opus_profile').value,
    listen_channel: Number($('listen_channel').value || 1),
    tx_channel: Number($('tx_channel').value || 1),
    mic_gain: Number($('mic_gain').value || 1),
    speaker_gain: Number($('speaker_gain').value || 1),
    button_count: Number($('button_count').value || 6),
    buttons: parseList($('buttons').value),
    button_keys: parseList($('button_keys').value),
    server_profiles: serverProfiles,
    disable_local_ui: false,
    window_mode: 'native',
  };
}

function renderCodecFields() {
  $('opus-profile-row').hidden = $('codec').value !== 'opus';
}

async function load() {
  if (!invoke) {
    setMessage('This page must be opened inside the Tauri mobile app.', 'error');
    return;
  }
  try {
    const settings = await invoke('mobile_load_settings');
    fill(settings);
    const status = await invoke('mobile_status');

    setControlsUrl(status.local_ui_url);
    const phase = status.phase || (status.running ? 'running' : 'stopped');
    setStatus(phase, phase === 'running' ? 'talk' : phase === 'failed' ? 'error' : phase === 'starting' ? 'starting' : 'offline');
    setMessage(status.last_error || (status.running ? 'Client is running. Open Controls for the main client UI.' : 'Choose a server, configure audio, and start the client.'), status.last_error ? 'error' : status.running ? 'running' : '');
  } catch (err) {
    try {
      fill(await invoke('mobile_default_settings'));
    } catch (_) {}
    setStatus('error', 'error');
    setMessage(String(err), 'error');
  }
}

$('codec').addEventListener('change', renderCodecFields);
$('server_host').addEventListener('input', syncEndpointFields);
$('advanced_endpoints').addEventListener('change', syncEndpointFields);

$('save').addEventListener('click', async event => {
  event.preventDefault();
  if (!invoke) return;
  try {
    await invoke('mobile_save_settings', { settings: collect() });
    setMessage('Saved. Restart the client to apply connection changes.', 'running');
  } catch (err) {
    setMessage(String(err), 'error');
  }
});

$('scan-servers').addEventListener('click', async event => {
  event.preventDefault();
  if (!invoke) return;
  $('scan-servers').disabled = true;
  $('server-list-status').textContent = 'Scanning local network...';
  try {
    const profiles = await invoke('mobile_discover_servers');
    setServerProfiles(profiles);
    setMessage(profiles.length ? 'Select a server or keep manual addresses.' : 'No Intercom servers found. Manual entry is still available.', profiles.length ? 'running' : '');
  } catch (err) {
    setMessage(String(err), 'error');
    $('server-list-status').textContent = 'Server scan failed.';
  } finally {
    $('scan-servers').disabled = false;
  }
});

$('server-picker').addEventListener('change', async event => {
  event.preventDefault();
  if (!invoke || !$('server-picker').value) return;
  const profile = serverProfiles.find(item => item.id === $('server-picker').value);
  if (!profile) return;
  try {
    const settings = await invoke('mobile_select_server', { profile });
    fill(settings);
    setMessage(`Selected ${profile.name || profile.control}.`, 'running');
  } catch (err) {
    setMessage(String(err), 'error');
  }
});

$('forget-server').addEventListener('click', async event => {
  event.preventDefault();
  if (!invoke || !$('server-picker').value) return;
  try {
    const settings = await invoke('mobile_forget_server', { id: $('server-picker').value });
    fill(settings);
    setMessage('Server removed from saved list.');
  } catch (err) {
    setMessage(String(err), 'error');
  }
});

$('stop').addEventListener('click', async event => {
  event.preventDefault();
  if (!invoke) return;
  try {
    await invoke('mobile_stop_client');
    setControlsUrl(null);
    setStatus('stopped', 'offline');
    setMessage('Client stopped.');
  } catch (err) {
    setStatus('error', 'error');
    setMessage(String(err), 'error');
  }
});

$('open-controls').addEventListener('click', event => {
  event.preventDefault();
  openControls();
});

$('close-config').addEventListener('click', event => {
  event.preventDefault();
  openControls();
});

$('mobile-form').addEventListener('submit', async event => {
  event.preventDefault();
  if (!invoke) return;
  try {
    setControlsUrl(null);
    setStatus('starting', 'starting');
    setMessage('Starting audio client...');
    const response = await invoke('mobile_start_client', { settings: collect() });
    setControlsUrl(response.local_ui_url);
    setStatus(response.phase || 'running', 'talk');
    setMessage(response.last_error || 'Opening client controls.', response.last_error ? 'error' : 'running');
    await openControls();
  } catch (err) {
    setStatus('error', 'error');
    setMessage(String(err), 'error');
  }
});

load();
