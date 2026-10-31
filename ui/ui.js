const $ = (s) => document.querySelector(s);
const statusEl = $('#status');

// Collapsed account ids, remembered between launches.
const collapsed = new Set((() => {
  try {
    return JSON.parse(localStorage.getItem('collapsed')) || [];
  } catch {
    return [];
  }
})());
function saveCollapsed() {
  try {
    localStorage.setItem('collapsed', JSON.stringify([...collapsed]));
  } catch {
    // not critical
  }
}

// Accounts whose hidden characters are currently revealed (remembered between launches).
const revealed = new Set((() => {
  try {
    return JSON.parse(localStorage.getItem('revealed')) || [];
  } catch {
    return [];
  }
})());
function saveRevealed() {
  try {
    localStorage.setItem('revealed', JSON.stringify([...revealed]));
  } catch {
    // not critical
  }
}
// Hand-drawn gold eye. `off` adds a slash through it.
const EYE_SVG = (off) => `<svg viewBox="0 0 24 24" width="18" height="18" fill="none" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
  <path d="M1.8 12C4.4 7.2 8 5 12 5s7.6 2.2 10.2 7c-2.6 4.8-6.2 7-10.2 7S4.4 16.8 1.8 12z" stroke="var(--gold)" stroke-width="1.8"/>
  <circle cx="12" cy="12" r="3.3" fill="var(--gold)"/>
  <circle cx="13.1" cy="10.9" r="1" fill="var(--bg)"/>
  ${off ? '<path d="M4 20L20 4" stroke="var(--panel)" stroke-width="4.5"/><path d="M4 20L20 4" stroke="var(--gold)" stroke-width="2"/>' : ''}
</svg>`;
const eye = (off) => {
  const n = el('span', { class: off ? 'eye-ico off' : 'eye-ico' });
  n.innerHTML = EYE_SVG(off);
  return n;
};

function setStatus(msg, isErr = false) {
  statusEl.textContent = msg || '';
  statusEl.className = isErr ? 'err' : '';
}

async function run(label, fn, btn) {
  if (btn) btn.disabled = true;
  setStatus(label);
  try {
    await fn();
    setStatus('');
  } catch (e) {
    setStatus(e.message, true);
  } finally {
    if (btn) btn.disabled = false;
  }
}

function el(tag, attrs = {}, ...kids) {
  const n = document.createElement(tag);
  for (const [k, v] of Object.entries(attrs)) {
    if (k.startsWith('on')) n.addEventListener(k.slice(2), v);
    else n.setAttribute(k, v);
  }
  for (const k of kids) n.append(k);
  return n;
}

// Member while the RUNESCAPE (shared RS3/OSRS) entry hasn't expired; activeSubscription = auto-renewing.
function membershipOf(c) {
  const e = (c.membership || []).find((m) => m.gameGroup === 'RUNESCAPE');
  if (!e) return { member: false, text: 'F2P' };
  const exp = new Date(e.expirationDate + (/[zZ]|[+-]\d\d:?\d\d$/.test(e.expirationDate) ? '' : 'Z'));
  const date = exp.toLocaleDateString(undefined, { year: 'numeric', month: 'short', day: 'numeric' });
  if (exp > new Date()) return { member: true, text: e.activeSubscription ? `Member · renews ${date}` : `Member · until ${date}` };
  return { member: false, text: `F2P · expired ${date}` };
}

// ---- drag to reorder (accounts among accounts, characters within their account) ----
let current = null;
let drag = null;
const clearMarks = () => document.querySelectorAll('.drop-before, .drop-after').forEach((n) => n.classList.remove('drop-before', 'drop-after'));

function makeSortable(node, handle, kind, id, acctId) {
  handle.draggable = true;
  handle.addEventListener('dragstart', (e) => {
    e.stopPropagation();
    drag = { kind, id, acctId };
    e.dataTransfer.effectAllowed = 'move';
    e.dataTransfer.setData('text/plain', id);
    e.dataTransfer.setDragImage(node, 12, 12);
    node.classList.add('dragging');
  });
  handle.addEventListener('dragend', () => {
    drag = null;
    node.classList.remove('dragging');
    clearMarks();
  });
  const ok = () => drag && drag.kind === kind && drag.acctId === acctId && drag.id !== id;
  node.addEventListener('dragover', (e) => {
    if (!ok()) return;
    e.preventDefault();
    e.stopPropagation();
    const r = node.getBoundingClientRect();
    clearMarks();
    node.classList.add(e.clientY > r.top + r.height / 2 ? 'drop-after' : 'drop-before');
  });
  node.addEventListener('drop', (e) => {
    if (!ok()) return;
    e.preventDefault();
    e.stopPropagation();
    const after = node.classList.contains('drop-after');
    const from = drag.id;
    clearMarks();
    const ids = (kind === 'acct' ? current.accounts : current.accounts.find((a) => a.id === acctId).characters.map((c) => ({ id: c.accountId })))
      .map((x) => x.id)
      .filter((x) => x !== from);
    ids.splice(ids.indexOf(id) + (after ? 1 : 0), 0, from);
    run('', () => (kind === 'acct' ? api.reorderAccounts(ids) : api.reorderCharacters(acctId, ids)));
  });
}

