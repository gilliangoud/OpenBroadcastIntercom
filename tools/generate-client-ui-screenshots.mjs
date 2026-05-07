#!/usr/bin/env node

import { createServer } from 'node:http';
import { spawn } from 'node:child_process';
import {
  access,
  copyFile,
  mkdir,
  readFile,
  rm,
  stat,
  writeFile
} from 'node:fs/promises';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const root = path.resolve(__dirname, '..');
const tmp = path.join('/private/tmp', `intercom-client-ui-shots-${process.pid}`);
const outDir = path.join(root, 'docs/assets/client-ui');

const chromeCandidates = [
  process.env.CHROME_BIN,
  '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome',
  '/Applications/Chromium.app/Contents/MacOS/Chromium',
  '/Applications/Brave Browser.app/Contents/MacOS/Brave Browser',
  'google-chrome',
  'chromium',
  'chromium-browser'
].filter(Boolean);

async function exists(file) {
  try {
    await access(file);
    return true;
  } catch {
    return false;
  }
}

async function findChrome() {
  for (const candidate of chromeCandidates) {
    if (candidate.includes(path.sep)) {
      if (await exists(candidate)) return candidate;
    } else {
      return candidate;
    }
  }
  throw new Error('Chrome or Chromium was not found. Set CHROME_BIN to a headless-capable browser.');
}

function json(value) {
  return JSON.stringify(value, null, 2);
}

function baseClientState(overrides = {}) {
  return {
    build: {
      version: '2026.5.4',
      release_tag: 'v2026.5.4',
      git_sha: 'docs',
      build_timestamp: '2026-05-06T00:00:00Z',
      dirty: false
    },
    user_id: 10,
    client_uid: 'docs-client',
    name: 'Director',
    listen: [1, 2, 4],
    tx: [4],
    vol: { 1: 0.7, 2: 1.0, 4: 1.0 },
    talker_vol: { 20: 0.8 },
    codec: 'pcm48',
    opus_profile: 'speech_48_high',
    talk_mode: 'ptt',
    regular_talk_active: false,
    priority: true,
    priority_channels: [4],
    processing: { preset: 'voice', engine: 'built_in', pipeline: [] },
    channel_rosters: [
      {
        channel_id: 1,
        name: 'Program',
        members: [
          { user_id: 10, name: 'Director', present: true, transmitting: false },
          { user_id: 30, name: 'Program Bridge', present: true, transmitting: true }
        ]
      },
      {
        channel_id: 2,
        name: 'Production PL',
        members: [
          { user_id: 10, name: 'Director', present: true, transmitting: false },
          { user_id: 20, name: 'Referee A', present: true, transmitting: false }
        ]
      },
      {
        channel_id: 4,
        name: 'Director IFB',
        members: [
          { user_id: 10, name: 'Director', present: true, transmitting: false },
          { user_id: 11, name: 'Producer', present: true, transmitting: false }
        ]
      }
    ],
    emergency: null,
    supported_codecs: ['pcm16', 'pcm24', 'pcm48', 'opus'],
    buttons: [
      {
        id: '1',
        label: 'Director',
        color: '#215fd1',
        mode: 'momentary',
        actions: [{ type: 'transmit', channels: [4], users: [] }]
      },
      {
        id: '2',
        label: 'Producer',
        color: '#137a45',
        mode: 'momentary',
        actions: [{ type: 'transmit', channels: [2], users: [] }]
      },
      {
        id: '3',
        label: 'Program',
        color: '#6f42c1',
        mode: 'latching',
        actions: [{ type: 'transmit', channels: [1], users: [] }]
      },
      {
        id: '4',
        label: 'PA',
        color: '#b42318',
        mode: 'momentary',
        actions: [{ type: 'transmit', channels: [6], users: [] }]
      },
      {
        id: '5',
        label: 'Replay',
        color: '#8a5a00',
        mode: 'momentary',
        actions: [{ type: 'alert', targets: [{ kind: 'channel', id: 2 }] }]
      },
      {
        id: '6',
        label: 'Medic',
        color: '#006d77',
        mode: 'momentary',
        actions: [{ type: 'alert', targets: [{ kind: 'user', id: 40 }] }]
      }
    ],
    active_buttons: ['3'],
    active_direct_calls: [
      {
        caller: 20,
        caller_name: 'Referee A',
        target: 10,
        target_name: 'Director',
        active: true,
        duck: true
      }
    ],
    last_direct_caller: 20,
    direct_call_history: [],
    active_alerts: [
      {
        id: 42,
        sender: 20,
        sender_name: 'Referee A',
        target: { kind: 'user', id: 10 },
        message: 'Ready for restart.',
        active: true,
        acknowledged_by: [],
        created_at_ms: 1710000000000
      }
    ],
    recent_alerts: [],
    advertised_buttons: [],
    ifb: {
      enabled: true,
      program: [1],
      interrupt: [4],
      duck_gain: 0.18
    },
    lockout: {},
    stereo: { enabled: false },
    mic_gain: 1.15,
    speaker_gain: 0.9,
    requested_input_backend: 'auto',
    active_input_backend: 'voice_processing',
    input_backend_note: null,
    macos_microphone_mode: {
      preferred: 'voiceIsolation',
      active: 'voiceIsolation',
      voice_isolation_active: true,
      system_ui_available: true,
      note: null
    },
    playback: {
      available_samples: 1920,
      capacity_samples: 9600,
      underflows: 0,
      overflows: 0
    },
    ...overrides
  };
}

