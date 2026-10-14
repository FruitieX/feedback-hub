// Triage UI: browse feedback from every app, look at screenshots and
// diagnostics, change status, comment, and queue items for coding agents.

interface Feedback {
  id: number;
  app: string;
  kind: string;
  message: string;
  reporter: string | null;
  page_url: string | null;
  page_title: string | null;
  user_agent: string | null;
  source: string;
  context: Record<string, unknown>;
  status: string;
  agent: string | null;
  attempts: number;
  screenshot_count: number;
  created_at: string;
  updated_at: string;
}
interface Detail extends Feedback {
  screenshots: { idx: number; mime: string; size: number }[];
  events: { id: number; at: string; author: string; kind: string; body: string }[];
}
interface AppInfo {
  id: string;
  name?: string;
  repo?: string;
}

const STATUSES = ['open', 'queued', 'in_progress', 'resolved', 'dismissed'];
const AGENTS = ['claude', 'codex', 'hermes'];
const TOKEN_KEY = 'feedbackHub.token';
const app = document.getElementById('app')!;

const storage = (() => {
  try {
    return window.localStorage;
  } catch {
    return null;
  }
})();

const state = {
  token: storage?.getItem(TOKEN_KEY) ?? '',
  statuses: new Set(['open', 'queued', 'in_progress']),
  app: '',
  items: [] as Feedback[],
  apps: [] as AppInfo[],
  selected: Number(new URLSearchParams(location.hash.slice(1)).get('id')) || null,
  detail: null as Detail | null,
  imageUrls: new Map<string, string>(),
  error: '',
};

