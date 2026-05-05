const invoke = window.__TAURI__?.core?.invoke;
const $ = id => document.getElementById(id);

let state = null;
let draft = null;
let dirty = false;
let heldButtons = new Set();
let expandedChannels = new Set();

function message(text, kind = 'ok') {
  $('message').className = kind;
  $('message').textContent = text || '';
}

async function api(path, opts = {}) {
  if (!invoke) throw new Error('Client controls require the Tauri app runtime.');
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
  if (method === 'POST' && match) return invoke(`client_button_${match[2]}`, { id: decodeURIComponent(match[1]) });
  match = path.match(/^\/calls\/(\d+)\/(down|up|toggle)$/);
  if (method === 'POST' && match) return invoke(`client_call_${match[2]}`, { id: Number(match[1]) });
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

function csv(values) { return (values || []).join(',') || '-'; }
function sortedNumbers(values) { return [...new Set((values || []).map(Number).filter(v => Number.isInteger(v) && v > 0))].sort((a, b) => a - b); }
function codecName(codec) { return { pcm16: 'PCM Low CPU', pcm24: 'PCM Balanced', pcm48: 'PCM High Quality', opus: 'Opus' }[codec] || codec || '-'; }
function normalizeOpusProfile(profile) { return { speech_low: 'speech_16_low', speech_standard: 'speech_24_standard', speech_high: 'speech_48_high', music_high: 'music_48' }[profile] || profile || 'speech_24_standard'; }
function backendName(value) { return value === 'voice_processing' ? 'voice processing' : value === 'raw' ? 'raw' : value === 'auto' ? 'auto' : value || '-'; }
function locks() { return state?.lockout || {}; }
function allowed(key) { return locks()[key] !== false && locks().allow_local_api !== false; }
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
function regularTalkActive() { return state?.talk_mode === 'open' || (state?.talk_mode === 'ptt' && state?.regular_talk_active); }
function transmitActions(button) { return (button.actions || []).filter(a => a.type === 'transmit'); }
function activeTalkChannels() {
  const channels = new Set();
  if (regularTalkActive()) for (const ch of state.tx || []) channels.add(ch);
  const active = new Set(state?.active_buttons || []);
  for (const button of state?.buttons || []) if (active.has(button.id)) for (const action of transmitActions(button)) for (const ch of action.channels || []) channels.add(ch);
  return channels;
}
function allRouteChannels() {
  const ids = new Set([...(state?.listen || []), ...(state?.tx || [])]);
  for (const button of state?.buttons || []) for (const action of transmitActions(button)) for (const ch of action.channels || []) ids.add(ch);
  const ifb = state?.ifb || {};
  for (const ch of ifb.program || []) ids.add(ch);
  for (const ch of ifb.interrupt || []) ids.add(ch);
  return [...ids].sort((a, b) => a - b);
}
function rosterForChannel(id) { return (state?.channel_rosters || []).find(r => r.channel_id === id); }
function directCallSummary() { return (state?.active_direct_calls || []).filter(c => c.active).map(c => `${c.caller}->${c.target}${c.duck ? ' duck' : ''}`).join(',') || '-'; }
function buttonActionSummary(button) {
  return (button.actions || []).map(a => a.type === 'transmit' ? `TX ch ${csv(a.channels)} users ${csv(a.users)}` : a.type === 'alert' ? `alert ${(a.targets || []).length}` : a.type).join(' | ') || 'no actions';
}

function cloneDraftFromState() {
  const ifb = state?.ifb || { enabled: false, program: [], interrupt: [], duck_gain: 0.125 };
  draft = {
    listen: sortedNumbers(state?.listen),
    tx: sortedNumbers(state?.tx),
    vol: { ...(state?.vol || {}) },
    talker_vol: { ...(state?.talker_vol || {}) },
    ifb: { enabled: !!ifb.enabled, program: sortedNumbers(ifb.program), interrupt: sortedNumbers(ifb.interrupt), duck_gain: ifb.duck_gain ?? 0.125 }
  };
}
function ensureDraft() { if (!draft) cloneDraftFromState(); return draft; }
function markDirty() { dirty = true; }
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
function allDraftChannels() {
  const d = ensureDraft();
  const ids = new Set([...d.listen, ...d.tx, ...Object.keys(d.vol).map(Number), ...d.ifb.program, ...d.ifb.interrupt]);
  for (const button of state?.buttons || []) for (const action of transmitActions(button)) for (const ch of action.channels || []) ids.add(ch);
  return sortedNumbers([...ids]);
}

async function refresh() {
  try {
    state = await api('/state');
    render();
  } catch (err) {
    $('status-tag').textContent = 'error';
    $('status-tag').className = 'tag error';
    message(String(err), 'error');
  }
}

function render() {
  if (!state) return;
  $('status-tag').textContent = state.talk_mode || 'running';
  $('status-tag').className = `tag ${regularTalkActive() ? 'talk' : state.talk_mode === 'muted' ? 'offline' : ''}`;
  $('client-title').textContent = state.name ? `${state.user_id} ${state.name}` : `Client ${state.user_id}`;
  $('user').textContent = state.user_id;
  $('name-label').textContent = state.name || '-';
  $('listen-label').textContent = csv(state.listen);
  $('tx-label').textContent = csv(state.tx);
  $('playback-label').textContent = `${state.playback?.available_samples ?? 0}/${state.playback?.capacity_samples ?? 0}`;
  $('playback-drops-label').textContent = `U ${state.playback?.underflows ?? 0} / O ${state.playback?.overflows ?? 0}`;
  $('codec-label').textContent = codecName(state.codec);
  $('active-buttons-label').textContent = csv(state.active_buttons);
  $('active-calls-label').textContent = directCallSummary();
  $('last-caller-label').textContent = state.last_direct_caller || '-';
  $('input-backend-label').textContent = `${backendName(state.active_input_backend)} (requested ${backendName(state.requested_input_backend)})`;
  $('lockout-label').textContent = lockedLabels().join(', ') || 'none';
  $('route-summary').textContent = `${(state.listen || []).length} listen / ${activeTalkChannels().size} talking`;
  renderAlerts();
  renderCuePanel();
  renderChannels();
  if (!dirty) fillForms();
  renderCodecs();
  if (!heldButtons.size) renderButtons();
  renderIfb();
  renderTalkControls();
  renderLockout();
}

function renderChannels() {
  const box = $('channel-list');
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
    row.innerHTML = `<div><div class="channel-name">Channel ${id}</div><div class="muted">${state.vol?.[id] && state.vol[id] !== 1 ? `gain ${state.vol[id]}` : 'normal gain'}${roster?.members?.length ? ` | ${roster.members.length} present` : ''}</div></div><div class="channel-tags">${listening ? '<span class="tag listen">listening</span>' : ''}${regularTx ? '<span class="tag">regular tx</span>' : ''}${activeTalk ? '<span class="tag talk">talking</span>' : ''}</div>`;
    row.onclick = () => { expandedChannels.has(id) ? expandedChannels.delete(id) : expandedChannels.add(id); renderChannels(); };
    item.appendChild(row);
    if (expandedChannels.has(id)) {
      const foldout = document.createElement('div');
      foldout.className = 'channel-foldout';
      const members = roster?.members || [];
      foldout.innerHTML = members.length ? members.map(m => `<div class="member-row"><span><span class="member-name">${m.name || 'Client'} ${m.user_id}</span> <span class="muted">${m.present ? 'present' : 'offline'}</span></span><span class="channel-tags">${m.transmitting ? '<span class="tag talk">transmitting</span>' : ''}${!m.present ? '<span class="tag offline">offline</span>' : ''}</span></div>`).join('') : '<span class="muted">No clients present.</span>';
      item.appendChild(foldout);
    }
    box.appendChild(item);
  }
}

