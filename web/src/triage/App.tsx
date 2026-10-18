import { Bot, LogOut, MessageSquare } from 'lucide-react';
import { Tabs, Tooltip } from 'radix-ui';
import { FormEvent, useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { ChoiceContext, WorkersContext, WorkersState } from './AgentPicker';
import { AgentsView } from './AgentsView';
import { api, getToken, onTokenChange, post, setToken, watchChanges } from './api';
import { FeedbackView } from './FeedbackView';
import { AgentChoice, AppInfo, Run, WorkerStatus } from './types';
import { ToastProvider, usePoll } from './ui';

type View = 'feedback' | 'agents';

interface Route {
  view: View;
  id: number | null;
  run: number | null;
}

function readRoute(): Route {
  const hash = new URLSearchParams(location.hash.slice(1));
  return {
    view: hash.get('view') === 'agents' ? 'agents' : 'feedback',
    id: Number(hash.get('id')) || null,
    run: Number(hash.get('run')) || null,
  };
}

function writeRoute(route: Route) {
  const params = new URLSearchParams();
  if (route.view === 'agents') {
    params.set('view', 'agents');
    if (route.run) params.set('run', String(route.run));
  } else if (route.id) {
    params.set('id', String(route.id));
  }
  const hash = `#${params}`;
  if (location.hash !== hash) history.pushState(null, '', hash);
}

export function App() {
  const [token, setTokenState] = useState(getToken());
  useEffect(() => onTokenChange(() => setTokenState(getToken())), []);
  return (
    <ToastProvider>
      <Tooltip.Provider delayDuration={300}>{token ? <Signed /> : <Login />}</Tooltip.Provider>
    </ToastProvider>
  );
}

function Login() {
  const [value, setValue] = useState('');
  const submit = (event: FormEvent) => {
    event.preventDefault();
    setToken(value.trim());
  };
  return (
    <main className="login">
      <form method="post" action="#" onSubmit={submit}>
        <h1>
          <MessageSquare size={20} /> Feedback Hub
        </h1>
        <p>Enter the admin token to view feedback.</p>
        {/* Lets password managers treat this as a normal login; only the token is checked. */}
        <input type="text" name="username" defaultValue="admin" autoComplete="username" className="visually-hidden" tabIndex={-1} aria-hidden="true" />
        <input
          type="password"
          name="token"
          placeholder="Admin token"
          autoComplete="current-password"
          required
          value={value}
          onChange={(e) => setValue(e.target.value)}
          autoFocus
        />
        <button className="button primary">Sign in</button>
      </form>
    </main>
  );
}

function Signed() {
  const [route, setRoute] = useState<Route>(readRoute);
  const [lastChoice, setLastChoice] = useState<AgentChoice | null>(null);
  const choice = useMemo(() => ({ last: lastChoice, remember: setLastChoice }), [lastChoice]);
  const [apps, setApps] = useState<AppInfo[]>([]);
  const [workers, setWorkers] = useState<WorkerStatus[]>([]);
  const [running, setRunning] = useState(0);
  const [refreshing, setRefreshing] = useState(false);
  const refreshStarted = useRef<string | null>(null);

  useEffect(() => {
    const onPop = () => setRoute(readRoute());
    window.addEventListener('popstate', onPop);
    fetch('/api/apps')
      .then((r) => r.json())
      .then(setApps)
      .catch(() => setApps([]));
    api<AgentChoice>('/api/agent-choice').then(setLastChoice, () => {});
    // Lets the widget on the owner's apps recognise them and offer "send & fix".
    void post('/api/session', {}).catch(() => {});
    return () => window.removeEventListener('popstate', onPop);
  }, []);
  // Views reload on the hub's change events (see usePoll).
  useEffect(() => watchChanges(), []);

  const go = useCallback((next: Partial<Route>) => {
    setRoute((current) => {
      const route = { ...current, ...next };
      writeRoute(route);
      return route;
    });
  }, []);

  const loadWorkers = useCallback(async () => {
    try {
      const [list, active] = await Promise.all([
        api<WorkerStatus[]>('/api/workers'),
        api<Run[]>('/api/runs?active=true&limit=200'),
      ]);
      setWorkers(list);
      setRunning(active.filter((r) => r.status === 'running').length);
      // A refresh is done once every worker has checked in again since it was requested.
      if (refreshStarted.current && list.every((w) => w.last_seen > refreshStarted.current!)) {
        refreshStarted.current = null;
        setRefreshing(false);
      }
    } catch {
      /* the next poll retries */
    }
  }, []);
  usePoll(loadWorkers, refreshing ? 2_000 : 10_000, [refreshing]);

  const workersState: WorkersState = useMemo(
    () => ({
      workers,
      refreshing,
      refresh: () => {
        // Model lists are re-read in the background; give the CLIs a moment.
        refreshStarted.current = new Date(Date.now() + 3_000).toISOString().replace(/\.\d+Z$/, 'Z');
        setRefreshing(true);
        void post('/api/workers/refresh', {}).catch(() => setRefreshing(false));
        setTimeout(() => setRefreshing(false), 60_000);
      },
    }),
    [workers, refreshing],
  );

  return (
    <WorkersContext.Provider value={workersState}>
      <ChoiceContext.Provider value={choice}>
      <div className="shell">
        <header className="top">
          <h1>
            <MessageSquare size={18} /> Feedback Hub
          </h1>
          <Tabs.Root value={route.view} onValueChange={(v) => go({ view: v as View })}>
            <Tabs.List className="tabs" aria-label="View">
              <Tabs.Trigger value="feedback" className="tab" data-view="feedback">
                <MessageSquare size={15} /> Feedback
              </Tabs.Trigger>
              <Tabs.Trigger value="agents" className="tab" data-view="agents">
                <Bot size={15} /> Agents
                {running > 0 && <span className="badge">{running}</span>}
              </Tabs.Trigger>
            </Tabs.List>
          </Tabs.Root>
          <div className="spacer" />
          <button
            className="button ghost"
            onClick={() => {
              void fetch('/api/session', { method: 'DELETE' }).finally(() => setToken(''));
            }}
            data-logout
          >
            <LogOut size={15} /> <span className="hide-mobile">Sign out</span>
          </button>
        </header>
        {route.view === 'agents' ? (
          <AgentsView
            selected={route.run}
            onSelect={(run) => go({ run })}
            onOpenFeedback={(id) => go({ view: 'feedback', id })}
          />
        ) : (
          <FeedbackView
            apps={apps}
            selected={route.id}
            onSelect={(id) => go({ id })}
            onOpenRun={(run) => go({ view: 'agents', run })}
          />
        )}
      </div>
      </ChoiceContext.Provider>
    </WorkersContext.Provider>
  );
}
