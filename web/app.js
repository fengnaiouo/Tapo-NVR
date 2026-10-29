const $ = (s) => document.querySelector(s);
const KIND_KEYS = {motion: '移動偵測', person: '偵測到人形', tamper: '鏡頭遭遮蔽', offline: '攝影機離線', online: '攝影機恢復連線'};
const kindLabel = (k) => t(KIND_KEYS[k] || k);

const state = {
  cameras: [],
  tiles: new Map(),
  focus: null,
  ptzMode: {},
  device: {},
  unseen: 0,
};

// ---------- API ----------
async function api(path, opts = {}) {
  const r = await fetch(path, {
    ...opts,
    headers: {...i18n.headers(), ...(opts.body ? {'Content-Type': 'application/json'} : {}), ...opts.headers},
  });
  const data = r.headers.get('content-type')?.includes('json') ? await r.json() : null;
  if (r.status === 401) {
    location.href = data?.setup ? '/setup.html' : '/login.html';
    throw new Error('未登入');
  }
  if (!r.ok) throw new Error(data?.error || r.statusText);
  return data;
}
const post = (path, body) => api(path, {method: 'POST', body: JSON.stringify(body ?? {})});

// ---------- 串流模式 ----------
function isPrivateHost(h) {
  return h === 'localhost' || h.endsWith('.local') || /^127\./.test(h) || /^10\./.test(h) ||
    /^192\.168\./.test(h) || /^172\.(1[6-9]|2\d|3[01])\./.test(h) ||
    /^100\.(6[4-9]|[7-9]\d|1[01]\d|12[0-7])\./.test(h) || h === '[::1]';
}
function streamMode() {
  const pref = localStorage.getItem('streamMode') || 'auto';
  if (pref !== 'auto') return pref;
  // 經 Cloudflare Tunnel 時 UDP 無法穿透，直接用 MSE（WebSocket）避免等待 WebRTC 逾時
  return isPrivateHost(location.hostname) ? 'webrtc,mse' : 'mse';
}

function createPlayer(stream) {
  const v = document.createElement('tapo-video');
  v.mode = streamMode();
  v.media = 'video,audio';
  v.src = new URL('/go2rtc/api/ws?src=' + encodeURIComponent(stream), location.href);
  return v;
}