const esc = (text: unknown) =>
  String(text ?? '').replace(/[&<>"']/g, (c) => `&#${c.charCodeAt(0)};`);

function ago(iso: string) {
  const seconds = (Date.now() - new Date(iso).getTime()) / 1000;
  if (seconds < 90) return 'just now';
  if (seconds < 5400) return `${Math.round(seconds / 60)} min ago`;
  if (seconds < 129600) return `${Math.round(seconds / 3600)} h ago`;
  return `${Math.round(seconds / 86400)} d ago`;
}

async function api<T>(path: string, init: RequestInit = {}): Promise<T> {
  const response = await fetch(path, {
    ...init,
    headers: { authorization: `Bearer ${state.token}`, 'content-type': 'application/json', ...init.headers },
  });
  if (response.status === 401) {
    state.token = '';
    storage?.removeItem(TOKEN_KEY);
    render();
    throw new Error('Unauthorized');
  }
  if (!response.ok) {
    const body = await response.json().catch(() => ({}));
    throw new Error(body.error ?? `HTTP ${response.status}`);
  }
  return response.status === 204 || response.status === 201 ? (undefined as T) : response.json();
}

async function imageUrl(id: number, idx: number) {
  const key = `${id}/${idx}`;
  if (!state.imageUrls.has(key)) {
    const response = await fetch(`/api/feedback/${id}/screenshots/${idx}`, {
      headers: { authorization: `Bearer ${state.token}` },
    });
    state.imageUrls.set(key, URL.createObjectURL(await response.blob()));
  }
  return state.imageUrls.get(key)!;
}

async function loadList() {
  const params = new URLSearchParams({ status: [...state.statuses].join(',') || 'all', limit: '300' });
  if (state.app) params.set('app', state.app);
  state.items = await api<Feedback[]>(`/api/feedback?${params}`);
}

async function loadDetail() {
  state.detail = state.selected ? await api<Detail>(`/api/feedback/${state.selected}`) : null;
}

async function refresh() {
  try {
    state.error = '';
    await Promise.all([loadList(), loadDetail()]);
  } catch (error) {
    state.error = String(error);
  }
  render();
}

function select(id: number | null) {
  state.selected = id;
  history.replaceState(null, '', id ? `#id=${id}` : '#');
  void refresh();
}

function renderLogin() {
  app.innerHTML = `
    <main class="login">
      <form>
        <h1>Feedback Hub</h1>
        <p>Enter the admin token to view feedback.</p>
        <input type="password" name="token" placeholder="Admin token" autocomplete="current-password" required>
        <button class="primary">Sign in</button>
      </form>
    </main>`;
  app.querySelector('form')!.addEventListener('submit', (event) => {
    event.preventDefault();
    state.token = (app.querySelector('input') as HTMLInputElement).value.trim();
    storage?.setItem(TOKEN_KEY, state.token);
    void init();
  });
}

function renderList() {
  const counts = new Map<string, number>();
  for (const item of state.items) counts.set(item.app, (counts.get(item.app) ?? 0) + 1);
  const appIds = [...new Set([...state.apps.map((a) => a.id), ...counts.keys(), state.app].filter(Boolean))].sort();
  return `
    <aside class="list">
      <div class="filters">
        <div class="chips">
          ${STATUSES.map((s) => `<button class="chip ${state.statuses.has(s) ? 'on' : ''}" data-status="${s}">${s.replace('_', ' ')}</button>`).join('')}
        </div>
        <select name="app" aria-label="App">
          <option value="">All apps</option>
          ${appIds.map((id) => `<option value="${esc(id)}" ${id === state.app ? 'selected' : ''}>${esc(id)}</option>`).join('')}
        </select>
      </div>
      ${state.error ? `<p class="error">${esc(state.error)}</p>` : ''}
      <ul>
        ${
          state.items.length === 0
            ? '<li class="empty">Nothing here. 🎉</li>'
            : state.items
                .map(
                  (item) => `
          <li>
            <button class="item ${item.id === state.selected ? 'selected' : ''}" data-id="${item.id}">
              <span class="row"><strong>#${item.id}</strong> <span class="app">${esc(item.app)}</span>
                <span class="pill ${item.status}">${item.status.replace('_', ' ')}</span></span>
              <span class="msg">${esc(item.message.split('\n')[0] || '(screenshot only)')}</span>
              <span class="meta">${{ bug: '🐞', idea: '💡', other: '💬' }[item.kind] ?? ''} ${ago(item.created_at)}${
                item.reporter ? ` · ${esc(item.reporter)}` : ''
              }${item.screenshot_count ? ` · 🖼 ${item.screenshot_count}` : ''}</span>
            </button>
          </li>`,
                )
                .join('')
        }
      </ul>
    </aside>`;
}

function renderLogs(title: string, entries: unknown) {
  if (!Array.isArray(entries) || entries.length === 0) return '';
  const lines = entries
    .map((e) =>
      typeof e === 'object' && e
        ? Object.values(e as Record<string, unknown>).map(String).join('  ')
        : String(e),
    )
    .join('\n');
  return `<details><summary>${title} (${entries.length})</summary><pre>${esc(lines)}</pre></details>`;
}

function renderDetail() {
  const d = state.detail;
  if (!d) return `<section class="detail empty-detail"><p>Select feedback to see details.</p></section>`;
  const { logs, network, ...context } = d.context as Record<string, unknown>;
  const repo = state.apps.find((a) => a.id === d.app)?.repo;
  const meta: [string, unknown][] = [
    ['App', d.app + (repo ? ` (${repo})` : '')],
    ['Reported', `${new Date(d.created_at).toLocaleString()} via ${d.source}${d.reporter ? ` by ${d.reporter}` : ''}`],
    ['Page', d.page_url ? `${d.page_url}${d.page_title ? ` — ${d.page_title}` : ''}` : null],
    ['Browser', d.user_agent],
    ...Object.entries(context).map(([k, v]) => [k, typeof v === 'string' ? v : JSON.stringify(v)] as [string, unknown]),
    ['Agent', d.agent ? `${d.agent} (${d.attempts} attempt${d.attempts === 1 ? '' : 's'})` : null],
  ];
  return `
    <section class="detail">
      <button class="back" data-back>← All feedback</button>
      <header>
        <h2>#${d.id} · ${{ bug: '🐞 Problem', idea: '💡 Idea', other: '💬 Other' }[d.kind] ?? d.kind}</h2>
        <span class="pill ${d.status}">${d.status.replace('_', ' ')}</span>
      </header>
      <blockquote>${esc(d.message) || '<em>No text — see screenshots.</em>'}</blockquote>
      ${
        d.screenshots.length
          ? `<div class="gallery">${d.screenshots
              .map((s) => `<a target="_blank" data-shot="${s.idx}"><img alt="Screenshot ${s.idx + 1}" data-shot-img="${s.idx}"></a>`)
              .join('')}</div>`
          : ''
      }
      <dl>${meta
        .filter(([, v]) => v !== null && v !== undefined && v !== '')
        .map(([k, v]) => `<dt>${esc(k)}</dt><dd>${esc(v)}</dd>`)
        .join('')}</dl>
      ${renderLogs('Console messages', logs)}
      ${renderLogs('Failed requests', network)}

      <div class="actions">
        <form class="queue">
          <label>Send to agent
            <select name="agent">${AGENTS.map((a) => `<option ${a === (d.agent ?? 'claude') ? 'selected' : ''}>${a}</option>`).join('')}</select>
          </label>
          <input name="note" placeholder="Extra instructions (optional)">
          <button class="primary">Queue</button>
        </form>
        <form class="status-form">
          <label>Status
            <select name="status">${STATUSES.map((s) => `<option ${s === d.status ? 'selected' : ''}>${s}</option>`).join('')}</select>
          </label>
          <input name="note" placeholder="Note (optional)">
          <button>Update</button>
        </form>
        <p class="cli">Agents: <code>feedback-hub show ${d.id}</code></p>
      </div>

      <h3>Timeline</h3>
      <ol class="timeline">
        ${d.events
          .map(
            (e) => `<li class="${e.kind}"><div class="ev-head"><strong>${esc(e.author)}</strong> <span>${esc(e.kind)} · ${ago(e.at)}</span></div><pre>${esc(e.body)}</pre></li>`,
          )
          .join('') || '<li class="empty">No activity yet.</li>'}
      </ol>
      <form class="comment">
        <textarea name="body" rows="2" placeholder="Add a comment"></textarea>
        <button>Comment</button>
      </form>
      <button class="danger" data-delete>Delete feedback</button>
    </section>`;
}

function render() {
  if (!state.token) return renderLogin();
  app.innerHTML = `
    <div class="layout ${state.selected ? 'has-detail' : ''}">
      <header class="top"><h1>Feedback Hub</h1><button class="link" data-logout>Sign out</button></header>
      ${renderList()}
      ${renderDetail()}
    </div>`;
  bind();
}

function bind() {
  app.querySelector('[data-logout]')?.addEventListener('click', () => {
    storage?.removeItem(TOKEN_KEY);
    state.token = '';
    render();
  });
  for (const chip of app.querySelectorAll<HTMLButtonElement>('[data-status]')) {
    chip.addEventListener('click', () => {
      const s = chip.dataset.status!;
      if (state.statuses.has(s)) state.statuses.delete(s);
      else state.statuses.add(s);
      void refresh();
    });
  }
  app.querySelector<HTMLSelectElement>('select[name=app]')?.addEventListener('change', (e) => {
    state.app = (e.target as HTMLSelectElement).value;
    void refresh();
  });
  for (const item of app.querySelectorAll<HTMLButtonElement>('.item')) {
    item.addEventListener('click', () => select(Number(item.dataset.id)));
  }
  app.querySelector('[data-back]')?.addEventListener('click', () => select(null));
  const d = state.detail;
  if (!d) return;
  for (const img of app.querySelectorAll<HTMLImageElement>('[data-shot-img]')) {
    const idx = Number(img.dataset.shotImg);
    void imageUrl(d.id, idx).then((url) => {
      img.src = url;
      (img.parentElement as HTMLAnchorElement).href = url;
    });
  }
  const formData = (form: HTMLFormElement) => Object.fromEntries(new FormData(form)) as Record<string, string>;
  const onSubmit = (selector: string, handler: (data: Record<string, string>) => Promise<unknown>) =>
    app.querySelector<HTMLFormElement>(selector)?.addEventListener('submit', async (event) => {
      event.preventDefault();
      const form = event.target as HTMLFormElement;
      form.querySelector('button')!.disabled = true;
      try {
        await handler(formData(form));
      } catch (error) {
        alert(String(error));
      }
      await refresh();
    });
  onSubmit('.queue', (data) =>
    api(`/api/feedback/${d.id}/queue`, { method: 'POST', body: JSON.stringify({ agent: data.agent, note: data.note || null }) }),
  );
  onSubmit('.status-form', (data) =>
    api(`/api/feedback/${d.id}`, { method: 'PATCH', body: JSON.stringify({ status: data.status, note: data.note || null }) }),
  );
  onSubmit('.comment', async (data) => {
    if (data.body.trim()) await api(`/api/feedback/${d.id}/events`, { method: 'POST', body: JSON.stringify({ body: data.body }) });
  });
  app.querySelector('[data-delete]')?.addEventListener('click', async () => {
    if (!confirm(`Delete feedback #${d.id} and its screenshots permanently?`)) return;
    await api(`/api/feedback/${d.id}`, { method: 'DELETE' });
    select(null);
  });
}

async function init() {
  if (!state.token) return render();
  try {
    state.apps = await fetch('/api/apps').then((r) => r.json());
  } catch {
    state.apps = [];
  }
  await refresh();
}

setInterval(() => {
  if (state.token && document.visibilityState === 'visible' && !app.querySelector(':focus')) void refresh();
}, 30_000);
void init();
