// Feedback widget: a small corner button that opens a feedback form with annotated
// screenshots. Injected into private app pages (by the gateway or a script tag):
//
//   <script async src="https://feedback.example.org/widget.js" data-app="myapp"></script>
//
// Optional data attributes: data-position="left|right" (bottom corner),
// data-version="1.2.3", data-label="Feedback".
// Apps can call window.FeedbackHub.open(), .setContext({...}), .setReporter(name).

import { domToCanvas } from 'modern-screenshot';
import { Annotator, encodeCanvas, type Shot } from './annotate';
import { WIDGET_CSS } from './widget-css';

declare global {
  interface Window {
    FeedbackHub?: FeedbackHubApi;
  }
}

interface FeedbackHubApi {
  open(): void;
  setContext(context: Record<string, unknown>): void;
  setReporter(name: string): void;
  hideButton(): void;
  showButton(): void;
}

type Kind = 'bug' | 'idea' | 'other';
interface LogEntry {
  at: string;
  level: string;
  message: string;
}
interface NetEntry {
  at: string;
  method: string;
  status: string;
  url: string;
}

/**
 * WebGL canvases (maps, Pixi scenes…) are blank to toDataURL() after each frame
 * unless created with preserveDrawingBuffer. The gateway injects this same patch
 * inline at the top of <head> (see the homelab's widget-injection.lua) so it runs
 * before the app creates its contexts; this copy covers script-tag installs.
 * Keep the `__fhGl` flag in sync with that snippet.
 */
export function preserveWebGlBuffers() {
  const w = window as unknown as { __fhGl?: number };
  if (w.__fhGl) return;
  w.__fhGl = 1;
  const proto = HTMLCanvasElement.prototype;
  const original = proto.getContext;
  proto.getContext = function (this: HTMLCanvasElement, type: string, attrs?: unknown) {
    if (/webgl/.test(type)) attrs = { ...(attrs as object), preserveDrawingBuffer: true };
    return original.call(this, type, attrs as never);
  } as typeof proto.getContext;
}

const MAX_SHOTS = 8;
const MAX_IMAGE_SIDE = 2560;
const STORAGE_REPORTER = 'feedbackHub.reporter';
const STORAGE_HIDDEN = 'feedbackHub.hidden';

function safeStorage(kind: 'local' | 'session'): Storage | null {
  try {
    return kind === 'local' ? window.localStorage : window.sessionStorage;
  } catch {
    return null;
  }
}

const script =
  (document.currentScript as HTMLScriptElement | null) ??
  document.querySelector<HTMLScriptElement>('script[src*="/widget.js"]');

const SENSITIVE_PARAM = /token|key|secret|pass|auth|sig|session|code|^[tsp]$/i;

function redactUrl(raw: string, full = false): string {
  try {
    const url = new URL(raw, location.href);
    for (const name of [...url.searchParams.keys()]) {
      if (SENSITIVE_PARAM.test(name)) url.searchParams.set(name, '…');
    }
    const text = url.origin === location.origin && !full ? url.pathname + url.search : url.href;
    return text.slice(0, 300);
  } catch {
    return raw.slice(0, 300);
  }
}

function describe(value: unknown): string {
  if (value instanceof Error) return `${value.name}: ${value.message}${value.stack ? `\n${value.stack.split('\n').slice(1, 4).join('\n')}` : ''}`;
  if (typeof value === 'string') return value;
  try {
    return JSON.stringify(value);
  } catch {
    return String(value);
  }
}

// --- Diagnostics: recent console errors/warnings and failed requests -------

const logs: LogEntry[] = [];
const network: NetEntry[] = [];
const now = () => new Date().toISOString().slice(11, 23);

function pushLog(level: string, args: unknown[]) {
  logs.push({ at: now(), level, message: args.map(describe).join(' ').slice(0, 600) });
  if (logs.length > 40) logs.shift();
}