const controlStates = {
  tauri: baseClientState({
    user_id: 10,
    name: 'Director',
    regular_talk_active: false
  }),
  desktop: baseClientState({
    user_id: 11,
    name: 'Producer',
    regular_talk_active: true,
    active_buttons: ['2'],
    active_direct_calls: [],
    active_alerts: [],
    last_direct_caller: null
  }),
  pi: baseClientState({
    user_id: 20,
    name: 'Ref Box',
    listen: [2, 4],
    tx: [2],
    regular_talk_active: false,
    active_buttons: [],
    active_direct_calls: [],
    active_alerts: [],
    last_direct_caller: null,
    mic_gain: undefined,
    speaker_gain: undefined,
    active_input_backend: 'raw',
    macos_microphone_mode: undefined,
    playback: {
      available_samples: 960,
      capacity_samples: 4800,
      underflows: 1,
      overflows: 0
    }
  })
};

const mobileSettings = {
  app_title: 'Intercom Suite',
  server_host: '192.168.12.84',
  server: '192.168.12.84:40000',
  control: 'ws://192.168.12.84:40001',
  admin: 'http://192.168.12.84:40002',
  advanced_endpoints: false,
  user_id: 10,
  codec: 'opus',
  opus_profile: 'speech_48_high',
  listen_channel: 2,
  tx_channel: 4,
  mic_gain: 1.15,
  speaker_gain: 0.9,
  button_count: 6,
  buttons: ['1=Director', '2=Producer', '3=Program', '4=PA', '5=Replay', '6=Medic'],
  button_keys: ['1=d', '2=p'],
  local_ui_bind: '127.0.0.1:41002',
  local_ui_token: null,
  disable_local_ui: false,
  window_mode: 'native',
  ui_open_delay_ms: 750,
  input_transient_suppression: true,
  input_backend: 'auto',
  input_device: null,
  output_device: null,
  server_profiles: [
    {
      id: 'studio',
      name: 'Studio Intercom',
      server_host: '192.168.12.84',
      server: '192.168.12.84:40000',
      control: 'ws://192.168.12.84:40001',
      admin: 'http://192.168.12.84:40002',
      auth: 'none',
      discovered: true,
      last_connected_ms: 1710000000000
    },
    {
      id: 'truck',
      name: 'Production Truck',
      server_host: '192.168.12.120',
      server: '192.168.12.120:40000',
      control: 'ws://192.168.12.120:40001',
      admin: 'http://192.168.12.120:40002',
      auth: 'required',
      discovered: false,
      last_connected_ms: null
    }
  ]
};