// ---------- 格狀畫面 ----------
function renderGrid() {
  const grid = $('#grid');
  grid.innerHTML = '';
  state.tiles.clear();
  if (state.cameras.length === 0) {
    grid.innerHTML = '<p class="empty">尚未新增攝影機。<a href="/settings.html#cameras">前往設定 →</a></p>';
    return;
  }
  for (const cam of state.cameras) {
    const tile = document.createElement('div');
    tile.className = 'tile';
    // 名稱與狀態放右上角，避開攝影機自己疊在左上角的時間戳記
    tile.innerHTML = `
      <div class="viewport"></div>
      <div class="privacy-cover" hidden>
        <div>🔒 隱私模式已開啟</div>
        <button data-act="privacy-off">關閉隱私模式</button>
      </div>
      <div class="offline-cover" hidden>
        <div>⚠️ 攝影機離線</div>
        <small class="offline-detail" data-no-i18n></small>
      </div>
      <div class="tile-top">
        <span class="badge" hidden></span>
        <span class="rec" hidden title="錄影中">● REC</span>
        <span class="tile-name"><span class="dot"></span><span class="name"></span></span>
      </div>
      <div class="tile-bar">
        <div class="tb-group" data-group="ptz" hidden>
          <button data-ptz="-1,0" title="向左（按住）">◀</button>
          <button data-ptz="0,1" title="向上（按住）">▲</button>
          <button data-ptz="0,-1" title="向下（按住）">▼</button>
          <button data-ptz="1,0" title="向右（按住）">▶</button>
        </div>
        <div class="tb-group" data-group="dev" hidden>
          <button data-act="privacy" title="隱私模式">🔒</button>
          <select data-act="night" title="夜視模式">
            <option value="auto">🌙 自動</option>
            <option value="on">🌙 開</option>
            <option value="off">🌙 關</option>
          </select>
        </div>
        <span class="spacer"></span>
        <span class="mode"></span>
        <button data-act="snap" title="截圖">📷</button>
        <button data-act="playback" title="回放">⏪</button>
        <button data-act="focus" title="放大 / 更多控制">⤢</button>
      </div>`;
    tile.querySelector('.name').textContent = cam.name;
    tile.querySelector('.name').setAttribute('data-no-i18n', '');
    applyRecording(tile, cam.recording);
    const player = createPlayer(cam.sub_stream);
    player.addEventListener('streammode', (e) => tile.querySelector('.mode').textContent = e.detail);
    tile.querySelector('.viewport').appendChild(player);
    tile.querySelector('.viewport').addEventListener('dblclick', () => openFocus(cam));
    tile.querySelector('[data-act=focus]').onclick = () => openFocus(cam);
    tile.querySelector('[data-act=snap]').onclick = () => downloadSnapshot(cam);
    tile.querySelector('[data-act=playback]').onclick = () => openPlayback(cam.id);

    if (cam.ptz) {
      tile.querySelector('[data-group=ptz]').hidden = false;
      tile.querySelectorAll('[data-ptz]').forEach((b) => bindHold(b, cam, ...b.dataset.ptz.split(',').map(Number)));
    }
    if (cam.controls) {
      tile.querySelector('[data-group=dev]').hidden = false;
      tile.querySelector('[data-act=privacy]').onclick = () =>
        setDevice(cam, {privacy: !state.device[cam.id]?.privacy});
      tile.querySelector('[data-act=privacy-off]').onclick = () => setDevice(cam, {privacy: false});
      tile.querySelector('[data-act=night]').onchange = (e) => setDevice(cam, {night_vision: e.target.value});
      loadDevice(cam);
    }
    grid.appendChild(tile);
    state.tiles.set(cam.id, tile);
    applyStatus(cam.id, cam.status);
    applyDevice(cam.id);
  }
  applyLayout();
}

// ---------- 攝影機本身的設定（Tapo 控制 API） ----------
async function loadDevice(cam, refresh = false) {
  try {
    state.device[cam.id] = await api(`/api/cameras/${cam.id}/device${refresh ? '?refresh=1' : ''}`);
  } catch (e) {
    state.device[cam.id] = {error: e.message};
  }
  applyDevice(cam.id);
}

async function setDevice(cam, patch) {
  const prev = state.device[cam.id];
  state.device[cam.id] = {...prev, ...patch, busy: true};
  applyDevice(cam.id);
  try {
    state.device[cam.id] = await api(`/api/cameras/${cam.id}/device`, {method: 'PUT', body: JSON.stringify(patch)});
  } catch (e) {
    state.device[cam.id] = prev;
    toast(`${cam.name}：${e.message}`, 'error');
  }
  applyDevice(cam.id);
}

function applyDevice(id) {
  const d = state.device[id];
  const tile = state.tiles.get(id);
  if (tile) {
    const privacy = d?.privacy === true;
    tile.classList.toggle('privacy', privacy);
    tile.querySelector('.privacy-cover').hidden = !privacy;
    const pb = tile.querySelector('[data-act=privacy]');
    pb.classList.toggle('active', privacy);
    pb.disabled = !!d?.busy || !d || !!d.error;
    pb.title = d?.error ? t('無法控制：{e}', {e: d.error}) : privacy ? t('關閉隱私模式') : t('開啟隱私模式（遮蔽鏡頭）');
    const night = tile.querySelector('[data-act=night]');
    night.disabled = pb.disabled;
    if (d?.night_vision) night.value = d.night_vision;
  }
  if (state.focus?.id === id) renderDevicePanel();
}

