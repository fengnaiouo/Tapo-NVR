const $ = (s, root = document) => root.querySelector(s);

async function api(path, opts = {}) {
  const r = await fetch(path, {
    ...opts,
    headers: {...i18n.headers(), ...(opts.body ? {'Content-Type': 'application/json'} : {})},
  });
  const data = r.headers.get('content-type')?.includes('json') ? await r.json() : null;
  if (r.status === 401) {
    location.href = data?.setup ? '/setup.html' : '/login.html';
    throw new Error('未登入');
  }
  if (!r.ok) throw new Error(data?.error || r.statusText);
  return data;
}
const send = (method, path, body) => api(path, {method, body: JSON.stringify(body ?? {})});

function toast(text, kind = '') {
  const el = document.createElement('div');
  el.className = `toast ${kind}`;
  el.textContent = text;
  $('#toasts').appendChild(el);
  setTimeout(() => el.remove(), 5000);
}

let settings = null;

// ---------- 分頁 ----------
function showTab() {
  const tab = (location.hash || '#cameras').slice(1);
  document.querySelectorAll('.tab').forEach((t) => t.hidden = t.id !== `tab-${tab}`);
  document.querySelectorAll('.settings-nav a').forEach((a) => a.classList.toggle('active', a.hash === `#${tab}`));
}
window.addEventListener('hashchange', showTab);

// ---------- 載入 ----------
async function load() {
  settings = await api('/api/settings');
  renderCameras();
  fillForm($('#form-notify'), {...settings.notify, ntfy_token: ''});
  $('#form-notify').elements.ntfy_token.placeholder = settings.notify.has_ntfy_token ? t('已設定（不變更請留空）') : t('選填');
  fillForm($('#form-recording'), settings.recording);
  loadRecordingStatus();
  fillForm($('#form-account'), {username: settings.account.username});
  fillForm($('#form-server'), {
    ...settings.server,
    session_days: settings.account.session_days,
    webrtc_candidates: settings.server.webrtc_candidates.join('\n'),
  });
  $('#config-path').textContent = settings.config_path;
  $('#restart-banner').hidden = !settings.restart_required;
  for (const b of [$('#btn-restart'), $('#btn-restart-banner')]) {
    b.disabled = !settings.can_restart;
    b.title = settings.can_restart ? '' : t('請用 scripts\\start.bat 啟動伺服器，才能從網頁重新啟動');
  }
}

function fillForm(form, values) {
  for (const [k, v] of Object.entries(values)) {
    const el = form.elements[k];
    if (!el) continue;
    if (el.type === 'checkbox') el.checked = !!v;
    else el.value = v ?? '';
  }
}

function formValues(form) {
  const out = {};
  for (const el of form.elements) {
    if (!el.name) continue;
    if (el.type === 'checkbox') out[el.name] = el.checked;
    else if (el.type === 'number') out[el.name] = el.value === '' ? null : Number(el.value);
    else out[el.name] = el.value;
  }
  return out;
}

// ---------- 攝影機 ----------
function renderCameras() {
  const list = $('#camera-list');
  list.innerHTML = '';
  if (!settings.cameras.length) {
    list.innerHTML = '<li class="hint">尚未新增攝影機。可以按「掃描區網攝影機」自動尋找。</li>';
    return;
  }
  settings.cameras.forEach((cam, i) => {
    const li = document.createElement('li');
    li.className = 'card';
    li.innerHTML = `
      <div class="card-main">
        <strong class="c-name"></strong> <code class="c-id"></code>
        <div class="hint c-meta"></div>
      </div>
      <div class="card-actions">
        <button data-act="up" title="上移">↑</button>
        <button data-act="down" title="下移">↓</button>
        <button data-act="edit">編輯</button>
        <button data-act="delete" class="danger">刪除</button>
      </div>`;
    $('.c-name', li).textContent = cam.name;
    $('.c-name', li).setAttribute('data-no-i18n', '');
    $('.c-id', li).textContent = cam.id;
    $('.c-meta', li).textContent = [
      cam.model || null,
      cam.host,
      cam.stream === 'tapo' ? t('Tapo 私有串流') : null,
      t({events: '事件錄影', continuous: '連續錄影', off: '不錄影'}[cam.record]),
      cam.ptz ? t('雲台') : null,
      t(cam.motion ? (cam.notify ? '偵測＋通知' : '偵測（不通知）') : '偵測關閉'),
      cam.has_cloud_password ? t('攝影機控制＋對講') : null,
      cam.has_password || cam.stream === 'tapo' ? null : t('⚠️ 未設定密碼'),
    ].filter(Boolean).join(' · ');
    $('.c-meta', li).setAttribute('data-no-i18n', '');
    $('[data-act=up]', li).disabled = i === 0;
    $('[data-act=down]', li).disabled = i === settings.cameras.length - 1;
    $('[data-act=up]', li).onclick = () => move(i, -1);
    $('[data-act=down]', li).onclick = () => move(i, 1);
    $('[data-act=edit]', li).onclick = () => openCameraDialog(cam);
    $('[data-act=delete]', li).onclick = () => removeCamera(cam);
    list.appendChild(li);
  });
}