function renderAlerts() {
  const alerts = state.active_alerts || [];
  const emergency = state.emergency;
  const show = alerts.length || emergency?.active;
  $('alerts-panel').hidden = !show;
  $('alerts-summary').textContent = emergency?.active ? 'emergency' : alerts.length ? `${alerts.length} active` : '';
  const box = $('alerts');
  box.innerHTML = '';
  if (emergency?.active) {
    const row = document.createElement('div');
    row.className = 'alert-row';
    row.innerHTML = `<div><div class="channel-name">Emergency from ${emergency.source}</div><div class="muted">${emergency.mute_others ? 'Normal audio muted' : 'Normal audio ducked'} for ${csv(emergency.recipients)}.</div></div>`;
    box.appendChild(row);
  }
  for (const alert of alerts) {
    const row = document.createElement('div');
    row.className = 'alert-row';
    row.innerHTML = `<div><div class="channel-name">Alert from ${alert.sender}</div><div class="muted">${alert.message || 'Call alert'}</div></div><button type="button" data-ack="${alert.id}">Ack</button>`;
    box.appendChild(row);
  }
  box.querySelectorAll('[data-ack]').forEach(button => button.onclick = () => api(`/alerts/${button.dataset.ack}/ack`, { method: 'POST' }).then(refresh).catch(err => message(String(err), 'error')));
}