function renderDevicePanel() {
  const cam = state.focus;
  const d = state.device[cam.id];
  $('#focus-privacy').hidden = d?.privacy !== true;
  $('#dev-missing').hidden = !!cam.controls;
  $('#dev-controls').hidden = !cam.controls;
  if (!cam.controls) return;
  $('#dev-error').hidden = !d?.error;
  $('#dev-error').textContent = d?.error ? t('無法讀取攝影機設定：{e}', {e: d.error}) : '';
  for (const el of document.querySelectorAll('#dev-controls [data-dev]')) {
    const v = d?.[el.dataset.dev];
    el.disabled = !d || !!d.error || !!d.busy || v == null;
    if (el.type === 'checkbox') el.checked = v === true;
    else if (v != null) el.value = v;
  }
  const sd = d?.sd_cards || [];
  $('#dev-sd').textContent = !d || d.error ? '' : sd.length
    ? sd.map((s) => t('💾 SD 卡：{status}，影片空間 {free} 可用 / 共 {total}', {
      status: s.status === 'normal' ? t('正常') : s.status || '—',
      free: s.video_free || '?',
      total: s.video_total || s.total || '?',
    })).join('\n')
    : t('💾 未插入 SD 卡');
  $('#dev-model').textContent = d?.model ? t('{model} · 韌體 {fw}', {model: d.model, fw: d.firmware || '?'}) : '';
  // 攝影機不支援的項目（回傳 null）直接隱藏
  for (const el of document.querySelectorAll('#dev-controls [data-dev]')) {
    el.closest('label').hidden = !!d && !d.error && !d.busy && d[el.dataset.dev] == null;
  }
  $('#dev-sd').hidden = !!d && d.sd_cards == null;
}

function applyLayout() {
  const layout = localStorage.getItem('layout') || 'auto';
  document.querySelectorAll('.layout-switch button').forEach((b) =>
    b.classList.toggle('active', b.dataset.layout === layout));
  const n = state.cameras.length;
  const cols = layout === 'auto' ? Math.max(1, Math.ceil(Math.sqrt(n))) : Number(layout);
  $('#grid').style.setProperty('--cols', cols);
}

function applyStatus(id, status) {
  const tile = state.tiles.get(id);
  if (!tile || !status) return;
  const dot = tile.querySelector('.dot');
  // 連線監控（reachable）優先；沒有結果時才用 ONVIF 事件訂閱狀態
  const offline = status.reachable === false;
  const online = status.reachable === true ? status.online !== false : status.online;
  dot.className = 'dot ' + (offline ? 'off' : online === true ? 'on' : online === false ? 'off' : '');
  dot.title = offline
    ? t('攝影機離線：{e}', {e: status.reach_error || ''})
    : status.error ? t('移動偵測連線異常：{e}', {e: status.error}) : online ? t('連線正常') : '';
  tile.classList.toggle('offline', offline);
  tile.querySelector('.offline-cover').hidden = !offline;
  tile.querySelector('.offline-detail').textContent = offline ? status.reach_error || '' : '';
  const badge = tile.querySelector('.badge');
  const active = status.active || [];
  badge.hidden = active.length === 0;
  badge.textContent = active.map(kindLabel).join(i18n.lang === 'en' ? ', ' : '、');
  tile.classList.toggle('alert', active.length > 0);
}

function applyRecording(tile, rec) {
  const el = tile.querySelector('.rec');
  el.hidden = !rec;
  el.classList.toggle('error', rec?.state === 'error');
  el.title = rec?.state === 'error' ? t('錄影異常：{e}', {e: rec.error || ''}) : t('錄影中');
}

function openPlayback(cameraId, ts) {
  const q = new URLSearchParams({camera: cameraId});
  if (ts) q.set('t', ts);
  location.href = `/playback.html?${q}`;
}

// ---------- 桌面版整合 ----------
// 桌面版沒有開放 IPC 給網頁，改用「導向特殊網址」由原生端攔截執行（只支援無害的指令）
const isDesktop = () => localStorage.getItem('client') === 'desktop';
function desktopCommand(cmd) {
  location.href = `https://tapo-app.invalid/cmd/${cmd}`;
}
window.tapoOpenCamera = (id) => {
  const cam = state.cameras.find((c) => c.id === id);
  if (cam) openFocus(cam);
};

// ---------- 截圖檢視 ----------
function showImage(src, caption = '') {
  const box = $('#lightbox');
  $('#lightbox-img').src = src;
  $('#lightbox-caption').textContent = caption;
  box.hidden = false;
}

function downloadSnapshot(cam) {
  const a = document.createElement('a');
  a.href = `/api/cameras/${cam.id}/snapshot`;
  a.download = '';
  a.click();
}

