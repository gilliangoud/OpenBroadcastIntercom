const clientApi = window.intercomClientApi;
const $ = id => document.getElementById(id);

let state = null;
let draft = null;
let dirty = false;
let heldButtons = new Set();
let expandedChannels = new Set();
let regularTalkDown = false;
let selectedChannelId = null;
let suppressNextChannelClick = false;

function message(text, kind = 'ok') {
  const el = $('message');
  if (!el) return;
  el.className = kind;
  el.textContent = text || '';
}

async function api(path, opts = {}) {
  if (!clientApi?.request) {
    throw new Error('Client controls API adapter is not loaded.');
  }
  return clientApi.request(path, opts);
}

function capability(name, fallback = true) {
  return clientApi?.capabilities?.[name] ?? fallback;
}

function setText(id, value) {
  const el = $(id);
  if (el) el.textContent = value ?? '';
}

function csv(values) {
  return (values || []).join(',') || '-';
}

function escapeHtml(value) {
  return String(value ?? '')
    .replaceAll('&', '&amp;')
    .replaceAll('<', '&lt;')
    .replaceAll('>', '&gt;')
    .replaceAll('"', '&quot;')
    .replaceAll("'", '&#39;');
}

function sortedNumbers(values) {
  return [...new Set((values || []).map(Number).filter(v => Number.isInteger(v) && v > 0))]
    .sort((a, b) => a - b);
}

function codecName(codec) {
  return {
    pcm16: 'PCM Low CPU',
    pcm24: 'PCM Balanced',
    pcm48: 'PCM High Quality',
    opus: 'Opus'
  }[codec] || codec || '-';
}

function normalizeOpusProfile(profile) {
  return {
    speech_low: 'speech_16_low',
    speech_standard: 'speech_24_standard',
    speech_high: 'speech_48_high',
    music_high: 'music_48'
  }[profile] || profile || 'speech_24_standard';
}

function backendName(value) {
  if (value === 'voice_processing') return 'voice processing';
  if (value === 'raw') return 'raw';
  if (value === 'auto') return 'auto';
  return value || '-';
}

function levelText(level) {
  const n = Number(level);
  if (!Number.isFinite(n) || n <= 0) return '-';
  return `${(20 * Math.log10(n)).toFixed(1)} dBFS`;
}

function locks() {
  return state?.lockout || {};
}

function allowed(key) {
  return locks()[key] !== false && locks().allow_local_api !== false;
}

function lockedLabels() {
  const l = locks();
  const labels = [];
  if (l.allow_local_api === false) labels.push('local controls');
  if (l.allow_channels === false) labels.push('channels');
  if (l.allow_volumes === false) labels.push('volumes');
  if (l.allow_codec === false) labels.push('codec');
  if (l.allow_talk_mode === false) labels.push('talk mode');
  if (l.allow_priority === false) labels.push('priority');
  if (l.allow_buttons === false) labels.push('buttons');
  if (l.allow_ifb === false) labels.push('IFB');
  return labels;
}

function setControl(id, enabled, title = 'Locked by admin') {
  const el = $(id);
  if (!el) return;
  el.disabled = !enabled;
  el.title = enabled ? '' : title;
}

function setControls(selector, enabled, title = 'Locked by admin') {
  document.querySelectorAll(selector).forEach(el => {
    el.disabled = !enabled;
    el.title = enabled ? '' : title;
  });
}

function regularTalkActive() {
  return state?.talk_mode === 'open' || (state?.talk_mode === 'ptt' && state?.regular_talk_active);
}

function transmitActions(button) {
  return (button.actions || []).filter(action => action.type === 'transmit');
}

function activeTalkChannels() {
  const channels = new Set();
  if (regularTalkActive()) {
    for (const ch of state.tx || []) channels.add(ch);
  }
  const active = new Set(state?.active_buttons || []);
  for (const button of state?.buttons || []) {
    if (!active.has(button.id)) continue;
    for (const action of transmitActions(button)) {
      for (const ch of action.channels || []) channels.add(ch);
    }
  }
  return channels;
}

function allRouteChannels() {
  const ids = new Set([...(state?.listen || []), ...(state?.tx || [])]);
  for (const button of state?.buttons || []) {
    for (const action of transmitActions(button)) {
      for (const ch of action.channels || []) ids.add(ch);
    }
  }
  const ifb = state?.ifb || {};
  for (const ch of ifb.program || []) ids.add(ch);
  for (const ch of ifb.interrupt || []) ids.add(ch);
  return [...ids].sort((a, b) => a - b);
}

function rosterForChannel(id) {
  return (state?.channel_rosters || []).find(roster => roster.channel_id === id);
}

function rosterNameForUser(id) {
  const userId = Number(id);
  if (userId === Number(state?.user_id) && state?.name) return state.name;
  for (const roster of state?.channel_rosters || []) {
    for (const member of roster.members || []) {
      if (Number(member.user_id) === userId && String(member.name || '').trim()) {
        return member.name;
      }
    }
  }
  return '';
}

function displayNameForUser(id, preferredName) {
  const name = String(preferredName || rosterNameForUser(id) || '').trim();
  return name || `Client ${id}`;
}

function directCallName(call, side) {
  return displayNameForUser(call[side], call[`${side}_name`]);
}

function directCallSummary() {
  return (state?.active_direct_calls || [])
    .filter(call => call.active)
    .map(call => {
      const caller = directCallName(call, 'caller');
      const target = directCallName(call, 'target');
      return `${caller} -> ${target}${call.duck ? ' duck' : ''}`;
    })
    .join(',') || '-';
}

