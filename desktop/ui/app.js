'use strict';

const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;

const $ = (sel, root = document) => root.querySelector(sel);

// ---------------------------------------------------------------- appearance

function themeChoice() {
  try { return localStorage.getItem('theme') || 'system'; } catch { return 'system'; }
}

/** "system" follows the OS; "light"/"dark" override it (page colors and the window frame). */
function applyTheme(mode) {
  if (mode === 'light' || mode === 'dark') document.documentElement.dataset.theme = mode;
  else delete document.documentElement.dataset.theme;
  try { window.__TAURI__.window?.getCurrentWindow().setTheme(mode === 'system' ? null : mode); } catch { /* preview */ }
}

function setTheme(mode) {
  try { localStorage.setItem('theme', mode); } catch { /* not persisted */ }
  applyTheme(mode);
}

applyTheme(themeChoice());
const esc = (s) =>
  String(s ?? '').replace(/[&<>"']/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' })[c]);

let snap = null;
let view = null;
const busy = new Set();

// ---------------------------------------------------------------- helpers

const ICONS = {
  sync: '<path d="M21 12a9 9 0 0 1-15.5 6.2L3 16"/><path d="M3 21v-5h5"/><path d="M3 12a9 9 0 0 1 15.5-6.2L21 8"/><path d="M21 3v5h-5"/>',
  plus: '<path d="M12 5v14M5 12h14"/>',
  gear: '<circle cx="12" cy="12" r="3"/><path d="M19.4 15a1.7 1.7 0 0 0 .3 1.8l.1.1a2 2 0 1 1-2.8 2.8l-.1-.1a1.7 1.7 0 0 0-1.8-.3 1.7 1.7 0 0 0-1 1.5V21a2 2 0 1 1-4 0v-.1a1.7 1.7 0 0 0-1.1-1.5 1.7 1.7 0 0 0-1.8.3l-.1.1a2 2 0 1 1-2.8-2.8l.1-.1a1.7 1.7 0 0 0 .3-1.8 1.7 1.7 0 0 0-1.5-1H3a2 2 0 1 1 0-4h.1a1.7 1.7 0 0 0 1.5-1.1 1.7 1.7 0 0 0-.3-1.8l-.1-.1a2 2 0 1 1 2.8-2.8l.1.1a1.7 1.7 0 0 0 1.8.3H9a1.7 1.7 0 0 0 1-1.5V3a2 2 0 1 1 4 0v.1a1.7 1.7 0 0 0 1 1.5 1.7 1.7 0 0 0 1.8-.3l.1-.1a2 2 0 1 1 2.8 2.8l-.1.1a1.7 1.7 0 0 0-.3 1.8V9a1.7 1.7 0 0 0 1.5 1H21a2 2 0 1 1 0 4h-.1a1.7 1.7 0 0 0-1.5 1z"/>',
  folder: '<path d="M3 7a2 2 0 0 1 2-2h4l2 2h8a2 2 0 0 1 2 2v8a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2z"/>',
  history: '<path d="M3 12a9 9 0 1 0 3-6.7L3 8"/><path d="M3 3v5h5"/><path d="M12 7v5l3 2"/>',
  edit: '<path d="M12 20h9"/><path d="M16.5 3.5a2.1 2.1 0 0 1 3 3L7 19l-4 1 1-4z"/>',
};
const icon = (name) => `<svg class="i" viewBox="0 0 24 24" aria-hidden="true">${ICONS[name]}</svg>`;

const rtf = new Intl.RelativeTimeFormat(undefined, { numeric: 'auto' });
function ago(ms) {
  if (!ms) return 'at an unknown time';
  const s = Math.round((ms - Date.now()) / 1000);
  if (Math.abs(s) < 45) return 'just now';
  const units = [['minute', 60], ['hour', 3600], ['day', 86400], ['week', 604800], ['month', 2629800], ['year', 31557600]];
  let [unit, div] = units[0];
  for (const [u, d] of units) if (Math.abs(s) >= d) [unit, div] = [u, d];
  return rtf.format(Math.round(s / div), unit);
}

function formatBytes(n) {
  if (n < 1024) return `${n} B`;
  const units = ['KB', 'MB', 'GB'];
  let v = n / 1024, i = 0;
  while (v >= 1024 && i < units.length - 1) { v /= 1024; i++; }
  return `${v.toFixed(v < 10 ? 1 : 0)} ${units[i]}`;
}

function slugify(name) {
  return name.normalize('NFKD').replace(/[̀-ͯ]/g, '').toLowerCase()
    .replace(/[^a-z0-9]+/g, '-').replace(/^-+|-+$/g, '').slice(0, 64);
}

function toast(message, kind = '') {
  const el = document.createElement('div');
  el.className = `toast ${kind}`;
  el.textContent = message;
  $('#toasts').append(el);
  setTimeout(() => el.remove(), kind === 'error' ? 7000 : 3500);
}

/** Calls a command; failures are shown as a toast and re-thrown. */
async function call(cmd, args) {
  try {
    return await invoke(cmd, args);
  } catch (e) {
    toast(String(e), 'error');
    throw e;
  }
}

/** Runs an action with its button showing progress. */
async function withBusy(key, fn) {
  if (busy.has(key)) return;
  busy.add(key);
  render();
  try { await fn(); } catch { /* already shown */ } finally { busy.delete(key); await refresh(); }
}

const gameById = (id) => snap?.games.find((g) => g.config.id === id);
const deviceName = (v) => esc(v?.device_name || 'another device');

// ---------------------------------------------------------------- data

async function refresh() {
  try {
    snap = await invoke('get_snapshot');
  } catch (e) {
    toast(String(e), 'error');
    return;
  }
  render();
}

let refreshTimer;
function scheduleRefresh() {
  clearTimeout(refreshTimer);
  refreshTimer = setTimeout(refresh, 150);
}

// ---------------------------------------------------------------- views

function render() {
  if (!snap) return;
  const next = snap.server ? 'main' : 'setup';
  // Don't rebuild the setup form under the user's fingers.
  if (next === 'main') renderMain();
  else if (view !== 'setup') renderSetup();
  view = next;
}

function renderSetup() {
  $('#app').innerHTML = `
    <main class="setup">
      <div class="setup-card">
        <img src="icon.png" class="logo" alt="">
        <h1>Connect to your SaveSync server</h1>
        <p class="muted">Saves sync through the SaveSync server on your NAS. You only need to do this once on each device.</p>
        <form id="pair-form" class="form">
          <label>Server address
            <input name="url" required placeholder="http://192.168.1.50:8420" autocomplete="off" spellcheck="false">
            <span class="hint">Your NAS's name or IP address, with port 8420.</span>
          </label>
          <label>Enroll key
            <input name="key" type="password" required autocomplete="off">
            <span class="hint">The <code>SAVESYNC_ENROLL_KEY</code> value from your docker-compose.yml.</span>
          </label>
          <label>This computer's name
            <input name="name" required value="${esc(snap.suggested_device_name)}">
            <span class="hint">Your other devices will show it, e.g. “New save from ${esc(snap.suggested_device_name)}”.</span>
          </label>
          <button class="btn primary" type="submit">Connect</button>
        </form>
      </div>
    </main>`;
  $('#pair-form').addEventListener('submit', async (e) => {
    e.preventDefault();
    const form = new FormData(e.target);
    const button = e.target.querySelector('button[type=submit]');
    button.disabled = true;
    button.textContent = 'Connecting…';
    try {
      await call('pair', { serverUrl: form.get('url'), enrollKey: form.get('key'), deviceName: form.get('name') });
      toast('Connected to your server');
      await refresh();
    } catch {
      button.disabled = false;
      button.textContent = 'Connect';
    }
  });
}

function renderMain() {
  const games = snap.games;
  const syncing = busy.has('sync');
  $('#app').innerHTML = `
    <header class="topbar">
      <div class="brand"><img src="icon.png" alt=""><span>SaveSync</span></div>
      <span class="pill ${snap.online ? 'ok' : 'off'}"><i></i>${snap.online ? 'Online' : 'Offline'}</span>
      <div class="spacer"></div>
      <button class="btn ghost ${syncing ? 'spin' : ''}" data-act="sync" ${syncing ? 'disabled' : ''}>${icon('sync')}<span class="label">${syncing ? 'Syncing…' : 'Sync now'}</span></button>
      <button class="btn primary" data-act="add">${icon('plus')}<span class="label">Add save</span></button>
      <button class="btn icon ghost" data-act="settings" title="Settings" aria-label="Settings">${icon('gear')}</button>
    </header>
    <main class="content">
      ${snap.online ? '' : `<div class="banner">Can't reach ${esc(snap.server.url)} right now. Saves are still captured on this computer and will upload once it's reachable.</div>`}
      ${games.length ? `<div class="grid">${games.map(card).join('')}</div>` : emptyState()}
    </main>`;
}

function emptyState() {
  return `
    <div class="empty">
      <img src="icon.png" alt="">
      <h2>No games yet</h2>
      <p>Add a save and point SaveSync at its folder. Give it the same Save ID on each of your devices so they sync together.</p>
      <button class="btn primary" data-act="add">${icon('plus')} Add your first save</button>
    </div>`;
}

function status(g) {
  const id = g.config.id;
  const isBusy = busy.has(id) ? 'disabled' : '';
  if (g.conflict) {
    const c = g.conflict;
    const mine = c.local_played_at ? ` (played ${ago(c.local_played_at)})` : '';
    return {
      kind: 'conflict',
      label: 'Conflict',
      detail: `This computer${mine} and ${deviceName(c.remote)} (played ${ago(c.remote.played_at || c.remote.created_at)}) both changed this save.`,
      actions: `
        <div class="actions">
          <button class="btn" data-act="conflict-local" ${isBusy}>Keep this computer's</button>
          <button class="btn primary" data-act="conflict-remote" ${isBusy}>Use ${deviceName(c.remote)}'s</button>
        </div>
        <p class="hint">Nothing is lost: replaced saves are kept as backups and in the server's history.</p>`,
    };
  }
  if (g.in_session) {
    return {
      kind: 'playing',
      label: 'Playing',
      detail: g.staged
        ? `A newer save from ${deviceName(g.staged)} is waiting until you close the game.`
        : 'Your progress will upload when you close the game.',
    };
  }
  if (g.staged) {
    return {
      kind: 'import',
      label: 'New save',
      detail: `From ${deviceName(g.staged)}, played ${ago(g.staged.played_at || g.staged.created_at)}.`,
      actions: `
        <div class="actions">
          <button class="btn" data-act="keep-mine" ${isBusy}>Keep mine</button>
          <button class="btn primary" data-act="import" ${isBusy}>Import</button>
        </div>`,
    };
  }
  if (g.last_error) return { kind: 'error', label: 'Needs attention', detail: esc(g.last_error) };
  if (g.queued_upload) {
    const when = ago(g.queued_upload.played_at || g.queued_upload.created_at);
    return snap.online
      ? { kind: 'queued', label: 'Uploading', detail: `Played ${when}.` }
      : { kind: 'queued', label: 'Waiting to upload', detail: `Played ${when}. It will upload once the server is reachable.` };
  }
  if (g.base_version === 0) {
    return { kind: 'idle', label: 'No saves yet', detail: 'Nothing synced yet. Play it here or on another device.' };
  }
  return { kind: 'synced', label: 'Synced', detail: `Up to date (version ${g.base_version}).` };
}

function card(g) {
  const st = status(g);
  return `
    <article class="card ${st.kind}" data-id="${esc(g.config.id)}">
      <div class="card-head">
        <h2>${esc(g.config.name)}</h2>
        <span class="badge ${st.kind}">${st.label}</span>
      </div>
      <p class="detail">${st.detail}</p>
      ${st.actions || ''}
      <footer class="card-foot">
        <span class="path" title="${esc(g.config.location)}">${icon('folder')}<span>${esc(g.config.location)}</span></span>
        <div class="foot-actions">
          <button class="btn icon sm ghost" data-act="backups" title="Backups" aria-label="Backups">${icon('history')}</button>
          <button class="btn icon sm ghost" data-act="edit" title="Edit" aria-label="Edit">${icon('edit')}</button>
        </div>
      </footer>
    </article>`;
}

// ---------------------------------------------------------------- modal

const modal = $('#modal');
let onModalClose = null;

function openModal(html, onClose) {
  modal.innerHTML = html;
  onModalClose = onClose || null;
  if (!modal.open) modal.showModal();
  modal.querySelectorAll('[data-close]').forEach((b) => b.addEventListener('click', closeModal));
}

function closeModal() {
  if (modal.open) modal.close();
}

modal.addEventListener('close', () => {
  const cb = onModalClose;
  onModalClose = null;
  if (cb) cb();
});

function confirmDialog({ title, body, ok = 'OK', danger = false }) {
  return new Promise((resolve) => {
    openModal(
      `<h2>${esc(title)}</h2>
       <p class="body">${body}</p>
       <div class="modal-actions">
         <button class="btn" data-close>Cancel</button>
         <button class="btn ${danger ? 'danger solid' : 'primary'}" data-ok>${esc(ok)}</button>
       </div>`,
      () => resolve(false),
    );
    $('[data-ok]', modal).addEventListener('click', () => {
      onModalClose = null;
      closeModal();
      resolve(true);
    });
  });
}

// ---------------------------------------------------------------- game form

function chipsHtml(field, values, placeholder, listId = '') {
  return `
    <div class="chips" data-field="${field}">
      ${values.map((v) => `<span class="chip">${esc(v)}<button type="button" data-remove="${esc(v)}" aria-label="Remove ${esc(v)}">×</button></span>`).join('')}
      <input data-chip-input placeholder="${esc(placeholder)}" ${listId ? `list="${listId}"` : ''} spellcheck="false">
    </div>`;
}

function gameForm(game) {
  const editing = !!game;
  const c = game?.config ?? { id: '', name: '', location: '', include: [], ignore: [], processes: [], import_policy: 'ask' };
  const lists = { include: [...c.include], ignore: [...c.ignore], processes: [...c.processes] };
  let idTouched = editing;

  const renderChips = (field, placeholder, listId) => {
    const box = $(`.chips[data-field=${field}]`, modal);
    box.outerHTML = chipsHtml(field, lists[field], placeholder, listId);
    wireChips(field, placeholder, listId);
  };
  const addChip = (field, raw, placeholder, listId) => {
    const values = raw.split(',').map((v) => v.trim()).filter(Boolean);
    let changed = false;
    for (const v of values) if (!lists[field].includes(v)) { lists[field].push(v); changed = true; }
    if (changed) renderChips(field, placeholder, listId);
    return changed;
  };
  const wireChips = (field, placeholder, listId) => {
    const box = $(`.chips[data-field=${field}]`, modal);
    const input = $('[data-chip-input]', box);
    box.addEventListener('click', (e) => {
      const remove = e.target.closest('[data-remove]');
      if (remove) {
        lists[field] = lists[field].filter((v) => v !== remove.dataset.remove);
        renderChips(field, placeholder, listId);
      } else {
        input.focus();
      }
    });
    input.addEventListener('keydown', (e) => {
      if ((e.key === 'Enter' || e.key === ',') && input.value.trim()) {
        e.preventDefault();
        addChip(field, input.value, placeholder, listId);
        $(`.chips[data-field=${field}] [data-chip-input]`, modal).focus();
      } else if (e.key === 'Enter') {
        e.preventDefault();
      } else if (e.key === 'Backspace' && !input.value && lists[field].length) {
        lists[field].pop();
        renderChips(field, placeholder, listId);
        $(`.chips[data-field=${field}] [data-chip-input]`, modal).focus();
      }
    });
    input.addEventListener('change', () => {
      // Picking from the suggestion list.
      if (input.value.trim() && listId) addChip(field, input.value, placeholder, listId);
    });
  };

  const FIELDS = {
    include: ['Add a file name or pattern, e.g. *.sav', ''],
    ignore: ['Add a file name or pattern, e.g. *.bak', ''],
    processes: ['Add an App/Game...', 'running-processes'],
  };
  const ID_PATTERN = /^[a-z0-9][a-z0-9._-]{0,63}$/;
  const folderExample = snap.platform === 'windows' ? 'e.g. C:\\Users\\you\\Documents\\My Game\\saves'
    : snap.platform === 'macos' ? 'e.g. /Users/you/Documents/My Game/saves'
    : 'e.g. /home/you/My Game/saves';

  openModal(`
    <form id="game-form" class="form" novalidate>
      <h2>${editing ? `Edit ${esc(c.name)}` : 'Add a save'}</h2>
      <label>Name
        <input name="name" value="${esc(c.name)}" placeholder="Game name">
      </label>
      <label>Save ID
        <input name="id" value="${esc(c.id)}" ${editing ? 'readonly' : ''} spellcheck="false" autocomplete="off">
        <span class="hint" id="id-hint">${editing ? "Can't be changed." : 'Use the same Save ID on every device. Lowercase letters, numbers, dashes, underscores and dots only, with no spaces, e.g. my-save-1.'}</span>
      </label>
      <label>Save folder
        <div class="row">
          <input name="location" value="${esc(c.location)}" spellcheck="false" placeholder="${esc(folderExample)}">
          <button type="button" class="btn" data-act="browse">Browse…</button>
        </div>
      </label>
      <fieldset>
        <legend>Only sync <span class="muted">(optional)</span></legend>
        ${chipsHtml('include', lists.include, FIELDS.include[0])}
      </fieldset>
      <fieldset>
        <legend>Never sync <span class="muted">(optional)</span></legend>
        ${chipsHtml('ignore', lists.ignore, FIELDS.ignore[0])}
      </fieldset>
      <fieldset>
        <legend>App/Game <span class="muted">(recommended)</span></legend>
        <div class="row">
          ${chipsHtml('processes', lists.processes, FIELDS.processes[0], 'running-processes')}
          <button type="button" class="btn" data-act="browse-app">Browse…</button>
        </div>
        <datalist id="running-processes"></datalist>
        <span class="hint">While this app/game is open, SaveSync won't replace your save with one from another device, and it uploads your progress only after you close it.</span>
      </fieldset>
      <fieldset>
        <legend>When another device has a newer save</legend>
        <div class="choices">
          <label class="choice"><input type="radio" name="policy" value="ask" ${c.import_policy === 'ask' ? 'checked' : ''}>
            <div><b>Ask me</b><span>Download it and let me choose when to import.</span></div></label>
          <label class="choice"><input type="radio" name="policy" value="auto_when_safe" ${c.import_policy === 'auto_when_safe' ? 'checked' : ''}>
            <div><b>Import automatically</b><span>Unless this computer has unsynced progress.</span></div></label>
        </div>
      </fieldset>
      ${editing ? `
        <div class="danger-zone">
          <button type="button" class="btn danger" data-act="remove-game">Stop syncing</button>
        </div>` : ''}
      <div class="modal-actions">
        <button type="button" class="btn" data-close>Cancel</button>
        <button type="submit" class="btn primary">${editing ? 'Save changes' : 'Add save'}</button>
      </div>
    </form>`);

  for (const [field, [placeholder, listId]] of Object.entries(FIELDS)) wireChips(field, placeholder, listId);

  const form = $('#game-form', modal);
  // form.elements, not form.id / form.name: those are the form's own attributes.
  const fields = form.elements;
  const submit = $('button[type=submit]', form);
  let saving = false;

  /** Red Save ID box for invalid characters; the submit button stays disabled until everything is filled in correctly. */
  const validate = () => {
    const id = fields.id.value.trim();
    const idInvalid = id !== '' && !ID_PATTERN.test(id);
    fields.id.classList.toggle('invalid', idInvalid);
    $('#id-hint', form).classList.toggle('error', idInvalid);
    submit.disabled = saving || !fields.name.value.trim() || !ID_PATTERN.test(id) || !fields.location.value.trim();
  };

  fields.name.addEventListener('input', () => { if (!idTouched) fields.id.value = slugify(fields.name.value); validate(); });
  fields.id.addEventListener('input', () => { idTouched = true; validate(); });
  fields.location.addEventListener('input', validate);

  $('[data-act=browse]', form).addEventListener('click', async () => {
    const folder = await call('pick_folder').catch(() => null);
    if (folder) { fields.location.value = folder; validate(); }
  });

  $('[data-act=browse-app]', form).addEventListener('click', async () => {
    const name = await call('pick_app').catch(() => null);
    if (name) addChip('processes', name, FIELDS.processes[0], FIELDS.processes[1]);
  });
  validate();

  invoke('running_processes').then((names) => {
    $('#running-processes', modal).innerHTML = names.map((n) => `<option value="${esc(n)}">`).join('');
  }).catch(() => {});

  $('[data-act=remove-game]', form)?.addEventListener('click', async () => {
    const ok = await confirmDialog({
      title: `Stop syncing ${c.name}?`,
      body: "SaveSync stops watching this game on this computer. Your save files and the server's copies aren't deleted.",
      ok: 'Stop syncing',
      danger: true,
    });
    if (ok) await withBusy(c.id, async () => { await call('remove_game', { id: c.id }); toast(`Stopped syncing ${c.name}`); });
  });

  form.addEventListener('submit', async (e) => {
    e.preventDefault();
    validate();
    if (submit.disabled) return;
    // Include anything typed but not yet turned into a chip.
    for (const [field, [placeholder, listId]] of Object.entries(FIELDS)) {
      const pending = $(`.chips[data-field=${field}] [data-chip-input]`, modal).value;
      if (pending.trim()) addChip(field, pending, placeholder, listId);
    }
    const config = {
      id: fields.id.value.trim(),
      name: fields.name.value.trim(),
      location: fields.location.value.trim(),
      include: lists.include,
      ignore: lists.ignore,
      processes: lists.processes,
      import_policy: fields.policy.value,
    };
    saving = true;
    validate();
    try {
      await call(editing ? 'update_game' : 'add_game', { config });
      closeModal();
      toast(editing ? 'Saved' : `Added ${config.name}`);
      await refresh();
    } catch {
      saving = false;
      validate();
    }
  });
}

// ---------------------------------------------------------------- backups

async function backupsDialog(game) {
  const backups = await call('list_backups', { id: game.config.id }).catch(() => null);
  if (!backups) return;
  openModal(`
    <h2>Backups of ${esc(game.config.name)}</h2>
    <p class="body">SaveSync backs up this computer's save before replacing it. Restoring one puts it back and syncs it as the newest version.</p>
    ${backups.length ? `<div class="list">${backups.map((b) => `
      <div class="list-item">
        <div><b>${esc(new Date(b.created_at).toLocaleString())}</b>
          <span>${esc(b.reason)} · ${b.file_count} file${b.file_count === 1 ? '' : 's'}, ${formatBytes(b.total_size)}</span></div>
        <button class="btn sm" data-restore="${b.id}">Restore</button>
      </div>`).join('')}</div>` : '<p class="muted" style="margin:14px 0 18px">No backups yet. One is made before each import.</p>'}
    <div class="modal-actions"><button class="btn" data-close>Close</button></div>`);
  modal.querySelectorAll('[data-restore]').forEach((button) => button.addEventListener('click', async () => {
    const id = Number(button.dataset.restore);
    const ok = await confirmDialog({
      title: 'Restore this backup?',
      body: `The current save of ${esc(game.config.name)} is backed up first, then this one is restored and synced to your other devices.`,
      ok: 'Restore',
    });
    if (ok) await withBusy(game.config.id, async () => { await call('restore_backup', { backupId: id }); toast('Backup restored'); });
  }));
}

// ---------------------------------------------------------------- settings

function updateText(u) {
  const v = `SaveSync ${esc(u.current)}`;
  switch (u.state) {
    case 'checking': return `${v} · checking for updates…`;
    case 'downloading': return `${v} · downloading ${esc(u.latest)}…`;
    case 'ready': return `${v} · <b>version ${esc(u.latest)} is ready</b> (installs automatically when no game is running)`;
    case 'installing': return `${v} · installing ${esc(u.latest)}…`;
    case 'up_to_date': return `${v} · up to date`;
    case 'error': return `${v} · update problem: ${esc(u.message)}`;
    case 'unsupported': return `${v} · ${esc(u.message)}`;
    default: return v;
  }
}

function settingsDialog() {
  const s = snap.server;
  openModal(`
    <h2>Settings</h2>
    <div class="section">
      <h3>Server</h3>
      <form id="url-form" class="row">
        <input name="url" value="${esc(s.url)}" spellcheck="false" aria-label="Server address">
        <button class="btn" type="submit">Save</button>
      </form>
      <span class="hint">Change this if your NAS's address changes.</span>
    </div>
    <div class="section">
      <h3>This computer</h3>
      <dl class="kv"><dt>Name</dt><dd>${esc(s.device_name)}</dd><dt>Device ID</dt><dd><code>${esc(s.device_id)}</code></dd></dl>
      <label class="switch">Start SaveSync when I log in <input type="checkbox" id="autostart" ${snap.autostart ? 'checked' : ''}></label>
    </div>
    <div class="section">
      <h3>Appearance</h3>
      <div class="pills" role="group" aria-label="Theme">
        ${['system', 'light', 'dark'].map((m) => `<button data-theme-choice="${m}" aria-pressed="${themeChoice() === m}">${m === 'system' ? 'System' : m === 'light' ? 'Light' : 'Dark'}</button>`).join('')}
      </div>
    </div>
    <div class="section">
      <h3>Updates</h3>
      <div class="row" style="align-items:center">
        <span style="flex:1">${updateText(snap.update)}</span>
        ${snap.update.state === 'ready'
          ? '<button class="btn primary" id="install-update">Restart and update</button>'
          : `<button class="btn" id="check-update" ${['checking', 'downloading', 'unsupported'].includes(snap.update.state) ? 'disabled' : ''}>Check now</button>`}
      </div>
    </div>
    <div class="modal-actions split">
      <button class="btn danger" id="unpair">Disconnect from server</button>
      <button class="btn" data-close>Done</button>
    </div>`);

  modal.querySelectorAll('[data-theme-choice]').forEach((b) => b.addEventListener('click', () => {
    setTheme(b.dataset.themeChoice);
    modal.querySelectorAll('[data-theme-choice]').forEach((x) => x.setAttribute('aria-pressed', String(x === b)));
  }));
  $('#check-update', modal)?.addEventListener('click', async (e) => {
    e.target.disabled = true;
    e.target.textContent = 'Checking…';
    await call('check_for_updates').catch(() => {});
    await refresh();
    settingsDialog();
  });
  $('#install-update', modal)?.addEventListener('click', () => call('install_update').catch(() => {}));
  $('#url-form', modal).addEventListener('submit', async (e) => {
    e.preventDefault();
    const url = e.target.url.value.trim();
    await call('set_server_url', { url }).then(() => { toast('Server address saved'); refresh(); }).catch(() => {});
  });
  $('#autostart', modal).addEventListener('change', async (e) => {
    await call('set_autostart', { enabled: e.target.checked }).catch(() => { e.target.checked = !e.target.checked; });
  });
  $('#unpair', modal).addEventListener('click', async () => {
    const ok = await confirmDialog({
      title: 'Disconnect from the server?',
      body: "This computer stops syncing until you connect again. Nothing is deleted.",
      ok: 'Disconnect',
      danger: true,
    });
    if (ok) { await call('unpair').catch(() => {}); await refresh(); }
  });
}

// ---------------------------------------------------------------- actions

document.addEventListener('click', async (e) => {
  const button = e.target.closest('#app [data-act]');
  if (!button) return;
  const id = button.closest('[data-id]')?.dataset.id;
  const game = id && gameById(id);
  switch (button.dataset.act) {
    case 'sync':
      return withBusy('sync', () => call('sync_now'));
    case 'add':
      return gameForm(null);
    case 'settings':
      return settingsDialog();
    case 'edit':
      return gameForm(game);
    case 'backups':
      return backupsDialog(game);
    case 'import':
      return withBusy(id, async () => { await call('import_save', { id }); toast(`Imported the new save for ${game.config.name}`); });
    case 'keep-mine': {
      const ok = await confirmDialog({
        title: 'Keep this computer\'s save?',
        body: `It becomes the newest version of ${esc(game.config.name)} and replaces the save from ${deviceName(game.staged)} on your other devices.`,
        ok: 'Keep mine',
      });
      if (ok) return withBusy(id, () => call('keep_local', { id }));
      return;
    }
    case 'conflict-local':
      return withBusy(id, async () => { await call('resolve_conflict', { id, resolution: 'keep_local' }); toast('Kept this computer\'s save'); });
    case 'conflict-remote':
      return withBusy(id, async () => { await call('resolve_conflict', { id, resolution: 'keep_remote' }); toast(`Using the save from ${game.conflict.remote.device_name || 'your other device'}`); });
  }
});

// ---------------------------------------------------------------- start

listen('update-status', () => scheduleRefresh());

listen('engine-event', ({ payload }) => {
  scheduleRefresh();
  if (payload.type === 'uploaded' && payload.created) {
    const g = gameById(payload.game_id);
    toast(`Uploaded your progress in ${g ? g.config.name : payload.game_id}`);
  }
});

// Keep "played 5 minutes ago" and the online state current.
setInterval(() => { if (!modal.open) refresh(); }, 20000);
refresh();