async function move(i, delta) {
  const ids = settings.cameras.map((c) => c.id);
  [ids[i], ids[i + delta]] = [ids[i + delta], ids[i]];
  await send('PUT', '/api/settings/cameras/order', {ids}).catch((e) => toast(e.message, 'error'));
  load();
}

async function removeCamera(cam) {
  if (!confirm(t('確定刪除「{name}」？\n（過去的事件紀錄會保留）', {name: cam.name}))) return;
  try {
    await api(`/api/settings/cameras/${cam.id}`, {method: 'DELETE'});
    toast(t('已刪除 {name}', {name: cam.name}));
    load();
  } catch (e) {
    toast(e.message, 'error');
  }
}

let editing = null;

function openCameraDialog(cam = null, preset = {}) {
  editing = cam;
  const form = $('#form-camera');
  form.reset();
  $('#test-result').hidden = true;
  $('#camera-dialog-title').textContent = cam ? t('編輯：{name}', {name: cam.name}) : t('新增攝影機');
  const values = cam ?? {
    name: '', host: '', username: settings.cameras[0]?.username ?? '', id: '',
    rtsp_port: 554, onvif_port: 2020, ptz: true, motion: true, notify: true, record: 'events', stream: 'rtsp', model: '', ...preset,
  };
  fillForm(form, {...values, password: '', cloud_password: '', clear_cloud_password: false});
  form.elements.id.disabled = !!cam;
  form.elements.password.required = !cam;
  form.elements.password.placeholder = cam ? t('留空 = 不變更') : '';
  form.elements.cloud_password.placeholder = cam?.has_cloud_password ? t('已設定（不變更請留空）') : t('不使用請留空');
  syncStreamFields();
  form.elements.clear_cloud_password.parentElement.hidden = !cam?.has_cloud_password;
  $('#camera-dialog').showModal();
  form.elements[values.host ? 'name' : 'host'].focus();
}

function cameraPayload() {
  const v = formValues($('#form-camera'));
  if (editing) v.id = editing.id;
  return v;
}

$('#btn-add-camera').onclick = () => openCameraDialog();
$('#btn-cancel').onclick = () => $('#camera-dialog').close();

$('#btn-test').onclick = async () => {
  const btn = $('#btn-test');
  const out = $('#test-result');
  btn.disabled = true;
  btn.textContent = '測試中…';
  out.hidden = false;
  out.innerHTML = '<li class="hint">連線中，約需 5–10 秒…</li>';
  try {
    const r = await send('POST', '/api/settings/cameras/test', cameraPayload());
    out.innerHTML = '';
    for (const line of r.lines) {
      const li = document.createElement('li');
      li.textContent = line;
      out.appendChild(li);
    }
    if (r.ptz === false) $('#form-camera').elements.ptz.checked = false;
    if (r.model) $('#form-camera').elements.model.value = r.model;
  } catch (e) {
    out.innerHTML = '';
    const li = document.createElement('li');
    li.textContent = `❌ ${e.message}`;
    out.appendChild(li);
  } finally {
    btn.disabled = false;
    btn.textContent = '🔌 測試連線';
  }
};

$('#form-camera').addEventListener('submit', async (e) => {
  e.preventDefault();
  const body = cameraPayload();
  try {
    if (editing) await send('PUT', `/api/settings/cameras/${editing.id}`, body);
    else await send('POST', '/api/settings/cameras', body);
    $('#camera-dialog').close();
    toast(t('已儲存「{name}」，設定立即生效', {name: body.name}));
    load();
  } catch (err) {
    toast(err.message, 'error');
  }
});