// ---------- 放大檢視 / 雲台 ----------
const zoom = {scale: 1, x: 0, y: 0, el: null};

function openFocus(cam) {
  closeFocus();
  state.focus = cam;
  $('#focus-name').textContent = cam.name;
  const vp = $('#focus-viewport');
  const player = createPlayer(cam.main_stream);
  player.addEventListener('streammode', (e) => $('#focus-mode').textContent = e.detail);
  vp.appendChild(player);
  zoom.el = player;
  setZoom(1, 0, 0);
  $('#btn-audio').textContent = '🔇 聲音';
  stopTalk();
  $('#btn-talk').hidden = !cam.talk_stream;
  $('#ptz-section').hidden = !cam.ptz;
  renderDevicePanel();
  if (cam.controls) loadDevice(cam);
  $('#focus').hidden = false;
  if (cam.ptz) loadPresets();
}

function closeFocus() {
  if (!state.focus) return;
  ptzRelease();
  stopTalk();
  if (isDesktop() && $('#focus').classList.contains('fs')) toggleFullscreen();
  $('#focus-viewport').innerHTML = '';
  $('#focus').hidden = true;
  state.focus = null;
  zoom.el = null;
  if (document.fullscreenElement) document.exitFullscreen();
}

function setZoom(scale, x, y) {
  const vp = $('#focus-viewport');
  scale = Math.min(4, Math.max(1, scale));
  const maxX = (scale - 1) * vp.clientWidth / 2;
  const maxY = (scale - 1) * vp.clientHeight / 2;
  Object.assign(zoom, {
    scale,
    x: Math.max(-maxX, Math.min(maxX, x)),
    y: Math.max(-maxY, Math.min(maxY, y)),
  });
  if (zoom.el) zoom.el.style.transform = `translate(${zoom.x}px, ${zoom.y}px) scale(${zoom.scale})`;
  $('#zoom').value = scale;
  $('#zoom-val').textContent = scale.toFixed(1) + '×';
}

function initZoom() {
  const vp = $('#focus-viewport');
  vp.addEventListener('wheel', (e) => {
    e.preventDefault();
    const factor = e.deltaY < 0 ? 1.15 : 1 / 1.15;
    const ns = Math.min(4, Math.max(1, zoom.scale * factor));
    const rect = vp.getBoundingClientRect();
    const cx = e.clientX - rect.left - rect.width / 2;
    const cy = e.clientY - rect.top - rect.height / 2;
    const k = ns / zoom.scale;
    setZoom(ns, cx - (cx - zoom.x) * k, cy - (cy - zoom.y) * k);
  }, {passive: false});
  let drag = null;
  vp.addEventListener('pointerdown', (e) => {
    if (zoom.scale <= 1) return;
    drag = {sx: e.clientX, sy: e.clientY, x: zoom.x, y: zoom.y};
    vp.setPointerCapture(e.pointerId);
    vp.classList.add('dragging');
  });
  vp.addEventListener('pointermove', (e) => {
    if (drag) setZoom(zoom.scale, drag.x + e.clientX - drag.sx, drag.y + e.clientY - drag.sy);
  });
  const end = () => { drag = null; vp.classList.remove('dragging'); };
  vp.addEventListener('pointerup', end);
  vp.addEventListener('pointercancel', end);
  vp.addEventListener('dblclick', () => setZoom(1, 0, 0));
  $('#zoom').addEventListener('input', (e) => setZoom(Number(e.target.value), zoom.x, zoom.y));
}