function buttonActionSummary(button) {
  return (button.actions || [])
    .map(action => {
      if (action.type === 'transmit') return `TX ch ${csv(action.channels)} users ${csv(action.users)}`;
      if (action.type === 'alert') return `alert ${(action.targets || []).length}`;
      return action.type;
    })
    .join(' | ') || 'no actions';
}

function normalizeButtonColor(value) {
  const color = String(value || '').trim();
  return /^#([0-9a-fA-F]{3}|[0-9a-fA-F]{6})$/.test(color) ? color : '';
}

function readableTextColor(color) {
  let hex = color.slice(1);
  if (hex.length === 3) hex = hex.split('').map(ch => ch + ch).join('');
  const value = Number.parseInt(hex, 16);
  const r = (value >> 16) & 255;
  const g = (value >> 8) & 255;
  const b = value & 255;
  return (r * 299 + g * 587 + b * 114) / 1000 >= 145 ? '#17212b' : '#ffffff';
}

function applyButtonColor(button, config) {
  const color = normalizeButtonColor(config.color);
  if (!color) return;
  button.style.setProperty('--button-bg', color);
  button.style.setProperty('--button-border', color);
  button.style.setProperty('--button-ink', readableTextColor(color));
}

function syncDockPadding() {
  const shell = document.querySelector('.phone-shell');
  const dock = document.querySelector('.bottom-dock');
  if (!shell || !dock) return;
  shell.style.setProperty('--dock-height', `${Math.ceil(dock.getBoundingClientRect().height)}px`);
}

function cloneDraftFromState() {
  const ifb = state?.ifb || { enabled: false, program: [], interrupt: [], duck_gain: 0.125 };
  draft = {
    listen: sortedNumbers(state?.listen),
    tx: sortedNumbers(state?.tx),
    vol: { ...(state?.vol || {}) },
    talker_vol: { ...(state?.talker_vol || {}) },
    ifb: {
      enabled: !!ifb.enabled,
      program: sortedNumbers(ifb.program),
      interrupt: sortedNumbers(ifb.interrupt),
      duck_gain: ifb.duck_gain ?? 0.125
    }
  };
}

function ensureDraft() {
  if (!draft) cloneDraftFromState();
  return draft;
}

function markDirty() {
  dirty = true;
}

function addNumberTo(list, value) {
  const n = Number(value);
  if (Number.isInteger(n) && n > 0 && !list.includes(n)) list.push(n);
  list.sort((a, b) => a - b);
  markDirty();
}

function removeNumberFrom(list, value) {
  const n = Number(value);
  const index = list.indexOf(n);
  if (index >= 0) list.splice(index, 1);
  markDirty();
}

function channelName(id) {
  const roster = rosterForChannel(id);
  const name = String(roster?.name || '').trim();
  return name || `Channel ${id}`;
}

const channelIcons = {
  listen: '<svg aria-hidden="true" focusable="false" viewBox="0 0 24 24"><path d="M4 14v-2a8 8 0 0 1 16 0v2"/><path d="M4 14h2a2 2 0 0 1 2 2v3a1 1 0 0 1-1 1H6a2 2 0 0 1-2-2v-4Z"/><path d="M20 14h-2a2 2 0 0 0-2 2v3a1 1 0 0 0 1 1h1a2 2 0 0 0 2-2v-4Z"/></svg>',
  tx: '<svg aria-hidden="true" focusable="false" viewBox="0 0 24 24"><path d="M12 3a3 3 0 0 0-3 3v6a3 3 0 0 0 6 0V6a3 3 0 0 0-3-3Z"/><path d="M19 10v2a7 7 0 0 1-14 0v-2"/><path d="M12 19v3"/><path d="M8 22h8"/></svg>'
};

function channelIconTag(kind, label) {
  return `<span class="tag icon-tag ${kind}" title="${label}" aria-label="${label}">${channelIcons[kind] || ''}</span>`;
}

function allDraftChannels() {
  const d = ensureDraft();
  const ids = new Set([
    ...d.listen,
    ...d.tx,
    ...Object.keys(d.vol).map(Number),
    ...d.ifb.program,
    ...d.ifb.interrupt
  ]);
  for (const button of state?.buttons || []) {
    for (const action of transmitActions(button)) {
      for (const ch of action.channels || []) ids.add(ch);
    }
  }
  return sortedNumbers([...ids]);
}

async function refresh() {
  try {
    state = await api('/state');
    render();
  } catch (err) {
    setText('status-tag', 'error');
    $('status-tag')?.classList.add('error');
    message(String(err), 'error');
  }
}

function render() {
  if (!state) return;
  renderStatus();
  renderAlerts();
  renderCuePanel();
  renderChannels();
  if (!dirty) fillForms();
  renderCodecs();
  if (!heldButtons.size) renderButtons();
  renderIfb();
  renderTalkControls();
  renderCapabilityPanels();
  renderLockout();
  syncDockPadding();
}

