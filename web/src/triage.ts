// Triage UI: browse feedback from every app, look at screenshots and
// diagnostics, change status and comment, and hand items to coding agents —
// then watch the agents work, steer them, stop them or continue their sessions.

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
interface Run {
  id: number;
  feedback_id: number;
  agent: string;
  model: string | null;
  effort: string | null;
  note: string | null;
  status: string;
  worker: string | null;
  workdir: string | null;
  branch: string | null;
  session_id: string | null;
  log_path: string | null;
  summary: string | null;
  cost_usd: number | null;
  tokens: number | null;
  requested_by: string;
  created_at: string;
  started_at: string | null;
  finished_at: string | null;
  updated_at: string;
  app: string;
  title: string;
  feedback_status: string;
  pending_messages: number;
}
interface RunEvent {
  id: number;
  at: string;
  kind: string;
  body: string;
}
interface RunDetail extends Run {
  events: RunEvent[];
}
interface Detail extends Feedback {
  screenshots: { idx: number; mime: string; size: number; note: string | null }[];
  events: { id: number; at: string; author: string; kind: string; body: string }[];
  runs: Run[];
}
interface AppInfo {
  id: string;
  name?: string;
  repo?: string;
}
interface AgentInfo {
  name: string;
  models: string[];
  efforts: string[];
  live_steering: boolean;
}
interface WorkerStatus {
  name: string;
  agents: AgentInfo[];
  running: number[];
  max_parallel: number;
  last_seen: string;
}

const STATUSES = ['open', 'queued', 'in_progress', 'resolved', 'dismissed'];
const ACTIVE = ['queued', 'running'];
// Used until a worker reports what it can run.
const FALLBACK_AGENTS: AgentInfo[] = [
  { name: 'claude', models: ['fable', 'opus', 'sonnet', 'haiku'], efforts: ['low', 'medium', 'high', 'xhigh', 'max'], live_steering: true },
  { name: 'codex', models: [], efforts: ['low', 'medium', 'high', 'xhigh'], live_steering: false },
  { name: 'hermes', models: [], efforts: ['low', 'medium', 'high', 'xhigh'], live_steering: false },
];
const TOKEN_KEY = 'feedbackHub.token';
const PREFS_KEY = 'feedbackHub.agentPrefs';
const app = document.getElementById('app')!;

const storage = (() => {
  try {
    return window.localStorage;
  } catch {
    return null;
  }
})();

function readPrefs(): { agent: string; model: string; effort: string } {
  try {
    return { agent: 'claude', model: '', effort: '', ...JSON.parse(storage?.getItem(PREFS_KEY) ?? '{}') };
  } catch {
    return { agent: 'claude', model: '', effort: '' };
  }
}

