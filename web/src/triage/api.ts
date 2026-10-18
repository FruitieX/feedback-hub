// Admin API client. The token lives in localStorage so the triage UI stays signed in.

const TOKEN_KEY = 'feedbackHub.token';

export const storage = (() => {
  try {
    return window.localStorage;
  } catch {
    return null;
  }
})();

let token = storage?.getItem(TOKEN_KEY) ?? '';
const listeners = new Set<() => void>();

export const getToken = () => token;

export function setToken(value: string) {
  token = value;
  if (value) storage?.setItem(TOKEN_KEY, value);
  else storage?.removeItem(TOKEN_KEY);
  for (const listener of listeners) listener();
}

export function onTokenChange(listener: () => void) {
  listeners.add(listener);
  return () => {
    listeners.delete(listener);
  };
}

export async function api<T>(path: string, init: RequestInit = {}): Promise<T> {
  const response = await fetch(path, {
    ...init,
    headers: { authorization: `Bearer ${token}`, 'content-type': 'application/json', ...init.headers },
  });
  if (response.status === 401) {
    setToken('');
    throw new Error('Signed out: the token was not accepted');
  }
  if (!response.ok) {
    const body = await response.json().catch(() => ({}));
    throw new Error(body.error ?? `HTTP ${response.status}`);
  }
  const text = await response.text();
  return (text ? JSON.parse(text) : undefined) as T;
}

const changeListeners = new Set<() => void>();

/** Calls `listener` whenever the hub reports a change; returns an unsubscribe function. */
export function onHubChange(listener: () => void) {
  changeListeners.add(listener);
  return () => {
    changeListeners.delete(listener);
  };
}

/**
 * Follows the hub's change stream (`/api/changes`, server-sent events) and
 * notifies `onHubChange` listeners, reconnecting with backoff. A stream with no
 * keep-alive for a while (e.g. after the phone slept) is replaced. Returns a stop function.
 */
export function watchChanges(): () => void {
  let stopped = false;
  let controller: AbortController | null = null;
  const notify = () => {
    for (const listener of changeListeners) listener();
  };
  void (async () => {
    let connected = false;
    let delay = 1_000;
    while (!stopped) {
      controller = new AbortController();
      const abort = controller.abort.bind(controller);
      let watchdog = setTimeout(abort, 45_000);
      try {
        const response = await fetch('/api/changes', {
          headers: { authorization: `Bearer ${token}` },
          signal: controller.signal,
        });
        if (response.status === 401) return setToken('');
        if (!response.ok || !response.body) throw new Error(`HTTP ${response.status}`);
        // Changes may have been missed while disconnected.
        if (connected) notify();
        connected = true;
        delay = 1_000;
        const reader = response.body.pipeThrough(new TextDecoderStream()).getReader();
        let buffer = '';
        for (;;) {
          const { value, done } = await reader.read();
          if (done) break;
          clearTimeout(watchdog);
          watchdog = setTimeout(abort, 45_000);
          buffer += value;
          const events = buffer.split('\n\n');
          buffer = events.pop() ?? '';
          if (events.some((e) => e.split('\n').includes('event: change'))) notify();
        }
      } catch {
        /* reconnect below */
      } finally {
        clearTimeout(watchdog);
      }
      if (stopped) return;
      await new Promise((resolve) => setTimeout(resolve, delay));
      delay = Math.min(delay * 2, 30_000);
    }
  })();
  return () => {
    stopped = true;
    controller?.abort();
  };
}

export const post = <T>(path: string, body: unknown) => api<T>(path, { method: 'POST', body: JSON.stringify(body) });
export const patch = <T>(path: string, body: unknown) => api<T>(path, { method: 'PATCH', body: JSON.stringify(body) });

const imageCache = new Map<string, Promise<string>>();

/** Screenshots need the token, so they are fetched and shown as blob URLs. */
export function screenshotUrl(id: number, idx: number) {
  const key = `${id}/${idx}`;
  if (!imageCache.has(key)) {
    imageCache.set(
      key,
      fetch(`/api/feedback/${id}/screenshots/${idx}`, { headers: { authorization: `Bearer ${token}` } })
        .then((r) => r.blob())
        .then((b) => URL.createObjectURL(b)),
    );
  }
  return imageCache.get(key)!;
}

export function ago(iso: string) {
  const seconds = (Date.now() - new Date(iso).getTime()) / 1000;
  if (seconds < 60) return 'just now';
  if (seconds < 3600) return `${Math.round(seconds / 60)} min ago`;
  if (seconds < 86400) return `${Math.round(seconds / 3600)} h ago`;
  return `${Math.round(seconds / 86400)} d ago`;
}

export function duration(from: string | null, to: string | null) {
  if (!from) return '';
  const seconds = Math.max(0, ((to ? new Date(to) : new Date()).getTime() - new Date(from).getTime()) / 1000);
  if (seconds < 60) return `${Math.round(seconds)} s`;
  if (seconds < 3600) return `${Math.floor(seconds / 60)} min`;
  return `${Math.floor(seconds / 3600)} h ${Math.round((seconds % 3600) / 60)} min`;
}

export const clock = (iso: string) =>
  new Date(iso).toLocaleTimeString([], { hour: '2-digit', minute: '2-digit', second: '2-digit', hourCycle: 'h23' });
