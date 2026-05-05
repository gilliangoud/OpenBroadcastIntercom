const invoke = window.__TAURI__.core.invoke;
const $ = id => document.getElementById(id);
let current = null;

function setMessage(text, kind = 'muted') {
  const el = $('message');
  el.className = kind;
  el.textContent = text;
}

function csv(values) {
  return (values || []).join(',');
}

function parseList(text) {
  return text.trim()
    ? text.split(',').map(value => value.trim()).filter(Boolean)
    : [];
}

function readNumber(id, fallback) {
  const value = Number($(id).value);
  return Number.isFinite(value) ? value : fallback;
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
  $('app_title').value = settings.app_title || 'Intercom Suite';
  $('server').value = settings.server || '127.0.0.1:40000';
  $('control').value = settings.control || 'ws://127.0.0.1:40001';
  $('user_id').value = settings.user_id ?? '';
  $('tx_channel').value = settings.tx_channel ?? 1;
  $('listen_channel').value = settings.listen_channel ?? 1;
  $('codec').value = settings.codec || 'pcm16';
  $('opus_profile').value = normalizeOpusProfile(settings.opus_profile);
  $('mic_gain').value = settings.mic_gain ?? 1;
  $('input_transient_suppression').checked = settings.input_transient_suppression !== false;
  $('speaker_gain').value = settings.speaker_gain ?? 1;
  $('jitter_ms').value = settings.jitter_ms ?? 40;
  $('input_backend').value = settings.input_backend || 'auto';
  $('input_device').value = settings.input_device || '';
  $('output_device').value = settings.output_device || '';
  $('buttons').value = csv(settings.buttons);
  $('button_keys').value = csv(settings.button_keys);
  $('local_ui_bind').value = settings.local_ui_bind || '127.0.0.1:41002';
  $('local_ui_token').value = settings.local_ui_token || '';
  $('disable_local_ui').checked = !!settings.disable_local_ui;
  $('window_mode').value = settings.window_mode || 'native';
  $('ui_open_delay_ms').value = settings.ui_open_delay_ms ?? 750;
}

function collect() {
  return {
    ...current,
    app_title: $('app_title').value.trim() || 'Intercom Suite',
    server: $('server').value.trim(),
    control: $('control').value.trim(),
    user_id: $('user_id').value ? readNumber('user_id', 1) : null,
    tx_channel: readNumber('tx_channel', 1),
    listen_channel: readNumber('listen_channel', 1),
    codec: $('codec').value,
    opus_profile: $('opus_profile').value,
    mic_gain: readNumber('mic_gain', 1),
    input_transient_suppression: $('input_transient_suppression').checked,
    speaker_gain: readNumber('speaker_gain', 1),
    jitter_ms: readNumber('jitter_ms', 40),
    input_backend: $('input_backend').value,
    input_device: $('input_device').value.trim() || null,
    output_device: $('output_device').value.trim() || null,
    buttons: parseList($('buttons').value),
    button_keys: parseList($('button_keys').value),
    local_ui_bind: $('local_ui_bind').value.trim(),
    local_ui_token: $('local_ui_token').value || null,
    disable_local_ui: $('disable_local_ui').checked,
    window_mode: $('window_mode').value,
    ui_open_delay_ms: readNumber('ui_open_delay_ms', 750)
  };
}

async function load() {
  try {
    fill(await invoke('load_native_settings'));
    setMessage('Settings loaded. Save changes and restart the client to apply startup settings.');
  } catch (err) {
    setMessage(String(err), 'error');
  }
}

async function loadDefaults() {
  try {
    fill(await invoke('default_native_settings'));
    setMessage('Defaults loaded. Save to replace the settings file.', 'ok');
  } catch (err) {
    setMessage(String(err), 'error');
  }
}

$('settings-form').addEventListener('submit', async event => {
  event.preventDefault();
  try {
    await invoke('save_native_settings', { settings: collect() });
    setMessage('Settings saved. Restart or reconnect the app to use startup-level changes.', 'ok');
  } catch (err) {
    setMessage(String(err), 'error');
  }
});

$('reload').addEventListener('click', load);
$('defaults').addEventListener('click', loadDefaults);
load();