function render(state) {
  current = state;
  $('#version').textContent = `v${state.version}`;
  $('#client').value = state.settings.client;
  $('#path-runelite').textContent = state.settings.paths.runelite || '(not set)';
  $('#path-official').textContent = state.settings.paths.official || '(not set)';

  const list = $('#accounts');
  list.replaceChildren();
  if (!state.accounts.length) {
    list.append(el('div', { class: 'empty' }, 'No accounts yet. Add a Jagex account to get started.'));
    return;
  }
  for (const a of state.accounts) {
    const refreshBtn = el('button', { title: 'Refresh characters' }, '↻');
    refreshBtn.addEventListener('click', () => run(`Refreshing ${a.name}…`, () => api.refreshCharacters(a.id), refreshBtn));
    const removeBtn = el('button', { class: 'danger', title: 'Remove account' }, 'Remove');
    let armed = null;
    removeBtn.addEventListener('click', () => {
      if (armed) {
        clearTimeout(armed);
        run('Removing…', () => api.removeAccount(a.id), removeBtn);
        return;
      }
      removeBtn.textContent = 'Sure?';
      armed = setTimeout(() => { armed = null; removeBtn.textContent = 'Remove'; }, 3000);
    });

    const hiddenCount = a.characters.filter((c) => c.hidden).length;
    const showHidden = revealed.has(a.id);
    const visible = a.characters.filter((c) => showHidden || !c.hidden);
    const plural = `${hiddenCount} hidden character${hiddenCount === 1 ? '' : 's'}`;
    const revealBtn = hiddenCount
      ? el('button', { class: 'eye', title: showHidden ? `Hide ${plural}` : `Show ${plural}` }, eye(!showHidden), el('span', { class: 'small' }, ` ${hiddenCount}`))
      : '';
    if (revealBtn) {
      revealBtn.addEventListener('click', () => {
        showHidden ? revealed.delete(a.id) : revealed.add(a.id);
        saveRevealed();
        render(current);
      });
    }

    const playing = state.running.filter((r) => r.accountId === a.id).length;
    const grip = el('span', { class: 'grip', title: 'Drag to reorder' }, '☰');
    const head = el('div', { class: 'acct-head', role: 'button', tabindex: '0' },
      grip,
      el('span', { class: 'chevron' }, '▾'),
      el('span', { class: 'acct-name' }, a.name),
      el('span', { class: 'muted count' }, `${visible.length} char${visible.length === 1 ? '' : 's'}`),
      playing ? el('span', { class: 'badge' }, `● ${playing}`) : '',
      revealBtn, refreshBtn, removeBtn);
    const card = el('div', { class: collapsed.has(a.id) ? 'acct collapsed' : 'acct' }, head);
    const toggle = (e) => {
      if (e.target.closest('button, .grip')) return;
      if (e.type === 'keydown' && e.key !== 'Enter' && e.key !== ' ') return;
      e.preventDefault();
      card.classList.toggle('collapsed') ? collapsed.add(a.id) : collapsed.delete(a.id);
      saveCollapsed();
    };
    head.addEventListener('click', toggle);
    head.addEventListener('keydown', toggle);
    makeSortable(card, grip, 'acct', a.id, null);
    const body = el('div', { class: 'acct-body' });
    card.append(body);
    if (!a.characters.length) body.append(el('div', { class: 'char muted' }, 'No characters loaded — press ↻'));
    else if (!visible.length) body.append(el('div', { class: 'char muted' }, 'All characters are hidden — use the eye above to show them'));
    for (const c of visible) {
      const n = state.running.filter((r) => r.accountId === a.id && r.characterId === c.accountId).length;
      const m = membershipOf(c);
      const launchBtn = el('button', { class: 'primary' }, 'Play');
      launchBtn.addEventListener('click', () => run(`Launching ${c.displayName || 'character'}…`, () => api.launch(a.id, c.accountId), launchBtn));
      const rowGrip = el('span', { class: 'grip', title: 'Drag to reorder' }, '☰');
      const hideBtn = el('button', { class: 'eye', title: c.hidden ? 'Unhide this character' : 'Hide this character' }, eye(c.hidden));
      hideBtn.addEventListener('click', () => run('', () => api.setHidden(a.id, c.accountId, !c.hidden), hideBtn));
      const row = el('div', { class: c.hidden ? 'char is-hidden' : 'char' },
          rowGrip,
          hideBtn,
          el('span', { class: 'char-name' }, c.displayName || el('span', { class: 'muted' }, '(no name set)')),
          el('span', { class: m.member ? 'member' : 'muted small' }, m.text),
          n ? el('span', { class: 'badge' }, `● running${n > 1 ? ` ×${n}` : ''}`) : '',
          launchBtn,
      );
      makeSortable(row, rowGrip, 'char', c.accountId, a.id);
      body.append(row);
    }
    list.append(card);
  }
}

