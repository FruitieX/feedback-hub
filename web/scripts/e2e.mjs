// End-to-end check: a demo "app" on another origin loads the widget, captures
// and annotates a screenshot, submits it, and the triage UI shows it.
import { spawn } from 'node:child_process';
import { createServer } from 'node:http';
import { mkdtempSync, mkdirSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { chromium } from 'playwright';

const bin = resolve(process.env.FEEDBACK_HUB_BIN ?? '../target/debug/feedback-hub');
const token = 'e2e-admin-token-0123456789';
const hubPort = 18431;
const appPort = 18432;
const hub = `http://127.0.0.1:${hubPort}`;
const out = 'test-results';
mkdirSync(out, { recursive: true });
const dataDir = mkdtempSync(join(tmpdir(), 'feedback-hub-e2e-'));

const server = spawn(bin, ['serve'], {
  env: {
    ...process.env,
    FEEDBACK_HUB_BIND: `127.0.0.1:${hubPort}`,
    FEEDBACK_HUB_DATA_DIR: dataDir,
    FEEDBACK_HUB_STATIC_DIR: resolve('dist'),
    FEEDBACK_HUB_ADMIN_TOKEN: token,
    FEEDBACK_HUB_ALLOWED_ORIGINS: 'http://localhost:*,http://127.0.0.1:*',
  },
  stdio: 'inherit',
});

// Must match the inline snippet in the homelab's widget-injection.lua.
const EARLY_PATCH =
  "(function(){var w=window,p=HTMLCanvasElement.prototype,g=p.getContext;if(w.__fhGl)return;w.__fhGl=1;p.getContext=function(t,a){if(/webgl/.test(t))a=Object.assign({},a,{preserveDrawingBuffer:true});return g.call(this,t,a)}})();";
const page = `<!doctype html><html><head><meta name="viewport" content="width=device-width">
<script>${EARLY_PATCH}</script>
<title>Demo app</title><meta name="app-version" content="1.2.3">
<script async src="${hub}/widget.js" data-app="demo"></script>
<style>body{font-family:sans-serif;margin:0;background:#f0fdf4} .scroller{height:300px;overflow:auto;border:1px solid #ccc;margin:20px}
.row{padding:12px;border-bottom:1px solid #ddd} button.broken{margin:20px;padding:12px 20px;background:#16a34a;color:#fff;border:0;border-radius:8px}</style></head>
<body><h1 style="margin:20px">Demo app</h1><button class="broken" onclick="fetch('/missing');console.error('Save failed', new Error('boom'))">Save</button>
<canvas id="gl" width="200" height="100" style="position:absolute;right:40px;top:40px"></canvas>
<script>const gl=document.getElementById('gl').getContext('webgl');gl.clearColor(1,0,0,1);gl.clear(gl.COLOR_BUFFER_BIT);</script>
<div class="scroller">${Array.from({ length: 40 }, (_, i) => `<div class="row">Row ${i + 1}</div>`).join('')}</div></body></html>`;
const appServer = createServer((req, res) => {
  if (req.url === '/') {
    res.writeHead(200, { 'content-type': 'text/html' }).end(page);
  } else {
    res.writeHead(404).end('nope');
  }
}).listen(appPort);

const fail = (message) => {
  throw new Error(message);
};

let browser;
try {
  for (let i = 0; ; i++) {
    try {
      if ((await fetch(`${hub}/healthz`)).ok) break;
    } catch {}
    if (i > 50) fail('server did not start');
    await new Promise((r) => setTimeout(r, 100));
  }
  browser = await chromium.launch({ executablePath: process.env.PLAYWRIGHT_CHROMIUM_EXECUTABLE || undefined });

  for (const [name, viewport] of [['desktop', { width: 1280, height: 800 }], ['mobile', { width: 390, height: 844 }]]) {
    const context = await browser.newContext({ viewport, deviceScaleFactor: name === 'mobile' ? 3 : 1, hasTouch: name === 'mobile' });
    const tab = await context.newPage();
    const errors = [];
    tab.on('pageerror', (e) => errors.push(String(e)));
    await tab.goto(`http://localhost:${appPort}/`);
    await tab.locator('.scroller').evaluate((el) => (el.scrollTop = 400));
    await tab.click('button.broken');
    const widgetTab = tab.locator('feedback-hub-widget .fab');
    await widgetTab.waitFor();
    await tab.screenshot({ path: `${out}/${name}-1-tab.png` });
    await widgetTab.click();
    const panel = tab.locator('feedback-hub-widget .panel');
    await panel.waitFor();
    await tab.locator('feedback-hub-widget .thumb img').waitFor();
    await tab.screenshot({ path: `${out}/${name}-2-panel.png` });

    // "Page snapshot" hides the form; the next button press captures.
    await tab.locator('feedback-hub-widget [data-add=page]').click();
    await panel.waitFor({ state: 'detached' });
    await tab.locator('feedback-hub-widget .fab.armed').waitFor();
    await tab.locator('.scroller').evaluate((el) => (el.scrollTop = 900));
    await tab.screenshot({ path: `${out}/${name}-2b-armed.png` });
    await tab.locator('feedback-hub-widget .fab').click();
    await panel.waitFor();
    if ((await tab.locator('feedback-hub-widget .shot').count()) !== 2) fail('second snapshot missing');
    await tab.locator('feedback-hub-widget .remove').nth(1).click();

    // Circle something on the screenshot.
    await tab.locator('feedback-hub-widget .thumb').click();
    const canvas = tab.locator('feedback-hub-widget .annotator canvas');
    await canvas.waitFor();
    await tab.locator('feedback-hub-widget [data-tool=ellipse]').click();
    const box = await canvas.boundingBox();
    await tab.mouse.move(box.x + box.width * 0.05, box.y + box.height * 0.1);
    await tab.mouse.down();
    await tab.mouse.move(box.x + box.width * 0.3, box.y + box.height * 0.25, { steps: 5 });
    await tab.mouse.up();
    await tab.locator('feedback-hub-widget [data-tool=text]').click();
    await tab.mouse.click(box.x + box.width * 0.5, box.y + box.height * 0.6);
    await tab.locator('feedback-hub-widget .text-entry').fill('Nothing happens here');
    await tab.keyboard.press('Enter');
    await tab.screenshot({ path: `${out}/${name}-3-annotate.png` });
    await tab.locator('feedback-hub-widget [data-action=done]').click();
    await tab.locator('feedback-hub-widget .badge', { hasText: 'marked' }).waitFor();

    await tab.locator('feedback-hub-widget .shot .note').first().fill('Save does nothing');
    await tab.screenshot({ path: `${out}/${name}-3b-notes.png` });
    await tab.locator('feedback-hub-widget textarea').fill(`Save button does nothing, see #1 (${name})`);
    await tab.locator('feedback-hub-widget button[type=submit]').click();
    await tab
      .locator('feedback-hub-widget .toast', { hasText: 'Thanks' })
      .waitFor({ timeout: 15000 })
      .catch(async (error) => {
        await tab.screenshot({ path: `${out}/${name}-failed.png` });
        fail(`submit failed: ${await tab.locator('feedback-hub-widget .status').textContent()} (${error.message})`);
      });
    await tab.screenshot({ path: `${out}/${name}-4-sent.png` });
    if (errors.length) fail(`page errors: ${errors.join('\n')}`);
    await context.close();
  }

  const list = await (await fetch(`${hub}/api/feedback`, { headers: { authorization: `Bearer ${token}` } })).json();
  if (list.length !== 2) fail(`expected 2 items, got ${list.length}`);
  const item = list[0];
  if (item.app !== 'demo' || item.screenshot_count !== 1) fail(`unexpected item ${JSON.stringify(item)}`);
  if (item.context.appVersion !== '1.2.3') fail('app version missing');
  const detail = await (await fetch(`${hub}/api/feedback/${item.id}`, { headers: { authorization: `Bearer ${token}` } })).json();
  if (detail.screenshots[0].note !== 'Save does nothing') fail(`screenshot note missing: ${JSON.stringify(detail.screenshots)}`);
  if (!item.page_url?.startsWith('http://localhost:')) fail(`page url missing origin: ${item.page_url}`);
  if (!item.context.logs.some((l) => l.message.includes('Save failed'))) fail('console error not captured');
  if (!item.context.network.some((n) => n.status === '404')) fail('failed request not captured');

  // WebGL content must survive the DOM snapshot (desktop: 1280px wide, 1x scale).
  const desktop = list.find((i) => i.message.includes('(desktop)'));
  const shot = await (
    await fetch(`${hub}/api/feedback/${desktop.id}/screenshots/0`, { headers: { authorization: `Bearer ${token}` } })
  ).arrayBuffer();
  const probe = await browser.newPage();
  const pixel = await probe.evaluate(async (b64) => {
    const img = new Image();
    img.src = `data:image/png;base64,${b64}`;
    await img.decode();
    const c = document.createElement('canvas');
    c.width = img.width;
    c.height = img.height;
    const ctx = c.getContext('2d');
    ctx.drawImage(img, 0, 0);
    return [...ctx.getImageData(1280 - 40 - 100, 40 + 50, 1, 1).data];
  }, Buffer.from(shot).toString('base64'));
  await probe.close();
  if (!(pixel[0] > 200 && pixel[1] < 60 && pixel[2] < 60)) fail(`WebGL canvas missing from snapshot: rgba(${pixel})`);

  // A worker announces what its CLIs offer; the pickers only show these.
  const auth = { authorization: `Bearer ${token}`, 'content-type': 'application/json' };
  const post = (path, body) => fetch(`${hub}${path}`, { method: 'POST', headers: auth, body: JSON.stringify(body) });
  const model = (id, name, efforts, default_effort = null) => ({ id, name, description: null, efforts, default_effort });
  await post('/api/worker/heartbeat', {
    name: 'e2e-box',
    agents: [
      { name: 'claude', live_steering: true, default_model: 'Opus 5.5', efforts: ['low', 'medium', 'high', 'xhigh', 'max'],
        models: [model('opus', 'Opus 5.5', ['low', 'medium', 'high', 'xhigh', 'max']), model('claude-haiku-4-5', 'Haiku 4.5', [])] },
      { name: 'codex', live_steering: false, default_model: 'GPT-6-Sol', efforts: ['low', 'medium', 'high'],
        models: [model('gpt-6-sol', 'GPT-6-Sol', ['low', 'medium', 'high', 'ultra'], 'medium')] },
    ],
    running: [],
    max_parallel: 2,
  });

  // Triage UI.
  const pick = async (page, scope, label, option) => {
    await page.click(`${scope} [data-select="${label}"]`);
    await page.getByRole('option', { name: option, exact: true }).click();
  };
  const context = await browser.newContext({ viewport: { width: 1280, height: 860 } });
  const triage = await context.newPage();
  const uiErrors = [];
  triage.on('pageerror', (e) => uiErrors.push(String(e)));
  await triage.goto(hub);
  await triage.fill('input[name=token]', token);
  await triage.click('button.primary');
  await triage.locator('.item').first().click();
  await triage.locator('.gallery img[src^="blob:"]').first().waitFor();

  // Edit the report.
  await triage.click('[data-edit]');
  await triage.fill('.dialog textarea[name=message]', 'Save button does nothing (edited)');
  await triage.click('.dialog .button.primary');
  await triage.locator('.message', { hasText: '(edited)' }).waitFor();

  // Queue it for Codex with a model and an effort only that model offers.
  await pick(triage, '.queue', 'Agent', 'codex');
  await pick(triage, '.queue', 'Model', 'GPT-6-Sol');
  await triage.click('.queue [data-select="Reasoning effort"]');
  await triage.screenshot({ path: `${out}/triage-picker.png` });
  await triage.getByRole('option', { name: 'ultra' }).click();
  await triage.click('[data-queue]');
  await triage.locator('.agent-card.active').waitFor();
  await triage.screenshot({ path: `${out}/triage-desktop.png` });

  // Bulk: tick the other item and send it to Claude.
  await triage.locator('.entry:has(.pill.open) [data-check]').click();
  await triage.locator('.bulk').waitFor();
  await pick(triage, '.bulk', 'Agent', 'claude');
  await pick(triage, '.bulk', 'Model', 'Opus 5.5');
  await pick(triage, '.bulk', 'Reasoning effort', 'high');
  await triage.fill('.bulk textarea[name=note]', 'Keep the fix small');
  await triage.screenshot({ path: `${out}/triage-bulk.png` });
  await triage.click('[data-bulk-send]');
  await triage.locator('.bulk').waitFor({ state: 'detached' });

  // Play the worker: claim the bulk run and stream some activity.
  const run = await (await post('/api/worker/claim-run', { worker: 'e2e-box', agents: ['claude'], defaultAgent: 'claude' })).json();
  if (run.model !== 'opus' || run.effort !== 'high' || run.note !== 'Keep the fix small') fail(`unexpected run ${JSON.stringify(run)}`);
  await post(`/api/runs/${run.id}/events`, [
    { kind: 'prompt', body: 'You have been dispatched by feedback-hub…' },
    { kind: 'system', body: 'Model: claude-opus' },
    { kind: 'text', body: 'Looking at the save handler first.' },
    { kind: 'tool', body: 'Bash: rg -n "save" src' },
    { kind: 'tool_result', body: 'src/save.ts:12: export function save() {\nsrc/app.ts:40: save()' },
  ]);

  await triage.click('[data-view=agents]');
  await triage.locator(`.run-item[data-run="${run.id}"]`).click();
  await triage.locator('.ev.tool').waitFor();
  await triage.fill('.composer textarea', 'Also check the mobile layout');
  await triage.click('[data-send]');
  await triage.locator('.ev.user', { hasText: 'mobile layout' }).waitFor();
  const commands = await (await post(`/api/runs/${run.id}/commands/take`, {})).json();
  if (commands[0]?.body !== 'Also check the mobile layout') fail(`steering message not delivered: ${JSON.stringify(commands)}`);
  await post(`/api/runs/${run.id}/events`, [{ kind: 'text', body: 'Checked mobile too; fix pushed.' }]);
  await triage.locator('.ev.text', { hasText: 'fix pushed' }).waitFor({ timeout: 10000 });
  await triage.screenshot({ path: `${out}/agents-desktop.png` });
  await triage.setViewportSize({ width: 390, height: 844 });
  await triage.screenshot({ path: `${out}/agents-mobile.png` });
  await triage.click('[data-view=feedback]');
  await triage.click('.back');
  await triage.screenshot({ path: `${out}/triage-mobile.png`, fullPage: true });
  if (uiErrors.length) fail(`triage UI errors: ${uiErrors.join('\n')}`);

  // Signed in to the hub, the owner gets "Send & fix" in the widget of an app on
  // the same site, using the last agent choice (the bulk send above).
  const appTab = await context.newPage();
  await appTab.setViewportSize({ width: 1280, height: 800 });
  await appTab.goto(`http://127.0.0.1:${appPort}/`);
  await appTab.locator('feedback-hub-widget .fab').click();
  const dispatch = appTab.locator('feedback-hub-widget .dispatch');
  await dispatch.waitFor({ state: 'visible' });
  const details = await appTab.locator('feedback-hub-widget .dispatch-details').textContent();
  if (details !== 'claude · Opus 5.5 · high effort') fail(`unexpected dispatch details: ${details}`);
  await appTab.locator('feedback-hub-widget textarea').fill('Header overlaps the menu');
  await appTab.screenshot({ path: `${out}/widget-owner.png` });
  await dispatch.click();
  await appTab.locator('feedback-hub-widget .toast', { hasText: 'sent to claude' }).waitFor({ timeout: 15000 });
  const owned = await (await fetch(`${hub}/api/runs?active=true`, { headers: auth })).json();
  if (!owned.some((r) => r.requested_by === 'widget' && r.model === 'opus' && r.title === 'Header overlaps the menu')) {
    fail(`widget dispatch did not create a run: ${JSON.stringify(owned)}`);
  }
  await context.close();

  // Anyone else sees only "Send".
  const stranger = await browser.newPage();
  await stranger.goto(`http://127.0.0.1:${appPort}/`);
  await stranger.locator('feedback-hub-widget .fab').click();
  await stranger.locator('feedback-hub-widget textarea').waitFor();
  await stranger.waitForTimeout(500);
  if (await stranger.locator('feedback-hub-widget .dispatch').isVisible()) fail('stranger was offered Send & fix');
  await stranger.screenshot({ path: `${out}/widget-stranger.png` });

  // A long-lived page (installed PWA) offers it once the owner signs in to the hub,
  // on the next open and without a reload.
  const session = await fetch(`${hub}/api/session`, { method: 'POST', headers: auth, body: '{}' });
  const [name, value] = session.headers.get('set-cookie').split(';')[0].split('=');
  await stranger.context().addCookies([{ name, value, url: hub }]);
  await stranger.locator('feedback-hub-widget .close').click();
  await stranger.locator('feedback-hub-widget .fab').click();
  await stranger.locator('feedback-hub-widget .dispatch').waitFor({ state: 'visible' });
  await stranger.close();
  console.log('e2e ok');
} finally {
  await browser?.close();
  appServer.close();
  server.kill();
  rmSync(dataDir, { recursive: true, force: true });
}
