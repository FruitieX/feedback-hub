// Feedback list with multi-select dispatch, and the item detail with editing,
// status, agent runs and timeline.

import {
  ArrowLeft,
  Bot,
  ChevronRight,
  Image as ImageIcon,
  MessageSquare,
  MoreHorizontal,
  Pencil,
  Send,
  Square,
  Trash2,
  X,
} from 'lucide-react';
import { Checkbox, Collapsible, Dialog, DropdownMenu, ToggleGroup } from 'radix-ui';
import { useCallback, useEffect, useMemo, useState } from 'react';
import { AgentPicker, choiceLabel, useAgentChoice } from './AgentPicker';
import { ago, api, patch, post, screenshotUrl } from './api';
import { ACTIVE_RUN, AgentChoice, AppInfo, Detail, Feedback, KINDS, Run, STATUSES } from './types';
import { AutoTextarea, Confirm, Select, StatusPill, submitShortcut, useAction, usePoll, useToast } from './ui';

const KIND_ICON: Record<string, string> = { bug: '🐞', idea: '💡', other: '💬' };

export function FeedbackView({
  apps,
  selected,
  onSelect,
  onOpenRun,
}: {
  apps: AppInfo[];
  selected: number | null;
  onSelect: (id: number | null) => void;
  onOpenRun: (id: number) => void;
}) {
  const [statuses, setStatuses] = useState<string[]>(['open', 'queued', 'in_progress']);
  const [appFilter, setAppFilter] = useState('');
  const [items, setItems] = useState<Feedback[] | null>(null);
  const [checked, setChecked] = useState<Set<number>>(new Set());
  const [detail, setDetail] = useState<Detail | null>(null);
  const [error, setError] = useState('');

  const loadList = useCallback(async () => {
    const params = new URLSearchParams({ status: statuses.join(',') || 'all', limit: '300' });
    if (appFilter) params.set('app', appFilter);
    try {
      const list = await api<Feedback[]>(`/api/feedback?${params}`);
      setItems(list);
      setError('');
      setChecked((current) => new Set([...current].filter((id) => list.some((i) => i.id === id))));
    } catch (e) {
      setError(String(e instanceof Error ? e.message : e));
    }
  }, [statuses, appFilter]);

  const loadDetail = useCallback(async () => {
    if (!selected) return setDetail(null);
    try {
      setDetail(await api<Detail>(`/api/feedback/${selected}`));
    } catch (e) {
      setError(String(e instanceof Error ? e.message : e));
    }
  }, [selected]);

  const reload = useCallback(() => Promise.all([loadList(), loadDetail()]), [loadList, loadDetail]);

  usePoll(loadList, 20_000, [loadList]);
  const hasActiveRun = !!detail?.runs.some((r) => ACTIVE_RUN.includes(r.status));
  usePoll(loadDetail, hasActiveRun ? 4_000 : 20_000, [loadDetail, hasActiveRun]);
  useEffect(() => {
    if (detail && detail.id !== selected) setDetail(null);
  }, [selected, detail]);

  const appIds = useMemo(
    () => [...new Set([...apps.map((a) => a.id), ...(items ?? []).map((i) => i.app)])].sort(),
    [apps, items],
  );

  return (
    <div className={`panes ${selected ? 'has-detail' : ''}`}>
      <aside className="list-pane">
        <div className="filters">
          <ToggleGroup.Root
            type="multiple"
            value={statuses}
            onValueChange={setStatuses}
            className="chips"
            aria-label="Status filter"
          >
            {STATUSES.map((s) => (
              <ToggleGroup.Item key={s} value={s} className="chip" data-status={s}>
                {s.replace('_', ' ')}
              </ToggleGroup.Item>
            ))}
          </ToggleGroup.Root>
          <div className="filter-row">
            <Checkbox.Root
              className="checkbox"
              aria-label="Select all shown"
              data-check-all
              checked={
                items?.length && items.every((i) => checked.has(i.id))
                  ? true
                  : checked.size
                    ? 'indeterminate'
                    : false
              }
              onCheckedChange={(on) => setChecked(on === true ? new Set(items?.map((i) => i.id)) : new Set())}
            >
              <Checkbox.Indicator className="checkbox-indicator">{checked.size && items?.some((i) => !checked.has(i.id)) ? '–' : '✓'}</Checkbox.Indicator>
            </Checkbox.Root>
            <Select
              label="App"
              value={appFilter || '__all'}
              onChange={(v) => setAppFilter(v === '__all' ? '' : v)}
              options={[{ value: '__all', label: 'All apps' }, ...appIds.map((id) => ({ value: id, label: id }))]}
              className="grow"
            />
          </div>
        </div>
        {error && <p className="error-text">{error}</p>}
        <ul className="items">
          {items === null ? (
            <li className="empty">Loading…</li>
          ) : items.length === 0 ? (
            <li className="empty">Nothing here. 🎉</li>
          ) : (
            items.map((item) => (
              <li key={item.id} className={`entry ${item.id === selected ? 'selected' : ''} ${checked.has(item.id) ? 'checked' : ''}`}>
                <Checkbox.Root
                  className="checkbox"
                  aria-label={`Select #${item.id}`}
                  data-check={item.id}
                  checked={checked.has(item.id)}
                  onCheckedChange={(on) =>
                    setChecked((current) => {
                      const next = new Set(current);
                      if (on === true) next.add(item.id);
                      else next.delete(item.id);
                      return next;
                    })
                  }
                >
                  <Checkbox.Indicator className="checkbox-indicator">✓</Checkbox.Indicator>
                </Checkbox.Root>
                <button className="item" data-id={item.id} onClick={() => onSelect(item.id)}>
                  <span className="item-row">
                    <strong>#{item.id}</strong>
                    <span className="muted">{item.app}</span>
                    <StatusPill status={item.status} />
                  </span>
                  <span className="item-message">{item.message.split('\n')[0] || '(screenshot only)'}</span>
                  <span className="item-meta">
                    {KIND_ICON[item.kind]} {ago(item.created_at)}
                    {item.screenshot_count > 0 && (
                      <>
                        {' · '}
                        <ImageIcon size={12} /> {item.screenshot_count}
                      </>
                    )}
                    {item.agent && item.status !== 'open' && (
                      <>
                        {' · '}
                        <Bot size={12} /> {item.agent}
                      </>
                    )}
                  </span>
                </button>
              </li>
            ))
          )}
        </ul>
        {checked.size > 0 && (
          <BulkBar
            ids={[...checked]}
            onClear={() => setChecked(new Set())}
            onDone={() => {
              setChecked(new Set());
              void reload();
            }}
          />
        )}
      </aside>
      <section className="detail-pane">
        {detail ? (
          <FeedbackDetail key={detail.id} detail={detail} apps={apps} onBack={() => onSelect(null)} onChanged={reload} onOpenRun={onOpenRun} onDeleted={() => { onSelect(null); void loadList(); }} />
        ) : (
          <div className="empty-detail">
            <MessageSquare size={28} />
            <p>Select feedback to see details, or tick several to send them to an agent.</p>
          </div>
        )}
      </section>
    </div>
  );
}

