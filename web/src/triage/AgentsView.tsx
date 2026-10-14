// Agent runs: workers, the run list, and a run's live activity with steering.

import {
  AlertTriangle,
  ArrowLeft,
  Bot,
  ChevronRight,
  CircleDot,
  FileText,
  Send,
  Square,
  Terminal,
  User,
} from 'lucide-react';
import { Collapsible, Switch, ToggleGroup } from 'radix-ui';
import { ReactNode, useCallback, useEffect, useLayoutEffect, useRef, useState } from 'react';
import { choiceLabel, liveSteering, useWorkers } from './AgentPicker';
import { ago, api, clock, duration, post } from './api';
import { ACTIVE_RUN, Run, RunDetail, RunEvent } from './types';
import { AutoTextarea, Confirm, StatusPill, submitShortcut, useAction, usePoll } from './ui';

export function AgentsView({
  selected,
  onSelect,
  onOpenFeedback,
}: {
  selected: number | null;
  onSelect: (id: number | null) => void;
  onOpenFeedback: (id: number) => void;
}) {
  const [filter, setFilter] = useState<'active' | 'all'>('active');
  const [runs, setRuns] = useState<Run[] | null>(null);
  const loadRuns = useCallback(async () => {
    try {
      setRuns(await api<Run[]>(`/api/runs?active=${filter === 'active'}&limit=200`));
    } catch {
      /* shown by the next successful poll */
    }
  }, [filter]);
  usePoll(loadRuns, 4_000, [loadRuns]);

  return (
    <div className={`panes ${selected ? 'has-detail' : ''}`}>
      <aside className="list-pane">
        <div className="filters">
          <Workers />
          <ToggleGroup.Root
            type="single"
            value={filter}
            onValueChange={(v) => v && setFilter(v as 'active' | 'all')}
            className="chips"
            aria-label="Runs filter"
          >
            <ToggleGroup.Item value="active" className="chip" data-run-filter="active">
              active
            </ToggleGroup.Item>
            <ToggleGroup.Item value="all" className="chip" data-run-filter="all">
              all runs
            </ToggleGroup.Item>
          </ToggleGroup.Root>
        </div>
        <ul className="items">
          {runs === null ? (
            <li className="empty">Loading…</li>
          ) : runs.length === 0 ? (
            <li className="empty">{filter === 'active' ? 'No agents working right now.' : 'No runs yet.'}</li>
          ) : (
            runs.map((r) => (
              <li key={r.id} className={`entry ${r.id === selected ? 'selected' : ''}`}>
                <button className="item run-item" data-run={r.id} onClick={() => onSelect(r.id)}>
                  <span className="item-row">
                    <strong>Run {r.id}</strong>
                    <span className="muted">{choiceLabel(r)}</span>
                    <StatusPill status={r.status} prefix="run-" />
                  </span>
                  <span className="item-message">
                    #{r.feedback_id} {r.app} — {r.title || '(screenshot only)'}
                  </span>
                  <span className="item-meta">
                    {r.worker && `${r.worker} · `}
                    {r.status === 'running' && r.started_at ? `running ${duration(r.started_at, null)}` : ago(r.updated_at)}
                    {r.pending_messages > 0 && ` · ✉ ${r.pending_messages} pending`}
                  </span>
                </button>
              </li>
            ))
          )}
        </ul>
      </aside>
      <section className="detail-pane run-pane">
        {selected ? (
          <RunView key={selected} id={selected} onBack={() => onSelect(null)} onOpenFeedback={onOpenFeedback} onChanged={loadRuns} />
        ) : (
          <div className="empty-detail">
            <Bot size={28} />
            <p>Select a run to watch the agent work and steer it.</p>
          </div>
        )}
      </section>
    </div>
  );
}

function Workers() {
  const { workers } = useWorkers();
  if (!workers.length) {
    return (
      <p className="workers none">
        No worker online. Start one with <code>feedback-hub worker</code>.
      </p>
    );
  }
  return (
    <ul className="workers">
      {workers.map((w) => {
        const stale = Date.now() - new Date(w.last_seen).getTime() > 90_000;
        return (
          <li key={w.name} className={stale ? 'stale' : ''}>
            <span className="dot" />
            <strong>{w.name}</strong>
            <span className="muted small">
              {w.running.length}/{w.max_parallel} busy · {w.agents.map((a) => a.name).join(', ')}
              {stale && ` · last seen ${ago(w.last_seen)}`}
            </span>
          </li>
        );
      })}
    </ul>
  );
}

