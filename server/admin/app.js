let state = { sessions: [], clients: [], devices: [], channels: [], metrics: {}, warnings: [], deepfilternet: { models: [] }, models: { models: [] } };
let selectedUser = null;
let refreshTimer = null;
let recordingModelTouched = false;
let clientEditorDirty = false;
let clientTypeDefaults = {};
try { clientTypeDefaults = JSON.parse(window.localStorage.getItem('redline.clientTypeDefaults') || '{}'); } catch { clientTypeDefaults = {}; }

const page = document.body.dataset.page || 'dashboard';
const $ = (id) => document.getElementById(id);
const root = () => $('page-root');
const modalRoot = () => $('modal-root');
const editorRoot = () => $('client-detail') || modalRoot();

async function api(path, opts = {}) {
  const res = await fetch('/admin/api' + path, {
    headers: { 'content-type': 'application/json' },
    ...opts,
  });
  if (!res.ok) {
    let msg = res.statusText;
    try { msg = (await res.json()).error || msg; } catch {}
    throw new Error(msg);
  }
  return res.json();
}

function esc(value) {
  return String(value ?? '').replace(/[&<>"']/g, (ch) => ({
    '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;',
  }[ch]));
}
function csv(values) { return [...(values || [])].sort((a, b) => Number(a) - Number(b)).join(','); }
function parseCsv(value, { allowZero = false } = {}) {
  const minimum = allowZero ? 0 : 1;
  return String(value || '').split(',').map((v) => Number(v.trim())).filter((v) => Number.isInteger(v) && v >= minimum);
}
function sorted(values, { allowZero = false } = {}) {
  const minimum = allowZero ? 0 : 1;
  return [...new Set((values || []).map(Number).filter((v) => Number.isInteger(v) && v >= minimum))].sort((a, b) => a - b);
}
function sortedChannels(values) { return sorted(values, { allowZero: true }); }
const METER_FLOOR_DB = -60;
function finite(value) {
  const number = Number(value);
  return Number.isFinite(number) ? number : 0;
}
function dbfs(value) {
  const number = finite(value);
  if (number <= 0) return METER_FLOOR_DB;
  return Math.max(METER_FLOOR_DB, Math.min(0, 20 * Math.log10(number)));
}
function dbfsText(value) {
  const number = finite(value);
  if (number <= 0) return '-inf dBFS';
  return `${dbfs(number).toFixed(0)} dBFS`;
}
function meterPercent(value) {
  return Math.max(0, Math.min(100, Math.round(((dbfs(value) - METER_FLOOR_DB) / -METER_FLOOR_DB) * 100)));
}
function levelText(value) {
  return `${dbfsText(value)} / ${pct(value)} linear`;
}
function meter(value, cls = '', peakValue = null) {
  const fill = meterPercent(value);
  const peak = peakValue == null ? null : meterPercent(peakValue);
  const title = peak == null ? `RMS ${levelText(value)}` : `RMS ${levelText(value)}; peak ${levelText(peakValue)}`;
  return `<span class="meter ${cls}" title="${esc(title)}" aria-label="${esc(title)}"><span class="fill" style="width:${fill}%"></span>${peak == null ? '' : `<span class="peak" style="left:${peak}%"></span>`}</span>`;
}
function badge(text, cls = '') { return `<span class="badge ${cls}">${esc(text)}</span>`; }
function pct(value) { return `${Math.round((Number(value) || 0) * 100)}%`; }
function desired(userId) { return (state.clients || []).find((client) => client.user_id === userId); }
function session(userId) { return (state.sessions || []).find((item) => item.user_id === userId); }
function deviceByUser(userId) { return (state.devices || []).find((item) => item.user_id === Number(userId)); }
function deviceByUid(uid) { return (state.devices || []).find((item) => item.client_uid === uid); }
function mergedUsers() {
  return sorted([...(state.clients || []).map((c) => c.user_id), ...(state.sessions || []).map((s) => s.user_id), ...(state.devices || []).map((d) => d.user_id)]);
}
function nextUserId() {
  const used = new Set(mergedUsers());
  let id = 1;
  while (used.has(id)) id += 1;
  return id;
}
function clientLabel(id) {
  const d = desired(Number(id)) || {};
  const s = session(Number(id)) || {};
  const device = deviceByUser(Number(id)) || {};
  const name = d.name || s.name || device.name || '';
  return name ? `${id} ${name}` : `${id}`;
}
function clientUidForUser(id) {
  return desired(Number(id))?.client_uid || session(Number(id))?.client_uid || deviceByUser(Number(id))?.client_uid || '';
}
function clientEditorName(id, cfg, live) {
  const device = deviceByUser(Number(id)) || {};
  const uid = cfg.client_uid || live.client_uid || device.client_uid || '';
  const role = cfg.role || live.role || device.role || '';
  const name = String(cfg.name || live.name || device.name || '').trim();
  return name && name !== uid && name !== role ? name : '';
}
function persistClientTypeDefaults() {
  window.localStorage.setItem('redline.clientTypeDefaults', JSON.stringify(clientTypeDefaults));
}
function defaultCapabilitiesForType(type) {
  const actions = ['transmit', 'alert', 'apply_preset', 'set_talk_mode', 'route_edit'];
  const base = {
    advertised: false,
    client_kind: type || 'unknown',
    supports_processing: false,
    supports_native_voice_processing: false,
    supports_esp32_audio: false,
    supports_stereo: false,
    supports_ifb: false,
    supports_local_api: false,
    supports_device_selection: false,
    button_action_types: actions,
  };
  if (type === 'desktop') return { ...base, supports_processing: true, supports_native_voice_processing: true, supports_stereo: true, supports_ifb: true, supports_local_api: true, supports_device_selection: true };
  if (type === 'mobile') return { ...base, supports_native_voice_processing: true, supports_stereo: true, supports_ifb: true };
  if (type === 'pi') return { ...base, supports_stereo: true, supports_ifb: true, supports_local_api: true, supports_device_selection: true };
  if (type === 'esp32') return { ...base, supports_esp32_audio: true, button_action_types: actions };
  if (type === 'bridge') return { ...base, supports_stereo: true, supports_device_selection: true, button_action_types: [] };
  return { ...base, client_kind: 'desktop', supports_processing: true, supports_native_voice_processing: true, supports_stereo: true, supports_ifb: true, supports_local_api: true, supports_device_selection: true };
}
function fallbackClientType(userId, cfg = {}, live = {}, device = {}) {
  const stored = clientTypeDefaults[userId];
  if (stored) return stored;
  if ((cfg.role || live.role || device.role) === 'bridge') return 'bridge';
  const kind = live.capture?.runtime?.client_kind || live.capabilities?.client_kind || device.capabilities?.client_kind;
  if (kind && kind !== 'unknown') return kind;
  return 'desktop';
}
function capabilityContext(userId, cfg = {}, live = {}, device = {}) {
  if (live.capabilities?.advertised) return { capabilities: live.capabilities, source: 'live advertisement', fallback: false };
  if (device.capabilities?.advertised) return { capabilities: device.capabilities, source: 'last-known advertisement', fallback: false };
  const type = fallbackClientType(userId, cfg, live, device);
  return { capabilities: defaultCapabilitiesForType(type), source: `${type} default`, fallback: true };
}
function clientSupportedCodecs(userId, cfg = {}, live = {}, device = {}, capabilities = {}) {
  const liveCodecs = live.supported_codecs || [];
  if (liveCodecs.length) return liveCodecs;
  const deviceCodecs = device.supported_codecs || [];
  if (deviceCodecs.length) return deviceCodecs;
  if ((capabilities.client_kind || '') === 'bridge') return ['pcm16', 'pcm24', 'pcm48', 'opus'];
  return ['pcm16', 'pcm24', 'pcm48', 'opus'];
}
function clientAdvertisedButtons(live = {}, device = {}) {
  return (live.advertised_buttons || []).length ? live.advertised_buttons : (device.advertised_buttons || []);
}
function channelLabel(id) {
  const ch = (state.channels || []).find((item) => item.id === Number(id));
  return ch ? `${id} ${ch.name}` : `${id}`;
}
function collectChannels(client, ids) {
  for (const ch of client.listen || []) ids.add(Number(ch));
  for (const ch of client.tx || []) ids.add(Number(ch));
  for (const ch of client.priority_channels || []) ids.add(Number(ch));
  for (const ch of Object.keys(client.vol || {})) ids.add(Number(ch));
  for (const ch of Object.keys(client.stereo?.channel_pan || {})) ids.add(Number(ch));
  for (const ch of client.ifb?.program || []) ids.add(Number(ch));
  for (const ch of client.ifb?.interrupt || []) ids.add(Number(ch));
  for (const button of client.buttons || []) {
    for (const action of button.actions || []) {
      if (action.type === 'transmit') for (const ch of action.channels || []) ids.add(Number(ch));
    }
  }
}
function allChannelIds(extra = []) {
  const ids = new Set((state.channels || []).map((ch) => Number(ch.id)));
  for (const item of state.clients || []) collectChannels(item, ids);
  for (const item of state.sessions || []) collectChannels(item, ids);
  for (const item of extra) collectChannels(item, ids);
  if (!ids.size) ids.add(0);
  return sortedChannels([...ids]);
}
function defaultLockout() {
  return {
    allow_channels: true, allow_volumes: true, allow_codec: true, allow_talk_mode: true,
    allow_priority: true, allow_buttons: true, allow_ifb: true, allow_device_selection: true,
    allow_local_api: true,
  };
}
function defaultProcessing() {
  return {
    mode: 'auto',
    engine: 'built_in',
    profile: 'voice',
    high_pass: true,
    noise_gate: true,
    compressor: true,
    presence: true,
    vad: true,
    transient_suppression: true,
    native_voice_processing: true,
    fallback_to_builtin: true,
    deep_filter_model: '',
    deep_filter_backend: 'auto',
    apple_compute_units: 'all',
    worker_queue_frames: 12,
    pipeline: [],
    normalization: {
      enabled: false,
      target_rms: 0.14,
      max_boost: 4,
      max_attenuation: 8,
      adaptation_ms: 250,
      noise_floor_rms: 0.012,
    },
  };
}
function processingPipelinePresetValue(pipeline) {
  const engines = (pipeline || []).filter(stage => stage?.enabled !== false).map(stage => stage.engine).join(',');
  const known = ['', 'webrtc,built_in', 'webrtc,rnnoise,built_in', 'rnnoise,built_in', 'deepfilternet,built_in'];
  return known.includes(engines) ? engines : '';
}
function processingPipelineFromPreset(value) {
  return (value || '').split(',').filter(Boolean).map(engine => ({ engine, enabled: true }));
}
function processingStageText(status) {
  const stages = status?.stages || [];
  if (!stages.length) return '';
  return stages.map(stage => {
    const backend = stage.backend ? `/${stage.backend}` : '';
    const time = Number.isFinite(stage.inference_ms) ? ` ${Number(stage.inference_ms).toFixed(1)}ms` : '';
    return `${stage.engine}${backend}${stage.available === false ? ' unavailable' : ''}${stage.bypassed ? ' bypassed' : ''}${time}`;
  }).join(' -> ');
}
function normalizationStatusText(status) {
  if (!status) return '';
  const gain = Number(status.applied_gain || 1).toFixed(2);
  const input = meterPercent(status.input_rms);
  const output = meterPercent(status.output_rms);
  const target = meterPercent(status.target_rms);
  if (status.active) {
    return `${gain}x input ${input}% -> ${output}% target ${target}%${status.reason && status.reason !== 'active' ? ` (${status.reason})` : ''}`;
  }
  return `bypassed: ${status.reason || 'inactive'} input ${input}% target ${target}%`;
}
function pipelineStatusHtml(live = {}) {
  const rows = [];
  const processing = live.processing_status || {};
  if (processing.engine) rows.push(`<span>Active engine ${esc(processing.engine)}</span>`);
  const stageText = processingStageText(processing);
  if (stageText) rows.push(`<span>Stages ${esc(stageText)}</span>`);
  const normalization = normalizationStatusText(processing.normalization);
  if (normalization) rows.push(`<span>Leveler ${esc(normalization)}</span>`);
  if (processing.engine_detail) rows.push(`<span>${esc(processing.engine_detail)}</span>`);
  if (state.deepfilternet?.detail) rows.push(`<span>${esc(state.deepfilternet.detail)}</span>`);
  if (!rows.length) rows.push('<span>No live processing status from this client yet.</span>');
  return `<div class="pipeline-status wide">${rows.join('')}</div>`;
}
function deepFilterNetModelOptions(selected) {
  const models = state.deepfilternet?.models || [];
  const values = new Set(models.map((model) => model.path));
  const options = [`<option value="">No DeepFilterNet model selected</option>`];
  if (selected && !values.has(selected)) {
    options.push(`<option value="${esc(selected)}" selected>Current custom path - ${esc(selected)}</option>`);
  }
  for (const model of models) {
    const selectedAttr = selected && selected === model.path ? ' selected' : '';
    options.push(`<option value="${esc(model.path)}"${selectedAttr}>${esc(model.name)}</option>`);
  }
  if (!models.length) {
    options.push(`<option value="" disabled>No models found in ${esc(state.deepfilternet?.model_dir || 'deepfilternet-models')}</option>`);
  }
  return options.join('');
}
function deepFilterNetBackendOptions() {
  const coremlLabel = state.deepfilternet?.coreml_runtime_available ? 'Apple Core ML' : 'Apple Core ML (planned)';
  return `<option value="auto">Auto</option><option value="tract">Tract CPU</option><option value="coreml">${esc(coremlLabel)}</option>`;
}
function defaultEsp32Audio() {
  return {
    enabled: false,
    adc_input: 'difference',
    mic_pga_gain_db: 9,
    capture_channel: 'left',
    mic_software_gain_percent: 100,
    speaker_software_gain_percent: 100,
    notification_gain_percent: 50,
    high_pass_enabled: true,
    alc_enabled: true,
    noise_gate_enabled: true,
    sidetone: { mode: 'off', firmware_gain_percent: 25, codec_bypass_gain_percent: 25, mic_bypass_gain_percent: 100 },
  };
}
function defaultClient(userId) {
  return {
    user_id: userId || nextUserId(), client_uid: null, role: 'client', name: '', listen: [], tx: [], vol: {}, talker_vol: {},
    codec: 'pcm16', opus_profile: 'speech_24_standard', talk_mode: 'ptt', priority: false,
    priority_channels: [], buttons: [], ifb: { enabled: false, program: [], interrupt: [], duck_gain: 0.125 },
    lockout: defaultLockout(), stereo: { enabled: false, channel_pan: {} }, processing: defaultProcessing(),
    esp32_audio: defaultEsp32Audio(),
  };
}
function shownClient(userId) { return { ...defaultClient(userId), ...(session(userId) || {}), ...(desired(userId) || {}) }; }
function codecName(codec) {
  return { pcm16: 'PCM 16', pcm24: 'PCM 24', pcm48: 'PCM 48', opus: 'Opus' }[codec] || codec || '-';
}
function normalizeOpusProfile(profile) {
  return {
    speech_low: 'speech_16_low',
    speech_standard: 'speech_24_standard',
    speech_high: 'speech_48_high',
    music_high: 'music_48',
  }[profile] || profile || 'speech_24_standard';
}
function opusProfileName(profile) {
  return {
    speech_16_low: 'Speech 16 Low',
    speech_24_standard: 'Speech 24 Standard',
    speech_48_high: 'Speech 48 High',
    music_48: 'Music 48',
  }[normalizeOpusProfile(profile)] || profile || 'Speech 24 Standard';
}
function audioLabel(item) {
  const codec = codecName(item.codec);
  return item.codec === 'opus' ? `${codec} ${opusProfileName(item.opus_profile)}` : codec;
}
function codecOptionsHtml(live = {}, cfg = {}, supportedCodecs = null) {
  const supported = new Set(supportedCodecs || live.supported_codecs || []);
  const canValidate = supported.size > 0;
  const current = cfg.codec || 'pcm16';
  const options = [
    ['pcm16', 'PCM Low CPU (16 kHz)'],
    ['pcm24', 'PCM Balanced (24 kHz)'],
    ['pcm48', 'PCM High Quality (48 kHz)'],
    ['opus', 'Opus'],
  ].filter(([value]) => !supported.size || supported.has(value));
  if (current && !options.some(([value]) => value === current)) {
    options.unshift([current, `${codecName(current)} (currently saved)`]);
  }
  return options.map(([value, label]) => {
    const unsupported = canValidate && !supported.has(value);
    const selected = current === value;
    const suffix = unsupported ? ' (unsupported by connected client)' : '';
    return `<option value="${value}" ${selected ? 'selected' : ''} ${unsupported && !selected ? 'disabled' : ''} ${unsupported && selected ? 'hidden' : ''}>${label}${suffix}</option>`;
  }).join('');
}
function liveCodecNoteHtml(live = {}, cfg = {}, supportedCodecs = null, source = 'client advertisement') {
  const codecs = supportedCodecs || live.supported_codecs || [];
  if (!codecs.length) return '';
  const supported = new Set(codecs);
  if (!supported.has(cfg.codec || 'pcm16')) {
    return `<p class="muted wide">Desired codec ${esc(codecName(cfg.codec))} is not advertised by this connected client. The server will save it as desired state, but live audio falls back to ${esc(codecName(live.codec))} until the client supports it.</p>`;
  }
  return `<p class="muted wide">Codecs from ${esc(source)}: ${codecs.map(codecName).join(', ')}</p>`;
}
function alertTargetLabel(target) { return !target ? '-' : `${target.kind} ${target.id || ''}`; }
function directCallSummary(calls) {
  const active = (calls || []).filter((call) => call.active);
  return active.length ? active.map((call) => `${call.caller}->${call.target}${call.duck ? ' duck' : ''}`).join(', ') : '-';
}
function buttonActionSummary(button) {
  const actions = button.actions || [];
  if (!actions.length) return 'no actions';
  return actions.map((action) => {
    if (action.type === 'transmit') return `TX ch ${csv(action.channels)} users ${csv(action.users)}${action.duck ? ' duck' : ''}`;
    if (action.type === 'alert') return `alert ${(action.targets || []).map(alertTargetLabel).join(', ')}`;
    if (action.type === 'apply_preset') return `apply ${action.preset_id}`;
    if (action.type === 'set_talk_mode') return `talk ${action.mode}`;
    if (action.type === 'route_edit') return 'route edit';
    return action.type;
  }).join(' | ');
}
function userRows() {
  return mergedUsers().map((id) => {
    const d = desired(id) || {};
    const s = session(id) || {};
    const item = { ...d, ...s };
    const input = s.input || {};
    const output = s.output || {};
    return `<tr class="clickable" data-open-client="${id}">
      <td><strong>${esc(clientLabel(id))}</strong></td>
      <td>${esc(item.role || 'client')}</td>
      <td>${bridgeText(item)}</td>
      <td>${s.addr ? badge('online') : badge('offline', 'offline')}</td>
      <td>${esc(audioLabel(item))}</td>
      <td>${item.stereo?.enabled ? badge(item.stereo_status?.active ? 'stereo' : 'configured', item.stereo_status?.active ? '' : 'warn') : '-'}</td>
      <td>${esc(item.talk_mode || 'ptt')}</td>
      <td>${s.regular_talk_active ? badge('active', 'talk') : '-'}</td>
      <td>${input.active ? badge('talking', 'talk') : '-'}</td>
      <td>${meter(input.rms, '', input.peak)}</td>
      <td>${meter(output.rms, 'out', output.peak)}</td>
      <td>${csv(item.listen)}</td>
      <td>${csv(item.tx)}</td>
      <td>${s.queue_depth ?? ''}</td>
      <td>${healthText(s)}</td>
    </tr>`;
  }).join('');
}
function clientField(label, value, cls = '') {
  return `<div class="client-field ${cls}"><dt>${esc(label)}</dt><dd>${value || '-'}</dd></div>`;
}
function clientCards() {
  const cards = mergedUsers().map((id) => {
    const d = desired(id) || {};
    const s = session(id) || {};
    const item = { ...d, ...s };
    const input = s.input || {};
    const output = s.output || {};
    const online = !!s.addr;
    const bridge = bridgeText(item);
    return `<article class="client-card clickable ${selectedUser === id ? 'active' : ''}" data-open-client="${id}" role="button" tabindex="0">
      <div class="client-card-head">
        <div class="client-title"><strong>${esc(clientLabel(id))}</strong><span>${esc(item.role || 'client')} · ${esc(audioLabel(item))}</span></div>
        <div class="client-badges">${online ? badge('online') : badge('offline', 'offline')}${item.stereo?.enabled ? badge(item.stereo_status?.active ? 'stereo' : 'stereo configured', item.stereo_status?.active ? '' : 'warn') : ''}</div>
      </div>
      ${(item.role || '') === 'bridge' ? `<div class="client-card-bridge">${bridge}</div>` : ''}
      <dl class="client-card-grid">
        ${clientField('Talk Mode', esc(item.talk_mode || 'ptt'))}
        ${clientField('Regular Talk', s.regular_talk_active ? badge('active', 'talk') : '-')}
        ${clientField('Talk', input.active ? badge('talking', 'talk') : '-')}
        ${clientField('Input', meter(input.rms, '', input.peak), 'meter-field')}
        ${clientField('Output', meter(output.rms, 'out', output.peak), 'meter-field')}
        ${clientField('Listen', esc(csv(item.listen) || '-'))}
        ${clientField('Regular TX', esc(csv(item.tx) || '-'))}
        ${clientField('Queue', esc(s.queue_depth ?? '-'))}
        ${clientField('Health', healthText(s), 'wide-field')}
      </dl>
    </article>`;
  }).join('');
  return cards ? `<div class="client-card-list">${cards}</div>` : '<p class="muted">No clients.</p>';
}
function healthText(s) {
  const parts = [];
  if (s.transport?.source_frames_dropped) parts.push(`${s.transport.source_frames_dropped} drops`);
  if (s.transport?.malformed_packets) parts.push(`${s.transport.malformed_packets} malformed`);
  if ((s.role || '') === 'bridge' && !s.bridge) parts.push('bridge status not reported');
  if (s.processing_status?.active) {
    const engine = s.processing_status.engine || s.processing?.engine || 'built_in';
    const engineWarn = s.processing_status.engine_available === false ? ' unavailable' : '';
    const stageCount = s.processing_status.stages?.length ? ` ${s.processing_status.stages.length} stages` : '';
    parts.push(`DSP ${engine}${engineWarn}${stageCount} ${s.processing_status.gate_open ? 'open' : 'gated'}`);
    if (s.processing_status.normalization?.active) {
      parts.push(`level ${Number(s.processing_status.normalization.applied_gain || 1).toFixed(2)}x`);
    }
  } else if (s.processing_status?.engine_available === false) {
    parts.push(`DSP ${(s.processing_status.engine || 'engine')} unavailable`);
  }
  if (s.output?.limiter_reduction_db > 0.1) parts.push(`limiter ${s.output.limiter_reduction_db.toFixed(1)} dB`);
  const telemetry = s.capture?.client_transport || {};
  const playback = s.capture?.playback || {};
  if (telemetry.malformed_packets || telemetry.decode_errors) parts.push(`rx drops ${telemetry.malformed_packets || 0}/${telemetry.decode_errors || 0}`);
  if (telemetry.packet_encode_errors || telemetry.tx_queue_drops || telemetry.tx_send_failures) parts.push(`tx drops ${telemetry.packet_encode_errors || 0}/${telemetry.tx_queue_drops || 0}/${telemetry.tx_send_failures || 0}`);
  if (playback.underflows || playback.overflows) parts.push(`playback U/O ${playback.underflows || 0}/${playback.overflows || 0}`);
  if (s.capture?.desktop?.post_gain_clipped_samples) parts.push(`desktop clip ${s.capture.desktop.post_gain_clipped_samples}`);
  if (s.capture?.desktop?.post_gain) parts.push(`mic ${dbfsText(s.capture.desktop.post_gain.rms)} rms`);
  if (s.capture?.audio?.post_gain_clipped_samples && !s.capture?.desktop) parts.push(`clip ${s.capture.audio.post_gain_clipped_samples}`);
  if (s.capture?.audio?.input && !s.capture?.desktop && !s.capture?.selected) parts.push(`mic ${dbfsText(s.capture.audio.input.rms)} rms`);
  if (!s.capture?.desktop && (s.capture?.raw_clipped_samples || s.capture?.software_clipped_samples)) parts.push(`capture clip ${s.capture.raw_clipped_samples || 0}/${s.capture.software_clipped_samples || 0}`);
  if (!s.capture?.desktop && s.capture?.selected?.dc_offset && Math.abs(s.capture.selected.dc_offset) > 0.08) parts.push(`capture DC ${pct(s.capture.selected.dc_offset)}`);
  if (s.capture?.selected && !s.capture?.desktop) parts.push(`mic ${dbfsText(s.capture.selected.rms)} rms`);
  return parts.length ? `<span class="health-warn">${esc(parts.join(', '))}</span>` : '-';
}
function bridgeText(item = {}) {
  if ((item.role || '') !== 'bridge') return '-';
  const bridge = item.bridge;
  if (!bridge) return '<span class="muted">not reported</span>';
  const parts = [bridge.mode || 'duplex'];
  if (bridge.input_device) parts.push(`in ${bridge.input_device}`);
  if (bridge.output_device) parts.push(`out ${bridge.output_device}`);
  if ((bridge.tx || []).length) parts.push(`TX ${csv(bridge.tx)}`);
  if ((bridge.listen || []).length) parts.push(`listen ${csv(bridge.listen)}`);
  const gains = [];
  if (Number.isFinite(Number(bridge.input_gain)) && Number(bridge.input_gain) !== 1) gains.push(`in ${Number(bridge.input_gain).toFixed(2)}x`);
  if (Number.isFinite(Number(bridge.output_gain)) && Number(bridge.output_gain) !== 1) gains.push(`out ${Number(bridge.output_gain).toFixed(2)}x`);
  if (gains.length) parts.push(`gain ${gains.join('/')}`);
  if (bridge.note) parts.push(bridge.note);
  return `<span class="bridge-detail" title="${esc(parts.join(' | '))}">${esc(parts.join(' | '))}</span>`;
}