$('#btn-discover').onclick = async () => {
  const btn = $('#btn-discover');
  btn.disabled = true;
  btn.textContent = '掃描中（約 3 秒）…';
  const list = $('#discover-list');
  try {
    const found = await send('POST', '/api/settings/cameras/discover');
    $('#discover-result').hidden = false;
    list.innerHTML = found.length ? '' : `<li class="hint">${t('找不到攝影機。請確認攝影機與伺服器在同一個網段；也可以手動新增。')}</li>`;
    for (const f of found) list.appendChild(discoveredCard(f));
  } catch (e) {
    toast(e.message, 'error');
  } finally {
    btn.disabled = false;
    btn.textContent = '🔍 掃描區網攝影機';
  }
};

// ---------- 錄影 ----------
const fmtBytes = (n) => {
  if (n == null) return '—';
  const u = ['B', 'KB', 'MB', 'GB', 'TB'];
  let i = 0;
  while (n >= 1024 && i < u.length - 1) { n /= 1024; i++; }
  return `${n.toFixed(i >= 3 ? 1 : 0)} ${u[i]}`;
};

async function loadRecordingStatus() {
  const box = $('#rec-status');
  try {
    const s = await api('/api/recordings/status');
    box.innerHTML = '';
    const head = document.createElement('p');
    if (!s.enabled) {
      head.textContent = '⏸ 尚未選擇錄影資料夾，目前不會錄影。';
      box.appendChild(head);
      return;
    }
    head.textContent = t('已使用 {used} · 磁碟剩餘 {free}', {used: fmtBytes(s.used_bytes), free: fmtBytes(s.free_bytes)});
    box.appendChild(head);
    const ul = document.createElement('ul');
    ul.className = 'card-list';
    for (const c of s.cameras) {
      const li = document.createElement('li');
      li.className = 'card';
      const mode = {events: '事件錄影', continuous: '連續錄影', off: '不錄影'}[c.mode];
      let state = '⏸ 未錄影';
      if (c.mode !== 'off') {
        if (c.status?.state === 'recording') state = '🔴 錄影中';
        else if (c.status?.state === 'error') state = `⚠️ ${c.status.error || t('錄影異常')}`;
        else state = '⏳ 啟動中';
      }
      li.textContent = t('{name}（{mode}）：{state}', {name: c.name, mode: t(mode), state: t(state)});
      li.setAttribute('data-no-i18n', '');
      ul.appendChild(li);
    }
    box.appendChild(ul);
  } catch (e) {
    box.textContent = e.message;
  }
}

$('#form-recording').addEventListener('submit', async (e) => {
  e.preventDefault();
  try {
    await send('PUT', '/api/settings/recording', formValues(e.target));
    toast('錄影設定已儲存並套用');
    await load();
  } catch (err) {
    toast(err.message, 'error');
  }
});
$('#btn-clear-folder').onclick = () => {
  if (!confirm('停用錄影？（已錄的影片不會被刪除）')) return;
  $('#form-recording').elements.path.value = '';
  $('#form-recording').requestSubmit();
};

// 伺服器端資料夾瀏覽：網頁與桌面版都能用，也能從遠端選擇伺服器那台電腦的資料夾
const fd = {path: '', parent: null};
async function browse(path) {
  try {
    const r = await api(`/api/settings/browse?path=${encodeURIComponent(path)}`);
    fd.path = r.path;
    fd.parent = r.parent;
    $('#fd-path').textContent = r.path || '本機磁碟';
    $('#fd-up').disabled = !r.path;
    $('#fd-ok').disabled = !r.path;
    $('#fd-mkdir').disabled = !r.path;
    $('#fd-free').textContent = r.path ? t('此磁碟剩餘 {free}', {free: fmtBytes(r.free_bytes)}) : t('請選擇磁碟');
    const list = $('#fd-list');
    list.innerHTML = '';
    if (!r.dirs.length) list.innerHTML = '<li class="hint">（沒有子資料夾）</li>';
    for (const d of r.dirs) {
      const li = document.createElement('li');
      li.textContent = `📁 ${d.name}${d.free_bytes != null ? t('（剩餘 {free}）', {free: fmtBytes(d.free_bytes)}) : ''}`;
      li.setAttribute('data-no-i18n', '');
      li.onclick = () => browse(d.path);
      list.appendChild(li);
    }
  } catch (e) {
    toast(e.message, 'error');
  }
}
$('#btn-pick-folder').onclick = () => {
  const cur = $('#form-recording').elements.path.value;
  $('#folder-dialog').showModal();
  browse(cur || '');
};
$('#fd-up').onclick = () => browse(fd.parent && fd.parent !== fd.path ? fd.parent : '');
$('#fd-cancel').onclick = () => $('#folder-dialog').close();
$('#fd-ok').onclick = () => {
  $('#form-recording').elements.path.value = fd.path;
  $('#folder-dialog').close();
  toast('已選擇資料夾，按「儲存」後開始錄影');
};
$('#fd-mkdir').onclick = async () => {
  const name = $('#fd-new').value.trim();
  if (!name) return;
  try {
    const r = await send('POST', '/api/settings/mkdir', {parent: fd.path, name});
    $('#fd-new').value = '';
    browse(r.path);
  } catch (e) {
    toast(e.message, 'error');
  }
};