// ---------- 雙向語音 ----------
// 以只送麥克風的 WebRTC 連線連到 {id}_talk 串流（go2rtc tapo:// 回傳聲道）
let talkEl = null;
function startTalk() {
  const cam = state.focus;
  if (!cam?.talk_stream) return;
  if (!window.isSecureContext) {
    toast('對講需要 HTTPS、localhost 或桌面版（瀏覽器限制麥克風權限）', 'error');
    return;
  }
  talkEl = document.createElement('tapo-video');
  talkEl.mode = 'webrtc';
  talkEl.media = 'microphone';
  talkEl.background = true;
  talkEl.style.display = 'none';
  talkEl.addEventListener('streammode', (e) => {
    if (e.detail === '錯誤') toast('對講連線失敗（需 WebRTC，外網經 Cloudflare Tunnel 時無法使用）', 'error');
  });
  document.body.appendChild(talkEl);
  talkEl.src = new URL('/go2rtc/api/ws?src=' + encodeURIComponent(cam.talk_stream), location.href);
  const b = $('#btn-talk');
  b.classList.add('active');
  b.textContent = '🎙 對講中…（點擊結束）';
}
function stopTalk() {
  if (talkEl) {
    talkEl.ondisconnect?.();
    talkEl.remove();
    talkEl = null;
  }
  const b = $('#btn-talk');
  if (b) {
    b.classList.remove('active');
    b.textContent = '🎙 對講';
  }
}

function toggleFullscreen() {
  const focus = $('#focus');
  if (isDesktop()) {
    const on = !focus.classList.contains('fs');
    focus.classList.toggle('fs', on);
    desktopCommand(on ? 'fullscreen-on' : 'fullscreen-off');
  } else if (document.fullscreenElement) {
    document.exitFullscreen();
  } else {
    focus.requestFullscreen();
  }
}

// 按住期間持續送出移動指令（伺服器在 1.5 秒未收到時會自動停止）
let ptzTimer = null;
let ptzActive = false;
let ptzCam = null;

async function ptzPress(pan, tilt, cam = state.focus) {
  if (!cam?.ptz) return;
  ptzRelease();
  ptzActive = true;
  ptzCam = cam;
  const speed = Number($('#ptz-speed').value);
  const send = async () => {
    try {
      const r = await post(`/api/cameras/${cam.id}/ptz/move`, {
        pan: pan * speed, tilt: tilt * speed, mode: state.ptzMode[cam.id],
      });
      state.ptzMode[cam.id] = r.mode;
    } catch (e) {
      toast(t('雲台控制失敗：{e}', {e: e.message}), 'error');
      ptzRelease(false);
    }
  };
  await send();
  if (!ptzActive) return;
  const interval = state.ptzMode[cam.id] === 'step' ? 500 : 700;
  ptzTimer = setInterval(send, interval);
}

function ptzRelease(sendStop = true) {
  clearInterval(ptzTimer);
  ptzTimer = null;
  const wasActive = ptzActive;
  ptzActive = false;
  const cam = ptzCam;
  if (sendStop && wasActive && cam && state.ptzMode[cam.id] !== 'step') {
    post(`/api/cameras/${cam.id}/ptz/stop`).catch(() => {});
  }
}

/** 按住按鈕轉動雲台，放開停止（cam 未指定時使用放大檢視中的攝影機） */
function bindHold(button, cam, pan, tilt) {
  button.addEventListener('pointerdown', (e) => {
    e.preventDefault();
    e.stopPropagation();
    button.setPointerCapture(e.pointerId);
    ptzPress(pan, tilt, cam ?? state.focus);
  });
  button.addEventListener('pointerup', () => ptzRelease());
  button.addEventListener('pointercancel', () => ptzRelease());
  button.addEventListener('lostpointercapture', () => ptzRelease());
  button.addEventListener('dblclick', (e) => e.stopPropagation());
}

function initPtz() {
  document.querySelectorAll('.dpad button[data-pan]').forEach((b) =>
    bindHold(b, null, Number(b.dataset.pan), Number(b.dataset.tilt)));
  // 放大檢視的攝影機設定
  document.querySelectorAll('#dev-controls [data-dev]').forEach((el) => {
    el.addEventListener('change', () => {
      const cam = state.focus;
      if (!cam) return;
      const v = el.type === 'checkbox' ? el.checked : el.value;
      setDevice(cam, {[el.dataset.dev]: v});
    });
  });
  $('#dev-refresh').onclick = () => state.focus && loadDevice(state.focus, true);
  $('#focus-privacy-off').onclick = () => state.focus && setDevice(state.focus, {privacy: false});
  $('#ptz-stop').onclick = () => {
    ptzActive = true;
    ptzRelease();
  };
  const keys = {ArrowUp: [0, 1], ArrowDown: [0, -1], ArrowLeft: [-1, 0], ArrowRight: [1, 0]};
  let held = null;
  document.addEventListener('keydown', (e) => {
    if (!state.focus || e.target.tagName === 'INPUT') return;
    if (e.key === 'Escape') {
      if (isDesktop() && $('#focus').classList.contains('fs')) return toggleFullscreen();
      return closeFocus();
    }
    const k = keys[e.key];
    if (!k || held === e.key) return;
    e.preventDefault();
    held = e.key;
    ptzPress(...k);
  });
  document.addEventListener('keyup', (e) => {
    if (e.key === held) {
      held = null;
      ptzRelease();
    }
  });
}