function renderShell() {
  document.querySelectorAll('[data-nav]').forEach((link) => link.classList.toggle('active', link.dataset.nav === page));
  const online = (state.sessions || []).filter((s) => s.addr).length;
  $('summary').textContent = `${online}/${mergedUsers().length} online | ${(state.channels || []).length} channels | ${(state.active_alerts || []).length} alerts`;
}
async function refresh() {
  state = await api('/state');
  renderShell();
  renderPage();
}
function renderPage() {
  if (page === 'clients') return renderClientsPage();
  if (page === 'routing') return renderRoutingPage();
  if (page === 'presets') return renderPresetsPage();
  if (page === 'calls') return renderCallsPage();
  if (page === 'recording') return renderRecordingPage();
  if (page === 'system') return renderSystemPage();
  return renderDashboardPage();
}

function renderWarnings(limit = 8) {
  const warnings = state.warnings || [];
  if (!warnings.length) return '<div class="muted">No warnings.</div>';
  return warnings.slice(0, limit).map((warn) => `<div class="warn-box"><strong>${esc(clientLabel(warn.user_id))}</strong>: ${esc(warn.message)}</div>`).join('');
}
function renderAlertTable(alerts = state.active_alerts || [], actions = false) {
  const rows = alerts.map((alert) => `<tr>
    <td>${alert.id}</td><td>${esc(clientLabel(alert.sender))}</td><td>${esc(alertTargetLabel(alert.target))}</td>
    <td>${esc(alert.message || '')}</td>
    <td>${(alert.recipients || []).map((r) => `${esc(clientLabel(r.user_id))}${r.acked_at_ms ? ' ack' : ''}`).join(', ')}</td>
    ${actions ? `<td><button data-cancel-alert="${alert.id}" type="button">Cancel</button></td>` : ''}
  </tr>`).join('');
  return `<div class="table-wrap"><table><thead><tr><th>ID</th><th>Sender</th><th>Target</th><th>Message</th><th>Recipients</th>${actions ? '<th></th>' : ''}</tr></thead><tbody>${rows || `<tr><td colspan="${actions ? 6 : 5}" class="muted">No active alerts</td></tr>`}</tbody></table></div>`;
}