function BulkBar({ ids, onClear, onDone }: { ids: number[]; onClear: () => void; onDone: () => void }) {
  const [choice, setChoice] = useAgentChoice();
  const [note, setNote] = useState('');
  const [busy, setBusy] = useState(false);
  const toast = useToast();
  const run = useAction();
  const submit = async () => {
    setBusy(true);
    await run(async () => {
      const result = await post<{ created: Run[]; skipped: { id: number; reason: string }[] }>('/api/runs', {
        ids,
        ...choice,
        note: note.trim() || null,
      });
      toast({
        title: `Sent ${result.created.length} item${result.created.length === 1 ? '' : 's'} to ${choiceLabel(choice)}`,
        description: result.skipped.length ? `Skipped ${result.skipped.map((s) => `#${s.id} (${s.reason})`).join(', ')}` : undefined,
        tone: result.created.length ? 'ok' : 'error',
      });
      setNote('');
      onDone();
    });
    setBusy(false);
  };
  return (
    <form
      className="bulk"
      onSubmit={(e) => {
        e.preventDefault();
        void submit();
      }}
    >
      <div className="bulk-head">
        <strong>{ids.length} selected</strong>
        <button type="button" className="icon-button" onClick={onClear} aria-label="Clear selection">
          <X size={15} />
        </button>
      </div>
      <AgentPicker value={choice} onChange={setChoice} />
      <AutoTextarea
        name="note"
        placeholder="Instructions for the agent (optional)"
        value={note}
        onChange={(e) => setNote(e.target.value)}
        onKeyDown={(e) => {
          if (e.key === 'Enter' && (e.metaKey || e.ctrlKey)) void submit();
        }}
      />
      <button className="button primary" disabled={busy} data-bulk-send>
        <Send size={15} /> Send {ids.length} to {choice.agent}
      </button>
    </form>
  );
}