const bridgeState = {
  config: {
    app_title: 'Intercom Bridge App',
    server_host: '192.168.12.84',
    server: '192.168.12.84:40000',
    control: 'ws://192.168.12.84:40001',
    admin: 'http://192.168.12.84:40002/admin/api/state',
    advanced_endpoints: false,
    bridge_bin: null,
    routes: [
      {
        id: 'program-in',
        name: 'Program Input',
        user_id: 90,
        mode: 'input',
        tx_channels: [1],
        listen_channels: [],
        codec: 'pcm48',
        opus_profile: 'speech_48_high',
        stereo: false,
        input_device: 'BlackHole 2ch',
        output_device: null,
        input_gain: 1,
        output_gain: 1,
        enabled: true,
        note: 'vMix program bus into IFB'
      },
      {
        id: 'pa-out',
        name: 'PA Output',
        user_id: 91,
        mode: 'output',
        tx_channels: [],
        listen_channels: [6],
        codec: 'pcm48',
        opus_profile: 'speech_48_high',
        stereo: true,
        input_device: null,
        output_device: 'USB Audio Interface',
        input_gain: 1,
        output_gain: 0.85,
        enabled: true,
        note: 'Arena PA feed'
      }
    ]
  },
  bridge_bin: '/Applications/Intercom Suite/bridge',
  routes: [
    { id: 'program-in', running: true, pid: 4242, started_at_ms: 1710000000000, exit: null },
    { id: 'pa-out', running: false, pid: null, started_at_ms: null, exit: null }
  ],
  input_devices: ['System default input', 'BlackHole 2ch', 'USB Audio Interface'],
  output_devices: ['System default output', 'USB Audio Interface', 'Headphones'],
  channels: [
    { id: 1, name: 'Program' },
    { id: 2, name: 'Production PL' },
    { id: 4, name: 'Director' },
    { id: 6, name: 'PA' }
  ],
  discovery_warnings: []
};

function mobileMockScript() {
  return `window.__TAURI__ = { core: { invoke: async (command) => {
    const settings = ${json(mobileSettings)};
    if (command === 'mobile_load_settings' || command === 'mobile_default_settings') return settings;
    if (command === 'mobile_status') return { phase: 'stopped', running: false, local_ui_url: null, last_error: null };
    if (command === 'mobile_discover_servers') return settings.server_profiles;
    if (command === 'mobile_select_server') return settings;
    if (command === 'mobile_forget_server') return settings;
    if (command === 'mobile_save_settings') return { ok: true };
    return { ok: true };
  } } };`;
}

function settingsMockScript() {
  return `window.__TAURI__ = { core: { invoke: async (command) => {
    const settings = ${json(mobileSettings)};
    if (command === 'load_native_settings' || command === 'default_native_settings') return settings;
    if (command === 'save_native_settings') return { ok: true };
    return settings;
  } } };`;
}

function controlsMockScript() {
  return `(() => {
    const states = ${json(controlStates)};
    function scenario() {
      return new URLSearchParams(window.location.search).get('scenario') || 'tauri';
    }
    window.intercomClientApi = {
      name: 'docs',
      capabilities: {
        setup: scenario() === 'tauri',
        gain: scenario() !== 'pi',
        macosMicrophoneModes: scenario() === 'desktop'
      },
      request: async (path, opts = {}) => {
        if (path === '/state') return structuredClone(states[scenario()] || states.tauri);
        if (opts.method === 'POST' || opts.method === 'PUT') return { ok: true };
        return { ok: true };
      },
      setup: () => {}
    };
  })();`;
}

function extractRawString(source, name) {
  const match = source.match(new RegExp(`const ${name}: &str = r#\"([\\s\\S]*?)\"#;`));
  if (!match) throw new Error(`Could not extract ${name} from bridge app source.`);
  return match[1];
}