function renderDashboardPage() {
  const online = (state.sessions || []).filter((s) => s.addr).length;
  const talking = (state.sessions || []).filter((s) => s.input?.active).length;
  const rec = state.recording || {};
  root().innerHTML = `
    <div class="grid four">
      <section class="card metric"><span class="muted">Online clients</span><strong>${online}</strong><span>${mergedUsers().length} configured or live</span></section>
      <section class="card metric"><span class="muted">Active talkers</span><strong>${talking}</strong><span>based on input RMS</span></section>
      <section class="card metric"><span class="muted">Alerts</span><strong>${(state.active_alerts || []).length}</strong><span>${(state.recent_alerts || []).length} recent</span></section>
      <section class="card metric"><span class="muted">Recording</span><strong>${rec.active ? 'On' : 'Off'}</strong><span>${rec.active ? esc(rec.session_id || '') : 'inactive'}</span></section>
    </div>
    ${state.emergency?.active ? `<section class="card">${badge('Emergency active', 'danger')} Source ${esc(clientLabel(state.emergency.source))}; recipients ${csv(state.emergency.recipients)}</section>` : ''}
    <div class="grid two">
      <section class="card"><div class="card-head"><h2>Clients</h2><a href="/admin/clients/">Open Clients</a></div>${clientTable(['User','Role','Status','Codec','Talk Mode','Talk','Input','Output'], userRowsForDashboard())}</section>
      <section class="card"><div class="card-head"><h2>Warnings</h2><a href="/admin/system/">System</a></div>${renderWarnings()}</section>
      <section class="card"><div class="card-head"><h2>Active Alerts</h2><a href="/admin/calls/">Calls & Alerts</a></div>${renderAlertTable()}</section>
      <section class="card"><div class="card-head"><h2>Recording</h2><a href="/admin/recording/">Recording</a></div>${recordingSummary()}</section>
    </div>`;
}
function userRowsForDashboard() {
  return mergedUsers().map((id) => {
    const d = desired(id) || {};
    const s = session(id) || {};
    const item = { ...d, ...s };
    return `<tr><td>${esc(clientLabel(id))}</td><td>${esc(item.role || 'client')}</td><td>${s.addr ? badge('online') : badge('offline', 'offline')}</td><td>${esc(audioLabel(item))}</td><td>${esc(item.talk_mode || 'ptt')}</td><td>${s.input?.active ? badge('talking', 'talk') : '-'}</td><td>${meter(s.input?.rms, '', s.input?.peak)}</td><td>${meter(s.output?.rms, 'out', s.output?.peak)}</td></tr>`;
  }).join('');
}
function clientTable(headers, rows) {
  return `<div class="table-wrap"><table><thead><tr>${headers.map((h) => `<th>${h}</th>`).join('')}</tr></thead><tbody>${rows || `<tr><td colspan="${headers.length}" class="muted">No clients.</td></tr>`}</tbody></table></div>`;
}
function recordingSummary() {
  const rec = state.recording || {};
  const live = state.transcription || {};
  const accel = live.acceleration?.active_backend || rec.engine?.acceleration?.active_backend || 'cpu';
  return `<div class="status-line">${rec.active ? badge('active') : badge('inactive', 'offline')}<span>Users ${csv(rec.recorded_users || []) || '-'}</span><span>Frames ${rec.frames_recorded || 0}</span><span>Transcripts ${rec.transcript_segments || 0}</span></div>
    <div class="status-line">${live.active ? badge('live transcription') : badge('live off', 'offline')}<span>Engine ${esc(live.engine || rec.engine?.mode || 'disabled')}</span><span>Mode ${esc(live.mode || 'balanced')}</span><span>Quality ${esc(live.model_quality || rec.engine?.model_quality || 'unknown')}</span><span>Accel ${esc(accel)}</span><span>Queued ${live.queued_jobs || 0}</span><span>Dropped ${live.dropped_jobs || 0}</span></div>
    <p class="muted">Whisper ${rec.engine?.available || live.available ? 'configured' : 'not configured'}${(live.model_recommendation || rec.engine?.model_recommendation) ? `: ${esc(live.model_recommendation || rec.engine.model_recommendation)}` : ''}${(live.last_error || rec.engine?.last_error) ? `: ${esc(live.last_error || rec.engine.last_error)}` : ''}</p>`;
}

function renderClientsPage() {
  root().innerHTML = `
    <div class="clients-page">
      <section class="card clients-enrollment">
        <div class="card-head"><h2>Device Enrollment</h2><span class="muted">Policy: ${esc(state.enrollment_policy || 'auto')}</span></div>
        <div class="table-wrap device-table"><table><thead><tr><th>UID</th><th>User</th><th>Status</th><th>Role</th><th>Last Seen</th><th>Warnings</th><th></th></tr></thead><tbody>${deviceRows() || '<tr><td colspan="7" class="muted">No enrolled or pending devices.</td></tr>'}</tbody></table></div>
      </section>
      <div class="clients-layout">
        <section class="card client-selection">
          <div class="card-head"><h2>Clients</h2><button id="add-client" class="primary" type="button">Add Client</button></div>
          ${clientCards()}
        </section>
        <section id="client-detail" class="card client-detail">
          ${selectedUser ? clientDetailHtml(selectedUser) : '<div class="empty-panel"><h2>Select a client</h2><p class="muted">Choose a client or add a new one to edit setup, routing, and the server audio pipeline.</p></div>'}
        </section>
      </div>
    </div>`;
  $('add-client').onclick = () => openClientEditor(nextUserId());
  root().querySelectorAll('[data-open-client]').forEach((row) => row.onclick = () => openClientEditor(Number(row.dataset.openClient)));
  root().querySelectorAll('[data-open-client]').forEach((row) => row.onkeydown = (event) => {
    if (event.key === 'Enter' || event.key === ' ') {
      event.preventDefault();
      openClientEditor(Number(row.dataset.openClient));
    }
  });
  root().querySelectorAll('[data-approve-device]').forEach((button) => button.onclick = () => updateDevice(button.dataset.approveDevice, 'approve'));
  root().querySelectorAll('[data-reject-device]').forEach((button) => button.onclick = () => updateDevice(button.dataset.rejectDevice, 'reject'));
  root().querySelectorAll('[data-delete-device]').forEach((button) => button.onclick = () => deleteDevice(button.dataset.deleteDevice));
  if (selectedUser) bindClientEditor(buildClientEditorConfig(selectedUser));
}

function deviceRows() {
  return (state.devices || []).map((device) => {
    const uid = device.client_uid || '';
    const statusActions = device.status === 'pending'
      ? `<button type="button" data-approve-device="${esc(uid)}">Approve</button><button type="button" data-reject-device="${esc(uid)}">Reject</button>`
      : device.status === 'rejected'
        ? `<button type="button" data-approve-device="${esc(uid)}">Approve</button>`
        : '';
    const actions = `${statusActions}<button type="button" class="danger" data-delete-device="${esc(uid)}">Delete</button>`;
    return `<tr><td><code>${esc(uid)}</code></td><td>${esc(clientLabel(device.user_id))}</td><td>${badge(device.status || 'enrolled', device.status === 'rejected' ? 'danger' : device.status === 'pending' ? 'warn' : '')}</td><td>${esc(device.role || 'client')}</td><td>${device.last_seen_ms || '-'}</td><td>${(device.warnings || []).map(esc).join(', ') || '-'}</td><td class="actions">${actions}</td></tr>`;
  }).join('');
}

async function updateDevice(uid, action) {
  try {
    await api(`/devices/${encodeURIComponent(uid)}/${action}`, { method: 'POST', body: JSON.stringify({}) });
    await refresh();
  } catch (err) {
    showError(err);
  }
}
async function deleteDevice(uid) {
  if (!window.confirm(`Delete device enrollment ${uid}? The client configuration stays, but this device will need to enroll again.`)) return;
  try {
    await api(`/devices/${encodeURIComponent(uid)}`, { method: 'DELETE' });
    await refresh();
  } catch (err) {
    showError(err);
  }
}

function renderRoutingPage() {
  root().innerHTML = `
    <div class="grid">
      <section class="card">${channelManagerHtml()}</section>
      <section class="card"><div class="card-head"><h2>Mix Matrix</h2><span class="muted">listen, regular TX, priority, gain</span></div>${mixMatrixHtml()}</section>
      <section class="card"><div class="card-head"><h2>Per-Talker Gains</h2><span class="muted">listener rows, talker columns</span></div>${talkerMatrixHtml()}</section>
    </div>`;
  bindChannelManager();
  bindMixMatrix();
  bindTalkerMatrix();
}
function channelManagerHtml() {
  const rows = (state.channels || []).map((ch) => `<tr><td>${ch.id}</td><td>${esc(ch.name)}</td><td><button data-edit-channel="${ch.id}" type="button">Edit</button> <button data-delete-channel="${ch.id}" type="button" class="danger">Delete Name</button></td></tr>`).join('');
  return `<div class="card-head"><h2>Channels</h2></div>
    <form id="channel-form" class="form-grid">
      <label>Channel ID<input id="channel-id" type="number" min="0" max="65535" required></label>
      <label>Name<input id="channel-name" type="text" required></label>
      <button class="primary" type="submit">Save Channel</button>
    </form>
    <div class="table-wrap"><table><thead><tr><th>ID</th><th>Name</th><th></th></tr></thead><tbody>${rows || '<tr><td colspan="3" class="muted">No named channels yet.</td></tr>'}</tbody></table></div>`;
}
function bindChannelManager() {
  $('channel-form').onsubmit = async (e) => {
    e.preventDefault();
    await api(`/channels/${Number($('channel-id').value)}`, { method: 'PUT', body: JSON.stringify({ name: $('channel-name').value.trim() }) });
    await refresh();
  };
  root().querySelectorAll('[data-edit-channel]').forEach((button) => button.onclick = () => {
    const ch = (state.channels || []).find((item) => item.id === Number(button.dataset.editChannel));
    if (ch) { $('channel-id').value = ch.id; $('channel-name').value = ch.name; }
  });
  root().querySelectorAll('[data-delete-channel]').forEach((button) => button.onclick = async () => {
    await api(`/channels/${Number(button.dataset.deleteChannel)}`, { method: 'DELETE' });
    await refresh();
  });
}
function mixMatrixHtml() {
  const channels = allChannelIds();
  const users = mergedUsers();
  if (!users.length) return '<p class="muted">Add clients to edit routing.</p>';
  let html = `<div class="table-wrap matrix"><table><thead><tr><th>Client</th>${channels.map((ch) => `<th>${esc(channelLabel(ch))}</th>`).join('')}</tr></thead><tbody>`;
  for (const id of users) {
    const item = shownClient(id);
    html += `<tr><td><strong>${esc(clientLabel(id))}</strong></td>${channels.map((ch) => `<td><div class="cell-stack">
      <label class="check"><input type="checkbox" data-listen-user="${id}" data-ch="${ch}" ${(item.listen || []).includes(ch) ? 'checked' : ''}> listen</label>
      <label class="check"><input type="checkbox" data-tx-user="${id}" data-ch="${ch}" ${(item.tx || []).includes(ch) ? 'checked' : ''}> regular TX</label>
      <label class="check"><input type="checkbox" data-priority-user="${id}" data-ch="${ch}" ${(item.priority_channels || []).includes(ch) ? 'checked' : ''}> priority</label>
      <label>gain<input type="number" min="0" max="4" step="0.05" data-gain-user="${id}" data-ch="${ch}" value="${item.vol?.[ch] ?? 1}"></label>
    </div></td>`).join('')}</tr>`;
  }
  return html + '</tbody></table></div>';
}
function bindMixMatrix() {
  root().querySelectorAll('[data-listen-user],[data-tx-user],[data-priority-user],[data-gain-user]').forEach((input) => input.onchange = saveMixCell);
}
async function saveMixCell(e) {
  const id = Number(e.target.dataset.listenUser || e.target.dataset.txUser || e.target.dataset.priorityUser || e.target.dataset.gainUser);
  const listen = new Set(shownClient(id).listen || []);
  const tx = new Set(shownClient(id).tx || []);
  const priority = new Set(shownClient(id).priority_channels || []);
  const vol = { ...(shownClient(id).vol || {}) };
  root().querySelectorAll(`[data-listen-user="${id}"]`).forEach((el) => el.checked ? listen.add(Number(el.dataset.ch)) : listen.delete(Number(el.dataset.ch)));
  root().querySelectorAll(`[data-tx-user="${id}"]`).forEach((el) => el.checked ? tx.add(Number(el.dataset.ch)) : tx.delete(Number(el.dataset.ch)));
  root().querySelectorAll(`[data-priority-user="${id}"]`).forEach((el) => el.checked ? priority.add(Number(el.dataset.ch)) : priority.delete(Number(el.dataset.ch)));
  root().querySelectorAll(`[data-gain-user="${id}"]`).forEach((el) => { vol[Number(el.dataset.ch)] = Number(el.value); });
  await api(`/clients/${id}`, { method: 'PATCH', body: JSON.stringify({ listen: sortedChannels([...listen]), tx: sortedChannels([...tx]), priority_channels: sortedChannels([...priority]), vol }) });
  await refresh();
}
function talkerMatrixHtml() {
  const users = mergedUsers();
  if (users.length < 2) return '<p class="muted">Add at least two clients to edit per-talker gains.</p>';
  let html = `<div class="table-wrap matrix"><table><thead><tr><th>Listener</th>${users.map((id) => `<th>${esc(clientLabel(id))}</th>`).join('')}</tr></thead><tbody>`;
  for (const listener of users) {
    const item = shownClient(listener);
    html += `<tr><td><strong>${esc(clientLabel(listener))}</strong></td>${users.map((talker) => listener === talker ? '<td class="muted">mix-minus</td>' : `<td><input type="number" min="0" max="4" step="0.05" data-listener="${listener}" data-talker="${talker}" value="${item.talker_vol?.[talker] ?? 1}"></td>`).join('')}</tr>`;
  }
  return html + '</tbody></table></div>';
}
function bindTalkerMatrix() {
  root().querySelectorAll('[data-listener]').forEach((input) => input.onchange = async (e) => {
    const listener = Number(e.target.dataset.listener);
    const talkerVol = { ...(shownClient(listener).talker_vol || {}) };
    root().querySelectorAll(`[data-listener="${listener}"]`).forEach((el) => {
      const gain = Number(el.value);
      if (gain === 1) delete talkerVol[el.dataset.talker]; else talkerVol[el.dataset.talker] = gain;
    });
    await api(`/clients/${listener}`, { method: 'PATCH', body: JSON.stringify({ talker_vol: talkerVol }) });
    await refresh();
  });
}