function pushNet(entry: NetEntry) {
  network.push(entry);
  if (network.length > 25) network.shift();
}

function installDiagnostics() {
  for (const level of ['error', 'warn'] as const) {
    const original = console[level].bind(console);
    console[level] = (...args: unknown[]) => {
      pushLog(level, args);
      original(...args);
    };
  }
  window.addEventListener('error', (event) => {
    pushLog('uncaught', [event.error ?? event.message]);
  });
  window.addEventListener('unhandledrejection', (event) => {
    pushLog('unhandledrejection', [event.reason]);
  });
  const originalFetch = window.fetch.bind(window);
  window.fetch = async (input: RequestInfo | URL, init?: RequestInit) => {
    const method = (init?.method ?? (input instanceof Request ? input.method : 'GET')).toUpperCase();
    const url = input instanceof Request ? input.url : String(input);
    try {
      const response = await originalFetch(input, init);
      if (response.status >= 400) {
        pushNet({ at: now(), method, status: String(response.status), url: redactUrl(url) });
      }
      return response;
    } catch (error) {
      if (!(error instanceof DOMException && error.name === 'AbortError')) {
        pushNet({ at: now(), method, status: 'network error', url: redactUrl(url) });
      }
      throw error;
    }
  };
}

// --- Image helpers -----------------------------------------------------------

function loadImage(src: string): Promise<HTMLImageElement> {
  return new Promise((resolve, reject) => {
    const img = new Image();
    img.onload = () => resolve(img);
    img.onerror = () => reject(new Error('Could not read the image'));
    img.src = src;
  });
}

async function normalizeImage(src: string): Promise<string> {
  const img = await loadImage(src);
  const ratio = Math.min(1, MAX_IMAGE_SIDE / Math.max(img.naturalWidth, img.naturalHeight));
  const canvas = document.createElement('canvas');
  canvas.width = Math.round(img.naturalWidth * ratio);
  canvas.height = Math.round(img.naturalHeight * ratio);
  canvas.getContext('2d')!.drawImage(img, 0, 0, canvas.width, canvas.height);
  return encodeCanvas(canvas);
}

function readFile(file: Blob): Promise<string> {
  return new Promise((resolve, reject) => {
    const reader = new FileReader();
    reader.onload = () => resolve(String(reader.result));
    reader.onerror = () => reject(reader.error);
    reader.readAsDataURL(file);
  });
}

// --- Widget ----------------------------------------------------------------

const ICON_CAMERA =
  '<svg viewBox="0 0 24 24" width="18" height="18" aria-hidden="true"><path fill="currentColor" d="M9 4h6l1.5 2H20a2 2 0 0 1 2 2v10a2 2 0 0 1-2 2H4a2 2 0 0 1-2-2V8a2 2 0 0 1 2-2h3.5L9 4zm3 4.5a4.5 4.5 0 1 0 0 9 4.5 4.5 0 0 0 0-9zm0 2a2.5 2.5 0 1 1 0 5 2.5 2.5 0 0 1 0-5z"/></svg>';
const ICON_CHAT =
  '<svg viewBox="0 0 24 24" width="16" height="16" aria-hidden="true"><path fill="currentColor" d="M4 4h16a2 2 0 0 1 2 2v10a2 2 0 0 1-2 2H9l-5 4v-4a2 2 0 0 1-2-2V6a2 2 0 0 1 2-2zm3 5v2h10V9H7zm0 4v2h7v-2H7z"/></svg>';

class Widget {
  private host: HTMLElement;
  private root: ShadowRoot;
  private tab: HTMLButtonElement;
  private dialog: HTMLElement | null = null;
  private shots: Shot[] = [];
  private kind: Kind = 'bug';
  private extraContext: Record<string, unknown> = {};
  private reporter = safeStorage('local')?.getItem(STORAGE_REPORTER) ?? '';
  private message = '';
  private includeDiagnostics = true;
  private busy = false;
  /** Set by "Page snapshot": the next button press captures instead of opening. */
  private armed = false;