async function loadPresets() {
  const cam = state.focus;
  const list = $('#preset-list');
  list.innerHTML = '<li class="hint">載入中…</li>';
  try {
    const presets = await api(`/api/cameras/${cam.id}/presets`);
    list.innerHTML = presets.length ? '' : '<li class="hint">尚無預設點</li>';
    for (const p of presets) {
      const li = document.createElement('li');
      const go = document.createElement('button');
      go.textContent = p.name;
      go.setAttribute('data-no-i18n', '');
      go.onclick = () => post(`/api/cameras/${cam.id}/presets/${encodeURIComponent(p.token)}/goto`)
        .catch((e) => toast(e.message, 'error'));
      const del = document.createElement('button');
      del.textContent = '🗑';
      del.title = '刪除';
      del.onclick = async () => {
        if (!confirm(t('刪除預設點「{name}」？', {name: p.name}))) return;
        await api(`/api/cameras/${cam.id}/presets/${encodeURIComponent(p.token)}`, {method: 'DELETE'})
          .catch((e) => toast(e.message, 'error'));
        loadPresets();
      };
      li.append(go, del);
      list.appendChild(li);
    }
  } catch (e) {
    list.innerHTML = '';
    const li = document.createElement('li');
    li.className = 'hint';
    li.textContent = t('無法讀取：{e}', {e: e.message});
    list.appendChild(li);
  }
}

// ---------- 事件 / 通知 ----------
function eventItem(ev) {
  const li = document.createElement('li');
  li.dataset.camera = ev.camera;
  const time = new Date(ev.ts).toLocaleString(i18n.locale, {hour12: false});
  if (ev.snapshot) {
    const img = document.createElement('img');
    img.loading = 'lazy';
    img.src = `/api/snapshots/${ev.snapshot}`;
    img.title = '點擊放大';
    img.onclick = () => showImage(img.src, `${ev.camera_name} · ${time}`);
    li.appendChild(img);
  }
  const text = document.createElement('div');
  const strong = document.createElement('strong');
  strong.textContent = ev.camera_name;
  strong.setAttribute('data-no-i18n', '');
  const kind = document.createElement('span');
  kind.textContent = kindLabel(ev.kind);
  const t = document.createElement('time');
  t.textContent = time;
  text.append(strong, kind, t);
  const play = document.createElement('button');
  play.className = 'link';
  play.textContent = '▶ 回放';
  play.onclick = () => openPlayback(ev.camera, ev.ts);
  text.appendChild(play);
  li.appendChild(text);
  return li;
}

function applyEventFilter() {
  const f = $('#event-filter').value;
  document.querySelectorAll('#event-list li').forEach((li) => li.hidden = f && li.dataset.camera !== f);
}

async function loadEvents() {
  const events = await api('/api/events?limit=200');
  const list = $('#event-list');
  list.innerHTML = '';
  events.forEach((ev) => list.appendChild(eventItem(ev)));
  if (!events.length) list.innerHTML = '<li class="hint">目前沒有事件</li>';
  applyEventFilter();
}

