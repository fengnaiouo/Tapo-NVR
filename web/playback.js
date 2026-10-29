const $ = (s) => document.querySelector(s);
const KIND_KEYS = {motion: '移動偵測', person: '偵測到人形', tamper: '鏡頭遭遮蔽', offline: '攝影機離線', online: '攝影機恢復連線'};
const kindLabel = (k) => t(KIND_KEYS[k] || k);
const HOUR = 3600e3;
const DAY = 24 * HOUR;

async function api(path) {
  const r = await fetch(path, {headers: i18n.headers()});
  const data = r.headers.get('content-type')?.includes('json') ? await r.json() : null;
  if (r.status === 401) {
    location.href = data?.setup ? '/setup.html' : '/login.html';
    throw new Error('未登入');
  }
  if (!r.ok) throw Object.assign(new Error(data?.error || r.statusText), {status: r.status});
  return data;
}

function toast(text, kind = '') {
  const el = document.createElement('div');
  el.className = `toast ${kind}`;
  el.textContent = text;
  $('#toasts').appendChild(el);
  setTimeout(() => el.remove(), 5000);
}

const pad = (n) => String(n).padStart(2, '0');
const fmtTime = (ms) => {
  const d = new Date(ms);
  return `${pad(d.getHours())}:${pad(d.getMinutes())}:${pad(d.getSeconds())}`;
};
const fmtDate = (d) => `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}`;
const dayStartOf = (dateStr) => new Date(`${dateStr}T00:00:00`).getTime();

const st = {
  cameras: [],
  camera: null,
  days: [],
  date: fmtDate(new Date()),
  segs: [],
  events: [],
  idx: -1,
  cur: null,
  span: 24,
  viewStart: 0,
  markIn: null,
  markOut: null,
  preRoll: 10e3,
};

const video = $('#video');
// 記住回放音量（與監控畫面分開保存）；預設靜音，按播放器的喇叭圖示即可開聲音
video.volume = Math.min(1, Math.max(0, Number(localStorage.getItem('playbackVolume') ?? 0.8)));
video.addEventListener('volumechange', () => localStorage.setItem('playbackVolume', String(video.volume)));

// ---------- 載入 ----------
async function init() {
  const params = new URLSearchParams(location.search);
  st.cameras = await api('/api/cameras');
  if (!st.cameras.length) return showEmpty('尚未新增攝影機。', '/settings.html#cameras', '前往設定');

  const sel = $('#camera');
  for (const c of st.cameras) sel.add(new Option(c.name, c.id));
  st.camera = st.cameras.some((c) => c.id === params.get('camera')) ? params.get('camera') : st.cameras[0].id;
  sel.value = st.camera;

  const t = params.get('t') ? new Date(params.get('t')).getTime() : null;
  if (t) st.date = fmtDate(new Date(t));
  try {
    const settings = await api('/api/settings');
    st.preRoll = (settings.recording?.pre_seconds ?? 10) * 1000;
  } catch { /* 沿用預設 */ }

  await loadCamera(t ? null : 'latest');
  if (t) playAt(t - st.preRoll);
  else if (st.segs.length) playLatest();
  setInterval(refreshToday, 60e3);
}

function showEmpty(text, href, label) {
  const el = $('#empty');
  el.innerHTML = '';
  const p = document.createElement('p');
  p.textContent = text;
  el.appendChild(p);
  if (href) {
    const a = document.createElement('a');
    a.href = href;
    a.className = 'button primary';
    a.textContent = label;
    el.appendChild(a);
  }
  el.hidden = false;
  $('#player-wrap').hidden = true;
}

async function loadCamera(pickDate) {
  try {
    st.days = await api(`/api/recordings/${st.camera}/days`);
  } catch (e) {
    if (e.status === 400) return showEmpty(e.message, '/settings.html#recording', '前往錄影設定');
    throw e;
  }
  $('#empty').hidden = true;
  $('#player-wrap').hidden = false;
  if (pickDate === 'latest' && st.days.length && !st.days.includes(st.date)) st.date = st.days.at(-1);
  const input = $('#date');
  input.min = st.days[0] || '';
  input.max = fmtDate(new Date());
  await loadDay();
}