$('#add').addEventListener('click', (e) => run('Waiting for sign-in in the popup window…', () => api.addAccount(), e.currentTarget));
$('#client').addEventListener('change', (e) => api.setClient(e.target.value));
document.querySelectorAll('[data-browse]').forEach((b) => b.addEventListener('click', () => api.browseClient(b.dataset.browse)));

api.onState(render);
api.state().then(render);

// ---- update check (latest GitHub release) ----
async function checkForUpdate(manual) {
  const banner = $('#update');
  try {
    const u = await api.checkUpdate();
    $('#version').textContent = `v${u.current}`;
    banner.replaceChildren();
    banner.hidden = !u.available;
    if (u.available) {
      const open = el('button', { class: 'primary' }, 'Download');
      open.addEventListener('click', () => run('', () => api.openLink(u.url)));
      banner.append(el('span', {}, `Update available: ${u.latest} (you have v${u.current})`), open);
    } else if (manual) {
      setStatus(`You're up to date (v${u.current}).`);
    }
  } catch (e) {
    if (manual) setStatus(e.message, true);
  }
}
$('#check-update').addEventListener('click', (e) => {
  e.currentTarget.disabled = true;
  checkForUpdate(true).finally(() => { e.currentTarget.disabled = false; });
});
// Any element with data-link opens that URL in the system browser (never in this window).
document.addEventListener('click', (e) => {
  const link = e.target.closest('[data-link]');
  if (!link) return;
  e.preventDefault();
  run('', () => api.openLink(link.dataset.link));
});
$('#feedback').addEventListener('click', () => run('', () => api.openLink('https://github.com/skeldoor/multijag/issues')));
checkForUpdate(false);

// ---- instant styled tooltips for anything with a title ----
// Native title tooltips are slow and easy to miss, so title is moved to data-tip (and
// aria-label for screen readers) the first time the pointer reaches an element.
(() => {
  const tip = $('#tip');
  let timer = null;
  let target = null;
  const hide = () => {
    clearTimeout(timer);
    target = null;
    tip.classList.remove('show');
  };
  const place = (el) => {
    tip.textContent = el.dataset.tip;
    const r = el.getBoundingClientRect();
    const t = tip.getBoundingClientRect();
    const x = Math.min(Math.max(8, r.left + r.width / 2 - t.width / 2), window.innerWidth - t.width - 8);
    let y = r.top - t.height - 8;
    if (y < 8) y = r.bottom + 8;
    tip.style.transform = `translate(${Math.round(x)}px, ${Math.round(y)}px)`;
    tip.classList.add('show');
  };
  document.addEventListener('mouseover', (e) => {
    const el = e.target.closest('[title], [data-tip]');
    if (el === target) return;
    hide();
    if (!el) return;
    if (el.hasAttribute('title')) {
      el.dataset.tip = el.getAttribute('title');
      if (!el.hasAttribute('aria-label')) el.setAttribute('aria-label', el.dataset.tip);
      el.removeAttribute('title');
    }
    target = el;
    timer = setTimeout(() => place(el), 120);
  });
  document.addEventListener('mouseout', (e) => {
    if (target && !target.contains(e.relatedTarget)) hide();
  });
  for (const ev of ['mousedown', 'dragstart', 'scroll', 'keydown']) document.addEventListener(ev, hide, true);
  window.addEventListener('blur', hide);
})();