$('#btn-test-push').onclick = async () => {
  try {
    await send('POST', '/api/settings/notify/test');
    toast('已送出測試通知，請查看手機');
  } catch (e) {
    toast(e.message, 'error');
  }
};
$('#topic-example').textContent = `tapo-${Math.random().toString(36).slice(2, 10)}`;

// ---------- 掃描結果 ----------
const MODEL_TYPES = [
  [/^C(1\d\d)/, '室內固定'], [/^C2\d\d/, '室內雲台'], [/^C3\d\d/, '戶外固定'], [/^C4\d\d/, '電池攝影機'],
  [/^C5\d\d/, '戶外雲台'], [/^C6\d\d/, '戶外雲台'], [/^C7\d\d/, '戶外固定'], [/^D\d/, '門鈴'], [/^TC7/, '室內雲台'],
  [/^TC[46]/, '室內固定'], [/^TC8/, '戶外固定'], [/^TC/, '室內固定'], [/^BM/, '嬰兒監視器'],
];
const modelType = (m) => (MODEL_TYPES.find(([re]) => re.test(m || '')) || [null, ''])[1];

function discoveredCard(f) {
  const li = document.createElement('li');
  li.className = 'card';
  li.innerHTML = '<div class="card-main"><strong data-no-i18n></strong><div class="hint d-meta" data-no-i18n></div><div class="d-badges"></div></div><div class="card-actions"></div>';
  const hub = /HUB/i.test(f.device_type || '');
  const title = [f.model || (f.tapo ? t('Tapo 裝置') : t('ONVIF 裝置')), f.name].filter(Boolean).join(' · ');
  $('strong', li).textContent = title;
  $('.d-meta', li).textContent = [
    f.host,
    t(modelType(f.model)) || null,
    f.hardware ? `HW ${f.hardware}` : null,
    f.firmware ? t('韌體 {fw}', {fw: f.firmware.split(' ')[0]}) : null,
  ].filter(Boolean).join(' · ');
  const badges = $('.d-badges', li);
  const badge = (ok, text) => {
    const s = document.createElement('span');
    s.className = ok ? 'pill ok' : 'pill warn';
    s.textContent = t(text);
    badges.appendChild(s);
  };
  if (hub) badge(false, 'Tapo Hub（電池攝影機需透過 Hub，目前不支援串流）');
  else {
    badge(f.rtsp_open, f.rtsp_open ? '已開放 RTSP' : '未開放 RTSP：可改用 Tapo 私有串流，或在 Tapo App 開啟「第三方相容性」並建立攝影機帳戶');
    badge(f.onvif_open, f.onvif_open ? 'ONVIF 可用' : 'ONVIF 未開啟（移動偵測通知需要）');
  }
  const act = $('.card-actions', li);
  if (f.configured_as) {
    act.textContent = t('已加入：{name}', {name: f.configured_as});
    act.classList.add('hint');
  } else if (!hub) {
    const add = document.createElement('button');
    add.className = 'primary';
    add.textContent = t('加入');
    add.onclick = () => openCameraDialog(null, {
      host: f.host,
      name: f.name || f.model || '',
      model: f.model || '',
      onvif_port: f.onvif_port || 2020,
      stream: f.rtsp_open ? 'rtsp' : 'tapo',
      ptz: /^(C2|C5|TC7)/.test(f.model || '') || !f.model,
    });
    act.appendChild(add);
  }
  return li;
}