function RunView({
  id,
  onBack,
  onOpenFeedback,
  onChanged,
}: {
  id: number;
  onBack: () => void;
  onOpenFeedback: (id: number) => void;
  onChanged: () => void;
}) {
  const [run, setRun] = useState<RunDetail | null>(null);
  const [error, setError] = useState('');
  const logRef = useRef<HTMLDivElement>(null);
  const stick = useRef(true);
  const inflight = useRef(false);

  const load = useCallback(async () => {
    if (inflight.current) return;
    inflight.current = true;
    try {
      const after = run?.events.at(-1)?.id ?? 0;
      const next = await api<RunDetail>(`/api/runs/${id}?after=${after}`);
      setRun((current) => (current ? { ...next, events: [...current.events, ...next.events] } : next));
      setError('');
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      inflight.current = false;
    }
  }, [id, run]);
  const active = !run || ACTIVE_RUN.includes(run.status);
  usePoll(load, active ? 2_000 : 15_000, [id, active, run?.events.length]);

  // Follow the log while the reader is at the bottom.
  const scroller = () => logRef.current?.closest('.run-pane') as HTMLElement | null;
  useEffect(() => {
    const el = scroller();
    if (!el) return;
    const onScroll = () => {
      stick.current = el.scrollHeight - el.scrollTop - el.clientHeight < 120;
    };
    el.addEventListener('scroll', onScroll);
    return () => el.removeEventListener('scroll', onScroll);
  }, [run === null]);
  useLayoutEffect(() => {
    const el = scroller();
    if (el && stick.current) el.scrollTop = el.scrollHeight;
  }, [run?.events.length]);

  if (!run) return <div className="empty-detail">{error || 'Loading…'}</div>;

  const prompt = run.events.find((e) => e.kind === 'prompt');
  const events = run.events.filter((e) => e.kind !== 'prompt');
  const meta: [string, string | null][] = [
    ['Agent', run.agent],
    ['Model', run.model ?? 'default'],
    ['Effort', run.effort ?? 'default'],
    ['Worker', run.worker],
    ['Worktree', run.workdir],
    ['Branch', run.branch],
    ['Session', run.session_id],
    ['Log', run.log_path],
    ['Queued', `${new Date(run.created_at).toLocaleString()} by ${run.requested_by}`],
    ['Runtime', run.started_at ? duration(run.started_at, run.finished_at) : null],
    [
      'Usage',
      [run.tokens ? `${run.tokens.toLocaleString()} tokens` : '', run.cost_usd ? `~$${run.cost_usd.toFixed(2)} (CLI estimate)` : '']
        .filter(Boolean)
        .join(' · ') || null,
    ],
  ];

  return (
    <article className="detail run-detail">
      <button className="button ghost back" onClick={onBack}>
        <ArrowLeft size={15} /> All runs
      </button>
      <header className="detail-head">
        <h2>Run {run.id}</h2>
        <StatusPill status={run.status} prefix="run-" />
        {run.pending_messages > 0 && (
          <span className="muted small">
            ✉ {run.pending_messages} message{run.pending_messages === 1 ? '' : 's'} pending
          </span>
        )}
      </header>
      <p className="run-feedback">
        <button className="link" onClick={() => onOpenFeedback(run.feedback_id)} data-feedback>
          #{run.feedback_id} {run.app}
        </button>{' '}
        — {run.title || '(screenshot only)'} <StatusPill status={run.feedback_status} />
      </p>
      <Collapsible.Root className="section">
        <Collapsible.Trigger className="section-toggle">
          <ChevronRight size={15} className="chevron" /> {choiceLabel(run)}
          {run.worker && ` on ${run.worker}`}
          {run.started_at && <span className="muted"> · {duration(run.started_at, run.finished_at)}</span>}
        </Collapsible.Trigger>
        <Collapsible.Content>
          <dl className="meta">
            {meta
              .filter(([, v]) => v)
              .map(([k, v]) => (
                <div key={k}>
                  <dt>{k}</dt>
                  <dd>{v}</dd>
                </div>
              ))}
          </dl>
          {run.note && (
            <p className="note">
              <strong>Instructions:</strong> {run.note}
            </p>
          )}
        </Collapsible.Content>
      </Collapsible.Root>
      {prompt && (
        <details className="prompt">
          <summary>
            <FileText size={14} /> Prompt the agent started with
          </summary>
          <pre>{prompt.body}</pre>
        </details>
      )}
      <div className="log" ref={logRef} data-log>
        {events.length === 0 ? <p className="empty">Waiting for the agent…</p> : <Log events={events} />}
        {run.status === 'running' && (
          <div className="working">
            <CircleDot size={12} className="pulse" /> working…
          </div>
        )}
      </div>
      <Composer run={run} onSent={() => { void load(); onChanged(); }} />
    </article>
  );
}

/** Pairs each tool call with the output that follows it. */
function Log({ events }: { events: RunEvent[] }) {
  const rows: ReactNode[] = [];
  for (let i = 0; i < events.length; i++) {
    const e = events[i];
    const next = events[i + 1];
    if (e.kind === 'tool' && next && ['tool_result', 'tool_error'].includes(next.kind)) {
      rows.push(<ToolRow key={e.id} call={e} result={next} />);
      i++;
    } else {
      rows.push(<EventRow key={e.id} event={e} />);
    }
  }
  return <>{rows}</>;
}

