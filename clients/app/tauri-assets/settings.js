const invoke = window.__TAURI__?.core?.invoke;
const $ = id => document.getElementById(id);
const CONTROLS_PAGE = 'client-controls.html';

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
  return `${profile.name || profile.control} - ${profile.server}${badges.length ? ` (${badges.join(', ')})` : ''}`;
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
  const currentProfile = serverProfiles.find(profile => profile.control === $('control').value.trim());
  if (currentProfile) picker.value = currentProfile.id;
  $('forget-server').disabled = !picker.value;
  $('server-list-status').textContent = `${serverProfiles.length} server${serverProfiles.length === 1 ? '' : 's'} available.`;
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

function normalizeOpusProfile(profile) {
  return {
    speech_low: 'speech_16_low',
    speech_standard: 'speech_24_standard',
    speech_high: 'speech_48_high',
    music_high: 'music_48'
  }[profile] || profile || 'speech_24_standard';
}

function fill(settings) {
  current = settings;
  $('server').value = settings.server || '127.0.0.1:40000';
  $('control').value = settings.control || 'ws://127.0.0.1:40001';
  $('user_id').value = settings.user_id ?? '';
  $('codec').value = settings.codec || 'pcm16';
  $('opus_profile').value = normalizeOpusProfile(settings.opus_profile);
  $('listen_channel').value = settings.listen_channel ?? 1;
  $('tx_channel').value = settings.tx_channel ?? 1;
  $('mic_gain').value = settings.mic_gain ?? 1;
  $('speaker_gain').value = settings.speaker_gain ?? 1;
  $('buttons').value = csv(settings.buttons);
  $('button_keys').value = csv(settings.button_keys);
  setServerProfiles(settings.server_profiles || serverProfiles);
  renderCodecFields();
}

function collect() {
  return {
    ...(current || {}),
    app_title: (current && current.app_title) || 'RedLine',
    server: $('server').value.trim(),
    control: $('control').value.trim(),
    user_id: numberOrNull($('user_id').value),
    codec: $('codec').value,
    opus_profile: $('opus_profile').value,
    listen_channel: Number($('listen_channel').value || 1),
    tx_channel: Number($('tx_channel').value || 1),
    mic_gain: Number($('mic_gain').value || 1),
    speaker_gain: Number($('speaker_gain').value || 1),
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

async function refreshStatus() {
  const status = await invoke('native_status');
  setControlsUrl(status.local_ui_url || (status.running ? CONTROLS_PAGE : null));
  const phase = status.phase || (status.running ? 'running' : 'stopped');
  setStatus(phase, phase === 'running' ? 'talk' : phase === 'failed' ? 'error' : phase === 'starting' ? 'starting' : 'offline');
  setMessage(status.last_error || (status.running ? 'Client is running. Open Controls for the main client UI.' : 'Choose a server, configure audio, and start the client.'), status.last_error ? 'error' : status.running ? 'running' : '');
}

async function load() {
  if (!invoke) {
    setMessage('This page must be opened inside the RedLine macOS app.', 'error');
    return;
  }
  try {
    const settings = await invoke('load_native_settings');
    fill(settings);
    await refreshStatus();
  } catch (err) {
    try {
      fill(await invoke('default_native_settings'));
    } catch (_) {}
    setStatus('error', 'error');
    setMessage(String(err), 'error');
  }
}

$('codec').addEventListener('change', renderCodecFields);

$('save').addEventListener('click', async event => {
  event.preventDefault();
  if (!invoke) return;
  try {
    await invoke('save_native_settings', { settings: collect() });
    setMessage('Saved. Use Start to connect with these settings.', 'running');
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
    const profiles = await invoke('native_discover_servers');
    setServerProfiles(profiles);
    setMessage(profiles.length ? 'Select a server or keep manual addresses.' : 'No RedLine servers found. Manual entry is still available.', profiles.length ? 'running' : '');
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
    const settings = await invoke('native_select_server', { profile });
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
    const settings = await invoke('native_forget_server', { id: $('server-picker').value });
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
    await invoke('native_stop_client');
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
    const response = await invoke('native_start_client', { settings: collect() });
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