// Tapo 私有串流不需要攝影機帳戶，但需要雲端密碼
function syncStreamFields() {
  const f = $('#form-camera').elements;
  const tapo = f.stream.value === 'tapo';
  f.username.required = !tapo;
  f.password.required = !tapo && !editing;
  f.username.closest('label').hidden = tapo;
  f.password.closest('label').hidden = tapo;
  f.cloud_password.required = tapo && !editing?.has_cloud_password;
  if (tapo) f.cloud_password.closest('details').open = true;
}
$('#form-camera').elements.stream.addEventListener('change', syncStreamFields);

// ---------- 其他表單 ----------
$('#form-notify').addEventListener('submit', async (e) => {
  e.preventDefault();
  try {
    await send('PUT', '/api/settings/notify', formValues(e.target));
    toast('已儲存');
  } catch (err) {
    toast(err.message, 'error');
  }
});

$('#form-account').addEventListener('submit', async (e) => {
  e.preventDefault();
  const v = formValues(e.target);
  if (v.new_password !== v.confirm) return toast('兩次輸入的新密碼不一致', 'error');
  try {
    const r = await send('PUT', '/api/settings/account', v);
    e.target.reset();
    fillForm(e.target, {username: v.username});
    toast(r.others_logged_out ? '密碼已變更，其他裝置已登出' : '已儲存');
  } catch (err) {
    toast(err.message, 'error');
  }
});

$('#form-server').addEventListener('submit', async (e) => {
  e.preventDefault();
  const v = formValues(e.target);
  v.webrtc_candidates = v.webrtc_candidates.split('\n');
  try {
    const r = await send('PUT', '/api/settings/server', v);
    toast(r.restart_required ? '已儲存，請重新啟動伺服器以套用' : '已儲存並套用');
    load();
  } catch (err) {
    toast(err.message, 'error');
  }
});

async function restart() {
  if (!confirm('重新啟動伺服器？畫面會中斷數秒。')) return;
  try {
    await send('POST', '/api/system/restart');
    toast('重新啟動中…');
    // 直連時若監聽連接埠改了，重啟後改連新的連接埠；經 HTTPS（Tunnel）則網址不變
    const port = settings.server.listen.split(':').pop();
    const target = location.protocol === 'http:' && port && port !== (location.port || '80')
      ? `http://${location.hostname}:${port}/settings.html#server` : location.href;
    for (let i = 0; i < 30; i++) {
      await new Promise((r) => setTimeout(r, 1000));
      try {
        const r = await fetch(new URL('/api/setup', target));
        if (r.ok) { location.href = target; return; }
      } catch { /* 伺服器尚未就緒 */ }
    }
    toast('伺服器沒有回應，請確認 start.bat 視窗', 'error');
  } catch (e) {
    toast(e.message, 'error');
  }
}
$('#btn-restart').onclick = restart;
$('#btn-restart-banner').onclick = restart;

// ---------- 此裝置 ----------
const device = $('#form-device');
device.elements.streamMode.value = localStorage.getItem('streamMode') || 'auto';
device.elements.lang.value = i18n.pref;
device.elements.layout.value = localStorage.getItem('layout') || 'auto';
device.addEventListener('change', (e) => {
  if (e.target.name === 'lang') return i18n.setLang(e.target.value);
  localStorage.setItem(e.target.name, e.target.value);
  toast('已儲存（此裝置）');
});

function updateNotifyState() {
  const s = $('#notify-state');
  if (!('Notification' in window) || !window.isSecureContext) {
    s.textContent = '目前連線不支援瀏覽器通知（需 HTTPS 或 localhost），只會顯示頁內提示';
    $('#btn-browser-notify').disabled = true;
  } else {
    s.textContent = {granted: '已啟用', denied: '已被封鎖，請在瀏覽器網址列的網站設定中允許', default: '尚未啟用'}[Notification.permission];
  }
}
$('#btn-browser-notify').onclick = async () => {
  await Notification.requestPermission();
  updateNotifyState();
};
if (localStorage.getItem('client') === 'desktop') {
  $('#desktop-hint').hidden = false;
  $('#btn-browser-notify').parentElement.hidden = true;
}

// ---------- 啟動 ----------
i18n.ready.then(() => {
  showTab();
  updateNotifyState();
  load().catch((e) => toast(e.message, 'error'));
});