function renderCallsPage() {
  if ($('calls-page')) {
    updateCallsPage();
    return;
  }
  root().innerHTML = `
    <div id="calls-page" class="grid two">
      <section class="card"><div class="card-head"><h2>Direct Call</h2></div>
        <form id="direct-call-form" class="form-grid">
          <label>Caller<input id="direct-caller" type="number" min="1" placeholder="1"></label>
          <label>Target<input id="direct-target" type="number" min="1" placeholder="2"></label>
          <label class="check"><input id="direct-duck" type="checkbox"> Duck target audio</label>
          <div class="actions wide"><button id="direct-call-start" type="button" class="primary">Start Direct Call</button><button id="direct-call-stop" type="button">Stop Direct Call</button></div>
        </form>
      </section>
      <section class="card"><div class="card-head"><h2>Emergency Override</h2></div>
        <form id="emergency-form" class="form-grid">
          <label>Source<input id="emergency-source" type="number" min="1" placeholder="1"></label>
          <label>Target<select id="emergency-target-type"><option value="all">All</option><option value="users">Users</option><option value="channels">Channels</option></select></label>
          <label>Target IDs<input id="emergency-target-ids" type="text" placeholder="2,3"></label>
          <label>Duck Gain<input id="emergency-duck-gain" type="number" min="0" max="1" step="0.01" value="0.125"></label>
          <label class="check"><input id="emergency-mute" type="checkbox"> Mute normal audio</label>
          <div class="actions wide"><button id="emergency-start" type="button" class="primary">Start Emergency</button><button id="emergency-stop" type="button">Stop Emergency</button></div>
        </form>
      </section>
      <section class="card wide"><div class="card-head"><h2>Alert / Announcement</h2></div>
        <form id="announcement-form" class="form-grid">
          <label>Sender<input id="announcement-sender" type="number" min="0" placeholder="0"></label>
          <label>Target Type<select id="announcement-target-type"><option value="user">User</option><option value="channel">Channel</option></select></label>
          <label>Target IDs<input id="announcement-target-ids" type="text" placeholder="2,3"></label>
          <label class="wide">Message<input id="announcement-message" type="text" maxlength="240" placeholder="Stand by for program audio"></label>
          <label class="check"><input id="announcement-text-alert" type="checkbox" checked> Send text alert</label>
          <label class="check"><input id="announcement-tts" type="checkbox"> Play spoken announcement</label>
          <label class="check"><input id="announcement-priority" type="checkbox" checked> Duck same-channel traffic</label>
          <label class="check"><input id="announcement-duck" type="checkbox"> Duck direct recipient audio</label>
          <label>Speech Level<input id="announcement-gain" type="number" min="0.02" max="1" step="0.01" value="0.18"></label>
          <button id="send-announcement" type="button" class="primary">Send</button>
        </form>
      </section>
      <section class="card"><div class="card-head"><h2>Active Calls</h2></div><div id="active-calls-table">${activeCallsHtml()}</div></section>
      <section class="card wide"><div class="card-head"><h2>Active Alerts</h2></div><div id="active-alerts-table">${renderAlertTable(state.active_alerts || [], true)}</div></section>
    </div>`;
  bindCallControls();
}
function updateCallsPage() {
  if ($('active-calls-table')) $('active-calls-table').innerHTML = activeCallsHtml();
  if ($('active-alerts-table')) $('active-alerts-table').innerHTML = renderAlertTable(state.active_alerts || [], true);
  bindAlertCancelControls();
}
function activeCallsHtml() {
  const rows = (state.sessions || []).flatMap((s) => (s.active_direct_calls || []).filter((c) => c.active).map((c) => `<tr><td>${esc(clientLabel(c.caller))}</td><td>${esc(clientLabel(c.target))}</td><td>${c.duck ? 'yes' : 'no'}</td></tr>`)).join('');
  return `<div class="table-wrap"><table><thead><tr><th>Caller</th><th>Target</th><th>Ducking</th></tr></thead><tbody>${rows || '<tr><td colspan="3" class="muted">No active direct calls.</td></tr>'}</tbody></table></div>`;
}
function emergencyTarget() {
  const kind = $('emergency-target-type').value;
  if (kind === 'all') return { kind: 'all' };
  const ids = parseCsv($('emergency-target-ids').value, { allowZero: kind === 'channels' });
  return kind === 'users' ? { kind: 'users', users: ids } : { kind: 'channels', channels: ids };
}
function bindCallControls() {
  async function direct(active) {
    const caller = Number($('direct-caller').value);
    const target = Number($('direct-target').value);
    if (!caller || !target) return window.alert('Set caller and target user IDs.');
    await api('/direct-call', { method: 'POST', body: JSON.stringify({ caller, target, active, duck: $('direct-duck').checked }) });
    await refresh();
  }
  async function emergency(active) {
    const source = Number($('emergency-source').value);
    if (!source) return window.alert('Set an emergency source user ID.');
    await api('/emergency', { method: 'POST', body: JSON.stringify({ source, active, target: emergencyTarget(), duck_gain: Number($('emergency-duck-gain').value), mute_others: $('emergency-mute').checked }) });
    await refresh();
  }
  $('direct-call-start').onclick = () => direct(true).catch(showError);
  $('direct-call-stop').onclick = () => direct(false).catch(showError);
  $('emergency-start').onclick = () => emergency(true).catch(showError);
  $('emergency-stop').onclick = () => emergency(false).catch(showError);
  $('send-announcement').onclick = async () => {
    const kind = $('announcement-target-type').value;
    const ids = parseCsv($('announcement-target-ids').value, { allowZero: kind === 'channel' });
    const message = $('announcement-message').value.trim();
    const textAlert = $('announcement-text-alert').checked;
    const tts = $('announcement-tts').checked;
    if (!ids.length) return window.alert('Set one or more target IDs.');
    if (!message) return window.alert('Set a message.');
    if (!textAlert && !tts) return window.alert('Enable text alert, spoken announcement, or both.');
    const targets = ids.map((id) => ({ kind, id }));
    await api('/announcements', { method: 'POST', body: JSON.stringify({ sender: Number($('announcement-sender').value) || 0, targets, message, text_alert: textAlert, tts, priority: $('announcement-priority').checked, duck: $('announcement-duck').checked, gain: Number($('announcement-gain').value) || 0.18 }) });
    $('announcement-message').value = '';
    await refresh();
  };
  bindAlertCancelControls();
}
function bindAlertCancelControls() {
  root().querySelectorAll('[data-cancel-alert]').forEach((button) => button.onclick = async () => {
    await api(`/alerts/${button.dataset.cancelAlert}/cancel`, { method: 'POST', body: JSON.stringify({ user_id: 0 }) });
    await refresh();
  });
}

function renderRecordingPage() {
  if ($('recording-page')) {
    updateRecordingPage();
    return;
  }
  root().innerHTML = `
    <div id="recording-page">
    <div class="grid two">
      <section class="card"><div class="card-head"><h2>Recording Session</h2></div><div id="recording-summary"></div>
        <div class="actions"><label class="check"><input id="record-transcribe" type="checkbox"> Transcribe with local Whisper on stop</label><button id="recording-start" type="button" class="primary">Start Recording</button><button id="recording-stop" type="button">Stop Recording</button></div>
        <div id="recording-sessions"></div>
      </section>
      <section class="card"><div class="card-head"><h2>Live Transcription</h2></div>
        <div id="live-transcription-status"></div>
        <div id="live-transcription-health"></div>
        <div id="live-transcription-error"></div>
        <form id="transcription-model-form" class="form-grid">
          <label>Whisper Model<select id="transcription-model"></select></label>
          <button id="select-transcription-model" type="button">Use Model</button>
        </form>
        <form id="live-transcription-form" class="form-grid">
          <label>Users<input id="live-transcription-users" type="text" placeholder="optional: 1,2"></label>
          <label>Mode<select id="live-transcription-mode"><option value="fast">Fast</option><option value="balanced">Balanced</option><option value="reliable">Reliable</option></select></label>
          <label>Max Jobs<input id="live-transcription-concurrency" type="number" min="1" max="16" step="1" value="2"></label>
          <label>Stale Job ms<input id="live-transcription-stale-ms" type="number" min="5000" max="300000" step="1000" value="20000"></label>
          <div class="actions"><button id="live-transcription-start" type="button" class="primary">Start Live Transcription</button><button id="live-transcription-stop" type="button">Stop Live Transcription</button></div>
        </form>
        <div id="live-transcription-users-table"></div>
      </section>
      <section class="card"><div class="card-head"><h2>Transcript Filters</h2></div>
        <form id="transcript-filter" class="form-grid">
          <label>User IDs<input id="transcript-users" type="text" placeholder="optional: 1,2"></label>
          <label>Channel IDs<input id="transcript-channels" type="text" placeholder="optional: 1,2"></label>
          <label>Direct User<input id="transcript-direct-user" type="number" min="1" placeholder="optional"></label>
          <label>Source<select id="transcript-source"><option value="">Any</option><option value="live">Live</option><option value="recording">Recording</option><option value="manual">Manual</option></select></label>
          <label>Search<input id="transcript-search" type="text" placeholder="optional text"></label>
          <button type="submit">Refresh Transcripts</button>
        </form>
      </section>
      <section class="card wide"><div class="card-head"><h2>Transcripts</h2></div><div id="transcript-results" class="grid"></div></section>
    </div>
    </div>`;
  $('recording-start').onclick = async () => { await api('/recording/start', { method: 'POST', body: JSON.stringify({ transcribe: $('record-transcribe').checked }) }); await refresh(); };
  $('recording-stop').onclick = async () => { await api('/recording/stop', { method: 'POST' }); await refresh(); };
  $('transcription-model').onchange = () => { recordingModelTouched = true; };
  $('select-transcription-model').onclick = async () => {
    const model = $('transcription-model').value;
    if (!model) return window.alert('Place .bin or .gguf Whisper models in the configured model folder first.');
    await api('/transcription/model', { method: 'PUT', body: JSON.stringify({ model }) });
    recordingModelTouched = false;
    await refresh();
  };
  $('live-transcription-start').onclick = async () => {
    const users = parseCsv($('live-transcription-users').value);
    const concurrency = Number($('live-transcription-concurrency').value);
    const staleJobMs = Number($('live-transcription-stale-ms').value);
    const body = {
      users: users.length ? users : null,
      mode: $('live-transcription-mode').value || 'balanced',
    };
    if (Number.isFinite(concurrency) && concurrency > 0) body.max_concurrent_jobs = concurrency;
    if (Number.isFinite(staleJobMs) && staleJobMs > 0) body.stale_job_ms = staleJobMs;
    await api('/transcription/live/start', { method: 'POST', body: JSON.stringify(body) });
    await refresh();
  };
  $('live-transcription-stop').onclick = async () => { await api('/transcription/live/stop', { method: 'POST' }); await refresh(); };
  $('transcript-filter').onsubmit = (e) => { e.preventDefault(); loadTranscripts().catch(showError); };
  updateRecordingPage();
}
function updateRecordingPage() {
  const live = state.transcription || {};
  const rec = state.recording || {};
  $('recording-summary').innerHTML = recordingSummary();
  $('recording-sessions').innerHTML = recordingSessionsTable(rec.recent_sessions || []);
  syncLiveTranscriptionControls(live);
  $('live-transcription-status').innerHTML = `<div class="status-line">${live.active ? badge('active') : badge('inactive', 'offline')}<span>Engine ${esc(live.engine || 'disabled')}</span><span>Mode ${esc(live.mode || 'balanced')}</span><span>Language ${esc(live.language || 'en')}</span><span>Quality ${esc(live.model_quality || 'unknown')}</span><span>Jobs ${live.active_jobs || 0}/${live.max_concurrent_jobs || 0}</span><span>Accel ${esc(live.acceleration?.active_backend || 'cpu')}</span><span>Model ${live.model ? esc(live.model) : '-'}</span><span>Folder ${esc(live.model_dir || '-')}</span></div>${live.model_recommendation ? `<div class="warn-box">${esc(live.model_recommendation)}</div>` : ''}`;
  $('live-transcription-health').innerHTML = `<div class="status-line"><span>Queued ${live.queued_jobs || 0}</span><span>Dropped jobs ${live.dropped_jobs || 0}</span><span>Dropped frames ${live.dropped_frames || 0}</span><span>Stale jobs ${live.stale_jobs || 0}</span><span>Stale frames ${live.stale_frames || 0}</span><span>Stale ms ${live.stale_job_ms || 0}</span><span>Segments ${live.completed_segments || 0}</span></div>`;
  $('live-transcription-error').innerHTML = live.last_error ? `<div class="error-box">${esc(live.last_error)}</div>` : '';
  updateModelSelect(live.models || []);
  $('live-transcription-users-table').innerHTML = liveUserTable(live.users || []);
  loadTranscripts().catch(console.warn);
}
function syncLiveTranscriptionControls(live) {
  const mode = $('live-transcription-mode');
  const concurrency = $('live-transcription-concurrency');
  const stale = $('live-transcription-stale-ms');
  if (mode && document.activeElement !== mode) mode.value = live.mode || 'balanced';
  if (concurrency && document.activeElement !== concurrency) concurrency.value = live.max_concurrent_jobs || 2;
  if (stale && document.activeElement !== stale) stale.value = live.stale_job_ms || 20000;
}
function recordingSessionsTable(sessions) {
  const rows = (sessions || []).slice(-8).reverse().map((session) => `<tr><td>${esc(session.id)}</td><td>${esc(csv(session.recorded_users || [])) || '-'}</td><td>${session.frames_recorded || 0}</td><td>${session.transcribe ? 'yes' : 'no'}</td><td>${esc(session.dir || '-')}</td></tr>`).join('');
  return `<div class="table-wrap compact"><table><thead><tr><th>Recent Session</th><th>Users</th><th>Frames</th><th>Transcribe</th><th>Folder</th></tr></thead><tbody>${rows || '<tr><td colspan="5" class="muted">No completed recording sessions yet.</td></tr>'}</tbody></table></div>`;
}
function updateModelSelect(models) {
  const select = $('transcription-model');
  if (!select) return;
  const selectedModel = (models || []).find((model) => model.selected)?.name || '';
  const preserveOperatorChoice = recordingModelTouched || document.activeElement === select;
  const value = preserveOperatorChoice ? select.value : selectedModel;
  const nextHtml = modelOptions(models || []);
  if (select.innerHTML !== nextHtml) select.innerHTML = nextHtml;
  if ([...select.options].some((option) => option.value === value)) select.value = value;
}
function modelOptions(models) {
  if (!models.length) return '<option value="">No models found</option>';
  return models.map((model) => `<option value="${esc(model.name)}" ${model.selected ? 'selected' : ''}>${esc(model.name)}${model.selected ? ' (selected)' : ''}</option>`).join('');
}
function liveUserTable(users) {
  const rows = users.map((user) => `<tr><td>${esc(clientLabel(user.user_id))}</td><td>${user.worker_running ? badge('running') : '-'}</td><td>${user.active_chunk ? badge('speech', 'talk') : '-'}</td><td>${user.queued_jobs}</td><td>${user.dropped_jobs}</td><td>${user.dropped_frames}</td><td>${user.stale_jobs || 0}</td><td>${user.stale_frames || 0}</td><td>${user.completed_segments}</td></tr>`).join('');
  return `<div class="table-wrap"><table><thead><tr><th>User</th><th>Worker</th><th>Chunk</th><th>Queued</th><th>Dropped Jobs</th><th>Dropped Frames</th><th>Stale Jobs</th><th>Stale Frames</th><th>Segments</th></tr></thead><tbody>${rows || '<tr><td colspan="9" class="muted">No live transcription users yet.</td></tr>'}</tbody></table></div>`;
}
async function loadTranscripts() {
  const params = new URLSearchParams();
  if ($('transcript-users')?.value) params.set('user_ids', $('transcript-users').value);
  if ($('transcript-channels')?.value) params.set('channel_ids', $('transcript-channels').value);
  if ($('transcript-direct-user')?.value) params.set('direct_user_id', $('transcript-direct-user').value);
  if ($('transcript-source')?.value) params.set('source', $('transcript-source').value);
  if ($('transcript-search')?.value) params.set('q', $('transcript-search').value);
  const items = await api('/transcripts' + (params.toString() ? `?${params}` : ''));
  const box = $('transcript-results');
  if (!box) return;
  box.innerHTML = (items || []).slice(-80).reverse().map((t) => `<div class="pill transcript-pill"><strong>${esc(clientLabel(t.user_id))}</strong><span>${esc(t.text)}</span><small>${esc(t.source || 'recording')} ${transcriptContextLabel(t.contexts || [])}</small></div>`).join('') || '<p class="muted">No transcript segments.</p>';
}
function transcriptContextLabel(contexts) {
  if (!contexts.length) return '';
  return contexts.map((context) => {
    if (context.kind === 'channel') return `ch ${context.id}`;
    if (context.kind === 'direct') return `direct ${context.id}`;
    return esc(context.kind || 'mixed');
  }).join(', ');
}