function onNewEvent(ev) {
  const list = $('#event-list');
  list.querySelector('.hint')?.remove();
  list.prepend(eventItem(ev));
  applyEventFilter();
  if ($('#events').hidden) {
    state.unseen++;
    $('#event-count').textContent = state.unseen;
    $('#event-count').hidden = false;
  }
  const label = kindLabel(ev.kind);
  if (ev.notify === false) return;
  toast(t('{name}：{label}', {name: ev.camera_name, label}), 'alert');
  // 桌面版由原生端顯示 Windows 通知，網頁不重複跳出
  if (!isDesktop() && 'Notification' in window && Notification.permission === 'granted') {
    const n = new Notification(t('{name}：{label}', {name: ev.camera_name, label}), {
      body: new Date(ev.ts).toLocaleTimeString(i18n.locale, {hour12: false}),
      icon: ev.snapshot ? `/api/snapshots/${ev.snapshot}` : undefined,
      image: ev.snapshot ? `/api/snapshots/${ev.snapshot}` : undefined,
      tag: `tapo-${ev.camera}`,
    });
    n.onclick = () => {
      window.focus();
      const cam = state.cameras.find((c) => c.id === ev.camera);
      if (cam) openFocus(cam);
    };
  }
}

function connectEvents(delay = 1000) {
  const ws = new WebSocket(`${location.protocol === 'https:' ? 'wss' : 'ws'}://${location.host}/api/ws`);
  ws.onopen = () => { delay = 1000; };
  ws.onmessage = (m) => {
    const msg = JSON.parse(m.data);
    if (msg.type === 'status') {
      const cam = state.cameras.find((c) => c.id === msg.camera);
      if (cam) cam.status = msg.status;
      applyStatus(msg.camera, msg.status);
    } else if (msg.type === 'event') {
      onNewEvent(msg.event);
    } else if (msg.type === 'config') {
      reloadCameras();
      loadArmState();
    } else if (msg.type === 'device') {
      state.device[msg.camera] = msg.device;
      applyDevice(msg.camera);
    }
  };
  ws.onclose = () => setTimeout(() => connectEvents(Math.min(delay * 2, 30000)), delay);
}

function toast(text, kind = '') {
  const el = document.createElement('div');
  el.className = `toast ${kind}`;
  el.textContent = text;
  $('#toasts').appendChild(el);
  setTimeout(() => el.remove(), 6000);
}

function updateNotifyButton() {
  const b = $('#btn-notify');
  if (!('Notification' in window) || !window.isSecureContext) {
    b.title = '瀏覽器通知需要 HTTPS 或 localhost；目前只顯示頁內提示';
    b.classList.add('muted');
  } else {
    b.classList.toggle('active', Notification.permission === 'granted');
    b.title = Notification.permission === 'granted' ? '桌面通知已啟用' : '啟用桌面通知';
  }
}

// 設定頁變更後（新增 / 刪除 / 修改攝影機）重新載入，只有實際變動時才重建畫面
let camerasKey = '';
async function reloadCameras() {
  const cams = await api('/api/cameras');
  const key = JSON.stringify(cams.map(({status, recording, ...c}) => c));
  for (const c of cams) {
    const tile = state.tiles.get(c.id);
    if (tile) applyRecording(tile, c.recording);
  }
  state.cameras = cams;
  const filter = $('#event-filter');
  const selected = filter.value;
  filter.length = 1;
  for (const c of cams) filter.add(new Option(c.name, c.id));
  filter.value = cams.some((c) => c.id === selected) ? selected : '';
  if (key === camerasKey) return;
  camerasKey = key;
  if (state.focus) {
    const cam = cams.find((c) => c.id === state.focus.id);
    if (cam) openFocus(cam); else closeFocus();
  }
  renderGrid();
}

// ---------- 布防 / 撤防、版本 ----------
let armed = null;
function renderArm() {
  const b = $('#btn-arm');
  b.hidden = armed == null;
  b.textContent = armed ? t('🛡 已布防') : t('🔕 已撤防');
  b.classList.toggle('armed', !!armed);
  b.classList.toggle('disarmed', armed === false);
  b.title = armed
    ? t('通知開啟中，按一下撤防（暫停所有通知，仍會錄影與記錄事件）')
    : t('通知已暫停，按一下布防（恢復通知）');
}
async function loadArmState() {
  try {
    const s = await api('/api/settings');
    armed = s.notify.enabled;
    renderArm();
  } catch { /* 忽略 */ }
}
async function toggleArm() {
  const next = !armed;
  try {
    await api('/api/notify/armed', {method: 'PUT', body: JSON.stringify({armed: next})});
    armed = next;
    renderArm();
    toast(next ? t('已布防，恢復通知') : t('已撤防，暫停通知（仍會錄影與記錄事件）'));
  } catch (e) {
    toast(e.message, 'error');
  }
}
async function loadVersion() {
  try {
    const a = await api('/api/about');
    const el = $('#app-version');
    el.textContent = `v${a.version}`;
    el.title = a.update_available ? t('有新版本 v{v}，請到設定 → 更新與關於', {v: a.update.latest}) : 'Tapo NVR';
    if (a.update_available) {
      el.textContent += ' ⬆';
      el.style.cursor = 'pointer';
      el.onclick = () => location.href = '/settings.html#about';
    }
  } catch { /* 忽略 */ }
}