function renderStatus() {
  const statusTag = $('status-tag');
  if (statusTag) {
    const muted = state.talk_mode === 'muted';
    const openMic = state.talk_mode === 'open';
    const talking = regularTalkActive();
    statusTag.textContent = muted ? 'Muted' : openMic ? 'Open mic' : talking ? 'Talking' : 'Ready';
    statusTag.className = `tag ${talking ? 'talk' : muted ? 'offline' : ''}`;
  }
  setText('client-title', state.name ? `${state.user_id} ${state.name}` : `Client ${state.user_id}`);
  setText(
    'connection-label',
    state.talk_mode === 'muted'
      ? 'Talk controls are muted'
      : state.talk_mode === 'open'
        ? 'Audio is transmitting'
        : regularTalkActive()
          ? `Transmitting on ${csv([...activeTalkChannels()])}`
          : 'Hold Talk to transmit'
  );
  setText('user', state.user_id);
  setText('name-label', state.name || '-');
  setText('listen-label', csv(state.listen));
  setText('tx-label', csv(state.tx));
  setText('playback-label', `${state.playback?.available_samples ?? 0}/${state.playback?.capacity_samples ?? 0}`);
  setText('playback-drops-label', `U ${state.playback?.underflows ?? 0} / O ${state.playback?.overflows ?? 0}`);
  const telemetry = state.telemetry || {};
  const runtime = telemetry.runtime || {};
  const audio = telemetry.audio || {};
  const playbackTelemetry = telemetry.playback || {};
  const transportTelemetry = telemetry.client_transport || {};
  setText('telemetry-runtime-label', telemetry.runtime ? `${runtime.client_kind || 'client'} ${runtime.phase || 'running'}` : '-');
  setText('telemetry-audio-label', telemetry.audio ? `${audio.backend || '-'} ${levelText(audio.input?.rms)}` : '-');
  setText('telemetry-transport-label', telemetry.client_transport ? `RX ${transportTelemetry.udp_rx_packets || 0} TX ${transportTelemetry.tx_packets || 0} drop ${(transportTelemetry.malformed_packets || 0) + (transportTelemetry.decode_errors || 0) + (transportTelemetry.tx_queue_drops || 0) + (transportTelemetry.tx_send_failures || 0)}` : `U/O ${playbackTelemetry.underflows || 0}/${playbackTelemetry.overflows || 0}`);
  setText('codec-label', codecName(state.codec));
  setText('supported-label', (state.supported_codecs || []).map(codecName).join(', ') || '-');
  setText('active-buttons-label', csv(state.active_buttons));
  setText('active-calls-label', directCallSummary());
  setText('last-caller-label', state.last_direct_caller || '-');
  setText('input-backend-label', `${backendName(state.active_input_backend)} (requested ${backendName(state.requested_input_backend)})`);
  setText('lockout-label', lockedLabels().join(', ') || 'none');
  setText('route-summary', `${(state.listen || []).length} listen / ${activeTalkChannels().size} talking`);
  setText('button-summary', `${(state.buttons || []).length} available`);

  const lockout = lockedLabels();
  const strip = $('lockout-strip');
  if (strip) strip.hidden = !lockout.length;
  setText('lockout-summary', lockout.join(', '));
}

function renderChannels() {
  const box = $('channel-list');
  if (!box) return;
  box.innerHTML = '';
  const talking = activeTalkChannels();
  const ids = allRouteChannels();
  if (!ids.length) {
    box.innerHTML = '<span class="muted">No channels configured.</span>';
    return;
  }
  for (const id of ids) {
    const listening = (state.listen || []).includes(id);
    const regularTx = (state.tx || []).includes(id);
    const activeTalk = talking.has(id);
    const roster = rosterForChannel(id);
    const item = document.createElement('div');
    const row = document.createElement('button');
    row.type = 'button';
    row.className = `channel-row ${expandedChannels.has(id) ? 'expanded' : ''}`;
    row.setAttribute('aria-expanded', expandedChannels.has(id) ? 'true' : 'false');
    row.innerHTML = `<div><div class="channel-name">${escapeHtml(channelName(id))}</div><div class="muted">${state.vol?.[id] && state.vol[id] !== 1 ? `gain ${state.vol[id]}` : 'normal gain'}${roster?.members?.length ? ` | ${roster.members.length} present` : ''}</div></div><div class="channel-tags">${listening ? channelIconTag('listen', 'Listening') : ''}${regularTx ? channelIconTag('tx', 'Regular TX') : ''}${activeTalk ? '<span class="tag talk">talking</span>' : ''}</div>`;
    row.title = 'Click to show roster. Long-press or right-click for channel settings.';
    row.onclick = () => {
      if (suppressNextChannelClick) {
        suppressNextChannelClick = false;
        return;
      }
      expandedChannels.has(id) ? expandedChannels.delete(id) : expandedChannels.add(id);
      renderChannels();
    };
    bindChannelSettingsGesture(row, id);
    item.appendChild(row);
    if (expandedChannels.has(id)) {
      const foldout = document.createElement('div');
      foldout.className = 'channel-foldout';
      const members = roster?.members || [];
      foldout.innerHTML = members.length
        ? members.map(member => `<div class="member-row"><span><span class="member-name">${member.name || 'Client'} ${member.user_id}</span> <span class="muted">${member.present ? 'present' : 'offline'}</span></span><span class="channel-tags">${member.transmitting ? '<span class="tag talk">transmitting</span>' : ''}${!member.present ? '<span class="tag offline">offline</span>' : ''}</span></div>`).join('')
        : '<span class="muted">No clients present.</span>';
      item.appendChild(foldout);
    }
    box.appendChild(item);
  }
}