  constructor(
    private hub: string,
    private app: string,
    private options: { position: 'left' | 'right'; label: string; version?: string },
  ) {
    this.host = document.createElement('feedback-hub-widget');
    this.host.style.cssText = 'all: initial; position: fixed; z-index: 2147483600; top: 0; left: 0; width: 0; height: 0;';
    this.root = this.host.attachShadow({ mode: 'open' });
    const style = document.createElement('style');
    style.textContent = WIDGET_CSS;
    this.root.append(style);
    this.tab = document.createElement('button');
    this.tab.className = `fab ${options.position}`;
    this.tab.type = 'button';
    this.tab.title = options.label;
    this.tab.setAttribute('aria-label', options.label);
    this.tab.innerHTML = `${ICON_CHAT}<span class="label"></span>`;
    this.tab.addEventListener('click', () => void this.open());
    this.root.append(this.tab);
    if (safeStorage('session')?.getItem(STORAGE_HIDDEN) === '1') this.tab.hidden = true;
    document.body.append(this.host);
  }

  api(): FeedbackHubApi {
    return {
      open: () => void this.open(),
      setContext: (context) => {
        this.extraContext = { ...this.extraContext, ...context };
      },
      setReporter: (name) => {
        this.reporter = name;
      },
      hideButton: () => {
        this.tab.hidden = true;
      },
      showButton: () => {
        this.tab.hidden = false;
        safeStorage('session')?.removeItem(STORAGE_HIDDEN);
      },
    };
  }

  private async snapshotPage(): Promise<string> {
    const scale = Math.min(window.devicePixelRatio || 1, 2);
    const body = getComputedStyle(document.body).backgroundColor;
    const html = getComputedStyle(document.documentElement).backgroundColor;
    const transparent = (c: string) => c === 'transparent' || c === 'rgba(0, 0, 0, 0)';
    const canvas = await domToCanvas(document.documentElement, {
      width: window.innerWidth,
      height: window.innerHeight,
      scale,
      timeout: 15000,
      backgroundColor: !transparent(body) ? body : !transparent(html) ? html : '#ffffff',
      filter: (node) => node !== this.host,
      features: { restoreScrollPosition: true },
      style: {
        transform: `translate(${-window.scrollX}px, ${-window.scrollY}px)`,
        transformOrigin: 'top left',
      },
    });
    return encodeCanvas(canvas);
  }

  private async captureScreen(): Promise<string> {
    // Ask the browser for this tab; accurate for canvas/WebGL content too.
    const stream = await navigator.mediaDevices.getDisplayMedia({
      video: { displaySurface: 'browser' },
      audio: false,
      preferCurrentTab: true,
    } as DisplayMediaStreamOptions);
    try {
      const video = document.createElement('video');
      video.srcObject = stream;
      video.muted = true;
      await video.play();
      await new Promise((r) => setTimeout(r, 300));
      const canvas = document.createElement('canvas');
      canvas.width = video.videoWidth;
      canvas.height = video.videoHeight;
      canvas.getContext('2d')!.drawImage(video, 0, 0);
      return await normalizeImage(canvas.toDataURL('image/png'));
    } finally {
      stream.getTracks().forEach((t) => t.stop());
    }
  }

  private toast(text: string) {
    const el = document.createElement('div');
    el.className = 'toast';
    el.textContent = text;
    this.root.append(el);
    return () => el.remove();
  }

  private setArmed(armed: boolean) {
    this.armed = armed;
    this.tab.classList.toggle('armed', armed);
    this.tab.title = armed ? 'Capture this page for your feedback' : this.options.label;
    this.tab.innerHTML = `${armed ? ICON_CAMERA : ICON_CHAT}<span class="label">${armed ? 'Capture' : ''}</span>`;
    if (armed) this.tab.hidden = false;
  }