async function loadDay() {
  $('#date').value = st.date;
  const data = await api(`/api/recordings/${st.camera}/segments?date=${st.date}`);
  st.segs = data.segments.map((s) => ({...s, start: new Date(s.start).getTime(), end: new Date(s.end).getTime()}));
  st.events = data.events.map((e) => ({...e, t: new Date(e.ts).getTime()}));
  st.idx = -1;
  st.viewStart = dayStartOf(st.date);
  st.span = 24;
  renderTimeline();
  renderEvents();
  if (!st.segs.length) {
    video.removeAttribute('src');
    video.load();
    $('#no-video').hidden = false;
    $('#no-video').textContent = st.days.length ? '這一天沒有錄影' : '尚無錄影。請確認「設定 → 錄影」已選擇資料夾，且攝影機的錄影模式不是「關閉」。';
  }
}

async function refreshToday() {
  if (st.date !== fmtDate(new Date())) return;
  const data = await api(`/api/recordings/${st.camera}/segments?date=${st.date}`).catch(() => null);
  if (!data) return;
  const curName = st.segs[st.idx]?.name;
  st.segs = data.segments.map((s) => ({...s, start: new Date(s.start).getTime(), end: new Date(s.end).getTime()}));
  st.events = data.events.map((e) => ({...e, t: new Date(e.ts).getTime()}));
  // 片段可能已從 .rec.mp4 轉成 .mp4，依開始時間對回目前播放的片段
  if (curName) {
    const same = (s) => s.name.slice(0, 8) === curName.slice(0, 8);
    const exact = st.segs.findIndex((s) => s.name === curName);
    st.idx = exact >= 0 ? exact : st.segs.findIndex((s) => same(s) && playable(s));
  }
  renderTimeline();
  renderEvents();
}

// ---------- 播放 ----------
// 錄製中的 .rec.mp4 仍在寫入，瀏覽器播到尾端會卡住；回放只播已完成的片段（約落後即時 30 秒）
const playable = (s) => !s.name.endsWith('.rec.mp4');

/** 預設從最新的已完成錄影往前 1 分鐘開始 */
function playLatest() {
  const done = st.segs.filter(playable);
  if (!done.length) {
    $('#no-video').hidden = false;
    $('#no-video').textContent = '錄影進行中，第一段影片完成後即可回放（約 1 分鐘）';
    setTimeout(() => refreshToday().then(() => st.idx < 0 && st.segs.some(playable) && playLatest()), 15000);
    return;
  }
  playAt(Math.max(done.at(-1).end - 60e3, done[0].start));
}

function segUrl(s) {
  return `/api/recordings/${st.camera}/video/${s.date}/${s.name}`;
}

function playAt(t) {
  let i = st.segs.findIndex((s) => playable(s) && t >= s.start && t < s.end);
  if (i < 0) {
    i = st.segs.findIndex((s) => playable(s) && s.start > t);
    if (i < 0) {
      const recording = st.segs.some((s) => !playable(s) && s.end > t);
      $('#no-video').hidden = false;
      $('#no-video').textContent = recording ? '這段仍在錄製中，稍後即可回放' : '這個時間之後沒有錄影';
      return;
    }
    t = st.segs[i].start;
  }
  loadSeg(i, (t - st.segs[i].start) / 1000);
}

function loadSeg(i, offset) {
  let s = st.segs[i];
  if (!s) return;
  if (!playable(s)) {
    // 同一段若已轉檔完成，改用完成的檔案
    const done = st.segs.findIndex((x) => playable(x) && x.name.slice(0, 8) === s.name.slice(0, 8));
    if (done < 0) return;
    i = done;
    s = st.segs[i];
  }
  $('#no-video').hidden = true;
  const rate = Number($('#speed').value);
  if (st.idx === i && video.src.endsWith(segUrl(s))) {
    video.currentTime = offset;
    video.play().catch(() => {});
    return;
  }
  st.idx = i;
  video.src = segUrl(s);
  video.defaultPlaybackRate = rate;
  video.playbackRate = rate;
  video.addEventListener('loadedmetadata', () => {
    if (offset > 0) video.currentTime = Math.min(offset, (video.duration || offset + 1) - 0.5);
    video.play().catch(() => {});
  }, {once: true});
}

