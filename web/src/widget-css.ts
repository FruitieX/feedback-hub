// Styles live inside the widget's shadow root so host apps can't affect them.
export const WIDGET_CSS = `
:host { all: initial; }
* { box-sizing: border-box; }
.tab, .backdrop, .annotator, .toast {
  --bg: #ffffff; --fg: #111827; --muted: #6b7280; --line: #e5e7eb; --soft: #f3f4f6;
  --accent: #4f46e5; --accent-fg: #ffffff; --danger: #dc2626;
  font: 14px/1.4 system-ui, -apple-system, "Segoe UI", Roboto, sans-serif;
  color: var(--fg);
}
@media (prefers-color-scheme: dark) {
  .tab, .backdrop, .annotator, .toast {
    --bg: #1f2937; --fg: #f9fafb; --muted: #9ca3af; --line: #374151; --soft: #111827;
    --accent: #818cf8; --accent-fg: #111827;
  }
}
button { font: inherit; color: inherit; cursor: pointer; }
[hidden] { display: none !important; }

.tab {
  position: fixed; z-index: 2147483600; display: flex; align-items: center; gap: 6px;
  writing-mode: vertical-rl; padding: 10px 5px; border: 1px solid var(--line);
  background: var(--bg); color: var(--fg); opacity: .72; font-size: 12px; font-weight: 600;
  letter-spacing: .02em; box-shadow: 0 2px 10px rgba(0,0,0,.18); transform: translateY(-50%);
  transition: opacity .15s, padding .15s;
}
.tab svg { transform: rotate(90deg); }
.tab:hover, .tab:focus-visible { opacity: 1; padding-inline: 7px; }
.tab.right { right: 0; border-right: 0; border-radius: 8px 0 0 8px; }
.tab.left { left: 0; border-left: 0; border-radius: 0 8px 8px 0; transform: translateY(-50%) rotate(180deg); }

.toast {
  position: fixed; z-index: 2147483647; left: 50%; bottom: 24px; transform: translateX(-50%);
  background: var(--fg); color: var(--bg); padding: 10px 16px; border-radius: 999px;
  box-shadow: 0 6px 24px rgba(0,0,0,.25); font-weight: 500; max-width: calc(100vw - 32px);
}

.backdrop {
  position: fixed; inset: 0; z-index: 2147483640; background: rgba(15,23,42,.45);
  display: flex; align-items: flex-end; justify-content: flex-end; padding: 16px;
}
.panel {
  width: min(440px, 100%); max-height: calc(100dvh - 32px); overflow: auto;
  background: var(--bg); border-radius: 14px; padding: 16px; display: flex; flex-direction: column; gap: 12px;
  box-shadow: 0 20px 50px rgba(0,0,0,.35); border: 1px solid var(--line);
}
@media (max-width: 520px) {
  .backdrop { padding: 0; align-items: stretch; }
  .panel { width: 100%; max-height: 100dvh; border-radius: 0; padding: 16px 16px max(16px, env(safe-area-inset-bottom)); }
}
header { display: flex; justify-content: space-between; align-items: flex-start; gap: 12px; }
h2 { margin: 0; font-size: 17px; }
.sub { margin: 2px 0 0; color: var(--muted); font-size: 12px; }
.icon { border: 0; background: transparent; width: 32px; height: 32px; border-radius: 8px; flex: none; }
.icon:hover { background: var(--soft); }

.kinds { display: flex; gap: 6px; }
.kinds button {
  flex: 1; padding: 8px 6px; border-radius: 9px; border: 1px solid var(--line); background: var(--bg);
}
.kinds button[aria-checked="true"] { border-color: var(--accent); box-shadow: inset 0 0 0 1px var(--accent); font-weight: 600; }

textarea, input:not([type]), input[name=reporter] {
  width: 100%; border: 1px solid var(--line); border-radius: 9px; padding: 9px 10px;
  background: var(--soft); color: var(--fg); font: inherit; resize: vertical;
}
textarea:focus, input:focus { outline: 2px solid var(--accent); outline-offset: -1px; }

.shots-header { display: flex; justify-content: space-between; align-items: baseline; font-weight: 600; }
.hint { color: var(--muted); font-weight: 400; font-size: 12px; }
.shots { display: grid; grid-template-columns: repeat(auto-fill, minmax(118px, 1fr)); gap: 8px; }
.shots .empty { margin: 0; color: var(--muted); font-size: 13px; }
.shot { position: relative; }
.thumb {
  display: block; width: 100%; aspect-ratio: 4 / 3; padding: 0; border: 1px solid var(--line);
  border-radius: 9px; overflow: hidden; background: var(--soft); position: relative;
}
.thumb img { width: 100%; height: 100%; object-fit: cover; object-position: top; display: block; }
.badge {
  position: absolute; left: 6px; bottom: 6px; background: rgba(17,24,39,.8); color: #fff;
  font-size: 11px; padding: 2px 7px; border-radius: 999px;
}
.remove {
  position: absolute; top: 4px; right: 4px; width: 24px; height: 24px; font-size: 12px;
  background: rgba(17,24,39,.75); color: #fff; border-radius: 999px;
}
.remove:hover { background: var(--danger); }
.add-row { display: flex; flex-wrap: wrap; gap: 6px; }

.secondary, .primary {
  border-radius: 9px; padding: 8px 12px; border: 1px solid var(--line); background: var(--bg); font-weight: 500;
}
.secondary:hover { background: var(--soft); }
.primary { background: var(--accent); border-color: var(--accent); color: var(--accent-fg); font-weight: 600; padding-inline: 18px; }
.primary:disabled { opacity: .6; cursor: progress; }
.status { margin: 0; min-height: 1em; color: var(--muted); font-size: 13px; }
.status:empty { display: none; }
.field { display: flex; flex-direction: column; gap: 4px; font-weight: 600; }
.field em { font-weight: 400; color: var(--muted); font-style: normal; }
.check { display: flex; gap: 8px; align-items: flex-start; color: var(--muted); font-size: 13px; }
.check input { margin-top: 2px; accent-color: var(--accent); }
footer { display: flex; justify-content: space-between; align-items: center; gap: 8px; }
.link { border: 0; background: none; color: var(--muted); font-size: 12px; text-decoration: underline; padding: 4px 0; }

.annotator {
  position: fixed; inset: 0; z-index: 2147483646; background: #0b1020; display: flex; flex-direction: column;
}
.toolbar {
  display: flex; flex-wrap: wrap; gap: 8px; padding: 8px max(8px, env(safe-area-inset-right)) 8px max(8px, env(safe-area-inset-left));
  background: var(--bg); border-bottom: 1px solid var(--line); align-items: center;
}
.toolbar .group { display: flex; gap: 4px; align-items: center; }
.toolbar .end { margin-left: auto; }
.toolbar [data-tool], .toolbar [data-action=undo], .toolbar [data-action=clear] {
  border: 1px solid var(--line); background: var(--bg); border-radius: 8px; padding: 6px 9px; display: flex; gap: 5px; align-items: center;
}
.toolbar [aria-pressed="true"][data-tool] { background: var(--accent); color: var(--accent-fg); border-color: var(--accent); }
.swatch { width: 26px; height: 26px; border-radius: 999px; border: 2px solid var(--line); background: var(--c); }
.swatch[aria-pressed="true"] { outline: 2px solid var(--accent); outline-offset: 2px; }
@media (max-width: 640px) { .toolbar button span { display: none; } }
.stage { flex: 1; min-height: 0; display: flex; align-items: center; justify-content: center; padding: 12px; }
.stage canvas {
  max-width: 100%; max-height: 100%; touch-action: none; cursor: crosshair; background: #fff;
  box-shadow: 0 10px 40px rgba(0,0,0,.5); border-radius: 4px;
}
`;