function activeIfbSources() {
  const ifb = state.ifb || {};
  const sources = [];
  for (const ch of ifb.interrupt || []) {
    const roster = rosterForChannel(ch);
    for (const member of roster?.members || []) if (member.transmitting && member.user_id !== state.user_id) sources.push(`${member.name || 'Client'} ${member.user_id} on ch ${ch}`);
  }
  return sources;
}
function renderCuePanel() {
  const ifb = state.ifb || {};
  const ifbSources = activeIfbSources();
  const activeCalls = (state.active_direct_calls || []).filter(call => call.active);
  const hasReply = !!state.last_direct_caller;
  const show = (ifb.enabled && ifbSources.length) || activeCalls.length || hasReply;
  $('cue-panel').hidden = !show;
  $('cue-summary').textContent = ifbSources.length ? 'interrupt active' : activeCalls.length ? 'direct call' : hasReply ? 'reply available' : '';
  const box = $('cue-list');
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
    row.innerHTML = `<div><div class="channel-name">Direct call ${call.caller} -> ${call.target}</div><div class="muted">${call.duck ? 'Ducking other audio' : 'Normal mix'}.</div></div><span class="tag talk">call</span>`;
    box.appendChild(row);
  }
}

function fillForms() {
  cloneDraftFromState();
  $('codec-input').value = state.codec;
  $('opus-profile-input').value = normalizeOpusProfile(state.opus_profile);
  $('opus-profile-field').hidden = $('codec-input').value !== 'opus';
  $('talk-mode-input').value = state.talk_mode || 'ptt';
  $('priority-input').checked = !!state.priority;
  $('mic-gain-input').value = state.mic_gain ?? 1;
  $('speaker-gain-input').value = state.speaker_gain ?? 1;
  $('ifb-enabled').checked = !!draft.ifb.enabled;
  $('ifb-duck-gain').value = draft.ifb.duck_gain;
  showGainValues();
  renderConfigEditor();
}
function showGainValues() {
  $('mic-gain-value').textContent = Number($('mic-gain-input').value).toFixed(2);
  $('speaker-gain-value').textContent = Number($('speaker-gain-input').value).toFixed(2);
}
function showIfbDuckGain() { $('ifb-duck-gain-value').textContent = Number($('ifb-duck-gain').value).toFixed(2); }
function renderConfigEditor() { renderRouteEditor(); renderTalkerGainEditor(); renderIfbEditor(); showIfbDuckGain(); }
function renderRouteEditor() {
  const d = ensureDraft();
  const box = $('route-editor');
  const channels = allDraftChannels();
  if (!channels.length) { box.innerHTML = '<span class="muted">No routing channels configured.</span>'; return; }
  box.innerHTML = '';
  for (const ch of channels) {
    const row = document.createElement('div');
    row.className = 'config-row';
    row.innerHTML = `<div class="config-row-head"><span class="config-title">Channel ${ch}</span><button data-route-remove="${ch}" type="button">Remove</button></div><div class="config-controls"><label class="pill-toggle"><input data-route-listen="${ch}" type="checkbox" ${d.listen.includes(ch) ? 'checked' : ''}> Listen</label><label class="pill-toggle"><input data-route-tx="${ch}" type="checkbox" ${d.tx.includes(ch) ? 'checked' : ''}> Regular TX</label><label>Receive Gain<input data-route-gain="${ch}" type="number" min="0" max="4" step="0.05" value="${d.vol[ch] ?? 1}"></label></div>`;
    box.appendChild(row);
  }
  box.querySelectorAll('[data-route-listen]').forEach(input => input.onchange = () => { const ch = Number(input.dataset.routeListen); input.checked ? addNumberTo(d.listen, ch) : removeNumberFrom(d.listen, ch); renderRouteEditor(); });
  box.querySelectorAll('[data-route-tx]').forEach(input => input.onchange = () => { const ch = Number(input.dataset.routeTx); input.checked ? addNumberTo(d.tx, ch) : removeNumberFrom(d.tx, ch); renderRouteEditor(); });
  box.querySelectorAll('[data-route-gain]').forEach(input => input.oninput = () => { d.vol[input.dataset.routeGain] = Number(input.value); markDirty(); });
  box.querySelectorAll('[data-route-remove]').forEach(button => button.onclick = () => { const ch = Number(button.dataset.routeRemove); removeNumberFrom(d.listen, ch); removeNumberFrom(d.tx, ch); delete d.vol[ch]; removeNumberFrom(d.ifb.program, ch); removeNumberFrom(d.ifb.interrupt, ch); renderConfigEditor(); });
}
function renderTalkerGainEditor() {
  const d = ensureDraft();
  const box = $('talker-gain-editor');
  const talkers = sortedNumbers(Object.keys(d.talker_vol));
  if (!talkers.length) { box.innerHTML = '<span class="muted">No per-talker gain overrides.</span>'; return; }
  box.innerHTML = '';
  for (const id of talkers) {
    const row = document.createElement('div');
    row.className = 'config-row';
    row.innerHTML = `<div class="config-row-head"><span class="config-title">Talker ${id}</span><button data-talker-remove="${id}" type="button">Remove</button></div><label>Gain<input data-talker-gain="${id}" type="number" min="0" max="4" step="0.05" value="${d.talker_vol[id] ?? 1}"></label>`;
    box.appendChild(row);
  }
  box.querySelectorAll('[data-talker-gain]').forEach(input => input.oninput = () => { d.talker_vol[input.dataset.talkerGain] = Number(input.value); markDirty(); });
  box.querySelectorAll('[data-talker-remove]').forEach(button => button.onclick = () => { delete d.talker_vol[button.dataset.talkerRemove]; markDirty(); renderTalkerGainEditor(); });
}
function renderIfbEditor() {
  const d = ensureDraft();
  const box = $('ifb-editor');
  const channels = sortedNumbers([...d.ifb.program, ...d.ifb.interrupt]);
  if (!channels.length) { box.innerHTML = '<span class="muted">No IFB program or interrupt channels.</span>'; return; }
  box.innerHTML = '';
  for (const ch of channels) {
    const row = document.createElement('div');
    row.className = 'config-row';
    row.innerHTML = `<div class="config-row-head"><span class="config-title">Channel ${ch}</span><button data-ifb-remove="${ch}" type="button">Remove</button></div><div class="config-controls"><label class="pill-toggle"><input data-ifb-program="${ch}" type="checkbox" ${d.ifb.program.includes(ch) ? 'checked' : ''}> Program</label><label class="pill-toggle"><input data-ifb-interrupt="${ch}" type="checkbox" ${d.ifb.interrupt.includes(ch) ? 'checked' : ''}> Interrupt</label></div>`;
    box.appendChild(row);
  }
  box.querySelectorAll('[data-ifb-program]').forEach(input => input.onchange = () => { const ch = Number(input.dataset.ifbProgram); input.checked ? addNumberTo(d.ifb.program, ch) : removeNumberFrom(d.ifb.program, ch); renderIfbEditor(); });
  box.querySelectorAll('[data-ifb-interrupt]').forEach(input => input.onchange = () => { const ch = Number(input.dataset.ifbInterrupt); input.checked ? addNumberTo(d.ifb.interrupt, ch) : removeNumberFrom(d.ifb.interrupt, ch); renderIfbEditor(); });
  box.querySelectorAll('[data-ifb-remove]').forEach(button => button.onclick = () => { const ch = Number(button.dataset.ifbRemove); removeNumberFrom(d.ifb.program, ch); removeNumberFrom(d.ifb.interrupt, ch); renderIfbEditor(); });
}
function configBody() {
  const d = ensureDraft();
  return { listen: sortedNumbers(d.listen), tx: sortedNumbers(d.tx), vol: d.vol, talker_vol: d.talker_vol, codec: $('codec-input').value, opus_profile: $('opus-profile-input').value, talk_mode: $('talk-mode-input').value, priority: $('priority-input').checked, priority_channels: state.priority_channels || [], ifb: { enabled: $('ifb-enabled').checked, program: sortedNumbers(d.ifb.program), interrupt: sortedNumbers(d.ifb.interrupt), duck_gain: Number($('ifb-duck-gain').value) } };
}
async function saveConfig() { try { await api('/config', { method: 'PUT', body: JSON.stringify(configBody()) }); dirty = false; message('Config submitted to server'); await refresh(); } catch (err) { message(String(err), 'error'); } }
async function saveGain() { try { await api('/gain', { method: 'POST', body: JSON.stringify({ mic_gain: Number($('mic-gain-input').value), speaker_gain: Number($('speaker-gain-input').value) }) }); message('Gain updated'); await refresh(); } catch (err) { message(String(err), 'error'); } }
function renderCodecs() {
  const box = $('codecs');
  box.innerHTML = '';
  for (const codec of state.supported_codecs || []) {
    const button = document.createElement('button');
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
  box.innerHTML = '';
  const buttons = state.buttons || [];
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
      const press = async () => { if (down || button.disabled) return; down = true; heldButtons.add(b.id); button.classList.add('active'); try { await api(`/buttons/${encodeURIComponent(b.id)}/down`, { method: 'POST' }); } catch (err) { down = false; heldButtons.delete(b.id); button.classList.remove('active'); message(String(err), 'error'); } };
      const release = async () => { if (!down) return; down = false; heldButtons.delete(b.id); button.classList.remove('active'); try { await api(`/buttons/${encodeURIComponent(b.id)}/up`, { method: 'POST' }); await refresh(); } catch (err) { message(String(err), 'error'); } };
      button.onpointerdown = e => { e.preventDefault(); button.setPointerCapture?.(e.pointerId); press(); };
      button.onpointerup = e => { e.preventDefault(); release(); };
      button.onpointercancel = release;
      button.onlostpointercapture = release;
    }
    box.appendChild(card);
  }
}
function renderIfb() {
  const ifb = state.ifb || { enabled: false, program: [], interrupt: [], duck_gain: 0.125 };
  $('ifb').innerHTML = ifb.enabled ? `Program ${csv(ifb.program)} ducks to ${ifb.duck_gain} while interrupt ${csv(ifb.interrupt)} is active` : '<span class="muted">Disabled</span>';
}
function renderTalkControls() {
  const muted = state.talk_mode === 'muted';
  $('mute').textContent = muted ? 'Unmute' : 'Mute';
  $('mute').classList.toggle('active', muted);
  $('talk').hidden = muted;
  $('talk').textContent = state.talk_mode === 'open' ? 'Open Mic' : 'Talk';
  $('talk').classList.toggle('active', regularTalkActive());
  setControl('mute', allowed('allow_talk_mode'), 'Talk mode locked by admin');
  setControl('talk', locks().allow_local_api !== false, 'Local controls locked by admin');
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
function openModal(id) { $(id).hidden = false; }
function closeModal(id) { $(id).hidden = true; }

$('stats-open').onclick = () => openModal('stats-modal');
$('stats-close').onclick = () => closeModal('stats-modal');
$('settings-open').onclick = () => openModal('settings-modal');
$('settings-close').onclick = () => closeModal('settings-modal');
$('setup-open').onclick = () => { window.location.href = 'mobile.html'; };
$('setup-open').hidden = sessionStorage.getItem('intercom-mobile-shell') !== '1';
document.querySelectorAll('#settings-form input,#settings-form select').forEach(el => el.addEventListener('input', () => { dirty = true; }));
document.querySelectorAll('#mic-gain-input,#speaker-gain-input').forEach(el => el.addEventListener('input', showGainValues));
$('codec-input').addEventListener('change', () => { $('opus-profile-field').hidden = $('codec-input').value !== 'opus'; });
$('ifb-duck-gain').addEventListener('input', () => { ensureDraft().ifb.duck_gain = Number($('ifb-duck-gain').value); showIfbDuckGain(); markDirty(); });
$('route-add-listen').onclick = e => { e.preventDefault(); addNumberTo(ensureDraft().listen, $('route-channel-input').value); $('route-channel-input').value = ''; renderConfigEditor(); };
$('route-add-tx').onclick = e => { e.preventDefault(); addNumberTo(ensureDraft().tx, $('route-channel-input').value); $('route-channel-input').value = ''; renderConfigEditor(); };
$('talker-add').onclick = e => { e.preventDefault(); const id = Number($('talker-id-input').value); if (Number.isInteger(id) && id > 0) { ensureDraft().talker_vol[id] = Number($('talker-gain-input').value); $('talker-id-input').value = ''; $('talker-gain-input').value = '1'; markDirty(); renderTalkerGainEditor(); } };
$('ifb-add-program').onclick = e => { e.preventDefault(); addNumberTo(ensureDraft().ifb.program, $('ifb-channel-input').value); $('ifb-channel-input').value = ''; renderIfbEditor(); };
$('ifb-add-interrupt').onclick = e => { e.preventDefault(); addNumberTo(ensureDraft().ifb.interrupt, $('ifb-channel-input').value); $('ifb-channel-input').value = ''; renderIfbEditor(); };
$('ifb-enabled').addEventListener('change', () => { ensureDraft().ifb.enabled = $('ifb-enabled').checked; markDirty(); });
$('save-config').onclick = e => { e.preventDefault(); saveConfig(); };
$('save-gain').onclick = e => { e.preventDefault(); saveGain(); };
$('mute').onclick = () => api(state?.talk_mode === 'muted' ? '/unmute' : '/mute', { method: 'POST' }).then(refresh).catch(err => message(String(err), 'error'));
let regularTalkDown = false;
async function regularTalkPress() {
  if (regularTalkDown || state?.talk_mode !== 'ptt') return;
  regularTalkDown = true;
  $('talk').classList.add('active');
  try {
    await api('/talk/down', { method: 'POST' });
  } catch (err) {
    regularTalkDown = false;
    $('talk').classList.remove('active');
    message(String(err), 'error');
  }
}
async function regularTalkRelease() {
  if (!regularTalkDown) return;
  regularTalkDown = false;
  $('talk').classList.remove('active');
  try {
    await api('/talk/up', { method: 'POST' });
    await refresh();
  } catch (err) {
    message(String(err), 'error');
  }
}
$('talk').onpointerdown = e => { e.preventDefault(); regularTalkPress(); };
$('talk').onpointerup = e => { e.preventDefault(); regularTalkRelease(); };
$('talk').onpointercancel = regularTalkRelease;
$('talk').onlostpointercapture = regularTalkRelease;

refresh();
setInterval(refresh, 1000);