// 播到最後一段（可能仍在錄製）時，等新片段出現後接著播，形成「接近即時」的回放
video.addEventListener('ended', async () => {
  if (st.idx < 0) return;
  const cur = st.segs[st.idx];
  const nextIdx = st.segs.findIndex((s, i) => i > st.idx && playable(s));
  if (nextIdx >= 0) return loadSeg(nextIdx, 0);
  if (st.date !== fmtDate(new Date())) return;
  const endedAt = st.cur;
  $('#no-video').hidden = false;
  $('#no-video').textContent = '已播到最新，等待下一段影片完成…';
  for (let i = 0; i < 30; i++) {
    await new Promise((r) => setTimeout(r, 4000));
    if (st.cur !== endedAt) return; // 使用者已跳到其他時間
    await refreshToday();
    const n = st.segs.findIndex((s) => playable(s) && s.start >= cur.start && s.name.slice(0, 8) !== cur.name.slice(0, 8));
    if (n >= 0) return loadSeg(n, 0);
  }
});
video.addEventListener('error', () => {
  if (st.idx < 0 || st.cur == null) return;
  // 片段剛轉檔改名時重新整理列表，從同一個時間點再試
  const t = st.cur;
  st.idx = -1;
  refreshToday().then(() => playAt(t));
});
video.addEventListener('timeupdate', () => {
  const s = st.segs[st.idx];
  if (!s) return;
  st.cur = s.start + video.currentTime * 1000;
  $('#clock').textContent = fmtTime(st.cur);
  // 播放到畫面外時自動捲動時間軸
  const spanMs = st.span * HOUR;
  if (st.cur < st.viewStart || st.cur > st.viewStart + spanMs) {
    setView(st.cur - spanMs * 0.25);
  } else {
    updateCursor();
  }
});

// ---------- 時間軸 ----------
function setView(start) {
  const day0 = dayStartOf(st.date);
  const spanMs = st.span * HOUR;
  st.viewStart = Math.max(day0, Math.min(day0 + DAY - spanMs, start));
  renderTimeline();
}

const pct = (t) => ((t - st.viewStart) / (st.span * HOUR)) * 100;

function renderTimeline() {
  document.querySelectorAll('.zoom-switch button').forEach((b) => b.classList.toggle('active', Number(b.dataset.span) === st.span));
  const track = $('#tl-track');
  track.innerHTML = '';
  const spanMs = st.span * HOUR;
  const viewEnd = st.viewStart + spanMs;

  // 刻度
  const step = st.span >= 24 ? 2 * HOUR : st.span >= 6 ? HOUR : st.span >= 1 ? 10 * 60e3 : 60e3;
  for (let t = Math.ceil(st.viewStart / step) * step; t <= viewEnd; t += step) {
    const tick = document.createElement('div');
    tick.className = 'tl-tick';
    tick.style.left = `${pct(t)}%`;
    const d = new Date(t);
    tick.textContent = `${pad(d.getHours())}:${pad(d.getMinutes())}`;
    track.appendChild(tick);
  }

  // 錄影區段（相鄰片段合併以減少元素數量）
  const blocks = [];
  for (const s of st.segs) {
    const last = blocks.at(-1);
    const live = !playable(s);
    if (last && last.live === live && s.start - last.end < 5e3) last.end = Math.max(last.end, s.end);
    else blocks.push({start: s.start, end: s.end, live});
  }
  for (const b of blocks) {
    if (b.end < st.viewStart || b.start > viewEnd) continue;
    const el = document.createElement('div');
    el.className = b.live ? 'tl-seg live' : 'tl-seg';
    const l = Math.max(0, pct(b.start));
    el.style.left = `${l}%`;
    el.style.width = `${Math.max(0.15, Math.min(100, pct(b.end)) - l)}%`;
    track.appendChild(el);
  }

  // 事件
  for (const e of st.events) {
    if (e.t < st.viewStart || e.t > viewEnd) continue;
    const el = document.createElement('div');
    el.className = 'tl-ev';
    el.style.left = `${pct(e.t)}%`;
    el.title = `${fmtTime(e.t)} ${kindLabel(e.kind)}`;
    track.appendChild(el);
  }
  updateCursor();
  updateRange();
}