async function prepareHarness() {
  await removeTree(tmp);
  await mkdir(path.join(tmp, 'controls'), { recursive: true });
  await mkdir(path.join(tmp, 'mobile'), { recursive: true });
  await mkdir(path.join(tmp, 'settings'), { recursive: true });
  await mkdir(path.join(tmp, 'bridge'), { recursive: true });
  await mkdir(outDir, { recursive: true });

  const shared = path.join(root, 'clients/shared-ui/talking');
  await copyFile(path.join(shared, 'client-controls.html'), path.join(tmp, 'controls/client-controls.html'));
  await copyFile(path.join(shared, 'client-controls.css'), path.join(tmp, 'controls/client-controls.css'));
  await copyFile(path.join(shared, 'client-controls.js'), path.join(tmp, 'controls/client-controls.js'));
  await writeFile(path.join(tmp, 'controls/client-api.js'), controlsMockScript());

  const appAssets = path.join(root, 'clients/app/tauri-assets');
  const mobileHtml = await readFile(path.join(appAssets, 'mobile.html'), 'utf8');
  await writeFile(
    path.join(tmp, 'mobile/mobile.html'),
    mobileHtml.replace('<script src="mobile.js"></script>', '<script src="mobile-mock.js"></script>\n    <script src="mobile.js"></script>')
  );
  await copyFile(path.join(appAssets, 'mobile.css'), path.join(tmp, 'mobile/mobile.css'));
  await copyFile(path.join(appAssets, 'mobile.js'), path.join(tmp, 'mobile/mobile.js'));
  await writeFile(path.join(tmp, 'mobile/mobile-mock.js'), mobileMockScript());

  const settingsHtml = await readFile(path.join(appAssets, 'settings.html'), 'utf8');
  await writeFile(
    path.join(tmp, 'settings/settings.html'),
    settingsHtml.replace('<script src="settings.js"></script>', '<script src="settings-mock.js"></script>\n    <script src="settings.js"></script>')
  );
  await copyFile(path.join(appAssets, 'settings.css'), path.join(tmp, 'settings/settings.css'));
  await copyFile(path.join(appAssets, 'settings.js'), path.join(tmp, 'settings/settings.js'));
  await writeFile(path.join(tmp, 'settings/settings-mock.js'), settingsMockScript());

  const bridgeSource = await readFile(path.join(root, 'clients/bridge-app/src/lib.rs'), 'utf8');
  await writeFile(path.join(tmp, 'bridge/index.html'), extractRawString(bridgeSource, 'INDEX_HTML'));
  await writeFile(path.join(tmp, 'bridge/style.css'), extractRawString(bridgeSource, 'STYLE_CSS'));
  await writeFile(path.join(tmp, 'bridge/app.js'), extractRawString(bridgeSource, 'APP_JS'));
}

async function removeTree(target) {
  try {
    await rm(target, { recursive: true, force: true });
  } catch (error) {
    if (error.code !== 'ENOENT' && error.code !== 'ENOTEMPTY') throw error;
  }
}

function contentType(file) {
  if (file.endsWith('.html')) return 'text/html; charset=utf-8';
  if (file.endsWith('.css')) return 'text/css; charset=utf-8';
  if (file.endsWith('.js')) return 'application/javascript; charset=utf-8';
  if (file.endsWith('.json')) return 'application/json; charset=utf-8';
  return 'application/octet-stream';
}

async function serveFile(res, file) {
  try {
    const body = await readFile(file);
    res.writeHead(200, { 'content-type': contentType(file), 'cache-control': 'no-store' });
    res.end(body);
  } catch (error) {
    res.writeHead(error.code === 'ENOENT' ? 404 : 500, { 'content-type': 'text/plain; charset=utf-8' });
    res.end(String(error.message || error));
  }
}

function startServer() {
  const server = createServer(async (req, res) => {
    const url = new URL(req.url, 'http://127.0.0.1');
    if (url.pathname === '/api/state') {
      res.writeHead(200, { 'content-type': 'application/json; charset=utf-8', 'cache-control': 'no-store' });
      res.end(json(bridgeState));
      return;
    }
    if (url.pathname.startsWith('/api/')) {
      res.writeHead(200, { 'content-type': 'application/json; charset=utf-8' });
      res.end('{"ok":true}');
      return;
    }

    let file;
    if (url.pathname === '/style.css') file = path.join(tmp, 'bridge/style.css');
    else if (url.pathname === '/app.js') file = path.join(tmp, 'bridge/app.js');
    else if (url.pathname === '/bridge/' || url.pathname === '/bridge') file = path.join(tmp, 'bridge/index.html');
    else {
      const normalized = path.normalize(decodeURIComponent(url.pathname)).replace(/^(\.\.[/\\])+/, '');
      file = path.join(tmp, normalized);
      if (url.pathname.endsWith('/')) file = path.join(file, 'index.html');
    }
    await serveFile(res, file);
  });
  return new Promise((resolve, reject) => {
    server.once('error', reject);
    server.listen(0, '127.0.0.1', () => resolve(server));
  });
}