  async open() {
    if (this.dialog || this.busy) return;
    this.busy = true;
    if (this.armed || this.shots.length === 0) {
      this.setArmed(false);
      this.tab.hidden = true;
      const done = this.toast('Capturing screenshot…');
      try {
        this.shots.push({ base: await this.snapshotPage(), strokes: [], output: null });
      } catch (error) {
        console.debug('feedback-hub: page snapshot failed', error);
        const failed = this.toast('Could not capture the page; add an image instead.');
        setTimeout(failed, 3000);
      } finally {
        done();
        this.tab.hidden = safeStorage('session')?.getItem(STORAGE_HIDDEN) === '1';
      }
    }
    this.busy = false;
    this.renderDialog();
  }

  private close(reset = false) {
    this.dialog?.remove();
    this.dialog = null;
    if (reset) {
      this.shots = [];
      this.message = '';
      this.kind = 'bug';
    }
    document.removeEventListener('keydown', this.onKey, true);
  }

  private onKey = (event: KeyboardEvent) => {
    if (event.key === 'Escape' && this.dialog && !this.root.querySelector('.annotator')) {
      event.stopPropagation();
      this.close();
    }
  };

  private async addShot(producer: () => Promise<string>, status: HTMLElement) {
    if (this.shots.length >= MAX_SHOTS) {
      status.textContent = `At most ${MAX_SHOTS} screenshots.`;
      return;
    }
    status.textContent = 'Adding screenshot…';
    try {
      const base = await producer();
      this.shots.push({ base, strokes: [], output: null });
      status.textContent = '';
      this.renderShots();
    } catch (error) {
      status.textContent =
        error instanceof DOMException && error.name === 'NotAllowedError'
          ? 'Screen capture was cancelled.'
          : `Could not add screenshot: ${describe(error)}`;
    }
  }