function updateCursor() {
  const c = $('#tl-cursor');
  if (st.cur == null || st.cur < st.viewStart || st.cur > st.viewStart + st.span * HOUR) {
    c.hidden = true;
    return;
  }
  c.hidden = false;
  c.style.left = `${pct(st.cur)}%`;
}

function updateRange() {
  const r = $('#tl-range');
  const has = st.markIn != null && st.markOut != null;
  $('#range').textContent = has
    ? t('{a} – {b}（{n} 秒）', {a: fmtTime(st.markIn), b: fmtTime(st.markOut), n: Math.round((st.markOut - st.markIn) / 1000)})
    : st.markIn != null ? `${fmtTime(st.markIn)} – ?` : t('未選取');
  $('#btn-export').disabled = !(has && st.markOut > st.markIn && st.markOut - st.markIn <= 2 * HOUR);
  if (!has) {
    r.hidden = true;
    return;
  }
  r.hidden = false;
  const l = Math.max(0, pct(st.markIn));
  r.style.left = `${l}%`;
  r.style.width = `${Math.max(0.2, Math.min(100, pct(st.markOut)) - l)}%`;
}

function timeFromEvent(e) {
  const rect = $('#timeline').getBoundingClientRect();
  const x = Math.min(Math.max(0, e.clientX - rect.left), rect.width);
  return st.viewStart + (x / rect.width) * st.span * HOUR;
}

$('#timeline').addEventListener('click', (e) => playAt(timeFromEvent(e)));
$('#timeline').addEventListener('mousemove', (e) => {
  const h = $('#tl-hover');
  const rect = $('#timeline').getBoundingClientRect();
  h.hidden = false;
  h.style.left = `${e.clientX - rect.left}px`;
  h.textContent = fmtTime(timeFromEvent(e));
});
$('#timeline').addEventListener('mouseleave', () => { $('#tl-hover').hidden = true; });
$('#timeline').addEventListener('wheel', (e) => {
  e.preventDefault();
  const delta = (e.deltaY || e.deltaX) > 0 ? 1 : -1;
  setView(st.viewStart + delta * st.span * HOUR * 0.15);
}, {passive: false});
document.querySelectorAll('.zoom-switch button').forEach((b) => b.onclick = () => {
  const center = st.cur ?? st.viewStart + st.span * HOUR / 2;
  st.span = Number(b.dataset.span);
  setView(center - st.span * HOUR / 2);
});

// ---------- 事件列表 ----------
function renderEvents() {
  const list = $('#events');
  list.innerHTML = '';
  $('#event-count').textContent = st.events.length ? `（${st.events.length}）` : '';
  if (!st.events.length) {
    list.innerHTML = '<li class="hint">這一天沒有事件</li>';
    return;
  }
  for (const e of [...st.events].sort((a, b) => b.t - a.t)) {
    const li = document.createElement('li');
    li.className = 'clickable';
    if (e.snapshot) {
      const img = document.createElement('img');
      img.loading = 'lazy';
      img.src = `/api/snapshots/${e.snapshot}`;
      li.appendChild(img);
    }
    const div = document.createElement('div');
    const strong = document.createElement('strong');
    strong.textContent = fmtTime(e.t);
    const span = document.createElement('span');
    span.textContent = kindLabel(e.kind);
    div.append(strong, span);
    li.appendChild(div);
    li.onclick = () => playAt(e.t - st.preRoll);
    list.appendChild(li);
  }
}