// ---------- 初始化 ----------
async function main() {
  await i18n.ready;
  const params = new URLSearchParams(location.search);
  if (params.get('client') === 'desktop') {
    localStorage.setItem('client', 'desktop');
    history.replaceState(null, '', '/');
  }
  await api('/api/me');
  await import('/player.js');
  await reloadCameras();
  $('#event-filter').onchange = applyEventFilter;

  $('#stream-mode').value = localStorage.getItem('streamMode') || 'auto';
  $('#stream-mode').onchange = (e) => {
    localStorage.setItem('streamMode', e.target.value);
    renderGrid();
    if (state.focus) openFocus(state.focus);
  };
  document.querySelectorAll('.layout-switch button').forEach((b) => b.onclick = () => {
    localStorage.setItem('layout', b.dataset.layout);
    applyLayout();
  });

  $('#btn-events').onclick = () => {
    const d = $('#events');
    d.hidden = !d.hidden;
    if (!d.hidden) {
      state.unseen = 0;
      $('#event-count').hidden = true;
    }
  };
  $('#btn-events-close').onclick = () => $('#events').hidden = true;
  $('#btn-notify').onclick = async () => {
    if (!('Notification' in window) || !window.isSecureContext) {
      toast('瀏覽器通知需要 HTTPS（例如 Cloudflare Tunnel）或 localhost', 'error');
      return;
    }
    await Notification.requestPermission();
    updateNotifyButton();
  };
  $('#btn-logout').onclick = async () => {
    await post('/api/logout').catch(() => {});
    location.href = '/login.html';
  };

  $('#btn-focus-close').onclick = closeFocus;
  $('#btn-snapshot').onclick = () => state.focus && downloadSnapshot(state.focus);
  $('#btn-audio').onclick = () => {
    const v = zoom.el?.video;
    if (!v) return;
    v.muted = !v.muted;
    $('#btn-audio').textContent = v.muted ? '🔇 聲音' : '🔊 聲音';
  };
  $('#btn-fullscreen').onclick = toggleFullscreen;
  $('#btn-talk').onclick = () => (talkEl ? stopTalk() : startTalk());
  $('#btn-playback').onclick = () => state.focus && openPlayback(state.focus.id);
  $('#lightbox').onclick = () => { $('#lightbox').hidden = true; };
  $('#preset-form').onsubmit = async (e) => {
    e.preventDefault();
    const name = $('#preset-name').value.trim();
    try {
      await post(`/api/cameras/${state.focus.id}/presets`, {name});
      $('#preset-name').value = '';
      toast(t('已儲存預設點「{name}」', {name}));
      loadPresets();
    } catch (err) {
      toast(t('儲存失敗：{e}', {e: err.message}), 'error');
    }
  };

  $('#btn-arm').onclick = toggleArm;
  loadArmState();
  loadVersion();

  initZoom();
  initPtz();
  updateNotifyButton();
  window.addEventListener('resize', () => setZoom(zoom.scale, zoom.x, zoom.y));
  loadEvents().catch(() => {});
  connectEvents();
  // 從通知 / 推播開啟指定攝影機：/?camera=living
  const openId = params.get('camera');
  if (openId) {
    history.replaceState(null, '', '/');
    window.tapoOpenCamera(openId);
  }
  // 錄影狀態每 30 秒更新一次
  setInterval(() => reloadCameras().catch(() => {}), 30000);
}

main().catch((e) => console.error(e));