function FeedbackDetail({
  detail: d,
  apps,
  onBack,
  onChanged,
  onOpenRun,
  onDeleted,
}: {
  detail: Detail;
  apps: AppInfo[];
  onBack: () => void;
  onChanged: () => void;
  onOpenRun: (id: number) => void;
  onDeleted: () => void;
}) {
  const run = useAction();
  const { logs, network, ...context } = d.context as Record<string, unknown>;
  const repo = apps.find((a) => a.id === d.app)?.repo;
  const meta: [string, unknown][] = [
    ['App', d.app + (repo ? ` (${repo})` : '')],
    ['Reported', `${new Date(d.created_at).toLocaleString()} via ${d.source}${d.reporter ? ` by ${d.reporter}` : ''}`],
    ['Page', d.page_url ? `${d.page_url}${d.page_title ? ` — ${d.page_title}` : ''}` : null],
    ['Browser', d.user_agent],
    ...Object.entries(context).map(([k, v]) => [k, typeof v === 'string' ? v : JSON.stringify(v)] as [string, unknown]),
  ];
  const setStatus = (status: string) =>
    run(async () => {
      await patch(`/api/feedback/${d.id}`, { status });
      onChanged();
    }, `#${d.id} is now ${status.replace('_', ' ')}`);

  return (
    <article className="detail">
      <button className="button ghost back" onClick={onBack}>
        <ArrowLeft size={15} /> All feedback
      </button>
      <header className="detail-head">
        <h2>
          #{d.id} <span className="muted">· {KIND_ICON[d.kind]} {KINDS[d.kind] ?? d.kind}</span>
        </h2>
        <Select
          label="Status"
          value={d.status}
          onChange={(s) => void setStatus(s)}
          options={STATUSES.map((s) => ({ value: s, label: s.replace('_', ' ') }))}
          className={`status-select pill-like ${d.status}`}
        />
        <div className="spacer" />
        <EditDialog detail={d} onSaved={onChanged} />
        <DropdownMenu.Root>
          <DropdownMenu.Trigger className="icon-button" aria-label="More actions">
            <MoreHorizontal size={16} />
          </DropdownMenu.Trigger>
          <DropdownMenu.Portal>
            <DropdownMenu.Content className="menu" align="end" sideOffset={4}>
              <DropdownMenu.Item className="menu-item" onSelect={() => void navigator.clipboard?.writeText(`feedback-hub show ${d.id}`)}>
                Copy <code>feedback-hub show {d.id}</code>
              </DropdownMenu.Item>
              <DropdownMenu.Separator className="menu-separator" />
              <Confirm
                title={`Delete feedback #${d.id}?`}
                description="The report, its screenshots, timeline and agent runs are removed permanently."
                action="Delete"
                onConfirm={() => void run(async () => {
                  await api(`/api/feedback/${d.id}`, { method: 'DELETE' });
                  onDeleted();
                }, `Deleted #${d.id}`)}
                trigger={
                  <DropdownMenu.Item className="menu-item danger" onSelect={(e) => e.preventDefault()} data-delete>
                    <Trash2 size={14} /> Delete…
                  </DropdownMenu.Item>
                }
              />
            </DropdownMenu.Content>
          </DropdownMenu.Portal>
        </DropdownMenu.Root>
      </header>

      <blockquote className="message">{d.message || <em className="muted">No text — see screenshots.</em>}</blockquote>
      {d.screenshots.length > 0 && <Gallery detail={d} />}

      <AgentCard detail={d} onChanged={onChanged} onOpenRun={onOpenRun} />

      <Collapsible.Root className="section">
        <Collapsible.Trigger className="section-toggle">
          <ChevronRight size={15} className="chevron" /> Details
          {(Array.isArray(logs) && logs.length > 0) || (Array.isArray(network) && network.length > 0) ? (
            <span className="muted"> · console & network logs</span>
          ) : null}
        </Collapsible.Trigger>
        <Collapsible.Content>
          <dl className="meta">
            {meta
              .filter(([, v]) => v !== null && v !== undefined && v !== '')
              .map(([k, v]) => (
                <div key={k}>
                  <dt>{k}</dt>
                  <dd>{String(v)}</dd>
                </div>
              ))}
          </dl>
          <LogBlock title="Console messages" entries={logs} />
          <LogBlock title="Failed requests" entries={network} />
        </Collapsible.Content>
      </Collapsible.Root>

      {d.runs.length > 0 && (
        <section className="section">
          <h3>Agent runs</h3>
          <ul className="run-rows">
            {d.runs.map((r) => (
              <li key={r.id}>
                <button className="run-row" onClick={() => onOpenRun(r.id)} data-run={r.id}>
                  <strong>Run {r.id}</strong>
                  <span className="muted">{choiceLabel(r)}</span>
                  <StatusPill status={r.status} prefix="run-" />
                  <span className="muted small">{ago(r.updated_at)}</span>
                  <ChevronRight size={14} />
                </button>
              </li>
            ))}
          </ul>
        </section>
      )}

      <section className="section">
        <h3>Timeline</h3>
        <ol className="timeline">
          {d.events.length === 0 && <li className="empty">No activity yet.</li>}
          {d.events.map((e) => (
            <li key={e.id} className={e.kind}>
              <div className="ev-head">
                <strong>{e.author}</strong> <span className="muted small">{e.kind} · {ago(e.at)}</span>
              </div>
              <div className="ev-body">{e.body}</div>
            </li>
          ))}
        </ol>
        <CommentForm id={d.id} onSaved={onChanged} />
      </section>
    </article>
  );
}

