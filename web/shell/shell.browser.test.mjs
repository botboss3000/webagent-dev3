// Browser fixture for the web/ shell: sidebar, main header, empty body, footer,
// and the phone drawer. Runs the real files from disk through a tiny static
// server so the module graph and the CSS both exercise the shipped assets.
//
// The chat is stripped on purpose: this fixture also proves it is really gone.

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

// Reads the mounted chat shell: its three regions and its rendered size.
async function probeShell(page) {
  return page.evaluate(() => {
    const panel = document.getElementById('chat-panel');
    if (!panel) return null;
    const header = document.getElementById('chat-header');
    const footer = document.getElementById('chat-footer');
    const inner = document.getElementById('chat-messages-inner');
    const box = panel.getBoundingClientRect();
    return {
      headerChildren: header ? header.children.length : -1,
      footerChildren: footer ? footer.children.length : -1,
      innerChildren: inner ? inner.children.length : -1,
      height: Math.round(box.height),
      width: Math.round(box.width),
    };
  });
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

        // The shell is header, empty body and footer: three regions, nothing else.
        assert.equal(await page.locator('.main-head').count(), 1, 'one main header');
        assert.equal(await page.locator('.main-foot').count(), 1, 'one main footer');
        assert.equal(await page.locator('.main-head').isVisible(), true, 'header is visible');
        assert.equal(await page.locator('.main-foot').isVisible(), true, 'footer is visible');
        assert.equal(await page.locator('#page-title').textContent(), 'All sessions', 'header names the route');
        assert.equal(await page.locator('#footer-status').textContent(), 'Signed in as administrator');

        // The body holds only the placeholder page: title plus one line.
        const bodyLines = await page.$$eval('#page-body > *', (nodes) => nodes.length);
        assert.ok(bodyLines >= 1, 'the placeholder page rendered into the body');

        // The chat is stripped: no column, no grip, no panel, no transcript.
        for (const selector of ['#chat-column', '#chat-resize-handle', '#chat-panel', '.msg', '.composer']) {
          assert.equal(await page.locator(selector).count(), 0, `${selector} must not exist`);
        }
        const chatVar = await page.evaluate(() =>
          getComputedStyle(document.documentElement).getPropertyValue('--chat-panel-width').trim(),
        );
        assert.equal(chatVar, '', 'the chat width variable is gone');

        // The shell fills the viewport with no sideways overflow at either width.
        const overflow = await page.evaluate(
          () => document.documentElement.scrollWidth - document.documentElement.clientWidth,
        );
        assert.ok(overflow <= 1, `no horizontal overflow (${overflow})`);

        if (viewport.name === 'desktop') {
          // Collapsing the sidebar leaves an icon rail (the width animates).
          await page.click('#sidebar-toggle');
          assert.equal(await page.evaluate(() => document.body.classList.contains('sidebar-collapsed')), true);
          await page.waitForFunction(() => document.querySelector('#sidebar').getBoundingClientRect().width <= 80);
          const rail = await page.$eval('#sidebar', (node) => node.getBoundingClientRect().width);
          assert.ok(rail <= 80, `collapsed sidebar is a rail (${rail}px)`);
          assert.equal(await page.locator('#sidebar .brand-name').isVisible(), false, 'labels hide in the rail');

          // The main panel takes the whole stage once the rail is narrow.
          const main = await page.$eval('#main-panel', (node) => node.getBoundingClientRect().width);
          assert.ok(main > 1000, `main panel fills the width beside the rail (${main}px)`);

          // The chat switch is a phone affordance only.
          assert.equal(await page.locator('#chat-switch').isVisible(), false, 'no chat switch on desktop');

          // The chat route mounts the ported shell: empty header, transcript and footer.
          await page.evaluate(() => {
            window.location.hash = '#/chat';
          });
          await page.waitForFunction(() => document.getElementById('page-title').textContent === 'Chat');
          await page.waitForSelector('#page-body #chat-panel');
          const shell = await probeShell(page);
          assert.ok(shell, 'chat panel is mounted on the chat route');
          assert.equal(shell.headerChildren, 0, 'header region is empty');
          assert.equal(shell.footerChildren, 0, 'footer region is empty');
          assert.equal(shell.innerChildren, 0, 'transcript region is empty');
          assert.ok(shell.height > 300, `chat panel fills the height (${shell.height}px)`);
          assert.ok(shell.width > 400, `chat panel fills the width (${shell.width}px)`);
        } else {
          // Phone: the sidebar is a drawer over the stage.
          assert.equal(await page.locator('#menu-open').isVisible(), true, 'menu button shows at phone width');
          await page.click('#menu-open');
          assert.equal(await page.evaluate(() => document.body.classList.contains('menu-open')), true);
          // The drawer slides in, so wait for the transform to settle before measuring.
          await page.waitForFunction(() => document.querySelector('#sidebar').getBoundingClientRect().x >= 0);
          const drawer = await page.$eval('#sidebar', (node) => node.getBoundingClientRect().x);
          assert.ok(drawer >= 0, `drawer slides into view (x=${drawer})`);
          // Click the backdrop to the right of the open drawer, not through the sidebar.
          await page.click('#sidebar-backdrop', { position: { x: viewport.width - 20, y: 300 } });
          assert.equal(await page.evaluate(() => document.body.classList.contains('menu-open')), false);

          // The header's chat icon switches to the chat page and back to the panel.
          assert.equal(await page.locator('#chat-switch').isVisible(), true, 'chat switch shows on a phone');
          await page.click('#chat-switch');
          await page.waitForFunction(() => window.location.hash === '#/chat');
          await page.waitForSelector('#page-body #chat-panel');
          await page.waitForFunction(() => document.getElementById('page-title').textContent === 'Chat');
          assert.equal(await page.locator('#page-title').textContent(), 'Chat', 'header switches to the chat');
          assert.equal(await page.locator('#chat-switch').getAttribute('aria-pressed'), 'true');
          assert.equal(await page.locator('#chat-switch').getAttribute('aria-label'), 'Back to the panel');

          // The phone chat is the same empty shell, filling the body.
          const phoneShell = await probeShell(page);
          assert.ok(phoneShell, 'chat panel is mounted on a phone');
          assert.equal(phoneShell.headerChildren, 0, 'header region is empty');
          assert.equal(phoneShell.footerChildren, 0, 'footer region is empty');
          assert.equal(phoneShell.innerChildren, 0, 'transcript region is empty');
          assert.ok(phoneShell.height > 300, `chat panel fills the height (${phoneShell.height}px)`);

          await page.click('#chat-switch');
          await page.waitForFunction(() => window.location.hash === '#/sessions');
          // The route renders asynchronously, so wait for the header to catch up.
          await page.waitForFunction(() => document.getElementById('page-title').textContent === 'All sessions');
          assert.equal(await page.locator('#page-title').textContent(), 'All sessions', 'and back to the panel');
          assert.equal(await page.locator('#chat-switch').getAttribute('aria-pressed'), 'false');
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
    assert.equal(await page.locator('#footer-status').textContent(), 'Signed out');
    assert.deepEqual(errors, []);
    await page.close();
  });
});