function bindChannelSettingsGesture(row, id) {
  let timer = null;
  const clear = () => {
    if (timer) window.clearTimeout(timer);
    timer = null;
  };
  row.addEventListener('contextmenu', event => {
    event.preventDefault();
    clear();
    openChannelSettings(id);
  });
  row.addEventListener('pointerdown', event => {
    if (event.button && event.button !== 0) return;
    clear();
    timer = window.setTimeout(() => {
      suppressNextChannelClick = true;
      row.setPointerCapture?.(event.pointerId);
      openChannelSettings(id);
    }, 550);
  });
  row.addEventListener('pointerup', clear);
  row.addEventListener('pointercancel', clear);
  row.addEventListener('pointerleave', clear);
}

function openChannelSettings(id) {
  selectedChannelId = Number(id);
  ensureDraft();
  renderChannelSettings();
  openModal('channel-settings-modal');
}

function renderChannelSettings() {
  if (!selectedChannelId) return;
  const d = ensureDraft();
  setText('channel-settings-title', channelName(selectedChannelId));
  setText('channel-settings-legend', `Channel ${selectedChannelId}`);
  if ($('channel-listen-toggle')) $('channel-listen-toggle').checked = d.listen.includes(selectedChannelId);
  if ($('channel-tx-toggle')) $('channel-tx-toggle').checked = d.tx.includes(selectedChannelId);
  if ($('channel-gain-input')) $('channel-gain-input').value = d.vol[selectedChannelId] ?? 1;
  setControl('channel-listen-toggle', allowed('allow_channels'), 'Channels locked by admin');
  setControl('channel-tx-toggle', allowed('allow_channels'), 'Channels locked by admin');
  setControl('channel-gain-input', allowed('allow_volumes'), 'Volumes locked by admin');
  setControl('channel-settings-remove', allowed('allow_channels'), 'Channels locked by admin');
  setControl('channel-settings-save', locks().allow_local_api !== false, 'Local controls locked by admin');
}

async function saveChannelSettings() {
  if (!selectedChannelId) return;
  const d = ensureDraft();
  const ch = selectedChannelId;
  $('channel-listen-toggle')?.checked ? addNumberTo(d.listen, ch) : removeNumberFrom(d.listen, ch);
  $('channel-tx-toggle')?.checked ? addNumberTo(d.tx, ch) : removeNumberFrom(d.tx, ch);
  d.vol[ch] = Number($('channel-gain-input')?.value ?? d.vol[ch] ?? 1);
  await saveConfig();
  closeModal('channel-settings-modal');
}

async function removeSelectedChannel() {
  if (!selectedChannelId) return;
  const d = ensureDraft();
  const ch = selectedChannelId;
  removeNumberFrom(d.listen, ch);
  removeNumberFrom(d.tx, ch);
  delete d.vol[ch];
  removeNumberFrom(d.ifb.program, ch);
  removeNumberFrom(d.ifb.interrupt, ch);
  await saveConfig();
  closeModal('channel-settings-modal');
}

function renderAlerts() {
  const alerts = state.active_alerts || [];
  const emergency = state.emergency;
  const show = alerts.length || emergency?.active;
  const panel = $('alerts-panel');
  if (panel) panel.hidden = !show;
  setText('alerts-summary', emergency?.active ? 'emergency' : alerts.length ? `${alerts.length} active` : '');
  const box = $('alerts');
  if (!box) return;
  box.innerHTML = '';
  if (emergency?.active) {
    const row = document.createElement('div');
    row.className = 'alert-row';
    const source = escapeHtml(displayNameForUser(emergency.source, emergency.source_name));
    row.innerHTML = `<div><div class="channel-name">Emergency from ${source}</div><div class="muted">${emergency.mute_others ? 'Normal audio muted' : 'Normal audio ducked'} for ${csv(emergency.recipients)}.</div></div>`;
    box.appendChild(row);
  }
  for (const alert of alerts) {
    const row = document.createElement('div');
    row.className = 'alert-row';
    const sender = escapeHtml(displayNameForUser(alert.sender, alert.sender_name));
    const message = escapeHtml(alert.message || 'Call alert');
    row.innerHTML = `<div><div class="channel-name">Alert from ${sender}</div><div class="muted">${message}</div></div><button type="button" data-ack="${alert.id}">Ack</button>`;
    box.appendChild(row);
  }
  box.querySelectorAll('[data-ack]').forEach(button => {
    button.onclick = () => ackAlert(button.dataset.ack);
  });
}

async function ackAlert(id) {
  try {
    await api(`/alerts/${id}/ack`, { method: 'POST' });
    await refresh();
  } catch (err) {
    message(String(err), 'error');
  }
}

function activeIfbSources() {
  const ifb = state.ifb || {};
  const sources = [];
  for (const ch of ifb.interrupt || []) {
    const roster = rosterForChannel(ch);
    for (const member of roster?.members || []) {
      if (member.transmitting && member.user_id !== state.user_id) {
        sources.push(`${member.name || 'Client'} ${member.user_id} on ch ${ch}`);
      }
    }
  }
  return sources;
}