function AgentCard({ detail: d, onChanged, onOpenRun }: { detail: Detail; onChanged: () => void; onOpenRun: (id: number) => void }) {
  const [choice, setChoice] = useAgentChoice();
  const [note, setNote] = useState('');
  const [busy, setBusy] = useState(false);
  const run = useAction();
  const active = d.runs.find((r) => ACTIVE_RUN.includes(r.status));

  if (active) {
    return (
      <div className="card agent-card active">
        <Bot size={18} />
        <div className="grow">
          <strong>{active.status === 'queued' ? 'Waiting for a worker' : 'An agent is working on this'}</strong>
          <div className="muted small">
            Run {active.id} · {choiceLabel(active)}
            {active.worker ? ` on ${active.worker}` : ''}
          </div>
        </div>
        <button className="button" onClick={() => onOpenRun(active.id)} data-run={active.id}>
          Watch & steer <ChevronRight size={14} />
        </button>
        <Confirm
          title={active.status === 'queued' ? `Cancel run ${active.id}?` : `Stop run ${active.id}?`}
          description={active.status === 'queued' ? 'The item goes back to open.' : 'The agent is stopped; its worktree is kept if it has unpushed work.'}
          action={active.status === 'queued' ? 'Cancel run' : 'Stop agent'}
          onConfirm={() => void run(async () => {
            await post(`/api/runs/${active.id}/commands`, { kind: 'stop' });
            onChanged();
          })}
          trigger={
            <button className="icon-button" aria-label="Stop">
              <Square size={14} />
            </button>
          }
        />
      </div>
    );
  }

  const queue = async () => {
    setBusy(true);
    const ok = await run(
      () => post(`/api/feedback/${d.id}/queue`, { ...choice, note: note.trim() || null }),
      `Queued #${d.id} for ${choiceLabel(choice)}`,
    );
    setBusy(false);
    if (ok) {
      setNote('');
      onChanged();
    }
  };
  return (
    <form
      className="card agent-card queue"
      onSubmit={(e) => {
        e.preventDefault();
        void queue();
      }}
    >
      <div className="card-title">
        <Bot size={16} /> Send to an agent
      </div>
      <AgentPicker value={choice} onChange={setChoice} />
      <AutoTextarea
        name="note"
        placeholder="Instructions for the agent (optional) — it always gets the full report and how to work"
        value={note}
        onChange={(e) => setNote(e.target.value)}
        onKeyDown={(e) => {
          if (e.key === 'Enter' && (e.metaKey || e.ctrlKey)) void queue();
        }}
      />
      <div className="row end">
        <span className="muted small">{submitShortcut}</span>
        <button className="button primary" disabled={busy} data-queue>
          <Send size={15} /> Queue
        </button>
      </div>
    </form>
  );
}