  private renderDialog() {
    this.dialog?.remove();
    const dialog = document.createElement('div');
    dialog.className = 'backdrop';
    dialog.innerHTML = `
      <form class="panel" role="dialog" aria-modal="true" aria-label="Send feedback">
        <header>
          <div>
            <h2>Send feedback</h2>
            <p class="sub">${escapeHtml(this.app)} · goes to the developer's AI agents</p>
          </div>
          <button type="button" class="icon close" aria-label="Close">✕</button>
        </header>
        <div class="kinds" role="radiogroup" aria-label="Type">
          ${(['bug', 'idea', 'other'] as Kind[])
            .map(
              (k) =>
                `<button type="button" role="radio" data-kind="${k}" aria-checked="${k === this.kind}">${
                  { bug: 'Problem', idea: 'Idea', other: 'Other' }[k]
                }</button>`,
            )
            .join('')}
        </div>
        <textarea name="message" rows="4" placeholder="What happened, and what did you expect? Steps to reproduce help a lot."></textarea>
        <div class="shots-header">
          <span>Screenshots</span>
          <span class="hint">Tap one to circle or mark things</span>
        </div>
        <div class="shots"></div>
        <div class="add-row">
          <button type="button" class="secondary" data-add="page">+ Page snapshot</button>
          ${
            navigator.mediaDevices && 'getDisplayMedia' in navigator.mediaDevices
              ? '<button type="button" class="secondary" data-add="screen">+ Screen capture</button>'
              : ''
          }
          <button type="button" class="secondary" data-add="file">+ Image</button>
          <input type="file" accept="image/*" multiple hidden>
        </div>
        <p class="status" aria-live="polite"></p>
        <label class="field">
          <span>Your name <em>(optional)</em></span>
          <input name="reporter" autocomplete="name" maxlength="100">
        </label>
        <label class="check">
          <input type="checkbox" name="diagnostics">
          <span>Include page address, device info and recent errors</span>
        </label>
        <footer>
          <button type="button" class="link hide-tab">Hide button for this session</button>
          <button type="submit" class="primary">Send</button>
        </footer>
      </form>`;
    this.root.append(dialog);
    this.dialog = dialog;
    document.addEventListener('keydown', this.onKey, true);

    const form = dialog.querySelector('form')!;
    const textarea = form.querySelector('textarea')!;
    const reporter = form.querySelector<HTMLInputElement>('input[name=reporter]')!;
    const diagnostics = form.querySelector<HTMLInputElement>('input[name=diagnostics]')!;
    const status = form.querySelector<HTMLElement>('.status')!;
    const fileInput = form.querySelector<HTMLInputElement>('input[type=file]')!;
    textarea.value = this.message;
    reporter.value = this.reporter;
    diagnostics.checked = this.includeDiagnostics;
    textarea.addEventListener('input', () => (this.message = textarea.value));
    reporter.addEventListener('input', () => (this.reporter = reporter.value));
    diagnostics.addEventListener('change', () => (this.includeDiagnostics = diagnostics.checked));

    dialog.addEventListener('click', (event) => {
      if (event.target === dialog) this.close();
    });
    form.querySelector('.close')!.addEventListener('click', () => this.close());
    form.querySelector('.hide-tab')!.addEventListener('click', () => {
      safeStorage('session')?.setItem(STORAGE_HIDDEN, '1');
      this.tab.hidden = true;
      this.close();
    });
    for (const button of form.querySelectorAll<HTMLButtonElement>('[data-kind]')) {
      button.addEventListener('click', () => {
        this.kind = button.dataset.kind as Kind;
        for (const b of form.querySelectorAll('[data-kind]')) {
          b.setAttribute('aria-checked', String(b === button));
        }
      });
    }
    form.querySelector('[data-add=page]')!.addEventListener('click', () => {
      if (this.shots.length >= MAX_SHOTS) {
        status.textContent = `At most ${MAX_SHOTS} screenshots.`;
        return;
      }
      // Let the user get the page into the right state first; the next
      // button press takes the snapshot and brings the form back.
      this.close();
      this.setArmed(true);
      const done = this.toast('Set up the page, then tap Capture.');
      setTimeout(done, 3000);
    });
    form.querySelector('[data-add=screen]')?.addEventListener('click', async () => {
      dialog.hidden = true;
      await this.addShot(() => this.captureScreen(), status);
      dialog.hidden = false;
    });
    form.querySelector('[data-add=file]')!.addEventListener('click', () => fileInput.click());
    fileInput.addEventListener('change', async () => {
      for (const file of fileInput.files ?? []) {
        await this.addShot(async () => normalizeImage(await readFile(file)), status);
      }
      fileInput.value = '';
    });
    form.addEventListener('paste', async (event) => {
      const files = [...(event.clipboardData?.files ?? [])].filter((f) => f.type.startsWith('image/'));
      if (files.length === 0) return;
      event.preventDefault();
      for (const file of files) await this.addShot(async () => normalizeImage(await readFile(file)), status);
    });
    form.addEventListener('submit', (event) => {
      event.preventDefault();
      void this.submit(form, status);
    });

    this.renderShots();
    setTimeout(() => textarea.focus(), 50);
  }

  private renderShots() {
    const container = this.dialog?.querySelector<HTMLElement>('.shots');
    if (!container) return;
    container.innerHTML = '';
    if (this.shots.length === 0) {
      container.innerHTML = '<p class="empty">No screenshots yet.</p>';
    }
    this.shots.forEach((shot, index) => {
      const item = document.createElement('div');
      item.className = 'shot';
      const img = document.createElement('img');
      img.src = shot.output ?? shot.base;
      img.alt = `Screenshot ${index + 1}`;
      const edit = document.createElement('button');
      edit.type = 'button';
      edit.className = 'thumb';
      edit.title = 'Draw on screenshot';
      edit.append(img);
      const badge = document.createElement('span');
      badge.className = 'badge';
      badge.textContent = shot.strokes.length ? '✎ marked' : '✎ mark';
      edit.append(badge);
      edit.addEventListener('click', () => this.annotate(index));
      const remove = document.createElement('button');
      remove.type = 'button';
      remove.className = 'icon remove';
      remove.setAttribute('aria-label', `Remove screenshot ${index + 1}`);
      remove.textContent = '✕';
      remove.addEventListener('click', () => {
        this.shots.splice(index, 1);
        this.renderShots();
      });
      item.append(edit, remove);
      container.append(item);
    });
  }