function renderCuePanel() {
  const ifb = state.ifb || {};
  const ifbSources = activeIfbSources();
  const activeCalls = (state.active_direct_calls || []).filter(call => call.active);
  const hasReply = !!state.last_direct_caller;
  const show = (ifb.enabled && ifbSources.length) || activeCalls.length || hasReply;
  const panel = $('cue-panel');
  if (panel) panel.hidden = !show;
  setText('cue-summary', ifbSources.length ? 'interrupt active' : activeCalls.length ? 'direct call' : hasReply ? 'reply available' : '');
  const box = $('cue-list');
  if (!box) return;
  box.innerHTML = '';
  for (const source of ifbSources) {
    const row = document.createElement('div');
    row.className = 'cue-row';
    row.innerHTML = `<div><div class="channel-name">Interrupt: ${source}</div><div class="muted">Program ${csv(ifb.program)} ducks to ${ifb.duck_gain ?? 0.125}.</div></div><span class="tag talk">IFB</span>`;
    box.appendChild(row);
  }
  for (const call of activeCalls) {
    const row = document.createElement('div');
    row.className = 'cue-row';
    const fromSelf = Number(call.caller) === Number(state.user_id);
    const otherName = escapeHtml(fromSelf ? directCallName(call, 'target') : directCallName(call, 'caller'));
    const title = fromSelf ? `Call to ${otherName}` : `Call from ${otherName}`;
    const replyButton = !fromSelf && Number(state.last_direct_caller) === Number(call.caller)
      ? '<button data-reply-direct-call type="button">Reply</button>'
      : '<span class="tag talk">call</span>';
    row.innerHTML = `<div><div class="channel-name">${title}</div><div class="muted">${call.duck ? 'Ducking other audio' : 'Normal mix'}.</div></div>${replyButton}`;
    box.appendChild(row);
  }
  if (hasReply && !activeCalls.some(call => Number(call.caller) === Number(state.last_direct_caller))) {
    const row = document.createElement('div');
    row.className = 'cue-row';
    const caller = escapeHtml(displayNameForUser(state.last_direct_caller));
    row.innerHTML = `<div><div class="channel-name">Reply to ${caller}</div><div class="muted">Tap to toggle a direct reply.</div></div><button data-reply-direct-call type="button">Reply</button>`;
    box.appendChild(row);
  }
  box.querySelectorAll('[data-reply-direct-call]').forEach(button => {
    button.onclick = () => api('/reply/toggle', { method: 'POST' }).then(refresh).catch(err => message(String(err), 'error'));
  });
}

function fillForms() {
  cloneDraftFromState();
  const codecInput = $('codec-input');
  const opusProfileInput = $('opus-profile-input');
  const talkModeInput = $('talk-mode-input');
  if (codecInput) codecInput.value = state.codec;
  if (opusProfileInput) opusProfileInput.value = normalizeOpusProfile(state.opus_profile);
  if (talkModeInput) talkModeInput.value = state.talk_mode || 'ptt';
  updateOpusProfileVisibility();
  if ($('priority-input')) $('priority-input').checked = !!state.priority;
  if ($('mic-gain-input')) $('mic-gain-input').value = state.mic_gain ?? 1;
  if ($('speaker-gain-input')) $('speaker-gain-input').value = state.speaker_gain ?? 1;
  if ($('ifb-enabled')) $('ifb-enabled').checked = !!draft.ifb.enabled;
  if ($('ifb-duck-gain')) $('ifb-duck-gain').value = draft.ifb.duck_gain;
  showGainValues();
  renderConfigEditor();
}

function showGainValues() {
  if ($('mic-gain-value') && $('mic-gain-input')) {
    $('mic-gain-value').textContent = Number($('mic-gain-input').value).toFixed(2);
  }
  if ($('speaker-gain-value') && $('speaker-gain-input')) {
    $('speaker-gain-value').textContent = Number($('speaker-gain-input').value).toFixed(2);
  }
}

function showIfbDuckGain() {
  if ($('ifb-duck-gain-value') && $('ifb-duck-gain')) {
    $('ifb-duck-gain-value').textContent = Number($('ifb-duck-gain').value).toFixed(2);
  }
}

function renderConfigEditor() {
  renderRouteEditor();
  renderTalkerGainEditor();
  renderIfbEditor();
  showIfbDuckGain();
}

function renderRouteEditor() {
  const d = ensureDraft();
  const box = $('route-editor');
  if (!box) return;
  const channels = allDraftChannels();
  if (!channels.length) {
    box.innerHTML = '<span class="muted">No routing channels configured.</span>';
    return;
  }
  box.innerHTML = '';
  for (const ch of channels) {
    const row = document.createElement('div');
    row.className = 'config-row';
    row.innerHTML = `<div class="config-row-head"><span class="config-title">Channel ${ch}</span><button data-route-remove="${ch}" type="button">Remove</button></div><div class="config-controls"><label class="pill-toggle"><input data-route-listen="${ch}" type="checkbox" ${d.listen.includes(ch) ? 'checked' : ''}> Listen</label><label class="pill-toggle"><input data-route-tx="${ch}" type="checkbox" ${d.tx.includes(ch) ? 'checked' : ''}> Regular TX</label><label>Receive Gain<input data-route-gain="${ch}" type="number" min="0" max="4" step="0.05" value="${d.vol[ch] ?? 1}"></label></div>`;
    box.appendChild(row);
  }
  box.querySelectorAll('[data-route-listen]').forEach(input => {
    input.onchange = () => {
      const ch = Number(input.dataset.routeListen);
      input.checked ? addNumberTo(d.listen, ch) : removeNumberFrom(d.listen, ch);
      renderRouteEditor();
    };
  });
  box.querySelectorAll('[data-route-tx]').forEach(input => {
    input.onchange = () => {
      const ch = Number(input.dataset.routeTx);
      input.checked ? addNumberTo(d.tx, ch) : removeNumberFrom(d.tx, ch);
      renderRouteEditor();
    };
  });
  box.querySelectorAll('[data-route-gain]').forEach(input => {
    input.oninput = () => {
      d.vol[input.dataset.routeGain] = Number(input.value);
      markDirty();
    };
  });
  box.querySelectorAll('[data-route-remove]').forEach(button => {
    button.onclick = () => {
      const ch = Number(button.dataset.routeRemove);
      removeNumberFrom(d.listen, ch);
      removeNumberFrom(d.tx, ch);
      delete d.vol[ch];
      removeNumberFrom(d.ifb.program, ch);
      removeNumberFrom(d.ifb.interrupt, ch);
      renderConfigEditor();
    };
  });
}