function Gallery({ detail }: { detail: Detail }) {
  const [urls, setUrls] = useState<Record<number, string>>({});
  useEffect(() => {
    for (const s of detail.screenshots) {
      void screenshotUrl(detail.id, s.idx).then((url) => setUrls((all) => ({ ...all, [s.idx]: url })));
    }
  }, [detail.id, detail.screenshots]);
  return (
    <div className="gallery">
      {detail.screenshots.map((s) => (
        <Dialog.Root key={s.idx}>
          <Dialog.Trigger asChild>
            <figure className="shot" tabIndex={0}>
              {urls[s.idx] ? <img src={urls[s.idx]} alt={`Screenshot ${s.idx + 1}`} /> : <div className="shot-placeholder" />}
              <figcaption>
                <strong>#{s.idx + 1}</strong> {s.note}
              </figcaption>
            </figure>
          </Dialog.Trigger>
          <Dialog.Portal>
            <Dialog.Overlay className="overlay" />
            <Dialog.Content className="lightbox" aria-describedby={undefined}>
              <Dialog.Title className="visually-hidden">Screenshot {s.idx + 1}</Dialog.Title>
              {urls[s.idx] && <img src={urls[s.idx]} alt={`Screenshot ${s.idx + 1}`} />}
              {s.note && <p>#{s.idx + 1} {s.note}</p>}
              <Dialog.Close className="icon-button lightbox-close" aria-label="Close">
                <X size={18} />
              </Dialog.Close>
            </Dialog.Content>
          </Dialog.Portal>
        </Dialog.Root>
      ))}
    </div>
  );
}

function EditDialog({ detail: d, onSaved }: { detail: Detail; onSaved: () => void }) {
  const [open, setOpen] = useState(false);
  const [message, setMessage] = useState(d.message);
  const [kind, setKind] = useState(d.kind);
  const [notes, setNotes] = useState<Record<number, string>>({});
  const run = useAction();
  useEffect(() => {
    if (open) {
      setMessage(d.message);
      setKind(d.kind);
      setNotes(Object.fromEntries(d.screenshots.map((s) => [s.idx, s.note ?? ''])));
    }
  }, [open, d]);
  const save = async () => {
    const ok = await run(
      () =>
        patch(`/api/feedback/${d.id}`, {
          message,
          kind,
          screenshot_notes: d.screenshots.map((s) => ({ idx: s.idx, note: notes[s.idx]?.trim() || null })),
        }),
      `Saved #${d.id}`,
    );
    if (ok) {
      setOpen(false);
      onSaved();
    }
  };
  return (
    <Dialog.Root open={open} onOpenChange={setOpen}>
      <Dialog.Trigger className="button" data-edit>
        <Pencil size={14} /> Edit
      </Dialog.Trigger>
      <Dialog.Portal>
        <Dialog.Overlay className="overlay" />
        <Dialog.Content className="dialog">
          <Dialog.Title className="dialog-title">Edit #{d.id}</Dialog.Title>
          <Dialog.Description className="dialog-description">
            Clarify the report before handing it to an agent. The previous text stays on the timeline.
          </Dialog.Description>
          <form
            className="edit-form"
            onSubmit={(e) => {
              e.preventDefault();
              void save();
            }}
          >
            <ToggleGroup.Root type="single" value={kind} onValueChange={(v) => v && setKind(v)} className="segmented" aria-label="Kind">
              {Object.entries(KINDS).map(([k, label]) => (
                <ToggleGroup.Item key={k} value={k} className="segment">
                  {KIND_ICON[k]} {label}
                </ToggleGroup.Item>
              ))}
            </ToggleGroup.Root>
            <label className="field">
              <span>Message</span>
              <AutoTextarea name="message" value={message} onChange={(e) => setMessage(e.target.value)} />
            </label>
            {d.screenshots.map((s) => (
              <label key={s.idx} className="field">
                <span>Note on screenshot #{s.idx + 1}</span>
                <input value={notes[s.idx] ?? ''} onChange={(e) => setNotes({ ...notes, [s.idx]: e.target.value })} />
              </label>
            ))}
            <div className="dialog-actions">
              <Dialog.Close className="button" type="button">
                Cancel
              </Dialog.Close>
              <button className="button primary">Save</button>
            </div>
          </form>
        </Dialog.Content>
      </Dialog.Portal>
    </Dialog.Root>
  );
}

function CommentForm({ id, onSaved }: { id: number; onSaved: () => void }) {
  const [body, setBody] = useState('');
  const run = useAction();
  const submit = async () => {
    if (!body.trim()) return;
    if (await run(() => post(`/api/feedback/${id}/events`, { body }))) {
      setBody('');
      onSaved();
    }
  };
  return (
    <form
      className="comment"
      onSubmit={(e) => {
        e.preventDefault();
        void submit();
      }}
    >
      <AutoTextarea
        name="body"
        placeholder="Add a comment (agents see comments in their prompt)"
        value={body}
        onChange={(e) => setBody(e.target.value)}
        onKeyDown={(e) => {
          if (e.key === 'Enter' && (e.metaKey || e.ctrlKey)) void submit();
        }}
      />
      <button className="button" disabled={!body.trim()}>
        Comment
      </button>
    </form>
  );
}

function LogBlock({ title, entries }: { title: string; entries: unknown }) {
  if (!Array.isArray(entries) || entries.length === 0) return null;
  const lines = entries
    .map((e) => (typeof e === 'object' && e ? Object.values(e as Record<string, unknown>).map(String).join('  ') : String(e)))
    .join('\n');
  return (
    <details className="logs">
      <summary>
        {title} ({entries.length})
      </summary>
      <pre>{lines}</pre>
    </details>
  );
}

export type { AgentChoice };
