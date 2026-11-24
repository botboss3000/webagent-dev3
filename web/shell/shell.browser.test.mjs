// Browser fixture for the web/ shell: sidebar, phone drawer, chat panel and grip.
// Runs the real files from disk through a tiny static server so the module graph,
// the partial fetch and the CSS all exercise the shipped assets.

import test from 'node:test';
import assert from 'node:assert/strict';
import { createServer } from 'node:http';
import { readFile } from 'node:fs/promises';
import { fileURLToPath } from 'node:url';
import path from 'node:path';

const HERE = path.dirname(fileURLToPath(import.meta.url));
const WEB = path.resolve(HERE, '..');

const TYPES = {
  '.html': 'text/html; charset=utf-8',
  '.css': 'text/css; charset=utf-8',
  '.js': 'text/javascript; charset=utf-8',
  '.mjs': 'text/javascript; charset=utf-8',
  '.json': 'application/json; charset=utf-8',
  '.svg': 'image/svg+xml',
};

// Serves web/ plus the two API answers the shell asks for. `admin` decides what
// /api/v3/auth/me reports, which is what gates the Administrator group.
function startServer(admin) {
  const server = createServer(async (request, response) => {
    const url = new URL(request.url, 'http://127.0.0.1');
    const send = (status, body, type = 'application/json; charset=utf-8') => {
      response.writeHead(status, { 'content-type': type, 'cache-control': 'no-store' });
      response.end(body);
    };

    if (url.pathname === '/api/v3/auth/me') {
      return send(200, JSON.stringify({ is_admin: admin, user_id: 'fixture', csrf_token: 'fixture' }));
    }
    if (url.pathname === '/api/v3/agents') {
      return send(200, JSON.stringify({ agents: [{ id: 'fixture-agent', name: 'Fixture Agent', owned: true }] }));
    }
    if (!url.pathname.startsWith('/web/')) return send(404, '{"error":"not found"}');

    const relative = url.pathname.slice('/web/'.length) || 'index.html';
    const file = path.join(WEB, relative);
    if (!file.startsWith(WEB)) return send(403, '{"error":"forbidden"}');
    try {
      const body = await readFile(file);
      return send(200, body, TYPES[path.extname(file)] || 'application/octet-stream');
    } catch {
      return send(404, '{"error":"not found"}');
    }
  });

  return new Promise((resolve) => {
    server.listen(0, '127.0.0.1', () => resolve({ server, origin: `http://127.0.0.1:${server.address().port}` }));
  });
}

async function withPage(admin, run) {
  const { chromium } = await import(process.env.PLAYWRIGHT_MODULE || 'playwright');
  const { server, origin } = await startServer(admin);
  const browser = await chromium.launch({
    headless: true,
    executablePath: process.env.CHROMIUM_PATH,
    args: ['--no-sandbox'],
  });
  try {
    await run({ browser, origin });
  } finally {
    await browser.close();
    await new Promise((resolve) => server.close(resolve));
  }
}

const VIEWPORTS = [
  { name: 'desktop', width: 1280, height: 900, touch: false },
  { name: 'phone', width: 390, height: 844, touch: true },
];