function renderTalkerGainEditor() {
  const d = ensureDraft();
  const box = $('talker-gain-editor');
  if (!box) return;
  const talkers = sortedNumbers(Object.keys(d.talker_vol));
  if (!talkers.length) {
    box.innerHTML = '<span class="muted">No per-talker gain overrides.</span>';
    return;
  }
  box.innerHTML = '';
  for (const id of talkers) {
    const row = document.createElement('div');
    row.className = 'config-row';
    row.innerHTML = `<div class="config-row-head"><span class="config-title">Talker ${id}</span><button data-talker-remove="${id}" type="button">Remove</button></div><label>Gain<input data-talker-gain="${id}" type="number" min="0" max="4" step="0.05" value="${d.talker_vol[id] ?? 1}"></label>`;
    box.appendChild(row);
  }
  box.querySelectorAll('[data-talker-gain]').forEach(input => {
    input.oninput = () => {
      d.talker_vol[input.dataset.talkerGain] = Number(input.value);
      markDirty();
    };
  });
  box.querySelectorAll('[data-talker-remove]').forEach(button => {
    button.onclick = () => {
      delete d.talker_vol[button.dataset.talkerRemove];
      markDirty();
      renderTalkerGainEditor();
    };
  });
}

function renderIfbEditor() {
  const d = ensureDraft();
  const box = $('ifb-editor');
  if (!box) return;
  const channels = sortedNumbers([...d.ifb.program, ...d.ifb.interrupt]);
  if (!channels.length) {
    box.innerHTML = '<span class="muted">No IFB program or interrupt channels.</span>';
    return;
  }
  box.innerHTML = '';
  for (const ch of channels) {
    const row = document.createElement('div');
    row.className = 'config-row';
    row.innerHTML = `<div class="config-row-head"><span class="config-title">Channel ${ch}</span><button data-ifb-remove="${ch}" type="button">Remove</button></div><div class="config-controls"><label class="pill-toggle"><input data-ifb-program="${ch}" type="checkbox" ${d.ifb.program.includes(ch) ? 'checked' : ''}> Program</label><label class="pill-toggle"><input data-ifb-interrupt="${ch}" type="checkbox" ${d.ifb.interrupt.includes(ch) ? 'checked' : ''}> Interrupt</label></div>`;
    box.appendChild(row);
  }
  box.querySelectorAll('[data-ifb-program]').forEach(input => {
    input.onchange = () => {
      const ch = Number(input.dataset.ifbProgram);
      input.checked ? addNumberTo(d.ifb.program, ch) : removeNumberFrom(d.ifb.program, ch);
      renderIfbEditor();
    };
  });
  box.querySelectorAll('[data-ifb-interrupt]').forEach(input => {
    input.onchange = () => {
      const ch = Number(input.dataset.ifbInterrupt);
      input.checked ? addNumberTo(d.ifb.interrupt, ch) : removeNumberFrom(d.ifb.interrupt, ch);
      renderIfbEditor();
    };
  });
  box.querySelectorAll('[data-ifb-remove]').forEach(button => {
    button.onclick = () => {
      const ch = Number(button.dataset.ifbRemove);
      removeNumberFrom(d.ifb.program, ch);
      removeNumberFrom(d.ifb.interrupt, ch);
      renderIfbEditor();
    };
  });
}

function configBody() {
  const d = ensureDraft();
  return {
    listen: sortedNumbers(d.listen),
    tx: sortedNumbers(d.tx),
    vol: d.vol,
    talker_vol: d.talker_vol,
    codec: $('codec-input')?.value || state.codec,
    opus_profile: $('opus-profile-input')?.value || normalizeOpusProfile(state.opus_profile),
    talk_mode: $('talk-mode-input')?.value || state.talk_mode,
    priority: !!$('priority-input')?.checked,
    priority_channels: state.priority_channels || [],
    ifb: {
      enabled: !!$('ifb-enabled')?.checked,
      program: sortedNumbers(d.ifb.program),
      interrupt: sortedNumbers(d.ifb.interrupt),
      duck_gain: Number($('ifb-duck-gain')?.value ?? d.ifb.duck_gain)
    }
  };
}

async function saveConfig() {
  try {
    await api('/config', { method: 'PUT', body: JSON.stringify(configBody()) });
    dirty = false;
    message('Config submitted to server');
    await refresh();
  } catch (err) {
    message(String(err), 'error');
  }
}

async function saveGain() {
  try {
    await api('/gain', {
      method: 'POST',
      body: JSON.stringify({
        mic_gain: Number($('mic-gain-input')?.value ?? 1),
        speaker_gain: Number($('speaker-gain-input')?.value ?? 1)
      })
    });
    message('Gain updated');
    await refresh();
  } catch (err) {
    message(String(err), 'error');
  }
}