function renderSystemPage() {
  root().innerHTML = `
    <div class="grid two">
      <section class="card"><div class="card-head"><h2>Security</h2></div><div class="warn-box"><strong>No admin authentication is enforced unless configured.</strong> Anyone who can reach the admin bind address can control every client.</div></section>
      <section class="card"><div class="card-head"><h2>Transcription Engine</h2></div>${recordingSummary()}</section>
      <section class="card wide"><div class="card-head"><h2>Model Assets</h2><button id="refresh-model-assets" type="button">Refresh</button></div>${modelAssetsHtml()}</section>
      <section class="card"><div class="card-head"><h2>Warnings</h2></div>${renderWarnings(100)}</section>
      <section class="card"><div class="card-head"><h2>Metrics</h2></div><pre>${esc(JSON.stringify(state.metrics || {}, null, 2))}</pre></section>
      <section class="card wide"><div class="card-head"><h2>Session Health</h2></div>${clientTable(['User','Role','Bridge','Address','Queue','Age ms','Input','Output','Health'], sessionHealthRows())}</section>
    </div>`;
  bindModelAssetControls();
}
function modelAssetsHtml() {
  const catalog = state.models || {};
  if (catalog.error) return `<div class="error-box">${esc(catalog.error)}</div>`;
  const models = catalog.models || [];
  if (!models.length) return '<p class="muted">No model catalog entries are configured.</p>';
  return `
    <div class="model-assets">
      ${modelAssetGroup('Transcription', models.filter((model) => model.category === 'transcription'))}
      ${modelAssetGroup('Audio Cleanup', models.filter((model) => model.category === 'deepfilternet_onnx' || model.category === 'deepfilternet_coreml'))}
    </div>`;
}
function modelAssetGroup(title, models) {
  if (!models.length) return '';
  return `<div class="model-group"><h3>${esc(title)}</h3><div class="model-list">${models.map(modelAssetRow).join('')}</div></div>`;
}
function modelAssetRow(model) {
  const downloadable = model.available && !model.installed && !model.downloading && model.url && model.sha256;
  const statusClass = model.status === 'installed' ? 'ok' : model.status === 'failed' ? 'danger' : model.status === 'downloading' ? 'talk' : 'offline';
  const flags = [model.default ? 'default' : '', model.recommended ? 'recommended' : '', model.runtime].filter(Boolean).join(' | ');
  return `
    <div class="model-row">
      <div class="model-main">
        <strong>${esc(model.name)}</strong>
        <span class="muted">${esc(flags)}${model.size ? ` | ${esc(model.size)}` : ''}</span>
        <small title="${esc(model.destination_path || '')}">${esc(model.destination_path || model.filename)}</small>
        ${model.notes ? `<small class="muted">${esc(model.notes)}</small>` : ''}
        ${model.error ? `<small class="health-warn">${esc(model.error)}</small>` : ''}
      </div>
      <div class="model-actions">
        ${badge(model.status || 'missing', statusClass)}
        ${downloadable ? `<button type="button" data-download-model="${esc(model.id)}">Download</button>` : ''}
      </div>
    </div>`;
}
function bindModelAssetControls() {
  const refreshButton = $('refresh-model-assets');
  if (refreshButton) refreshButton.onclick = () => refresh().catch(showError);
  root().querySelectorAll('[data-download-model]').forEach((button) => {
    button.onclick = async () => {
      button.disabled = true;
      try {
        await api('/models/download', { method: 'POST', body: JSON.stringify({ id: button.dataset.downloadModel }) });
        await refresh();
      } catch (err) {
        showError(err);
        await refresh();
      }
    };
  });
}
function sessionHealthRows() {
  return (state.sessions || []).map((s) => `<tr><td>${esc(clientLabel(s.user_id))}</td><td>${esc(s.role || 'client')}</td><td>${bridgeText(s)}</td><td>${esc(s.addr || '-')}</td><td>${s.queue_depth}</td><td>${s.age_ms}</td><td>${meter(s.input?.rms, '', s.input?.peak)}</td><td>${meter(s.output?.rms, 'out', s.output?.peak)}</td><td>${healthText(s)}</td></tr>`).join('');
}

function renderPresetsPage() {
  root().innerHTML = `
    <div class="grid two">
      <section class="card wide"><div class="card-head"><h2>Workflow Defaults</h2><span class="muted">New state includes Program, Production PL, Referee PL, Director IFB, Producer Cue, PA, and Utility channels plus IFB role templates.</span></div><p class="muted">Use <code>small-show-ifb</code> as a starting preset, then edit and save your own show-specific version. Emergency uses the server override path, not a default channel.</p></section>
      <section class="card"><div class="card-head"><h2>Presets</h2><button id="new-preset" type="button">New Preset</button></div><div class="pill-list">${presetPills()}</div></section>
      <section class="card"><div class="card-head"><h2>Client Templates</h2><button id="new-template" type="button">New Template</button></div><div class="pill-list">${templatePills()}</div></section>
      <section class="card wide"><div class="card-head"><h2>Preset Editor</h2><span class="muted">Capture current desired configs for normal use; JSON remains available for advanced edits.</span></div>${presetEditorHtml()}</section>
      <section class="card wide"><div class="card-head"><h2>Template Editor</h2><span class="muted">Build templates from existing desired clients, then apply to any user ID.</span></div>${templateEditorHtml()}</section>
    </div>`;
  bindPresetControls();
}
function presetPills() {
  return (state.presets || []).map((preset) => `<button class="pill" data-load-preset="${esc(preset.id)}" type="button">${esc(preset.id)}: ${esc(preset.name)} (${(preset.clients || []).length})</button>`).join('') || '<span class="muted">No presets.</span>';
}
function templatePills() {
  return (state.templates || []).map((template) => `<button class="pill" data-load-template="${esc(template.id)}" type="button">${esc(template.id)}: ${esc(template.name)}</button>`).join('') || '<span class="muted">No templates.</span>';
}
function presetEditorHtml() {
  return `<form id="preset-form" class="form-grid">
    <label>Preset ID<input id="preset-id" type="text" required placeholder="refs-game"></label>
    <label>Name<input id="preset-name" type="text" required placeholder="Refs Game"></label>
    <label class="wide">Advanced Client Configs JSON<textarea id="preset-clients" class="json-text" placeholder="[]"></textarea></label>
    <div class="actions wide"><button type="submit" class="primary">Save Preset</button><button id="capture-preset" type="button">Capture Current Desired</button><button id="apply-preset" type="button">Apply Preset</button><button id="delete-preset" class="danger" type="button">Delete Preset</button></div>
  </form>`;
}
function templateEditorHtml() {
  return `<form id="template-form" class="form-grid">
    <label>Template ID<input id="template-id" type="text" required placeholder="referee"></label>
    <label>Name<input id="template-name" type="text" required placeholder="Referee"></label>
    <label>Source Desired Client<select id="template-source"><option value="">Choose client</option>${(state.clients || []).map((client) => `<option value="${client.user_id}">${esc(clientLabel(client.user_id))}</option>`).join('')}</select></label>
    <label>Apply To User ID<input id="template-apply-user" type="number" min="1" placeholder="1"></label>
    <label class="wide">Template Preview<textarea id="template-client-json" class="json-text" placeholder="{}"></textarea></label>
    <div class="actions wide"><button type="submit" class="primary">Save Template</button><button id="capture-template" type="button">Capture Source Client</button><button id="apply-template" type="button">Apply Template</button><button id="delete-template" class="danger" type="button">Delete Template</button></div>
  </form>`;
}
function bindPresetControls() {
  root().querySelectorAll('[data-load-preset]').forEach((button) => button.onclick = () => {
    const preset = (state.presets || []).find((item) => item.id === button.dataset.loadPreset);
    if (!preset) return;
    $('preset-id').value = preset.id; $('preset-name').value = preset.name; $('preset-clients').value = JSON.stringify(preset.clients || [], null, 2);
  });
  root().querySelectorAll('[data-load-template]').forEach((button) => {
    button.onclick = () => {
      const template = (state.templates || []).find((item) => item.id === button.dataset.loadTemplate);
      if (!template) return;
      $('template-id').value = template.id; $('template-name').value = template.name; $('template-client-json').value = JSON.stringify(template.client || {}, null, 2);
    };
  });
  $('new-preset').onclick = () => { $('preset-id').value = ''; $('preset-name').value = ''; $('preset-clients').value = '[]'; };
  $('new-template').onclick = () => { $('template-id').value = ''; $('template-name').value = ''; $('template-client-json').value = JSON.stringify(defaultClient(0), null, 2); };
  $('capture-preset').onclick = () => { $('preset-clients').value = JSON.stringify(state.clients || [], null, 2); };
  $('capture-template').onclick = () => {
    const id = Number($('template-source').value);
    const client = desired(id);
    if (!client) return window.alert('Choose an existing desired client first.');
    const { user_id, ...templateClient } = client;
    $('template-client-json').value = JSON.stringify(templateClient, null, 2);
  };
  $('preset-form').onsubmit = async (e) => {
    e.preventDefault();
    await api(`/presets/${encodeURIComponent($('preset-id').value.trim())}`, { method: 'PUT', body: JSON.stringify({ name: $('preset-name').value.trim(), clients: JSON.parse($('preset-clients').value || '[]') }) });
    await refresh();
  };
  $('apply-preset').onclick = async () => {
    const id = $('preset-id').value.trim();
    if (id && window.confirm(`Apply preset ${id}?`)) { await api(`/presets/${encodeURIComponent(id)}`, { method: 'POST' }); await refresh(); }
  };
  $('delete-preset').onclick = async () => {
    const id = $('preset-id').value.trim();
    if (id) { await api(`/presets/${encodeURIComponent(id)}`, { method: 'DELETE' }); await refresh(); }
  };
  $('template-form').onsubmit = async (e) => {
    e.preventDefault();
    await api(`/templates/${encodeURIComponent($('template-id').value.trim())}`, { method: 'PUT', body: JSON.stringify({ name: $('template-name').value.trim(), client: JSON.parse($('template-client-json').value || '{}') }) });
    await refresh();
  };
  $('apply-template').onclick = async () => {
    const id = $('template-id').value.trim();
    const user_id = Number($('template-apply-user').value);
    if (id && user_id && window.confirm(`Apply template ${id} to client ${user_id}?`)) {
      await api(`/templates/${encodeURIComponent(id)}/apply`, { method: 'POST', body: JSON.stringify({ user_id }) });
      await refresh();
    }
  };
  $('delete-template').onclick = async () => {
    const id = $('template-id').value.trim();
    if (id) { await api(`/templates/${encodeURIComponent(id)}`, { method: 'DELETE' }); await refresh(); }
  };
}