  private annotate(index: number) {
    const shot = this.shots[index];
    const annotator = new Annotator(this.root, shot, (updated) => {
      if (updated) this.shots[index] = updated;
      this.renderShots();
    });
    void annotator.open();
  }

  private async submit(form: HTMLFormElement, status: HTMLElement) {
    const button = form.querySelector<HTMLButtonElement>('button[type=submit]')!;
    if (!this.message.trim() && this.shots.length === 0) {
      status.textContent = 'Add a short description or a screenshot.';
      return;
    }
    button.disabled = true;
    status.textContent = 'Sending…';
    safeStorage('local')?.setItem(STORAGE_REPORTER, this.reporter.trim());
    const diagnostics = this.includeDiagnostics;
    const context: Record<string, unknown> = { ...this.extraContext };
    if (this.options.version) context.appVersion = this.options.version;
    if (diagnostics) {
      Object.assign(context, {
        viewport: `${window.innerWidth}x${window.innerHeight} @${window.devicePixelRatio}x`,
        language: navigator.language,
        timezone: Intl.DateTimeFormat().resolvedOptions().timeZone,
        colorScheme: matchMedia('(prefers-color-scheme: dark)').matches ? 'dark' : 'light',
        installedPwa: matchMedia('(display-mode: standalone)').matches,
        online: navigator.onLine,
        logs,
        network,
      });
    }
    try {
      const response = await fetch(`${this.hub}/api/feedback`, {
        method: 'POST',
        headers: { 'content-type': 'application/json' },
        credentials: 'omit',
        body: JSON.stringify({
          app: this.app,
          kind: this.kind,
          message: this.message.trim(),
          reporter: this.reporter.trim() || null,
          url: diagnostics ? redactUrl(location.href, true) : null,
          title: diagnostics ? document.title : null,
          userAgent: diagnostics ? navigator.userAgent : null,
          source: 'web',
          context,
          screenshots: this.shots.map((s) => s.output ?? s.base),
        }),
      });
      const body = await response.json().catch(() => ({}));
      if (!response.ok) throw new Error(body.error ?? `HTTP ${response.status}`);
      this.close(true);
      const done = this.toast(`Thanks! Feedback #${body.id} was sent.`);
      setTimeout(done, 3500);
    } catch (error) {
      status.textContent = `Sending failed: ${describe(error)}. Your text is kept; try again.`;
      button.disabled = false;
    }
  }
}

function escapeHtml(text: string) {
  return text.replace(/[&<>"']/g, (c) => `&#${c.charCodeAt(0)};`);
}

function start() {
  if (window.FeedbackHub || !script?.src) return;
  const hub = new URL(script.src).origin;
  const data = script.dataset;
  const app = data.app || location.hostname.split('.')[0] || 'unknown';
  const meta = document.querySelector<HTMLMetaElement>('meta[name="app-version"], meta[name="version"]');
  const widget = new Widget(hub, app, {
    position: data.position === 'left' ? 'left' : 'right',
    label: data.label || 'Feedback',
    version: data.version || meta?.content || undefined,
  });
  window.FeedbackHub = widget.api();
  window.dispatchEvent(new Event('feedbackhub:ready'));
}

preserveWebGlBuffers();
installDiagnostics();
if (document.body) start();
else document.addEventListener('DOMContentLoaded', start, { once: true });