// ---------- 控制 ----------
$('#camera').onchange = async (e) => {
  st.camera = e.target.value;
  history.replaceState(null, '', `?camera=${st.camera}`);
  await loadCamera('latest');
  if (st.segs.length) playLatest();
};
$('#date').onchange = async (e) => {
  if (!e.target.value) return;
  st.date = e.target.value;
  await loadDay();
  if (st.segs.length) playAt(st.segs[0].start);
};
function shiftDay(dir) {
  const i = st.days.indexOf(st.date);
  let next;
  if (i >= 0) next = st.days[i + dir];
  else next = dir < 0 ? [...st.days].reverse().find((d) => d < st.date) : st.days.find((d) => d > st.date);
  if (!next) return toast(dir < 0 ? '沒有更早的錄影' : '沒有更晚的錄影');
  st.date = next;
  loadDay().then(() => st.segs.length && playAt(st.segs[0].start));
}
$('#prev-day').onclick = () => shiftDay(-1);
$('#next-day').onclick = () => shiftDay(1);

$('#btn-play').onclick = () => (video.paused ? video.play() : video.pause());
$('#btn-back').onclick = () => st.cur != null && playAt(st.cur - 10e3);
$('#btn-fwd').onclick = () => st.cur != null && playAt(st.cur + 30e3);
$('#btn-prev-seg').onclick = () => st.idx > 0 && loadSeg(st.idx - 1, 0);
$('#btn-next-seg').onclick = () => st.idx + 1 < st.segs.length && loadSeg(st.idx + 1, 0);
$('#speed').onchange = (e) => {
  video.playbackRate = Number(e.target.value);
  video.defaultPlaybackRate = Number(e.target.value);
};

$('#btn-snap').onclick = () => {
  if (!video.videoWidth) return toast('目前沒有畫面', 'error');
  const c = document.createElement('canvas');
  c.width = video.videoWidth;
  c.height = video.videoHeight;
  c.getContext('2d').drawImage(video, 0, 0);
  c.toBlob((blob) => {
    const a = document.createElement('a');
    const d = new Date(st.cur);
    a.href = URL.createObjectURL(blob);
    a.download = `${st.camera}_${fmtDate(d).replaceAll('-', '')}_${fmtTime(st.cur).replaceAll(':', '')}.jpg`;
    a.click();
    setTimeout(() => URL.revokeObjectURL(a.href), 5000);
  }, 'image/jpeg', 0.92);
};

$('#btn-mark-in').onclick = () => {
  if (st.cur == null) return;
  st.markIn = st.cur;
  if (st.markOut != null && st.markOut <= st.markIn) st.markOut = null;
  updateRange();
};
$('#btn-mark-out').onclick = () => {
  if (st.cur == null) return;
  st.markOut = st.cur;
  if (st.markIn == null || st.markIn >= st.markOut) st.markIn = Math.max(st.markOut - 60e3, dayStartOf(st.date));
  updateRange();
};
$('#btn-export').onclick = () => {
  const q = new URLSearchParams({
    start: new Date(st.markIn).toISOString(),
    end: new Date(st.markOut).toISOString(),
  });
  const a = document.createElement('a');
  a.href = `/api/recordings/${st.camera}/export?${q}`;
  a.download = '';
  a.click();
  toast('匯出中，完成後會自動下載…');
};

document.addEventListener('keydown', (e) => {
  if (e.target.tagName === 'INPUT' || e.target.tagName === 'SELECT') return;
  if (e.key === ' ') {
    e.preventDefault();
    $('#btn-play').click();
  } else if (e.key === 'ArrowLeft') {
    $('#btn-back').click();
  } else if (e.key === 'ArrowRight') {
    $('#btn-fwd').click();
  }
});

i18n.ready.then(init).catch((e) => toast(e.message, 'error'));
