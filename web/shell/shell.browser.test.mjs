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
    // The shell reads its geometry from here; the fixture serves it so the measured
    // values come from config, not from CSS fallbacks.
    if (url.pathname === '/config/web/chat/chat-shell.json') {
      return send(
        200,
        JSON.stringify({
          version: 1,
          common: {
            surface: { background: 'transparent', border: 'none', radius: '0', shadow: 'none' },
            column: { default_width_px: 380, min_width_px: 250, flex_basis: '45%', draggable: true },
            divider: { hit_width_px: 14, grip_width_px: 2, grip_height_px: 36, grip_height_active_px: 56 },
            breakpoints: { mobile_px: 800, column_clamp_px: 514 },
          },
          desktop: { chat_is: 'column', narrow_follows_mobile: false },
          mobile: { chat_is: 'page', narrow_follows_mobile: true },
          widget: { chat_is: 'embed', narrow_follows_mobile: false },
        }),
      );
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
    const panel = document.querySelector('.chat-surface');
    if (!panel) return null;
    const header = panel.querySelector('.chat-panel-head');
    const footer = panel.querySelector('#composer');
    const inner = panel.querySelector('#chat-messages-inner');
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
        assert.equal(await page.locator('.content-head').count(), 1, 'one content header');
        assert.equal(await page.locator('.main-foot').count(), 0, 'no main footer');
        assert.equal(await page.locator('.content-head').isVisible(), true, 'header is visible');
        assert.equal(await page.locator('.main-foot').count(), 0, 'the shell has no footer strip');
        assert.equal(await page.locator('#page-title').textContent(), 'All sessions', 'header names the route');
        assert.equal(await page.locator('.main-foot').count(), 0, 'no footer strip on the content region');
        assert.equal(await page.locator('#refresh-button').isVisible(), true, 'the refresh action shows for an administrator');

        // The body holds only the placeholder page: title plus one line.
        const bodyLines = await page.$$eval('#page-body > *', (nodes) => nodes.length);
        assert.ok(bodyLines >= 1, 'the placeholder page rendered into the body');

        // The card is the raised surface: no border, elevation instead.
        const cardStyle = await page.$eval('#page-body .card', (node) => {
          const styles = getComputedStyle(node);
          return { border: styles.borderTopWidth, shadow: styles.boxShadow };
        });
        assert.equal(cardStyle.border, '0px', 'the card has no border');
        assert.notEqual(cardStyle.shadow, 'none', 'the card carries elevation');

        // The old chat chrome is gone: no divider grip and no message bubbles.
        for (const selector of ['#chat-resize-handle', '.msg']) {
          assert.equal(await page.locator(selector).count(), 0, `${selector} must not exist`);
        }
        const chatVar = await page.evaluate(() =>
          getComputedStyle(document.documentElement).getPropertyValue('--chat-panel-width').trim(),
        );
        assert.equal(chatVar, '380px', 'the column width comes from the shell config');

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

          // The stage splits between the main panel, the divider and the chat column.
          const shared = await page.evaluate(() => {
            const width = (id) => Math.round(document.getElementById(id).getBoundingClientRect().width);
            const stage = document.querySelector('.shell-body');
            const styles = getComputedStyle(stage);
            // The stage carries the shell gap, so compare against its content width.
            return {
              stage: Math.round(
                stage.getBoundingClientRect().width -
                  parseFloat(styles.paddingLeft) -
                  parseFloat(styles.paddingRight),
              ),
              main: width('content'),
              column: width('chat-panel'),
              divider: width('chat-divider'),
            };
          });
          assert.ok(shared.main > 500, `main panel keeps the larger share (${shared.main}px)`);
          assert.ok(
            Math.abs(shared.main + shared.divider + shared.column - shared.stage) <= 2,
            `main, divider and column fill the stage (${JSON.stringify(shared)})`,
          );

          // The chat switch is a phone affordance only.
          assert.equal(await page.locator('#chat-switch').isVisible(), false, 'no chat switch on desktop');

          // The chat is a column beside the main panel, always visible and edgeless.
          await page.waitForSelector('#chat-panel .chat-surface');
          const geometry = await page.evaluate(() => {
            const col = document.getElementById('chat-panel');
            const divider = document.getElementById('chat-divider');
            const panel = document.getElementById('chat-panel');
            const styles = getComputedStyle(panel);
            return {
              columnWidth: Math.round(col.getBoundingClientRect().width),
              dividerWidth: Math.round(divider.getBoundingClientRect().width),
              border: styles.borderTopWidth,
              radius: styles.borderTopLeftRadius,
              shadow: styles.boxShadow,
            };
          });
          assert.ok(geometry.columnWidth > 200, `chat column is visible (${geometry.columnWidth}px)`);
          assert.equal(geometry.dividerWidth, 14, 'the divider is the configured 14px hit area');
          assert.equal(geometry.border, '0px', 'the panel has no border');
          assert.equal(geometry.radius, '0px', 'the panel has no radius');
          assert.equal(geometry.shadow, 'none', 'the panel has no shadow');

          const shell = await probeShell(page);
          assert.ok(shell, 'chat panel is mounted in the column');
          assert.equal(shell.headerChildren, 0, 'header region is empty');
          assert.equal(shell.footerChildren, 0, 'footer region is empty');
          assert.equal(shell.innerChildren, 0, 'transcript region is empty');
          assert.ok(shell.height > 300, `chat panel fills the height (${shell.height}px)`);

          // Dragging the divider left widens the column and clamps at the config floor.
          const before = geometry.columnWidth;
          const handle = await page.locator('#chat-divider').boundingBox();
          await page.mouse.move(handle.x + handle.width / 2, handle.y + handle.height / 2);
          await page.mouse.down();
          await page.mouse.move(handle.x + handle.width / 2 - 80, handle.y + handle.height / 2, { steps: 8 });
          await page.mouse.up();
          const after = await page.$eval('#chat-panel', (node) => Math.round(node.getBoundingClientRect().width));
          assert.ok(after > before + 40, `dragging left widened the chat column (${before} -> ${after})`);
          assert.equal(await page.evaluate(() => document.body.classList.contains('chat-resizing')), false);
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
          await page.waitForSelector('#page-body .chat-surface');
          await page.waitForFunction(() => document.getElementById('page-title').textContent === 'Chat');
          assert.equal(await page.locator('#page-title').textContent(), 'Chat', 'header switches to the chat');
          assert.equal(await page.locator('#chat-switch').getAttribute('aria-pressed'), 'true');
          assert.equal(await page.locator('#chat-switch').getAttribute('aria-label'), 'Back to the panel');

          // The column and divider stand down on a phone; the chat is its own page.
          assert.equal(await page.locator('#chat-panel').isVisible(), false, 'no chat panel on a phone');
          assert.equal(await page.locator('#chat-divider').isVisible(), false, 'no divider on a phone');

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
    assert.equal(await page.locator('.main-foot').count(), 0, 'no footer strip signed out either');
    assert.equal(await page.locator('#refresh-button').isVisible(), false, 'the refresh action stays hidden when signed out');
    assert.deepEqual(errors, []);
    await page.close();
  });
});