function renderCodecs() {
  const box = $('codecs');
  if (!box) return;
  box.innerHTML = '';
  for (const codec of state.supported_codecs || []) {
    const button = document.createElement('button');
    button.type = 'button';
    button.textContent = codecName(codec);
    button.className = codec === state.codec ? 'active' : '';
    button.disabled = !allowed('allow_codec');
    button.onclick = () => api('/codec', { method: 'POST', body: JSON.stringify({ codec }) }).then(refresh).catch(err => message(String(err), 'error'));
    box.appendChild(button);
  }
}

function renderButtons() {
  const box = $('buttons');
  const wrap = $('bottom-special');
  if (!box || !wrap) return;
  box.innerHTML = '';
  const buttons = state.buttons || [];
  box.dataset.count = String(buttons.length);
  wrap.hidden = !buttons.length;
  for (const b of buttons) {
    const active = (state.active_buttons || []).includes(b.id);
    const mode = b.mode || 'momentary';
    const card = document.createElement('div');
    card.className = 'button-card';
    const button = document.createElement('button');
    button.type = 'button';
    button.className = `special-talk ${active ? 'active' : ''}`;
    button.textContent = b.label || b.id;
    button.title = mode === 'latching' ? 'Tap to toggle this action.' : 'Hold to run this action; release to stop transmit actions.';
    applyButtonColor(button, b);
    button.disabled = !allowed('allow_buttons');
    const meta = document.createElement('div');
    meta.className = 'button-meta';
    meta.textContent = `${mode} | ${buttonActionSummary(b)}`;
    card.appendChild(button);
    card.appendChild(meta);
    if (mode === 'latching') {
      button.onclick = () => api(`/buttons/${encodeURIComponent(b.id)}/toggle`, { method: 'POST' }).then(refresh).catch(err => message(String(err), 'error'));
    } else {
      let down = false;
      const press = async () => {
        if (down || button.disabled) return;
        down = true;
        heldButtons.add(b.id);
        button.classList.add('active');
        try {
          await api(`/buttons/${encodeURIComponent(b.id)}/down`, { method: 'POST' });
        } catch (err) {
          down = false;
          heldButtons.delete(b.id);
          button.classList.remove('active');
          message(String(err), 'error');
        }
      };
      const release = async () => {
        if (!down) return;
        down = false;
        heldButtons.delete(b.id);
        button.classList.remove('active');
        try {
          await api(`/buttons/${encodeURIComponent(b.id)}/up`, { method: 'POST' });
          await refresh();
        } catch (err) {
          message(String(err), 'error');
        }
      };
      button.onpointerdown = event => {
        event.preventDefault();
        button.setPointerCapture?.(event.pointerId);
        press();
      };
      button.onpointerup = event => {
        event.preventDefault();
        release();
      };
      button.onpointercancel = release;
      button.onlostpointercapture = release;
    }
    box.appendChild(card);
  }
}

function renderIfb() {
  const ifb = state.ifb || { enabled: false, program: [], interrupt: [], duck_gain: 0.125 };
  const el = $('ifb');
  if (!el) return;
  el.innerHTML = ifb.enabled
    ? `Program ${csv(ifb.program)} ducks to ${ifb.duck_gain} while interrupt ${csv(ifb.interrupt)} is active`
    : '<span class="muted">Disabled</span>';
}

function renderTalkControls() {
  const muted = state.talk_mode === 'muted';
  if ($('mute')) {
    $('mute').textContent = muted ? 'Unmute' : 'Mute';
    $('mute').classList.toggle('active', muted);
  }
  if ($('talk')) {
    $('talk').hidden = muted;
    $('talk').textContent = state.talk_mode === 'open' ? 'Open Mic' : 'Talk';
    $('talk').classList.toggle('active', regularTalkActive());
  }
  setControl('mute', allowed('allow_talk_mode'), 'Talk mode locked by admin');
  setControl('talk', locks().allow_local_api !== false, 'Local controls locked by admin');
}

function renderCapabilityPanels() {
  const gainSupported = capability('gain', true) && (state.mic_gain !== undefined || state.speaker_gain !== undefined);
  document.querySelectorAll('[data-requires-gain]').forEach(el => {
    el.hidden = !gainSupported;
  });
  const macosSupported = capability('macosMicrophoneModes', false) && state.macos_microphone_mode !== undefined;
  if ($('macos-mic-mode-row')) $('macos-mic-mode-row').hidden = !macosSupported;
  setText('macos-mic-mode-label', macosSupported ? `Current mode: ${state.macos_microphone_mode || 'standard'}` : '');
  if ($('setup-open')) $('setup-open').hidden = !mobileShell();
}

function renderLockout() {
  setControls('#route-editor input,#route-editor button,#route-channel-input,#route-add-listen,#route-add-tx', allowed('allow_channels'), 'Channels locked by admin');
  setControls('#route-editor [data-route-gain],#talker-gain-editor input,#talker-gain-editor button,#talker-id-input,#talker-gain-input,#talker-add', allowed('allow_volumes'), 'Volumes locked by admin');
  setControl('codec-input', allowed('allow_codec'));
  setControl('opus-profile-input', allowed('allow_codec'));
  setControl('talk-mode-input', allowed('allow_talk_mode'));
  setControl('priority-input', allowed('allow_priority'));
  setControls('#ifb-enabled,#ifb-channel-input,#ifb-add-program,#ifb-add-interrupt,#ifb-duck-gain,#ifb-editor input,#ifb-editor button', allowed('allow_ifb'), 'IFB locked by admin');
  setControl('save-config', locks().allow_local_api !== false);
}

function openModal(id) {
  const el = $(id);
  if (el) el.hidden = false;
}

