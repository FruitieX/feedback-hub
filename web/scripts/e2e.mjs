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
    FEEDBACK_HUB_ALLOWED_ORIGINS: 'http://localhost:*',
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
    await tab.screenshot({ path: `${out}/${name}-3-annotate.png` });
    await tab.locator('feedback-hub-widget [data-action=done]').click();
    await tab.locator('feedback-hub-widget .badge', { hasText: 'marked' }).waitFor();

    await tab.locator('feedback-hub-widget textarea').fill(`Save button does nothing (${name})`);
    await tab.locator('feedback-hub-widget input[name=reporter]').fill('Tester');
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
  if (item.app !== 'demo' || item.screenshot_count !== 1 || item.reporter !== 'Tester') fail(`unexpected item ${JSON.stringify(item)}`);
  if (item.context.appVersion !== '1.2.3') fail('app version missing');
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

  // Triage UI.
  const context = await browser.newContext({ viewport: { width: 1280, height: 860 } });
  const triage = await context.newPage();
  await triage.goto(hub);
  await triage.fill('input[name=token]', token);
  await triage.click('button.primary');
  await triage.locator('.item').first().click();
  await triage.locator('.gallery img[src^="blob:"]').first().waitFor();
  await triage.selectOption('.queue select', 'codex');
  await triage.click('.queue button');
  await triage.locator('.detail .pill.queued').waitFor();
  await triage.screenshot({ path: `${out}/triage-desktop.png` });
  await triage.setViewportSize({ width: 390, height: 844 });
  await triage.screenshot({ path: `${out}/triage-mobile.png`, fullPage: true });
  await context.close();
  console.log('e2e ok');
} finally {
  await browser?.close();
  appServer.close();
  server.kill();
  rmSync(dataDir, { recursive: true, force: true });
}