function buildClientEditorConfig(userId) {
  const live = session(userId) || {};
  const device = deviceByUser(userId) || {};
  const desiredCfg = desired(userId) || {};
  const cfg = {
    ...defaultClient(userId),
    client_uid: live.client_uid || device.client_uid || null,
    role: live.role || device.role || 'client',
    name: device.name || '',
    ...desiredCfg,
    user_id: userId,
  };
  cfg.name = clientEditorName(userId, cfg, live);
  return cfg;
}
function clientDetailHtml(userId) {
  const live = session(userId) || {};
  const device = deviceByUser(userId) || {};
  const cfg = buildClientEditorConfig(userId);
  const deviceUid = cfg.client_uid || live.client_uid || device.client_uid || clientUidForUser(userId);
  const capability = capabilityContext(userId, cfg, live, device);
  const caps = capability.capabilities;
  const supportedCodecs = clientSupportedCodecs(userId, cfg, live, device, caps);
  const advertisedButtons = clientAdvertisedButtons(live, device);
  const actionTypes = caps.button_action_types || [];
  return `
    <div class="client-detail-head"><div><h2>Client Setup - ${esc(clientLabel(userId))}</h2><p class="muted">Capabilities: ${esc(capability.source)}</p></div><button id="close-client-panel" type="button">Close</button></div>
    <form id="client-form" class="config-form">
      <fieldset><legend>Setup</legend>
        <label>User ID<input id="user-id" type="number" min="1" max="65535" value="${cfg.user_id}" required title="Unique numeric client ID."></label>
        <label>Stable Device UID<span class="readonly-value" title="${esc(deviceUid || 'No linked device UID')}">${esc(deviceUid || 'No linked device UID')}</span><input id="client-uid" type="hidden" value="${esc(deviceUid || '')}"></label>
        <label>Display Name<input id="client-name" type="text" value="${esc(cfg.name)}" placeholder="Optional" autocomplete="off"></label>
        <label>Role<select id="client-role"><option value="client">Client</option><option value="bridge">Bridge</option></select></label>
        ${capability.fallback ? `<label>Client Type<select id="client-type-default"><option value="desktop">Desktop</option><option value="mobile">Mobile</option><option value="pi">Pi</option><option value="esp32">ESP32</option><option value="bridge">Bridge</option></select></label>` : `<label>Client Type<span class="readonly-value">${esc(caps.client_kind || 'unknown')}</span></label>`}
        <label>Codec<select id="codec">${codecOptionsHtml(live, cfg, supportedCodecs)}</select></label>
        <label id="opus-profile-field">Quality<select id="opus-profile"><option value="speech_16_low">Speech 16 Low</option><option value="speech_24_standard">Speech 24 Standard</option><option value="speech_48_high">Speech 48 High</option><option value="music_48">Music 48</option></select></label>
        ${liveCodecNoteHtml(live, cfg, supportedCodecs, capability.source)}
        <label>Talk Mode<select id="talk-mode"><option value="muted">Muted</option><option value="ptt">PTT</option><option value="open">Open</option></select></label>
        <label class="check"><input id="priority" type="checkbox"> Priority enabled</label>
      </fieldset>
      <fieldset class="wide"><legend>Routing & Buttons</legend>
        ${editorRoutingHtml(cfg, { includeIfb: false })}
        ${buttonSetupHtml(cfg, advertisedButtons, actionTypes)}
      </fieldset>
      <fieldset class="wide audio-pipeline-section"><legend>Audio Pipeline</legend>
        <label>Mode<select id="processing-mode"><option value="auto">Auto</option><option value="enabled">Enabled</option><option value="disabled">Disabled</option></select></label>
        <label>Engine<select id="processing-engine"><option value="built_in">Built-in lightweight DSP</option><option value="webrtc">WebRTC APM</option><option value="rnnoise">RNNoise</option><option value="deepfilternet">DeepFilterNet</option></select></label>
        <label>Ordered Server Stages<select id="processing-pipeline"><option value="">Use selected engine only</option><option value="webrtc,built_in">WebRTC -> Built-in cleanup</option><option value="webrtc,rnnoise,built_in">WebRTC -> RNNoise -> Built-in cleanup</option><option value="rnnoise,built_in">RNNoise -> Built-in cleanup</option><option value="deepfilternet,built_in">DeepFilterNet -> Built-in cleanup</option></select></label>
        <label>Profile<select id="processing-profile"><option value="raw">Raw</option><option value="voice">Voice</option><option value="voice_isolation">Voice Isolation</option><option value="broadcast">Broadcast</option></select></label>
        <label class="check"><input id="processing-high-pass" type="checkbox"> High-pass</label>
        <label class="check"><input id="processing-noise-gate" type="checkbox"> Noise gate</label>
        <label class="check"><input id="processing-vad" type="checkbox"> Speech VAD gate</label>
        <label class="check"><input id="processing-transient" type="checkbox"> Transient suppression</label>
        <label class="check"><input id="processing-compressor" type="checkbox"> Compressor</label>
        <label class="check"><input id="processing-presence" type="checkbox"> Presence</label>
        <label class="check"><input id="processing-fallback" type="checkbox"> Use built-in fallback if selected engine is unavailable</label>
        <label class="check"><input id="normalization-enabled" type="checkbox"> Loudness normalization</label>
        <label>Target RMS<input id="normalization-target" type="number" min="0.02" max="0.4" step="0.01" title="Speech loudness target after cleanup, before mixing. 0.14 is a conservative intercom default."></label>
        <label>Max Boost<input id="normalization-max-boost" type="number" min="1" max="16" step="0.25" title="Maximum linear gain applied to quiet speech. Higher values can make whispers clearer but may lift background noise."></label>
        <label>Max Attenuation<input id="normalization-max-attenuation" type="number" min="1" max="32" step="0.25" title="Maximum linear reduction applied to loud speech. 8 means the leveler may reduce to 1/8 gain."></label>
        <label>Adaptation ms<input id="normalization-adaptation-ms" type="number" min="20" max="5000" step="10" title="How quickly the leveler moves toward the target. Lower is faster but can pump."></label>
        <label>Noise Floor RMS<input id="normalization-noise-floor" type="number" min="0" max="0.2" step="0.001" title="Do not boost frames below this RMS, so silence and room noise stay quiet."></label>
        <label id="deep-filter-backend-field">DeepFilterNet Backend<select id="processing-deep-filter-backend">${deepFilterNetBackendOptions()}</select></label>
        <label id="apple-compute-units-field">Apple Compute Units<select id="processing-apple-compute-units"><option value="all">All</option><option value="cpu_and_gpu">CPU + GPU</option><option value="cpu_and_neural_engine">CPU + Neural Engine</option><option value="cpu_only">CPU only</option></select></label>
        <label id="deep-filter-model-field">DeepFilterNet Model<select id="processing-deep-filter-model">${deepFilterNetModelOptions(cfg.processing?.deep_filter_model || '')}</select></label>
        <label>Worker Queue Frames<input id="processing-worker-queue" type="number" min="1" max="200" step="1"></label>
        ${pipelineStatusHtml(live)}
      </fieldset>
      <details class="wide advanced-client-features"><summary>Advanced Client Features</summary>
        <div class="advanced-grid">
          <fieldset class="wide"><legend>Client Telemetry</legend>${captureHealthHtml(live)}</fieldset>
          ${(caps.supports_processing || caps.supports_native_voice_processing) ? `<fieldset><legend>Client-Local Processing</legend><label class="check"><input id="processing-native" type="checkbox"> Use native OS voice processing when available</label></fieldset>` : ''}
          ${caps.supports_esp32_audio ? `<fieldset class="wide"><legend>ESP32 Audio Hardware</legend>${esp32AudioEditorHtml()}</fieldset>` : ''}
          ${caps.supports_stereo ? `<fieldset class="wide"><legend>Stereo Receive</legend><label class="check"><input id="stereo-enabled" type="checkbox"> Stereo receive</label><div id="stereo-pan-wrap">${panEditorHtml(cfg)}</div></fieldset>` : ''}
          ${caps.supports_ifb ? `<fieldset class="wide"><legend>IFB</legend><label class="check"><input id="ifb-enabled" type="checkbox"> IFB enabled</label><label>Duck Gain<input id="ifb-duck-gain" type="number" min="0" max="1" step="0.01" value="${cfg.ifb?.duck_gain ?? 0.125}"></label><div class="table-wrap">${ifbRoutingHtml(cfg)}</div></fieldset>` : ''}
          <fieldset class="wide"><legend>Client Lockout</legend>${lockoutHtml(cfg.lockout, caps)}</fieldset>
        </div>
      </details>
      <div class="actions wide"><button type="submit" class="primary">Save Client</button><button id="delete-client" type="button" class="danger">Delete Desired Config</button></div>
    </form>`;
}
function openClientEditor(userId) {
  selectedUser = userId;
  clientEditorDirty = false;
  renderClientsPage();
}
function closeClientEditor() {
  selectedUser = null;
  clientEditorDirty = false;
  renderClientsPage();
}
function closeModal() { closeClientEditor(); modalRoot().innerHTML = ''; }
function captureHealthHtml(live) {
  const capture = live.capture;
  if (!capture) return '<p class="muted">No client telemetry reported by this client.</p>';
  const runtime = capture.runtime || {};
  const audio = clientAudioTelemetry(capture);
  const playback = clientPlaybackTelemetry(capture);
  const clientTransport = clientTransportTelemetry(capture);
  const codec = capture.codec_config;
  const sidetone = codec?.sidetone;
  const wifi = capture.wifi || {};
  const memory = capture.memory || {};
  const stack = capture.task_stack_high_water_bytes || {};
  const display = capture.display || {};
  const battery = capture.battery || {};
  const row = (label, item) => `<tr><td>${esc(label)}</td><td>${meter(item?.rms, '', item?.peak)}</td><td>${esc(dbfsText(item?.rms))}</td><td>${esc(dbfsText(item?.peak))}</td><td>${esc(pct(item?.dc_offset))}</td></tr>`;
  const platformRows = [];
  if (capture.wifi || capture.battery) {
    platformRows.push(`<div class="status-line"><span>RSSI ${esc(wifi.rssi_dbm ?? '-')} dBm</span><span>Wi-Fi ${wifi.connect_count || 0}/${wifi.disconnect_count || 0}</span><span>Control ${wifi.control_connect_count || 0}/${wifi.control_disconnect_count || 0}</span><span>Battery ${esc(battery.status || 'unknown')}</span></div>`);
  }
  if (codec) {
    platformRows.push(`<div class="status-line"><span>Codec ${esc(codec.chip || '-')}</span><span>Active ${codecName(codec.active_codec)}</span><span>Server audio ${codec.server_control_enabled ? 'on' : 'off'}</span><span>Backend ${esc(codec.audio_backend || '-')}</span><span>HW ${esc(codec.hardware_sample_rate_hz || codec.i2s_sample_rate_hz || '-')} Hz ${esc(codec.hardware_channels || '-')}ch/${esc(codec.hardware_bits_per_sample || '-')}bit</span><span>I2S ${esc(codec.i2s_format || '-')} ${esc(codec.i2s_slot_width || '-')}</span><span>Mic SW ${esc(codec.mic_software_gain_percent ?? '-')}%</span><span>Speaker SW ${esc(codec.speaker_software_gain_percent ?? '-')}%</span><span>Notifications ${esc(codec.notification_gain_percent ?? '-')}%</span><span>ALC ${codec.alc_enabled ? 'on' : 'off'}</span><span>Gate ${codec.noise_gate_enabled ? 'on' : 'off'}</span><span>Sidetone ${esc(sidetone?.mode || 'off')}</span><span>FW monitor ${esc(sidetone?.firmware_gain_percent ?? '-')}%</span><span>Line bypass ${esc(sidetone?.codec_bypass_gain_percent ?? '-')}%</span><span>Mic bypass ${esc(sidetone?.mic_bypass_gain_percent ?? '-')}%</span><span>Bypass source ${esc(sidetone?.active_bypass_source || '-')}</span><span>Bypass keeps DAC ${sidetone?.codec_bypass_preserves_dac ? 'yes' : 'no'}</span></div>`);
  }
  if (capture.memory || capture.free_heap_bytes || capture.min_free_heap_bytes) {
    platformRows.push(`<div class="status-line"><span>Heap ${memory.free_heap_bytes || capture.free_heap_bytes || 0}</span><span>Min heap ${memory.min_free_heap_bytes || capture.min_free_heap_bytes || 0}</span><span>Internal ${memory.internal_free_heap_bytes || 0}</span><span>Internal block ${memory.internal_largest_free_block_bytes || 0}</span><span>PSRAM ${memory.spiram_free_heap_bytes || 0}</span><span>PSRAM block ${memory.spiram_largest_free_block_bytes || 0}</span></div>`);
  }
  if (capture.task_stack_high_water_bytes) {
    platformRows.push(`<div class="status-line"><span>Stack UDP ${stack.udp || 0}</span><span>Reg ${stack.registration || 0}</span><span>Playback ${stack.playback || 0}</span><span>Capture ${stack.capture || 0}</span><span>Buttons ${stack.buttons || 0}</span><span>Display ${stack.display || 0}</span></div>`);
  }
  if (capture.display) {
    platformRows.push(`<div class="status-line"><span>Display ${display.enabled ? (display.initialized ? 'ready' : 'enabled') : 'disabled'}</span><span>FB ${display.framebuffer_bytes || 0}</span><span>FB PSRAM ${display.framebuffer_in_psram ? 'yes' : 'no'}</span></div>`);
  }
  return `
    <div class="status-line"><span>Kind ${esc(runtime.client_kind || live.role || 'client')}</span><span>Phase ${esc(runtime.phase || 'running')}</span><span>Uptime ${Math.round((capture.uptime_ms || 0) / 1000)}s</span>${runtime.last_error ? `<span>Error ${esc(runtime.last_error)}</span>` : ''}</div>
    <div class="status-line"><span>Audio ${esc(audio.backend || capture.adc_input || '-')}</span><span>Input ${esc(audio.input_device || '-')}</span><span>Output ${esc(audio.output_device || '-')}</span><span>Format ${esc(audio.sample_format || '-')}</span><span>Rate ${esc(audio.sample_rate_hz || '-')} Hz</span><span>Channels ${esc(audio.channels ?? '-')}</span><span>Mode ${esc(audio.channel_mode || capture.capture_channel || '-')}</span><span>Mic gain ${audio.mic_gain == null ? '-' : Number(audio.mic_gain).toFixed(2)}</span></div>
    <div class="status-line"><span>Playback ${playback.available_samples || playback.queue_depth || capture.playback_queue_depth || 0}/${playback.capacity_samples || '-'}</span><span>Prebuffer ${playback.prebuffer_samples ?? '-'}</span><span>Channels ${playback.channels ?? '-'}</span><span>Started ${playback.started ? 'yes' : 'no'}</span><span>Underflows ${playback.underflows || capture.playback_underflows || 0}</span><span>Overflows ${playback.overflows || capture.playback_overflows || 0}</span><span>Dropped samples ${playback.dropped_samples || 0}</span></div>
    <div class="status-line"><span>RX ${clientTransport.udp_rx_packets || 0}</span><span>Malformed ${clientTransport.malformed_packets || 0}</span><span>Decode ${clientTransport.decode_errors || 0}</span><span>Codec drops ${clientTransport.codec_drops || 0}</span><span>Payload ${clientTransport.payload_decode_errors || 0}</span><span>Packet encode ${clientTransport.packet_encode_errors || 0}</span><span>TX ${clientTransport.tx_packets || capture.tx_packets_sent || 0}</span><span>TX failures ${clientTransport.tx_send_failures || capture.tx_send_failures || 0}</span><span>Queue drops ${clientTransport.tx_queue_drops || 0}</span></div>
    ${platformRows.join('')}
    <div class="table-wrap"><table><thead><tr><th>Stage</th><th>Level</th><th>RMS</th><th>Peak</th><th>DC</th></tr></thead><tbody>${row('Input', audio.input)}${row('Pre gain', audio.pre_gain)}${row('Post gain', audio.post_gain)}${row('Left', capture.left)}${row('Right', capture.right)}${row('Selected', capture.selected)}</tbody></table></div>`;
}
function clientAudioTelemetry(capture) {
  if (capture.audio) return capture.audio;
  if (capture.desktop) {
    return {
      backend: capture.desktop.backend,
      input_device: capture.desktop.device,
      sample_format: capture.desktop.sample_format,
      sample_rate_hz: capture.desktop.sample_rate_hz,
      channels: capture.desktop.channels,
      channel_mode: capture.desktop.channel_mode,
      mic_gain: capture.desktop.mic_gain,
      input: capture.desktop.post_gain,
      pre_gain: capture.desktop.pre_gain,
      post_gain: capture.desktop.post_gain,
      pre_gain_clipped_samples: capture.desktop.pre_gain_clipped_samples,
      post_gain_clipped_samples: capture.desktop.post_gain_clipped_samples,
      dropped_frames: capture.desktop.dropped_frames,
    };
  }
  return {
    backend: capture.codec_config?.audio_backend || capture.adc_input || '',
    sample_rate_hz: capture.codec_config?.hardware_sample_rate_hz || capture.codec_config?.i2s_sample_rate_hz || 0,
    channels: capture.codec_config?.hardware_channels || 0,
    channel_mode: capture.capture_channel || '',
    mic_gain: capture.software_gain_percent == null ? null : Number(capture.software_gain_percent) / 100,
    input: capture.selected || {},
    pre_gain: capture.selected || {},
    post_gain: capture.selected || {},
  };
}
function clientPlaybackTelemetry(capture) {
  return capture.playback || {
    available_samples: capture.playback_queue_depth || 0,
    queue_depth: capture.playback_queue_depth || 0,
    underflows: capture.playback_underflows || 0,
    overflows: capture.playback_overflows || 0,
  };
}
function clientTransportTelemetry(capture) {
  const esp32 = capture.transport || {};
  return capture.client_transport || {
    udp_rx_packets: esp32.udp_rx_packets || 0,
    malformed_packets: esp32.udp_decode_errors || 0,
    decode_errors: esp32.opus_decode_failures || 0,
    codec_drops: esp32.udp_codec_drops || 0,
    payload_decode_errors: esp32.udp_payload_decode_errors || 0,
    packet_encode_errors: 0,
    tx_packets: capture.tx_packets_sent || 0,
    tx_send_failures: capture.tx_send_failures || esp32.udp_tx_send_failures || 0,
    tx_queue_drops: esp32.audio_tx_queue_drops || 0,
  };
}
function esp32AudioEditorHtml() {
  return `<div class="status-line"><span>When enabled, the server overrides runtime-changeable ESP32 menuconfig audio defaults on config update.</span></div>
    <label class="check"><input id="esp32-audio-enabled" type="checkbox"> Server controls ESP32 audio</label>
    <div id="esp32-audio-fields" class="config-grid">
      <label>ADC Input<select id="esp32-adc-input"><option value="difference">Differential board mic</option><option value="mic1">Mic 1</option><option value="mic2">Mic 2</option><option value="line1">Line 1</option><option value="line2">Line 2</option></select></label>
      <label>Mic PGA Gain dB<input id="esp32-mic-pga" type="number" min="0" max="24" step="3"></label>
      <label>Capture Channel<select id="esp32-capture-channel"><option value="left">Left</option><option value="right">Right</option><option value="average">Average</option></select></label>
      <label class="check"><input id="esp32-high-pass" type="checkbox"> Capture high-pass / DC blocker</label>
      <label class="check"><input id="esp32-alc" type="checkbox"> ES8388 automatic level control</label>
      <label class="check"><input id="esp32-noise-gate" type="checkbox"> ES8388 noise gate</label>
      <label>Mic Software Gain %<input id="esp32-mic-sw-gain" type="number" min="0" max="400" step="1"></label>
      <label>Speaker Software Gain %<input id="esp32-speaker-sw-gain" type="number" min="0" max="400" step="1"></label>
      <label>Notification Gain %<input id="esp32-notification-gain" type="number" min="0" max="200" step="1"></label>
      <label>Sidetone Mode<select id="esp32-sidetone-mode"><option value="off">Off</option><option value="firmware">Firmware monitor</option><option value="codec_bypass">ES8388 line-bypass</option></select></label>
      <label>Firmware Sidetone Gain %<input id="esp32-sidetone-gain" type="number" min="0" max="200" step="1"></label>
      <label>Line-Bypass Gain %<input id="esp32-codec-bypass-gain" type="number" min="0" max="200" step="1"></label>
      <label>Mic-Bypass Gain %<input id="esp32-mic-bypass-gain" type="number" min="0" max="400" step="1"></label>
    </div>`;
}
function editorRoutingHtml(cfg, { includeIfb = true } = {}) {
  const listen = new Set(cfg.listen || []);
  const tx = new Set(cfg.tx || []);
  const priority = new Set(cfg.priority_channels || []);
  const ifbProgram = new Set(cfg.ifb?.program || []);
  const ifbInterrupt = new Set(cfg.ifb?.interrupt || []);
  const ifbHead = includeIfb ? '<th>IFB Program</th><th>IFB Interrupt</th>' : '';
  return `<div class="table-wrap"><table><thead><tr><th>Channel</th><th>Listen</th><th>Regular TX</th><th>Priority</th><th>Gain</th>${ifbHead}</tr></thead><tbody>${allChannelIds([cfg]).map((ch) => `<tr>
    <td>${esc(channelLabel(ch))}</td>
    <td><input data-editor-listen="${ch}" type="checkbox" ${listen.has(ch) ? 'checked' : ''}></td>
    <td><input data-editor-tx="${ch}" type="checkbox" ${tx.has(ch) ? 'checked' : ''}></td>
    <td><input data-editor-priority="${ch}" type="checkbox" ${priority.has(ch) ? 'checked' : ''}></td>
    <td><input data-editor-vol="${ch}" type="number" min="0" max="4" step="0.05" value="${cfg.vol?.[ch] ?? 1}"></td>
    ${includeIfb ? `<td><input data-ifb-program="${ch}" type="checkbox" ${ifbProgram.has(ch) ? 'checked' : ''}></td><td><input data-ifb-interrupt="${ch}" type="checkbox" ${ifbInterrupt.has(ch) ? 'checked' : ''}></td>` : ''}
  </tr>`).join('')}</tbody></table></div>`;
}
function ifbRoutingHtml(cfg) {
  const ifbProgram = new Set(cfg.ifb?.program || []);
  const ifbInterrupt = new Set(cfg.ifb?.interrupt || []);
  return `<table><thead><tr><th>Channel</th><th>Program</th><th>Interrupt</th></tr></thead><tbody>${allChannelIds([cfg]).map((ch) => `<tr><td>${esc(channelLabel(ch))}</td><td><input data-ifb-program="${ch}" type="checkbox" ${ifbProgram.has(ch) ? 'checked' : ''}></td><td><input data-ifb-interrupt="${ch}" type="checkbox" ${ifbInterrupt.has(ch) ? 'checked' : ''}></td></tr>`).join('')}</tbody></table>`;
}
function panEditorHtml(cfg) {
  const pan = cfg.stereo?.channel_pan || {};
  return `<div class="pan-list">${allChannelIds([cfg]).map((ch) => {
    const value = Math.round((Number(pan[ch]) || 0) * 100);
    return `<div class="pan-row"><strong>${esc(channelLabel(ch))}</strong><span class="pan-slider-wrap"><input data-pan="${ch}" type="range" min="-100" max="100" step="1" value="${value}"></span><input data-pan-number="${ch}" type="number" min="-100" max="100" step="1" value="${value}"></div>`;
  }).join('')}</div>`;
}
function advertisedButtonHtml(live, cfg) {
  const configured = new Set((cfg.buttons || []).map((button) => button.id));
  const buttons = live.advertised_buttons || [];
  if (!buttons.length) return '<span class="muted">No advertised buttons from this connected client.</span>';
  return buttons.map((button) => `<button class="pill" data-add-advertised="${esc(button.id)}" data-label="${esc(button.label || button.id)}" type="button">${esc(button.id)}: ${esc(button.label || button.id)}${configured.has(button.id) ? ' configured' : ' available'}</button>`).join('');
}
function buttonSetupHtml(cfg, advertisedButtons = [], actionTypes = []) {
  const actions = actionTypes;
  const configured = new Set((cfg.buttons || []).map((button) => button.id));
  const advertised = advertisedButtons.length
    ? advertisedButtons.map((button) => `<button class="pill" data-add-advertised="${esc(button.id)}" data-label="${esc(button.label || button.id)}" type="button">${esc(button.id)}: ${esc(button.label || button.id)}${configured.has(button.id) ? ' configured' : ' available'}</button>`).join('')
    : '<span class="muted">No advertised physical buttons from this client.</span>';
  if (!advertisedButtons.length && !(cfg.buttons || []).length) {
    return `<div class="button-setup"><h3>Advertised Buttons</h3><div class="pill-list">${advertised}</div><input id="button-action-types" type="hidden" value="${esc(actions.join(','))}"></div>`;
  }
  return `<div class="button-setup"><h3>Advertised Buttons</h3><div class="pill-list">${advertised}</div><input id="button-action-types" type="hidden" value="${esc(actions.join(','))}"><div id="button-editor">${(cfg.buttons || []).map((button) => buttonRowHtml(button, actions)).join('')}</div><button id="add-button-row" type="button">Add Button</button></div>`;
}
function buttonColor(value) {
  const color = String(value || '').trim();
  return /^#([0-9a-fA-F]{3}|[0-9a-fA-F]{6})$/.test(color) ? color : '#2f7dd3';
}
function buttonRowHtml(button = { id: '', label: '', color: '#2f7dd3', mode: 'momentary', actions: [] }, actionTypes = []) {
  const show = (type) => actionTypes.includes(type);
  const tx = (button.actions || []).find((action) => action.type === 'transmit') || {};
  const alert = (button.actions || []).find((action) => action.type === 'alert') || {};
  const alertTarget = (alert.targets || [])[0] || {};
  const preset = (button.actions || []).find((action) => action.type === 'apply_preset') || {};
  const talk = (button.actions || []).find((action) => action.type === 'set_talk_mode') || {};
  const route = (button.actions || []).find((action) => action.type === 'route_edit') || {};
  return `<div class="button-row">
    <label>ID<input data-button-id value="${esc(button.id)}" placeholder="director"></label>
    <label>Label<input data-button-label value="${esc(button.label || button.id)}" placeholder="Director"></label>
    <label>Color<input data-button-color type="color" value="${buttonColor(button.color)}"></label>
    <label>Mode<select data-button-mode><option value="momentary">Momentary</option><option value="latching">Latching</option></select></label>
    <button data-remove-button type="button" class="danger">Remove</button>
    ${show('transmit') ? `<label>TX Channels<input data-button-tx-channels value="${csv(tx.channels)}" placeholder="1,4"></label><label>TX Users<input data-button-tx-users value="${csv(tx.users)}" placeholder="2,3"></label><label class="check"><input data-button-tx-duck type="checkbox" ${tx.duck ? 'checked' : ''}> Duck direct targets</label>` : ''}
    ${show('alert') ? `<label>Alert Type<select data-button-alert-kind><option value="">None</option><option value="user">User</option><option value="channel">Channel</option></select></label><label>Alert ID<input data-button-alert-id type="number" min="0" value="${alertTarget.id ?? ''}"></label><label>Alert Message<input data-button-alert-message value="${esc(alert.message || '')}"></label>` : ''}
    ${show('apply_preset') ? `<label>Apply Preset<input data-button-preset value="${esc(preset.preset_id || '')}" placeholder="preset-id"></label>` : ''}
    ${show('set_talk_mode') ? `<label>Set Talk Users<input data-button-talk-users value="${csv(talk.users)}" placeholder="2,3"></label><label>Set Talk Mode<select data-button-talk-mode><option value="">No change</option><option value="muted">Muted</option><option value="ptt">PTT</option><option value="open">Open</option></select></label>` : ''}
    ${show('route_edit') ? `<label>Listen Add<input data-route-listen-add value="${csv(route.listen_add)}"></label><label>Listen Remove<input data-route-listen-remove value="${csv(route.listen_remove)}"></label><label>Listen Toggle<input data-route-listen-toggle value="${csv(route.listen_toggle)}"></label><label>TX Add<input data-route-tx-add value="${csv(route.tx_add)}"></label><label>TX Remove<input data-route-tx-remove value="${csv(route.tx_remove)}"></label><label>TX Toggle<input data-route-tx-toggle value="${csv(route.tx_toggle)}"></label>` : ''}
  </div>`;
}
function talkerGainEditorHtml(cfg) {
  const users = mergedUsers().filter((id) => id !== cfg.user_id);
  if (!users.length) return '<p class="muted">No other clients yet.</p>';
  return `<div class="table-wrap"><table><thead><tr><th>Talker</th><th>Gain</th></tr></thead><tbody>${users.map((id) => `<tr><td>${esc(clientLabel(id))}</td><td><input data-editor-talker="${id}" type="number" min="0" max="4" step="0.05" value="${cfg.talker_vol?.[id] ?? 1}"></td></tr>`).join('')}</tbody></table></div>`;
}
function lockoutHtml(lockout = {}, caps = {}) {
  const cfg = { ...defaultLockout(), ...lockout };
  const items = [
    ['lock-channels', 'allow_channels', 'Channels'], ['lock-volumes', 'allow_volumes', 'Volumes'], ['lock-codec', 'allow_codec', 'Codec'],
    ['lock-talk-mode', 'allow_talk_mode', 'Talk Mode'], ['lock-priority', 'allow_priority', 'Priority'], ['lock-buttons', 'allow_buttons', 'Buttons'],
    ...(caps.supports_ifb ? [['lock-ifb', 'allow_ifb', 'IFB']] : []),
    ...(caps.supports_device_selection ? [['lock-device-selection', 'allow_device_selection', 'Device Selection']] : []),
    ...(caps.supports_local_api ? [['lock-local-api', 'allow_local_api', 'Local API']] : []),
  ];
  return `<div class="pill-list">${items.map(([id, key, label]) => `<label class="check"><input id="${id}" type="checkbox" ${cfg[key] ? 'checked' : ''}> ${label}</label>`).join('')}</div>`;
}
function bindClientEditor(cfg) {
  $('client-role').value = cfg.role || 'client';
  if ($('client-type-default')) {
    $('client-type-default').value = fallbackClientType(cfg.user_id, cfg, session(cfg.user_id) || {}, deviceByUser(cfg.user_id) || {});
    $('client-type-default').onchange = () => {
      clientTypeDefaults[cfg.user_id] = $('client-type-default').value;
      persistClientTypeDefaults();
      clientEditorDirty = false;
      renderClientsPage();
    };
  }
  $('codec').value = cfg.codec || 'pcm16';
  $('opus-profile').value = normalizeOpusProfile(cfg.opus_profile || 'speech_24_standard');
  $('talk-mode').value = cfg.talk_mode || 'ptt';
  $('priority').checked = !!cfg.priority;
  const processing = { ...defaultProcessing(), ...(cfg.processing || {}) };
  const esp32Audio = { ...defaultEsp32Audio(), ...(cfg.esp32_audio || {}) };
  esp32Audio.sidetone = { ...defaultEsp32Audio().sidetone, ...(cfg.esp32_audio?.sidetone || {}) };
  $('processing-mode').value = processing.mode;
  $('processing-engine').value = processing.engine || 'built_in';
  $('processing-pipeline').value = processingPipelinePresetValue(processing.pipeline);
  $('processing-profile').value = processing.profile;
  $('processing-high-pass').checked = processing.high_pass;
  $('processing-noise-gate').checked = processing.noise_gate;
  $('processing-vad').checked = processing.vad;
  $('processing-transient').checked = processing.transient_suppression;
  $('processing-compressor').checked = processing.compressor;
  $('processing-presence').checked = processing.presence;
  if ($('processing-native')) $('processing-native').checked = processing.native_voice_processing;
  $('processing-fallback').checked = processing.fallback_to_builtin !== false;
  const normalization = { ...defaultProcessing().normalization, ...(processing.normalization || {}) };
  $('normalization-enabled').checked = !!normalization.enabled;
  $('normalization-target').value = normalization.target_rms;
  $('normalization-max-boost').value = normalization.max_boost;
  $('normalization-max-attenuation').value = normalization.max_attenuation;
  $('normalization-adaptation-ms').value = normalization.adaptation_ms;
  $('normalization-noise-floor').value = normalization.noise_floor_rms;
  $('processing-deep-filter-backend').value = processing.deep_filter_backend || 'auto';
  $('processing-apple-compute-units').value = processing.apple_compute_units || 'all';
  $('processing-deep-filter-model').value = processing.deep_filter_model || '';
  $('processing-worker-queue').value = processing.worker_queue_frames || 12;
  if ($('esp32-audio-enabled')) {
    $('esp32-audio-enabled').checked = !!esp32Audio.enabled;
    $('esp32-adc-input').value = esp32Audio.adc_input;
    $('esp32-mic-pga').value = esp32Audio.mic_pga_gain_db;
    $('esp32-capture-channel').value = esp32Audio.capture_channel;
    $('esp32-high-pass').checked = !!esp32Audio.high_pass_enabled;
    $('esp32-alc').checked = esp32Audio.alc_enabled !== false;
    $('esp32-noise-gate').checked = esp32Audio.noise_gate_enabled !== false;
    $('esp32-mic-sw-gain').value = esp32Audio.mic_software_gain_percent;
    $('esp32-speaker-sw-gain').value = esp32Audio.speaker_software_gain_percent;
    $('esp32-notification-gain').value = esp32Audio.notification_gain_percent;
    $('esp32-sidetone-mode').value = esp32Audio.sidetone.mode;
    $('esp32-sidetone-gain').value = esp32Audio.sidetone.firmware_gain_percent;
    $('esp32-codec-bypass-gain').value = esp32Audio.sidetone.codec_bypass_gain_percent;
    $('esp32-mic-bypass-gain').value = esp32Audio.sidetone.mic_bypass_gain_percent;
  }
  if ($('stereo-enabled')) $('stereo-enabled').checked = !!cfg.stereo?.enabled;
  if ($('ifb-enabled')) $('ifb-enabled').checked = !!cfg.ifb?.enabled;
  updateOpusProfileVisibility();
  if ($('stereo-enabled')) updateStereoPanVisibility();
  updateDeepFilterModelVisibility();
  $('codec').onchange = updateOpusProfileVisibility;
  $('processing-engine').onchange = updateDeepFilterModelVisibility;
  $('processing-pipeline').onchange = updateDeepFilterModelVisibility;
  $('processing-deep-filter-backend').onchange = updateDeepFilterModelVisibility;
  if ($('stereo-enabled')) $('stereo-enabled').onchange = updateStereoPanVisibility;
  if ($('esp32-audio-enabled')) {
    $('esp32-audio-enabled').onchange = updateEsp32AudioVisibility;
    updateEsp32AudioVisibility();
  }
  if ($('close-client-panel')) $('close-client-panel').onclick = closeClientEditor;
  if ($('add-button-row')) $('add-button-row').onclick = (e) => {
    e.preventDefault();
    const actionTypes = ($('button-action-types')?.value || '').split(',').filter(Boolean);
    $('button-editor').insertAdjacentHTML('beforeend', buttonRowHtml(undefined, actionTypes));
    bindButtonRows();
  };
  editorRoot().querySelectorAll('[data-add-advertised]').forEach((button) => button.onclick = () => {
    const actionTypes = ($('button-action-types')?.value || '').split(',').filter(Boolean);
    if (!$('button-editor')) button.closest('.button-setup').insertAdjacentHTML('beforeend', `<div id="button-editor"></div><button id="add-button-row" type="button">Add Button</button>`);
    $('button-editor').insertAdjacentHTML('beforeend', buttonRowHtml({ id: button.dataset.addAdvertised, label: button.dataset.label, mode: 'momentary', actions: [] }, actionTypes));
    bindClientEditor(cfg);
  });
  bindButtonRows();
  bindPanControls();
  $('client-form').onsubmit = saveClientEditor;
  editorRoot().querySelectorAll('input,select,textarea').forEach((el) => {
    el.addEventListener('input', () => { clientEditorDirty = true; });
    el.addEventListener('change', () => { clientEditorDirty = true; });
  });
  $('delete-client').onclick = async () => {
    const id = Number($('user-id').value);
    if (id) { await api(`/clients/${id}`, { method: 'DELETE' }); selectedUser = null; clientEditorDirty = false; await refresh(); }
  };
}
function updateOpusProfileVisibility() { $('opus-profile-field').classList.toggle('hide', $('codec').value !== 'opus'); }
function updateStereoPanVisibility() { if ($('stereo-pan-wrap') && $('stereo-enabled')) $('stereo-pan-wrap').classList.toggle('hide', !$('stereo-enabled').checked); }
function updateDeepFilterModelVisibility() {
  const pipeline = $('processing-pipeline').value || '';
  const usesDeepFilter = $('processing-engine').value === 'deepfilternet' || pipeline.split(',').includes('deepfilternet');
  $('deep-filter-backend-field').classList.toggle('hide', !usesDeepFilter);
  $('apple-compute-units-field').classList.toggle('hide', !usesDeepFilter || $('processing-deep-filter-backend').value === 'tract');
  $('deep-filter-model-field').classList.toggle('hide', !usesDeepFilter);
}
function updateEsp32AudioVisibility() {
  if (!$('esp32-audio-enabled') || !$('esp32-audio-fields')) return;
  const enabled = $('esp32-audio-enabled').checked;
  $('esp32-audio-fields').classList.toggle('muted-fields', !enabled);
  editorRoot().querySelectorAll('#esp32-audio-fields input,#esp32-audio-fields select').forEach((el) => el.disabled = !enabled);
}
function bindButtonRows() {
  editorRoot().querySelectorAll('[data-remove-button]').forEach((button) => button.onclick = () => button.closest('.button-row').remove());
  editorRoot().querySelectorAll('[data-button-mode]').forEach((select) => {
    const row = select.closest('.button-row');
    const id = row.querySelector('[data-button-id]').value;
    const cfg = (desired(selectedUser)?.buttons || []).find((button) => button.id === id);
    if (cfg) select.value = cfg.mode || 'momentary';
  });
  editorRoot().querySelectorAll('[data-button-alert-kind]').forEach((select) => {
    const row = select.closest('.button-row');
    const id = row.querySelector('[data-button-id]').value;
    const cfg = (desired(selectedUser)?.buttons || []).find((button) => button.id === id);
    const alert = (cfg?.actions || []).find((action) => action.type === 'alert');
    if (alert) select.value = (alert.targets || [])[0]?.kind || '';
  });
  editorRoot().querySelectorAll('[data-button-talk-mode]').forEach((select) => {
    const row = select.closest('.button-row');
    const id = row.querySelector('[data-button-id]').value;
    const cfg = (desired(selectedUser)?.buttons || []).find((button) => button.id === id);
    const talk = (cfg?.actions || []).find((action) => action.type === 'set_talk_mode');
    if (talk) select.value = talk.mode || '';
  });
}
function clampPanUi(value) {
  let next = Math.max(-100, Math.min(100, Number(value) || 0));
  if (Math.abs(next) <= 3) next = 0;
  return Math.round(next);
}
function bindPanControls() {
  editorRoot().querySelectorAll('[data-pan]').forEach((slider) => {
    const number = editorRoot().querySelector(`[data-pan-number="${slider.dataset.pan}"]`);
    slider.oninput = () => { const value = clampPanUi(slider.value); slider.value = value; number.value = value; };
  });
  editorRoot().querySelectorAll('[data-pan-number]').forEach((input) => {
    const slider = editorRoot().querySelector(`[data-pan="${input.dataset.panNumber}"]`);
    input.onkeydown = (e) => {
      if (e.key === 'ArrowLeft' || e.key === 'ArrowDown') { e.preventDefault(); input.value = clampPanUi(Number(input.value) - 1); slider.value = input.value; }
      if (e.key === 'ArrowRight' || e.key === 'ArrowUp') { e.preventDefault(); input.value = clampPanUi(Number(input.value) + 1); slider.value = input.value; }
    };
    input.onchange = () => { const value = clampPanUi(input.value); input.value = value; slider.value = value; };
  });
}
function checkedNumbers(selector, attr) {
  return sortedChannels([...editorRoot().querySelectorAll(selector)].filter((el) => el.checked).map((el) => Number(el.dataset[attr])));
}
function numberMap(selector, attr, skipDefault = true) {
  const out = {};
  editorRoot().querySelectorAll(selector).forEach((el) => {
    const value = Number(el.value);
    const id = Number(el.dataset[attr]);
    if (Number.isFinite(value) && (!skipDefault || value !== 1)) out[id] = value;
  });
  return out;
}
function readPanMap() {
  const out = {};
  if (!$('stereo-enabled') || !$('stereo-enabled').checked) return out;
  editorRoot().querySelectorAll('[data-pan-number]').forEach((el) => {
    const value = clampPanUi(el.value);
    if (value !== 0) out[el.dataset.panNumber] = value / 100;
  });
  return out;
}
function readButtons() {
  const actionTypes = ($('button-action-types')?.value || '').split(',').filter(Boolean);
  const actionVisible = (type, selector) => actionTypes.includes(type) && !!selector;
  return [...editorRoot().querySelectorAll('.button-row')].map((row) => {
    const id = row.querySelector('[data-button-id]').value.trim();
    const original = (shownClient(selectedUser)?.buttons || []).find((button) => button.id === id) || {};
    const actions = (original.actions || []).filter((action) => !actionTypes.includes(action.type));
    const txChannels = row.querySelector('[data-button-tx-channels]');
    const txUsers = row.querySelector('[data-button-tx-users]');
    if (actionVisible('transmit', txChannels && txUsers)) {
      const channels = parseCsv(txChannels.value, { allowZero: true });
      const users = parseCsv(txUsers.value);
      if (channels.length || users.length) actions.push({ type: 'transmit', channels, users, duck: !!row.querySelector('[data-button-tx-duck]')?.checked });
    }
    const alertKindInput = row.querySelector('[data-button-alert-kind]');
    if (actionVisible('alert', alertKindInput)) {
      const alertKind = alertKindInput.value;
      const alertId = Number(row.querySelector('[data-button-alert-id]')?.value);
      const alertIdValid = Number.isInteger(alertId) && (alertKind === 'channel' ? alertId >= 0 : alertId > 0);
      if (alertKind && alertIdValid) actions.push({ type: 'alert', targets: [{ kind: alertKind, id: alertId }], message: row.querySelector('[data-button-alert-message]')?.value.trim() || null });
    }
    const presetInput = row.querySelector('[data-button-preset]');
    if (actionVisible('apply_preset', presetInput)) {
      const presetId = presetInput.value.trim();
      if (presetId) actions.push({ type: 'apply_preset', preset_id: presetId });
    }
    const talkModeInput = row.querySelector('[data-button-talk-mode]');
    if (actionVisible('set_talk_mode', talkModeInput)) {
      const talkMode = talkModeInput.value;
      const talkUsers = parseCsv(row.querySelector('[data-button-talk-users]')?.value);
      if (talkMode && talkUsers.length) actions.push({ type: 'set_talk_mode', users: talkUsers, mode: talkMode });
    }
    const routeInput = row.querySelector('[data-route-listen-add]');
    if (actionVisible('route_edit', routeInput)) {
      const route = {
        users: [],
        listen_add: parseCsv(row.querySelector('[data-route-listen-add]')?.value, { allowZero: true }),
        listen_remove: parseCsv(row.querySelector('[data-route-listen-remove]')?.value, { allowZero: true }),
        listen_toggle: parseCsv(row.querySelector('[data-route-listen-toggle]')?.value, { allowZero: true }),
        tx_add: parseCsv(row.querySelector('[data-route-tx-add]')?.value, { allowZero: true }),
        tx_remove: parseCsv(row.querySelector('[data-route-tx-remove]')?.value, { allowZero: true }),
        tx_toggle: parseCsv(row.querySelector('[data-route-tx-toggle]')?.value, { allowZero: true }),
      };
      if (Object.entries(route).some(([key, value]) => key !== 'users' && value.length)) actions.push({ type: 'route_edit', ...route });
    }
    const color = buttonColor(row.querySelector('[data-button-color]').value);
    return { id, label: row.querySelector('[data-button-label]').value.trim() || id, color, mode: row.querySelector('[data-button-mode]').value, actions };
  }).filter((button) => button.id);
}
function lockoutBody() {
  const existing = shownClient(selectedUser)?.lockout || defaultLockout();
  const checked = (id, key) => $(id) ? $(id).checked : existing[key];
  return {
    allow_channels: checked('lock-channels', 'allow_channels'),
    allow_volumes: checked('lock-volumes', 'allow_volumes'),
    allow_codec: checked('lock-codec', 'allow_codec'),
    allow_talk_mode: checked('lock-talk-mode', 'allow_talk_mode'),
    allow_priority: checked('lock-priority', 'allow_priority'),
    allow_buttons: checked('lock-buttons', 'allow_buttons'),
    allow_ifb: checked('lock-ifb', 'allow_ifb'),
    allow_device_selection: checked('lock-device-selection', 'allow_device_selection'),
    allow_local_api: checked('lock-local-api', 'allow_local_api'),
  };
}
function processingBody(existing = {}) {
  const model = $('processing-deep-filter-model').value.trim();
  return {
    mode: $('processing-mode').value,
    engine: $('processing-engine').value,
    pipeline: processingPipelineFromPreset($('processing-pipeline').value),
    profile: $('processing-profile').value,
    high_pass: $('processing-high-pass').checked,
    noise_gate: $('processing-noise-gate').checked,
    vad: $('processing-vad').checked,
    transient_suppression: $('processing-transient').checked,
    compressor: $('processing-compressor').checked,
    presence: $('processing-presence').checked,
    native_voice_processing: $('processing-native') ? $('processing-native').checked : existing.native_voice_processing !== false,
    fallback_to_builtin: $('processing-fallback').checked,
    deep_filter_model: model || null,
    deep_filter_backend: $('processing-deep-filter-backend').value,
    apple_compute_units: $('processing-apple-compute-units').value,
    worker_queue_frames: Math.max(1, Number($('processing-worker-queue').value) || 12),
    normalization: {
      enabled: $('normalization-enabled').checked,
      target_rms: Math.max(0.02, Math.min(0.4, Number($('normalization-target').value) || 0.14)),
      max_boost: Math.max(1, Math.min(16, Number($('normalization-max-boost').value) || 4)),
      max_attenuation: Math.max(1, Math.min(32, Number($('normalization-max-attenuation').value) || 8)),
      adaptation_ms: Math.max(20, Math.min(5000, Number($('normalization-adaptation-ms').value) || 250)),
      noise_floor_rms: Math.max(0, Math.min(0.2, Number($('normalization-noise-floor').value) || 0.012)),
    },
  };
}
function esp32AudioBody(existing = defaultEsp32Audio()) {
  if (!$('esp32-audio-enabled')) return existing;
  return {
    enabled: $('esp32-audio-enabled').checked,
    adc_input: $('esp32-adc-input').value,
    mic_pga_gain_db: Number($('esp32-mic-pga').value),
    capture_channel: $('esp32-capture-channel').value,
    mic_software_gain_percent: Number($('esp32-mic-sw-gain').value),
    speaker_software_gain_percent: Number($('esp32-speaker-sw-gain').value),
    notification_gain_percent: Number($('esp32-notification-gain').value),
    high_pass_enabled: $('esp32-high-pass').checked,
    alc_enabled: $('esp32-alc').checked,
    noise_gate_enabled: $('esp32-noise-gate').checked,
    sidetone: {
      mode: $('esp32-sidetone-mode').value,
      firmware_gain_percent: Number($('esp32-sidetone-gain').value),
      codec_bypass_gain_percent: Number($('esp32-codec-bypass-gain').value),
      mic_bypass_gain_percent: Number($('esp32-mic-bypass-gain').value),
    },
  };
}
async function saveClientEditor(e) {
  e.preventDefault();
  const id = Number($('user-id').value);
  const existing = shownClient(id);
  const hasButtons = !!$('button-editor');
  const hasTalkerGains = editorRoot().querySelectorAll('[data-editor-talker]').length > 0;
  const hasIfb = !!$('ifb-enabled');
  const hasStereo = !!$('stereo-enabled');
  const body = {
    client_uid: $('client-uid').value.trim() || null,
    role: $('client-role').value,
    name: $('client-name').value.trim(),
    listen: checkedNumbers('[data-editor-listen]', 'editorListen'),
    tx: checkedNumbers('[data-editor-tx]', 'editorTx'),
    vol: numberMap('[data-editor-vol]', 'editorVol', false),
    talker_vol: hasTalkerGains ? numberMap('[data-editor-talker]', 'editorTalker') : existing.talker_vol || {},
    codec: $('codec').value,
    opus_profile: $('opus-profile').value,
    talk_mode: $('talk-mode').value,
    priority: $('priority').checked,
    priority_channels: checkedNumbers('[data-editor-priority]', 'editorPriority'),
    buttons: hasButtons ? readButtons() : existing.buttons || [],
    ifb: hasIfb ? { enabled: $('ifb-enabled').checked, program: checkedNumbers('[data-ifb-program]', 'ifbProgram'), interrupt: checkedNumbers('[data-ifb-interrupt]', 'ifbInterrupt'), duck_gain: Number($('ifb-duck-gain').value) } : existing.ifb || defaultClient(id).ifb,
    lockout: lockoutBody(),
    stereo: hasStereo ? { enabled: $('stereo-enabled').checked, channel_pan: readPanMap() } : existing.stereo || defaultClient(id).stereo,
    processing: processingBody(existing.processing || defaultProcessing()),
    esp32_audio: esp32AudioBody(existing.esp32_audio || defaultEsp32Audio()),
  };
  try {
    await api(`/clients/${id}`, { method: 'PUT', body: JSON.stringify(body) });
    selectedUser = id;
    clientEditorDirty = false;
    await refresh();
  } catch (err) {
    showError(err);
  }
}
function showError(err) { window.alert(err.message || String(err)); }

document.addEventListener('keydown', (e) => { if (e.key === 'Escape' && selectedUser) closeClientEditor(); });
refresh().catch(showError);
refreshTimer = window.setInterval(() => {
  if (!modalRoot().innerHTML && !clientEditorDirty && !(page === 'clients' && selectedUser)) refresh().catch(console.warn);
}, 1000);