test('a pull refreshes, and a longer pull runs the hard refresh', async () => {
  await withPage(true, async ({ browser, origin }) => {
    const context = await browser.newContext({
      viewport: { width: 390, height: 844 },
      hasTouch: true,
      isMobile: true,
    });
    const page = await context.newPage();
    const errors = [];
    page.on('pageerror', (error) => errors.push(String(error)));
    page.setDefaultTimeout(8000);

    // Pulls from the top band by a share of the viewport height and reports the hint.
    const pull = (share) =>
      page.evaluate((fraction) => {
        const start = new Touch({ identifier: 1, target: document.body, clientX: 180, clientY: 10 });
        document.dispatchEvent(new TouchEvent('touchstart', { touches: [start], bubbles: true }));
        const moved = new Touch({
          identifier: 1,
          target: document.body,
          clientX: 180,
          clientY: 10 + window.innerHeight * fraction,
        });
        document.dispatchEvent(new TouchEvent('touchmove', { touches: [moved], cancelable: true, bubbles: true }));
        return {
          hidden: document.getElementById('ptr-indicator').hidden,
          hint: document.getElementById('ptr-hint').textContent,
        };
      }, share);

    const release = () =>
      page.evaluate(() => {
        document.dispatchEvent(new TouchEvent('touchend', { touches: [], bubbles: true }));
      });

    await page.goto(`${origin}/web/index.html`, { waitUntil: 'load' });
    await page.waitForSelector('#sidebar-nav .nav-item');

    // A short pull shows the indicator, half a screen arms the refresh, most of one the hard refresh.
    assert.deepEqual(await pull(0.3), { hidden: false, hint: 'PULL TO REFRESH' });
    assert.deepEqual(await pull(0.6), { hidden: false, hint: 'RELEASE TO REFRESH' });
    assert.deepEqual(await pull(0.9), { hidden: false, hint: 'HARD REFRESH' });

    // Releasing at the hard stage resets the origin's browser data and reloads past the cache.
    await page.evaluate(() => localStorage.setItem('webagent.web.theme', 'light'));
    await page.evaluate(() => localStorage.setItem('probe-key', 'should-not-survive'));
    await page.evaluate(
      () =>
        new Promise((resolve, reject) => {
          const request = indexedDB.open('webagent-probe', 1);
          request.onupgradeneeded = () => request.result.createObjectStore('rows');
          request.onsuccess = () => {
            request.result.close();
            resolve(true);
          };
          request.onerror = () => reject(request.error);
        }),
    );
    const hardNavigation = page.waitForNavigation({ waitUntil: 'load' });
    await release();
    await hardNavigation;
    assert.match(page.url(), /[?&]_=\d+/, 'the hard refresh adds a cache-buster');
    assert.equal(
      await page.evaluate(() => localStorage.getItem('webagent.web.theme')),
      null,
      'web storage was cleared',
    );
    assert.equal(
      await page.evaluate(() => localStorage.getItem('probe-key')),
      null,
      'a key outside the shell was cleared too, so the reset is not a key allowlist',
    );
    const databases = await page.evaluate(async () => {
      if (typeof indexedDB.databases !== 'function') return [];
      return (await indexedDB.databases()).map((row) => row?.name);
    });
    assert.equal(databases.includes('webagent-probe'), false, 'the IndexedDB database was deleted');
    // The HttpOnly session cookie is out of page script's reach, so the user stays signed in.
    await page.waitForFunction(
      () => document.getElementById('account-name')?.textContent === 'Administrator',
    );

    // A plain refresh keeps browser data and does not add the cache-buster.
    await page.goto(`${origin}/web/index.html`, { waitUntil: 'load' });
    await page.waitForSelector('#sidebar-nav .nav-item');
    await page.evaluate(() => localStorage.setItem('webagent.web.theme', 'light'));
    assert.equal((await pull(0.6)).hint, 'RELEASE TO REFRESH');
    const softNavigation = page.waitForNavigation({ waitUntil: 'load' });
    await release();
    await softNavigation;
    assert.doesNotMatch(page.url(), /[?&]_=\d+/, 'a plain refresh has no cache-buster');
    assert.equal(
      await page.evaluate(() => localStorage.getItem('webagent.web.theme')),
      'light',
      'a plain refresh keeps browser data',
    );

    assert.deepEqual(errors, [], `no page errors: ${errors.join(' | ')}`);
    await context.close();
  });
});