for (const viewport of VIEWPORTS) {
  for (const light of [false, true]) {
    const theme = light ? 'light' : 'dark';

    test(`shell renders at ${viewport.name} in ${theme}`, async () => {
      await withPage(true, async ({ browser, origin }) => {
        const context = await browser.newContext({
          viewport: { width: viewport.width, height: viewport.height },
          hasTouch: viewport.touch,
          isMobile: viewport.touch,
        });
        const page = await context.newPage();
        const errors = [];
        page.on('pageerror', (error) => errors.push(String(error)));
        page.setDefaultTimeout(8000);

        await page.goto(`${origin}/web/index.html`, { waitUntil: 'load' });
        if (light) {
          await page.evaluate(() => document.body.classList.add('light-mode'));
        }

        // Sidebar renders the signed-in groups and the agent list from the API.
        await page.waitForSelector('#sidebar-nav .nav-item');
        const groups = await page.$$eval('.nav-group-title', (nodes) => nodes.map((node) => node.textContent));
        assert.deepEqual(groups, ['Administrator', 'Session Management', 'Agent Management', 'Wiki']);
        const agentLabels = await page.$$eval('.nav-item .nav-label', (nodes) => nodes.map((node) => node.textContent));
        assert.ok(agentLabels.includes('Fixture Agent'), 'agent from /api/v3/agents is listed');
        assert.ok(agentLabels.includes('New agent'), 'New agent entry is listed');

        // The ported chat panel mounts from its partial, with the sample transcript.
        await page.waitForSelector('#chat-panel .msg', { state: 'attached' });
        const messages = await page.$$eval('#chat-panel .msg', (nodes) => nodes.length);
        assert.ok(messages >= 3, `expected the sample transcript, saw ${messages} messages`);
        assert.equal(await page.locator('#chat-panel .composer-send').isDisabled(), true, 'send starts disabled');
        const activityOpen = await page.locator('.activity').first().getAttribute('class');
        assert.match(activityOpen, /activity-open/, 'activity group starts open');

        const chatWidth = () => page.$eval('#chat-column', (node) => node.getBoundingClientRect().width);
        // A phone opens on the panel view, so the chat column is hidden there until
        // the Panel/Chat switch is used; only the desktop measurement is meaningful.
        const before = viewport.name === 'desktop' ? await chatWidth() : 0;
        if (viewport.name === 'desktop') {
          assert.ok(before > 200, `chat column has width on desktop (${before})`);
        }

        if (viewport.name === 'desktop') {
          // The grip resizes the chat column by dragging the divider.
          const handle = await page.locator('#chat-resize-handle').boundingBox();
          assert.ok(handle && handle.width > 0, 'grip is laid out on desktop');
          await page.mouse.move(handle.x + handle.width / 2, handle.y + handle.height / 2);
          await page.mouse.down();
          await page.mouse.move(handle.x + handle.width / 2 - 90, handle.y + handle.height / 2, { steps: 8 });
          await page.mouse.up();
          const after = await chatWidth();
          assert.ok(after > before + 40, `dragging left widened the chat column (${before} -> ${after})`);
          assert.equal(await page.evaluate(() => document.body.classList.contains('chat-resizing')), false);

          // Collapsing the sidebar leaves an icon rail (the width animates).
          await page.click('#sidebar-toggle');
          assert.equal(await page.evaluate(() => document.body.classList.contains('sidebar-collapsed')), true);
          await page.waitForFunction(() => document.querySelector('#sidebar').getBoundingClientRect().width <= 80);
          const rail = await page.$eval('#sidebar', (node) => node.getBoundingClientRect().width);
          assert.ok(rail <= 80, `collapsed sidebar is a rail (${rail}px)`);
          assert.equal(await page.locator('#sidebar .brand-name').isVisible(), false, 'labels hide in the rail');
        } else {
          // Phone: the sidebar is a drawer and the grip gives way to the Panel/Chat switch.
          assert.equal(await page.locator('#chat-resize-handle').isVisible(), false, 'grip hidden at phone width');
          await page.click('#menu-open');
          assert.equal(await page.evaluate(() => document.body.classList.contains('menu-open')), true);
          // The drawer slides in, so wait for the transform to settle before measuring.
          await page.waitForFunction(() => document.querySelector('#sidebar').getBoundingClientRect().x >= 0);
          const drawer = await page.$eval('#sidebar', (node) => node.getBoundingClientRect().x);
          assert.ok(drawer >= 0, `drawer slides into view (x=${drawer})`);
          // Click the backdrop to the right of the open drawer, not through the sidebar.
          await page.click('#sidebar-backdrop', { position: { x: viewport.width - 20, y: 300 } });
          assert.equal(await page.evaluate(() => document.body.classList.contains('menu-open')), false);

          await page.click('.phone-switch button[data-view="chat"]');
          assert.equal(await page.evaluate(() => document.body.classList.contains('view-chat')), true);
          assert.equal(await page.locator('#chat-column').isVisible(), true, 'chat is reachable on a phone');
        }

        assert.deepEqual(errors, [], `no page errors: ${errors.join(' | ')}`);
        await context.close();
      });
    });
  }
}

test('signed-out visitors get no Administrator group and no admin page', async () => {
  await withPage(false, async ({ browser, origin }) => {
    const page = await browser.newPage();
    const errors = [];
    page.on('pageerror', (error) => errors.push(String(error)));
    page.setDefaultTimeout(8000);
    await page.goto(`${origin}/web/index.html#/admin/dashboard`, { waitUntil: 'load' });
    await page.waitForSelector('#sidebar-nav .nav-item');

    const groups = await page.$$eval('.nav-group-title', (nodes) => nodes.map((node) => node.textContent));
    assert.equal(groups.includes('Administrator'), false, 'Administrator group is hidden when not an admin');

    await page.waitForFunction(() => document.getElementById('page-body')?.textContent?.trim().length > 0);
    const body = await page.$eval('#page-body', (node) => node.textContent);
    assert.match(body, /Administrator only/, 'the admin route refuses without the flag');
    assert.deepEqual(errors, []);
    await page.close();
  });
});