function run(command, args, timeoutMs = 20000) {
  return new Promise((resolve, reject) => {
    const child = spawn(command, args, { stdio: ['ignore', 'pipe', 'pipe'] });
    let stdout = '';
    let stderr = '';
    const timer = setTimeout(() => {
      child.kill('SIGKILL');
      reject(new Error(`${command} timed out after ${timeoutMs}ms\n${stdout}\n${stderr}`));
    }, timeoutMs);
    child.stdout.on('data', chunk => { stdout += chunk; });
    child.stderr.on('data', chunk => { stderr += chunk; });
    child.on('error', error => {
      clearTimeout(timer);
      reject(error);
    });
    child.on('close', code => {
      clearTimeout(timer);
      if (code === 0) resolve({ stdout, stderr });
      else reject(new Error(`${command} exited ${code}\n${stdout}\n${stderr}`));
    });
  });
}

async function capture(chrome, url, output, width, height) {
  const profile = path.join(tmp, `chrome-${path.basename(output, '.png')}`);
  await mkdir(profile, { recursive: true });
  await rm(output, { force: true });
  await runUntilScreenshot(chrome, [
    '--headless=new',
    '--disable-gpu',
    '--disable-dev-shm-usage',
    '--hide-scrollbars',
    '--no-first-run',
    '--no-default-browser-check',
    '--run-all-compositor-stages-before-draw',
    '--timeout=3000',
    `--user-data-dir=${profile}`,
    `--window-size=${width},${height}`,
    `--screenshot=${output}`,
    url
  ], output, 20000);
  const info = await stat(output);
  if (!info.size) throw new Error(`Chrome wrote an empty screenshot: ${output}`);
}

function runUntilScreenshot(command, args, output, timeoutMs = 20000) {
  return new Promise((resolve, reject) => {
    const child = spawn(command, args, { stdio: ['ignore', 'pipe', 'pipe'] });
    let stdout = '';
    let stderr = '';
    let settled = false;

    async function maybeDone() {
      if (settled) return;
      try {
        const info = await stat(output);
        if (info.size > 0) {
          settled = true;
          child.kill('SIGTERM');
          resolve({ stdout, stderr });
        }
      } catch {
        // Screenshot not written yet.
      }
    }

    const poller = setInterval(maybeDone, 250);
    const timer = setTimeout(() => {
      if (settled) return;
      settled = true;
      child.kill('SIGKILL');
      reject(new Error(`${command} timed out after ${timeoutMs}ms\n${stdout}\n${stderr}`));
    }, timeoutMs);

    child.stdout.on('data', chunk => {
      stdout += chunk;
      maybeDone();
    });
    child.stderr.on('data', chunk => {
      stderr += chunk;
      maybeDone();
    });
    child.on('error', error => {
      if (settled) return;
      settled = true;
      clearInterval(poller);
      clearTimeout(timer);
      reject(error);
    });
    child.on('close', async code => {
      clearInterval(poller);
      clearTimeout(timer);
      if (settled) return;
      settled = true;
      try {
        const info = await stat(output);
        if (info.size > 0) {
          resolve({ stdout, stderr });
        } else {
          reject(new Error(`${command} exited ${code} before writing ${output}\n${stdout}\n${stderr}`));
        }
      } catch {
        reject(new Error(`${command} exited ${code} before writing ${output}\n${stdout}\n${stderr}`));
      }
    });
  });
}

async function main() {
  await prepareHarness();
  const chrome = await findChrome();
  const server = await startServer();
  const { port } = server.address();
  const base = `http://127.0.0.1:${port}`;
  try {
    const shots = [
      ['mobile-setup.png', `${base}/mobile/mobile.html`, 500, 844],
      ['tauri-operator-console.png', `${base}/controls/client-controls.html?scenario=tauri`, 500, 844],
      ['native-settings.png', `${base}/settings/settings.html`, 980, 980],
      ['desktop-operator-console.png', `${base}/controls/client-controls.html?scenario=desktop`, 1280, 900],
      ['pi-operator-console.png', `${base}/controls/client-controls.html?scenario=pi`, 900, 760],
      ['bridge-app.png', `${base}/bridge/`, 1280, 900]
    ];
    for (const [name, url, width, height] of shots) {
      const output = path.join(outDir, name);
      await capture(chrome, url, output, width, height);
      console.log(`wrote ${path.relative(root, output)}`);
    }
  } finally {
    await new Promise(resolve => server.close(resolve));
    await removeTree(tmp);
  }
}

main().catch(error => {
  console.error(error);
  process.exit(1);
});