function ToolRow({ call, result }: { call: RunEvent; result?: RunEvent }) {
  const output = result?.body ?? '';
  const lines = output ? output.split('\n').length : 0;
  const failed = result?.kind === 'tool_error';
  return (
    <div className={`ev tool ${failed ? 'failed' : ''}`}>
      <time>{clock(call.at)}</time>
      <details>
        <summary>
          <Terminal size={13} />
          <code>{call.body}</code>
          {result && <span className="muted small">{failed ? 'failed' : lines ? `${lines} line${lines === 1 ? '' : 's'}` : 'no output'}</span>}
        </summary>
        {output && <pre>{output}</pre>}
      </details>
    </div>
  );
}

function EventRow({ event: e }: { event: RunEvent }) {
  const time = <time>{clock(e.at)}</time>;
  switch (e.kind) {
    case 'text':
      return (
        <div className="ev text">
          {time}
          <div className="body">{e.body}</div>
        </div>
      );
    case 'tool':
      return <ToolRow call={e} />;
    case 'tool_result':
    case 'tool_error':
    case 'output':
      return (
        <div className={`ev tool ${e.kind === 'tool_error' ? 'failed' : ''}`}>
          {time}
          <details>
            <summary>
              <Terminal size={13} /> <code>{e.body.split('\n')[0].slice(0, 140) || '(no output)'}</code>
            </summary>
            <pre>{e.body}</pre>
          </details>
        </div>
      );
    case 'user':
    case 'user (interrupt)':
      return (
        <div className="ev user">
          {time}
          <div className="bubble">
            <span className="who">
              <User size={12} /> You{e.kind === 'user (interrupt)' ? ' · interrupt' : ''}
            </span>
            {e.body}
          </div>
        </div>
      );
    case 'error':
      return (
        <div className="ev error">
          {time}
          <div className="body">
            <AlertTriangle size={14} /> <pre>{e.body}</pre>
          </div>
        </div>
      );
    case 'plan':
      return (
        <div className="ev plan">
          {time}
          <pre>{e.body}</pre>
        </div>
      );
    default:
      return (
        <div className="ev note">
          {time}
          <span>{e.body}</span>
        </div>
      );
  }
}

function Composer({ run, onSent }: { run: Run; onSent: () => void }) {
  const { workers } = useWorkers();
  const [body, setBody] = useState('');
  const [interrupt, setInterrupt] = useState(false);
  const [busy, setBusy] = useState(false);
  const act = useAction();
  const active = ACTIVE_RUN.includes(run.status);
  const live = liveSteering(workers, run.agent);

  if (!active && !run.session_id) {
    return <p className="composer-hint">This run has no agent session to continue. Queue a new run from the feedback item instead.</p>;
  }
  const send = async () => {
    if (!body.trim()) return;
    setBusy(true);
    const ok = await act(() => post(`/api/runs/${run.id}/commands`, { kind: 'message', body, interrupt }));
    setBusy(false);
    if (ok) {
      setBody('');
      setInterrupt(false);
      onSent();
    }
  };
  const hint = !active
    ? 'Sending resumes the agent session on the same worker.'
    : live
      ? 'Reaches the agent right away, even mid-step.'
      : interrupt
        ? 'Stops the current step and resumes the session with your message.'
        : 'Delivered when the agent finishes its current turn.';
  return (
    <form
      className="composer"
      onSubmit={(e) => {
        e.preventDefault();
        void send();
      }}
    >
      <AutoTextarea
        name="body"
        placeholder={active ? 'Steer the agent…' : 'Follow up with the agent…'}
        value={body}
        onChange={(e) => setBody(e.target.value)}
        onKeyDown={(e) => {
          if (e.key === 'Enter' && (e.metaKey || e.ctrlKey)) void send();
        }}
      />
      <div className="row">
        <span className="muted small grow">{hint}</span>
        {active && !live && run.status === 'running' && (
          <label className="switch-label">
            <Switch.Root className="switch" checked={interrupt} onCheckedChange={setInterrupt} name="interrupt">
              <Switch.Thumb className="switch-thumb" />
            </Switch.Root>
            Interrupt
          </label>
        )}
        <span className="muted small">{submitShortcut}</span>
        <button className="button primary" disabled={busy || !body.trim()} data-send>
          <Send size={14} /> {active ? 'Send' : 'Continue'}
        </button>
        {active && (
          <Confirm
            title={run.status === 'queued' ? `Cancel run ${run.id}?` : `Stop run ${run.id}?`}
            description={
              run.status === 'queued'
                ? 'The item goes back to open.'
                : 'The agent is stopped. Its worktree is kept if it has unpushed work, and you can continue the session later.'
            }
            action={run.status === 'queued' ? 'Cancel run' : 'Stop agent'}
            onConfirm={() => void act(async () => {
              await post(`/api/runs/${run.id}/commands`, { kind: 'stop' });
              onSent();
            })}
            trigger={
              <button type="button" className="button danger" data-stop>
                <Square size={13} /> {run.status === 'queued' ? 'Cancel' : 'Stop'}
              </button>
            }
          />
        )}
      </div>
    </form>
  );
}