const hash = new URLSearchParams(location.hash.slice(1));
const state = {
  token: storage?.getItem(TOKEN_KEY) ?? '',
  view: (hash.get('view') === 'agents' ? 'agents' : 'feedback') as 'feedback' | 'agents',
  statuses: new Set(['open', 'queued', 'in_progress']),
  app: '',
  items: [] as Feedback[],
  apps: [] as AppInfo[],
  selected: Number(hash.get('id')) || null,
  detail: null as Detail | null,
  checked: new Set<number>(),
  imageUrls: new Map<string, string>(),
  runs: [] as Run[],
  runFilter: 'active' as 'active' | 'all',
  runSelected: Number(hash.get('run')) || null,
  run: null as RunDetail | null,
  workers: [] as WorkerStatus[],
  runningCount: 0,
  prefs: readPrefs(),
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

function duration(from: string | null, to: string | null) {
  if (!from) return '';
  const seconds = Math.max(0, ((to ? new Date(to) : new Date()).getTime() - new Date(from).getTime()) / 1000);
  if (seconds < 60) return `${Math.round(seconds)} s`;
  if (seconds < 3600) return `${Math.floor(seconds / 60)} min ${Math.round(seconds % 60)} s`;
  return `${Math.floor(seconds / 3600)} h ${Math.round((seconds % 3600) / 60)} min`;
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

// --- Agents, models and efforts -------------------------------------------

function agentOptions(): AgentInfo[] {
  const merged = new Map<string, AgentInfo>();
  for (const worker of state.workers) {
    for (const agent of worker.agents) {
      const known = merged.get(agent.name);
      merged.set(
        agent.name,
        known
          ? {
              ...known,
              models: [...new Set([...known.models, ...agent.models])],
              efforts: [...new Set([...known.efforts, ...agent.efforts])],
            }
          : { ...agent },
      );
    }
  }
  return merged.size ? [...merged.values()] : FALLBACK_AGENTS;
}

const agentInfo = (name: string) => agentOptions().find((a) => a.name === name);
const liveSteering = (name: string) => agentInfo(name)?.live_steering ?? name === 'claude';

function effortOptions(agent: string, selected: string) {
  const efforts = agentInfo(agent)?.efforts ?? [];
  return [`<option value="">default effort</option>`, ...efforts.map((e) => `<option ${e === selected ? 'selected' : ''}>${esc(e)}</option>`)].join('');
}

/** Agent, model (free text with suggestions) and reasoning effort fields. */
function agentPicker(agent = state.prefs.agent) {
  const agents = agentOptions();
  if (!agents.some((a) => a.name === agent)) agent = agents[0]?.name ?? 'claude';
  const prefs = state.prefs.agent === agent ? state.prefs : { model: '', effort: '' };
  return `
    <span class="picker">
      <select name="agent" aria-label="Agent">${agents.map((a) => `<option ${a.name === agent ? 'selected' : ''}>${esc(a.name)}</option>`).join('')}</select>
      <input name="model" list="models-${esc(agent)}" placeholder="default model" value="${esc(prefs.model)}" aria-label="Model" size="12">
      <select name="effort" aria-label="Reasoning effort">${effortOptions(agent, prefs.effort)}</select>
    </span>`;
}

function modelLists() {
  return agentOptions()
    .map((a) => `<datalist id="models-${esc(a.name)}">${a.models.map((m) => `<option value="${esc(m)}">`).join('')}</datalist>`)
    .join('');
}

function bindPickers(root: HTMLElement | null = app) {
  for (const picker of root?.querySelectorAll<HTMLElement>('.picker') ?? []) {
    if (picker.dataset.bound) continue;
    picker.dataset.bound = '1';
    const agent = picker.querySelector<HTMLSelectElement>('[name=agent]')!;
    const model = picker.querySelector<HTMLInputElement>('[name=model]')!;
    const effort = picker.querySelector<HTMLSelectElement>('[name=effort]')!;
    agent.addEventListener('change', () => {
      model.setAttribute('list', `models-${agent.value}`);
      model.value = '';
      effort.innerHTML = effortOptions(agent.value, '');
    });
  }
}

function runRequest(data: Record<string, string>) {
  state.prefs = { agent: data.agent, model: data.model?.trim() ?? '', effort: data.effort ?? '' };
  storage?.setItem(PREFS_KEY, JSON.stringify(state.prefs));
  return { agent: data.agent, model: state.prefs.model || null, effort: state.prefs.effort || null, note: data.note?.trim() || null };
}

const runLabel = (r: Run) => [r.agent, r.model, r.effort].filter(Boolean).join(' · ');

// --- Loading ----------------------------------------------------------------

async function loadList() {
  const params = new URLSearchParams({ status: [...state.statuses].join(',') || 'all', limit: '300' });
  if (state.app) params.set('app', state.app);
  state.items = await api<Feedback[]>(`/api/feedback?${params}`);
  const shown = new Set(state.items.map((i) => i.id));
  for (const id of state.checked) if (!shown.has(id)) state.checked.delete(id);
}

async function loadDetail() {
  state.detail = state.selected ? await api<Detail>(`/api/feedback/${state.selected}`) : null;
}

async function loadRuns() {
  const [runs, workers] = await Promise.all([
    api<Run[]>(`/api/runs?active=${state.runFilter === 'active'}&limit=200`),
    api<WorkerStatus[]>('/api/workers'),
  ]);
  state.runs = runs;
  state.workers = workers;
  state.runningCount = runs.filter((r) => r.status === 'running').length;
}

async function loadRun() {
  state.run = state.runSelected ? await api<RunDetail>(`/api/runs/${state.runSelected}`) : null;
}

async function refresh() {
  try {
    state.error = '';
    if (state.view === 'agents') {
      await Promise.all([loadRuns(), loadRun()]);
    } else {
      const [, , workers, active] = await Promise.all([
        loadList(),
        loadDetail(),
        api<WorkerStatus[]>('/api/workers'),
        api<Run[]>('/api/runs?active=true&limit=200'),
      ]);
      state.workers = workers;
      state.runningCount = active.filter((r) => r.status === 'running').length;
    }
  } catch (error) {
    state.error = String(error);
  }
  render();
}

function navigate(changes: Partial<Pick<typeof state, 'view' | 'selected' | 'runSelected'>>) {
  Object.assign(state, changes);
  const params = new URLSearchParams();
  if (state.view === 'agents') {
    params.set('view', 'agents');
    if (state.runSelected) params.set('run', String(state.runSelected));
  } else if (state.selected) {
    params.set('id', String(state.selected));
  }
  history.replaceState(null, '', `#${params}`);
  void refresh();
}

// --- Rendering --------------------------------------------------------------

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

function renderBulkBar() {
  if (!state.checked.size) return '';
  return `
    <form class="bulk">
      <div class="bulk-head"><strong>${state.checked.size} selected</strong><button type="button" class="link" data-clear>Clear</button></div>
      ${agentPicker()}
      <input name="note" placeholder="Instructions for the agent (optional)">
      <button class="primary">Send ${state.checked.size} to agent</button>
    </form>`;
}

function renderList() {
  const counts = new Map<string, number>();
  for (const item of state.items) counts.set(item.app, (counts.get(item.app) ?? 0) + 1);
  const appIds = [...new Set([...state.apps.map((a) => a.id), ...counts.keys(), state.app].filter(Boolean))].sort();
  const allChecked = state.items.length > 0 && state.items.every((i) => state.checked.has(i.id));
  return `
    <aside class="list">
      <div class="filters">
        <div class="chips">
          ${STATUSES.map((s) => `<button class="chip ${state.statuses.has(s) ? 'on' : ''}" data-status="${s}">${s.replace('_', ' ')}</button>`).join('')}
        </div>
        <div class="row">
          <label class="check-all"><input type="checkbox" data-check-all ${allChecked ? 'checked' : ''} aria-label="Select all shown"> All</label>
          <select name="app" aria-label="App">
            <option value="">All apps</option>
            ${appIds.map((id) => `<option value="${esc(id)}" ${id === state.app ? 'selected' : ''}>${esc(id)}</option>`).join('')}
          </select>
        </div>
        <div data-bulk>${renderBulkBar()}</div>
      </div>
      ${state.error ? `<p class="error">${esc(state.error)}</p>` : ''}
      <ul>
        ${
          state.items.length === 0
            ? '<li class="empty">Nothing here. 🎉</li>'
            : state.items
                .map(
                  (item) => `
          <li class="entry">
            <input type="checkbox" data-check="${item.id}" ${state.checked.has(item.id) ? 'checked' : ''} aria-label="Select #${item.id}">
            <button class="item ${item.id === state.selected ? 'selected' : ''}" data-id="${item.id}">
              <span class="row"><strong>#${item.id}</strong> <span class="app">${esc(item.app)}</span>
                <span class="pill ${item.status}">${item.status.replace('_', ' ')}</span></span>
              <span class="msg">${esc(item.message.split('\n')[0] || '(screenshot only)')}</span>
              <span class="meta">${{ bug: '🐞', idea: '💡', other: '💬' }[item.kind] ?? ''} ${ago(item.created_at)}${
                item.reporter ? ` · ${esc(item.reporter)}` : ''
              }${item.screenshot_count ? ` · 🖼 ${item.screenshot_count}` : ''}${item.agent && item.status !== 'open' ? ` · 🤖 ${esc(item.agent)}` : ''}</span>
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

function renderRunRows(runs: Run[], withFeedback: boolean) {
  return runs
    .map(
      (r) => `
      <li>
        <button class="item run-item ${r.id === state.runSelected && state.view === 'agents' ? 'selected' : ''}" data-run="${r.id}">
          <span class="row"><strong>Run ${r.id}</strong> <span class="app">${esc(runLabel(r))}</span>
            <span class="pill run-${r.status}">${r.status}</span></span>
          ${withFeedback ? `<span class="msg">#${r.feedback_id} ${esc(r.app)} — ${esc(r.title || '(screenshot only)')}</span>` : ''}
          <span class="meta">${r.worker ? `${esc(r.worker)} · ` : ''}${
            r.status === 'running' && r.started_at ? `running ${duration(r.started_at, null)}` : ago(r.updated_at)
          }${r.pending_messages ? ` · ✉ ${r.pending_messages} pending` : ''}${r.cost_usd ? ` · ~$${r.cost_usd.toFixed(2)}` : ''}</span>
        </button>
      </li>`,
    )
    .join('');
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
  const activeRun = d.runs.find((r) => ACTIVE.includes(r.status));
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
              .map(
                (s) =>
                  `<figure><a target="_blank" data-shot="${s.idx}"><img alt="Screenshot ${s.idx + 1}" data-shot-img="${s.idx}"></a><figcaption><strong>#${s.idx + 1}</strong>${s.note ? ` ${esc(s.note)}` : ''}</figcaption></figure>`,
              )
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
        ${
          activeRun
            ? `<p class="active-run">🤖 <button class="link" data-run="${activeRun.id}">Run ${activeRun.id}</button> (${esc(runLabel(activeRun))}) is ${activeRun.status}.</p>`
            : `<form class="queue">
          <label>Send to agent</label>
          ${agentPicker()}
          <input name="note" placeholder="Instructions for the agent (optional)">
          <button class="primary">Queue</button>
        </form>`
        }
        <form class="status-form">
          <label>Status
            <select name="status">${STATUSES.map((s) => `<option ${s === d.status ? 'selected' : ''}>${s}</option>`).join('')}</select>
          </label>
          <input name="note" placeholder="Note (optional)">
          <button>Update</button>
        </form>
        <p class="cli">Agents: <code>feedback-hub show ${d.id}</code></p>
      </div>

      ${d.runs.length ? `<h3>Agent runs</h3><ul class="runs">${renderRunRows(d.runs, false)}</ul>` : ''}

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

function renderWorkers() {
  if (!state.workers.length) {
    return `<p class="workers none">No worker online. Start one with <code>feedback-hub worker</code>.</p>`;
  }
  return `<ul class="workers">${state.workers
    .map((w) => {
      const stale = Date.now() - new Date(w.last_seen).getTime() > 90_000;
      return `<li class="${stale ? 'stale' : ''}"><span class="dot"></span><strong>${esc(w.name)}</strong>
        <span class="meta">${w.running.length}/${w.max_parallel} busy · ${esc(w.agents.map((a) => a.name).join(', '))}${stale ? ` · last seen ${ago(w.last_seen)}` : ''}</span></li>`;
    })
    .join('')}</ul>`;
}

function renderRunsList() {
  return `
    <aside class="list runs-list">
      <div class="filters">
        ${renderWorkers()}
        <div class="chips">
          <button class="chip ${state.runFilter === 'active' ? 'on' : ''}" data-run-filter="active">active</button>
          <button class="chip ${state.runFilter === 'all' ? 'on' : ''}" data-run-filter="all">all runs</button>
        </div>
      </div>
      ${state.error ? `<p class="error">${esc(state.error)}</p>` : ''}
      <ul>${state.runs.length ? renderRunRows(state.runs, true) : `<li class="empty">${state.runFilter === 'active' ? 'No agents working right now.' : 'No runs yet.'}</li>`}</ul>
    </aside>`;
}

function eventTime(iso: string) {
  return new Date(iso).toLocaleTimeString([], { hour: '2-digit', minute: '2-digit', second: '2-digit', hourCycle: 'h23' });
}

function renderEvent(e: RunEvent) {
  const time = `<time>${eventTime(e.at)}</time>`;
  switch (e.kind) {
    case 'text':
      return `<li class="ev text">${time}<div class="body">${esc(e.body)}</div></li>`;
    case 'tool':
      return `<li class="ev tool">${time}<code>› ${esc(e.body)}</code></li>`;
    case 'tool_result':
    case 'tool_error':
    case 'output': {
      const lines = e.body.split('\n');
      const first = lines[0].slice(0, 140) || '(no output)';
      return `<li class="ev result ${e.kind === 'tool_error' ? 'err' : ''}">${time}<details><summary>${esc(first)}${lines.length > 1 ? ` <span class="meta">+${lines.length - 1} lines</span>` : ''}</summary><pre>${esc(e.body)}</pre></details></li>`;
    }
    case 'user':
    case 'user (interrupt)':
      return `<li class="ev user">${time}<div class="body"><span class="who">You${e.kind === 'user (interrupt)' ? ' (interrupt)' : ''}</span>${esc(e.body)}</div></li>`;
    case 'error':
      return `<li class="ev error">${time}<pre>${esc(e.body)}</pre></li>`;
    case 'plan':
      return `<li class="ev plan">${time}<pre>${esc(e.body)}</pre></li>`;
    default:
      return `<li class="ev note">${time}<span>${esc(e.body)}</span></li>`;
  }
}

function renderRunHead(r: Run) {
  const meta: [string, string | null][] = [
    ['Agent', r.agent],
    ['Model', r.model ?? 'default'],
    ['Effort', r.effort ?? 'default'],
    ['Worker', r.worker],
    ['Worktree', r.workdir],
    ['Branch', r.branch],
    ['Session', r.session_id],
    ['Log', r.log_path],
    ['Queued', `${new Date(r.created_at).toLocaleString()} by ${r.requested_by}`],
    ['Runtime', r.started_at ? duration(r.started_at, r.finished_at) : null],
    ['Usage', [r.tokens ? `${r.tokens.toLocaleString()} tokens` : '', r.cost_usd ? `~$${r.cost_usd.toFixed(2)} (CLI estimate)` : ''].filter(Boolean).join(' · ') || null],
  ];
  return `
    <header>
      <h2>Run ${r.id}</h2>
      <span class="pill run-${r.status}">${r.status}</span>
      ${r.pending_messages ? `<span class="meta">✉ ${r.pending_messages} message${r.pending_messages === 1 ? '' : 's'} pending</span>` : ''}
    </header>
    <p class="run-feedback"><button class="link" data-feedback="${r.feedback_id}">#${r.feedback_id} ${esc(r.app)}</button> — ${esc(r.title || '(screenshot only)')}
      <span class="pill ${r.feedback_status}">${r.feedback_status.replace('_', ' ')}</span></p>
    <details class="run-meta" ${r.status === 'queued' ? 'open' : ''}><summary>${esc(runLabel(r))}${r.worker ? ` on ${esc(r.worker)}` : ''}</summary>
      <dl>${meta
        .filter(([, v]) => v)
        .map(([k, v]) => `<dt>${k}</dt><dd>${esc(v)}</dd>`)
        .join('')}</dl>
      ${r.note ? `<p><strong>Instructions:</strong> ${esc(r.note)}</p>` : ''}
    </details>`;
}

function renderSteer(r: Run) {
  const active = ACTIVE.includes(r.status);
  const live = liveSteering(r.agent);
  if (!active && !r.session_id) {
    return `<p class="hint">This run has no agent session to continue. Queue a new run from the feedback item instead.</p>`;
  }
  const hint = !active
    ? 'Sending a message resumes the agent session on the same worker.'
    : live
      ? 'Messages reach the running agent right away.'
      : 'Messages are delivered when the current turn ends, or right away if you interrupt.';
  return `
    <form class="steer">
      <textarea name="body" rows="2" placeholder="${active ? 'Steer the agent…' : 'Follow-up for the agent…'}" required></textarea>
      <div class="row">
        <span class="hint">${hint}</span>
        ${active && !live && r.status === 'running' ? `<label class="check"><input type="checkbox" name="interrupt"> Interrupt</label>` : ''}
        <button class="primary">${active ? 'Send' : 'Continue session'}</button>
        ${active ? `<button type="button" class="stop" data-stop>${r.status === 'queued' ? 'Cancel' : 'Stop'}</button>` : ''}
      </div>
    </form>`;
}

function renderRunDetail() {
  const r = state.run;
  if (!r) return `<section class="detail empty-detail"><p>Select a run to see what the agent is doing.</p></section>`;
  return `
    <section class="detail run-detail">
      <button class="back" data-back>← All runs</button>
      <div data-run-head>${renderRunHead(r)}</div>
      <ol class="log" data-log>${r.events.map(renderEvent).join('') || '<li class="empty">Waiting for the agent…</li>'}</ol>
      <div class="steer-box" data-steer>${renderSteer(r)}</div>
    </section>`;
}

function render() {
  if (!state.token) return renderLogin();
  const agents = state.view === 'agents';
  const hasDetail = agents ? !!state.runSelected : !!state.selected;
  app.innerHTML = `
    <div class="layout ${hasDetail ? 'has-detail' : ''}">
      <header class="top">
        <h1>Feedback Hub</h1>
        <nav>
          <button class="tab ${agents ? '' : 'on'}" data-view="feedback">Feedback</button>
          <button class="tab ${agents ? 'on' : ''}" data-view="agents">Agents${state.runningCount ? ` <span class="badge">${state.runningCount}</span>` : ''}</button>
        </nav>
        <button class="link" data-logout>Sign out</button>
      </header>
      ${agents ? renderRunsList() + renderRunDetail() : renderList() + renderDetail()}
      ${modelLists()}
    </div>`;
  bind();
  if (agents) scrollLog(true);
}

function scrollLog(force: boolean) {
  const section = app.querySelector<HTMLElement>('.run-detail');
  if (!section) return;
  if (force || section.scrollHeight - section.scrollTop - section.clientHeight < 120) {
    section.scrollTop = section.scrollHeight;
  }
}

// --- Events -----------------------------------------------------------------

const formData = (form: HTMLFormElement) => Object.fromEntries(new FormData(form)) as Record<string, string>;

function onSubmit(selector: string, handler: (data: Record<string, string>) => Promise<unknown>) {
  app.querySelector<HTMLFormElement>(selector)?.addEventListener('submit', async (event) => {
    event.preventDefault();
    const form = event.target as HTMLFormElement;
    for (const button of form.querySelectorAll('button')) button.disabled = true;
    try {
      await handler(formData(form));
    } catch (error) {
      alert(String(error));
    }
    await refresh();
  });
}

function bind() {
  app.querySelector('[data-logout]')?.addEventListener('click', () => {
    storage?.removeItem(TOKEN_KEY);
    state.token = '';
    render();
  });
  for (const tab of app.querySelectorAll<HTMLButtonElement>('[data-view]')) {
    tab.addEventListener('click', () => navigate({ view: tab.dataset.view as 'feedback' | 'agents' }));
  }
  for (const button of app.querySelectorAll<HTMLButtonElement>('[data-run]')) {
    button.addEventListener('click', () => navigate({ view: 'agents', runSelected: Number(button.dataset.run) }));
  }
  bindPickers();
  if (state.view === 'agents') bindAgents();
  else bindFeedback();
}

function bindFeedback() {
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
  for (const item of app.querySelectorAll<HTMLButtonElement>('.item[data-id]')) {
    item.addEventListener('click', () => navigate({ selected: Number(item.dataset.id) }));
  }
  for (const box of app.querySelectorAll<HTMLInputElement>('[data-check]')) {
    box.addEventListener('change', () => {
      const id = Number(box.dataset.check);
      if (box.checked) state.checked.add(id);
      else state.checked.delete(id);
      updateBulk();
    });
  }
  app.querySelector<HTMLInputElement>('[data-check-all]')?.addEventListener('change', (e) => {
    const on = (e.target as HTMLInputElement).checked;
    for (const item of state.items) on ? state.checked.add(item.id) : state.checked.delete(item.id);
    for (const box of app.querySelectorAll<HTMLInputElement>('[data-check]')) box.checked = on;
    updateBulk();
  });
  bindBulk();
  app.querySelector('[data-back]')?.addEventListener('click', () => navigate({ selected: null }));
  const d = state.detail;
  if (!d) return;
  for (const img of app.querySelectorAll<HTMLImageElement>('[data-shot-img]')) {
    const idx = Number(img.dataset.shotImg);
    void imageUrl(d.id, idx).then((url) => {
      img.src = url;
      (img.parentElement as HTMLAnchorElement).href = url;
    });
  }
  onSubmit('.queue', (data) => api(`/api/feedback/${d.id}/queue`, { method: 'POST', body: JSON.stringify(runRequest(data)) }));
  onSubmit('.status-form', (data) =>
    api(`/api/feedback/${d.id}`, { method: 'PATCH', body: JSON.stringify({ status: data.status, note: data.note || null }) }),
  );
  onSubmit('.comment', async (data) => {
    if (data.body.trim()) await api(`/api/feedback/${d.id}/events`, { method: 'POST', body: JSON.stringify({ body: data.body }) });
  });
  app.querySelector('[data-delete]')?.addEventListener('click', async () => {
    if (!confirm(`Delete feedback #${d.id} and its screenshots permanently?`)) return;
    await api(`/api/feedback/${d.id}`, { method: 'DELETE' });
    navigate({ selected: null });
  });
}

/** Re-renders only the bulk bar so the list keeps its scroll position. */
function updateBulk() {
  const bar = app.querySelector('[data-bulk]');
  if (!bar) return;
  bar.innerHTML = renderBulkBar();
  const all = app.querySelector<HTMLInputElement>('[data-check-all]');
  if (all) all.checked = state.items.length > 0 && state.items.every((i) => state.checked.has(i.id));
  bindBulk();
}

function bindBulk() {
  bindPickers(app.querySelector<HTMLElement>('[data-bulk]'));
  app.querySelector('[data-clear]')?.addEventListener('click', () => {
    state.checked.clear();
    for (const box of app.querySelectorAll<HTMLInputElement>('[data-check]')) box.checked = false;
    updateBulk();
  });
  onSubmit('.bulk', async (data) => {
    const result = await api<{ created: Run[]; skipped: { id: number; reason: string }[] }>('/api/runs', {
      method: 'POST',
      body: JSON.stringify({ ids: [...state.checked], ...runRequest(data) }),
    });
    state.checked.clear();
    if (result.skipped.length) alert(`Skipped:\n${result.skipped.map((s) => `#${s.id}: ${s.reason}`).join('\n')}`);
  });
}

function bindAgents() {
  for (const chip of app.querySelectorAll<HTMLButtonElement>('[data-run-filter]')) {
    chip.addEventListener('click', () => {
      state.runFilter = chip.dataset.runFilter as 'active' | 'all';
      void refresh();
    });
  }
  app.querySelector('[data-back]')?.addEventListener('click', () => navigate({ runSelected: null }));
  bindRunDetail();
}

function bindRunDetail() {
  bindRunHead();
  bindSteer();
}

function bindRunHead() {
  const r = state.run;
  if (!r) return;
  app.querySelector<HTMLButtonElement>('[data-feedback]')?.addEventListener('click', () =>
    navigate({ view: 'feedback', selected: r.feedback_id }),
  );
}

function bindSteer() {
  const r = state.run;
  if (!r) return;
  onSubmit('.steer', (data) =>
    api(`/api/runs/${r.id}/commands`, {
      method: 'POST',
      body: JSON.stringify({ kind: 'message', body: data.body, interrupt: data.interrupt === 'on' }),
    }),
  );
  app.querySelector('[data-stop]')?.addEventListener('click', async () => {
    if (!confirm(r.status === 'queued' ? `Cancel run ${r.id}?` : `Stop the agent in run ${r.id}?`)) return;
    try {
      await api(`/api/runs/${r.id}/commands`, { method: 'POST', body: JSON.stringify({ kind: 'stop' }) });
    } catch (error) {
      alert(String(error));
    }
    await refresh();
  });
  app.querySelector<HTMLTextAreaElement>('.steer textarea')?.addEventListener('keydown', (event) => {
    if (event.key === 'Enter' && (event.metaKey || event.ctrlKey)) (event.target as HTMLTextAreaElement).form?.requestSubmit();
  });
}

/** Appends new events and updates the header without touching the steering form. */
async function pollRun() {
  const r = state.run;
  if (!r) return;
  const after = r.events.at(-1)?.id ?? 0;
  const next = await api<RunDetail>(`/api/runs/${r.id}?after=${after}`);
  if (state.run?.id !== r.id) return;
  const statusChanged = next.status !== r.status || !!next.session_id !== !!r.session_id;
  state.run = { ...next, events: [...r.events, ...next.events] };
  const log = app.querySelector('[data-log]');
  if (log && next.events.length) {
    if (!r.events.length) log.innerHTML = '';
    log.insertAdjacentHTML('beforeend', next.events.map(renderEvent).join(''));
    scrollLog(false);
  }
  const head = app.querySelector('[data-run-head]');
  const metaOpen = head?.querySelector<HTMLDetailsElement>('.run-meta')?.open;
  if (head) {
    head.innerHTML = renderRunHead(state.run);
    const details = head.querySelector<HTMLDetailsElement>('.run-meta');
    if (details && metaOpen !== undefined) details.open = metaOpen;
    bindRunHead();
  }
  const steer = app.querySelector('[data-steer]');
  const typing = steer?.querySelector('textarea')?.value;
  if (steer && statusChanged && !typing) {
    steer.innerHTML = renderSteer(state.run);
    bindSteer();
  }
}

/** Refreshes the runs list and workers in place. */
async function pollRuns() {
  await loadRuns();
  const list = app.querySelector('.runs-list');
  if (!list) return;
  list.outerHTML = renderRunsList();
  for (const button of app.querySelectorAll<HTMLButtonElement>('.runs-list [data-run]')) {
    button.addEventListener('click', () => navigate({ view: 'agents', runSelected: Number(button.dataset.run) }));
  }
  for (const chip of app.querySelectorAll<HTMLButtonElement>('[data-run-filter]')) {
    chip.addEventListener('click', () => {
      state.runFilter = chip.dataset.runFilter as 'active' | 'all';
      void refresh();
    });
  }
  const badge = state.runningCount ? ` <span class="badge">${state.runningCount}</span>` : '';
  const tab = app.querySelector('[data-view=agents]');
  if (tab) tab.innerHTML = `Agents${badge}`;
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

let ticks = 0;
setInterval(() => {
  if (!state.token || document.visibilityState !== 'visible') return;
  ticks++;
  if (state.view === 'agents') {
    if (state.run && (ACTIVE.includes(state.run.status) || ticks % 5 === 0)) void pollRun().catch(() => {});
    if (ticks % 3 === 0) void pollRuns().catch(() => {});
  } else if (ticks % 15 === 0 && !app.querySelector(':focus') && !state.checked.size) {
    void refresh();
  }
}, 2_000);
void init();