function closeModal(id) {
  const el = $(id);
  if (el) el.hidden = true;
}

function updateOpusProfileVisibility() {
  const field = $('opus-profile-field');
  const input = $('codec-input');
  if (field && input) field.hidden = input.value !== 'opus';
}

function mobileShell() {
  return capability('setup', false) || sessionStorage.getItem('intercom-mobile-shell') === '1';
}

function openSetup() {
  if (clientApi?.setup) {
    clientApi.setup();
  } else if (mobileShell() && history.length > 1) {
    history.back();
  }
}

function bindEvents() {
  $('stats-open')?.addEventListener('click', () => openModal('stats-modal'));
  $('stats-close')?.addEventListener('click', () => closeModal('stats-modal'));
  $('settings-open')?.addEventListener('click', () => openModal('settings-modal'));
  $('settings-close')?.addEventListener('click', () => closeModal('settings-modal'));
  $('channel-settings-close')?.addEventListener('click', () => closeModal('channel-settings-modal'));
  $('channel-settings-save')?.addEventListener('click', event => {
    event.preventDefault();
    saveChannelSettings().catch(err => message(String(err), 'error'));
  });
  $('channel-settings-remove')?.addEventListener('click', event => {
    event.preventDefault();
    removeSelectedChannel().catch(err => message(String(err), 'error'));
  });
  $('setup-open')?.addEventListener('click', openSetup);
  document.querySelectorAll('#settings-form input,#settings-form select').forEach(el => {
    el.addEventListener('input', () => {
      dirty = true;
    });
  });
  document.querySelectorAll('#mic-gain-input,#speaker-gain-input').forEach(el => el.addEventListener('input', showGainValues));
  $('codec-input')?.addEventListener('change', updateOpusProfileVisibility);
  $('ifb-duck-gain')?.addEventListener('input', () => {
    ensureDraft().ifb.duck_gain = Number($('ifb-duck-gain').value);
    showIfbDuckGain();
    markDirty();
  });
  $('route-add-listen')?.addEventListener('click', event => {
    event.preventDefault();
    addNumberTo(ensureDraft().listen, $('route-channel-input').value);
    $('route-channel-input').value = '';
    renderConfigEditor();
  });
  $('route-add-tx')?.addEventListener('click', event => {
    event.preventDefault();
    addNumberTo(ensureDraft().tx, $('route-channel-input').value);
    $('route-channel-input').value = '';
    renderConfigEditor();
  });
  $('talker-add')?.addEventListener('click', event => {
    event.preventDefault();
    const id = Number($('talker-id-input').value);
    if (Number.isInteger(id) && id > 0) {
      ensureDraft().talker_vol[id] = Number($('talker-gain-input').value);
      $('talker-id-input').value = '';
      $('talker-gain-input').value = '1';
      markDirty();
      renderTalkerGainEditor();
    }
  });
  $('ifb-add-program')?.addEventListener('click', event => {
    event.preventDefault();
    addNumberTo(ensureDraft().ifb.program, $('ifb-channel-input').value);
    $('ifb-channel-input').value = '';
    renderIfbEditor();
  });
  $('ifb-add-interrupt')?.addEventListener('click', event => {
    event.preventDefault();
    addNumberTo(ensureDraft().ifb.interrupt, $('ifb-channel-input').value);
    $('ifb-channel-input').value = '';
    renderIfbEditor();
  });
  $('ifb-enabled')?.addEventListener('change', () => {
    ensureDraft().ifb.enabled = $('ifb-enabled').checked;
    markDirty();
  });
  $('save-config')?.addEventListener('click', event => {
    event.preventDefault();
    saveConfig();
  });
  $('save-gain')?.addEventListener('click', event => {
    event.preventDefault();
    saveGain();
  });
  $('macos-mic-mode-open')?.addEventListener('click', event => {
    event.preventDefault();
    api('/macos/microphone-modes', { method: 'POST' }).catch(err => message(String(err), 'error'));
  });
  $('mute')?.addEventListener('click', () => {
    api(state?.talk_mode === 'muted' ? '/unmute' : '/mute', { method: 'POST' }).then(refresh).catch(err => message(String(err), 'error'));
  });
  const talk = $('talk');
  if (talk) {
    talk.onpointerdown = event => {
      event.preventDefault();
      talk.setPointerCapture?.(event.pointerId);
      regularTalkPress();
    };
    talk.onpointerup = event => {
      event.preventDefault();
      regularTalkRelease();
    };
    talk.onpointercancel = regularTalkRelease;
    talk.onlostpointercapture = regularTalkRelease;
  }
  window.addEventListener('resize', syncDockPadding);
  if (window.ResizeObserver) {
    new ResizeObserver(syncDockPadding).observe(document.querySelector('.bottom-dock'));
  }
}

async function regularTalkPress() {
  if (regularTalkDown || state?.talk_mode !== 'ptt') return;
  regularTalkDown = true;
  $('talk')?.classList.add('active');
  try {
    await api('/talk/down', { method: 'POST' });
  } catch (err) {
    regularTalkDown = false;
    $('talk')?.classList.remove('active');
    message(String(err), 'error');
  }
}

async function regularTalkRelease() {
  if (!regularTalkDown) return;
  regularTalkDown = false;
  $('talk')?.classList.remove('active');
  try {
    await api('/talk/up', { method: 'POST' });
    await refresh();
  } catch (err) {
    message(String(err), 'error');
  }
}

bindEvents();
refresh();
setInterval(refresh, 1000);
