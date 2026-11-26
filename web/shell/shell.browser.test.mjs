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
    // Chat header placement: the sidebar control in the mobile block only.
    if (url.pathname === '/config/web/chat/chat-header.json') {
      return send(200, JSON.stringify({
        version: 1,
        common: {
          rows: [
            { left: [], center: [], right: [] },
            { left: [], center: [], right: [] },
            { left: [], center: [], right: [] },
          ],
        },
        desktop: {},
        mobile: {
          rows: [
            { left: ['sidebar'], center: [], right: [] },
            { left: [], center: [], right: [] },
            { left: [], center: [], right: [] },
          ],
        },
        widget: {},
      }));
    }
    // Composer placement: the four buttons in the third row, attach left, the rest right.
    if (url.pathname === '/config/web/chat/chat-footer.json') {
      return send(
        200,
        JSON.stringify({
          version: 2,
          common: {
            controls: { input: { label: 'Message', placeholder: null, rows: 1 } },
            commands: { fallback: 'message', entries: {} },
            active: {
              rows: {
                '1': { left: [], center: ['input'], right: [] },
                '2': { left: [], center: [], right: [] },
                '3': { left: ['attach'], center: [], right: ['mic', 'stop', 'send'] },
              },
            },
            idle: {
              rows: {
                '1': { left: [], center: [], right: [] },
                '2': { left: ['attach'], center: ['input'], right: ['mic', 'send'] },
                '3': { left: [], center: [], right: [] },
              },
            },
          },
          desktop: {},
          mobile: {},
          widget: {},
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
        assert.equal(await page.locator('.content-head-left .sidebar-button').count(), 1, 'the content header carries the sidebar button');
        assert.equal(await page.locator('.content-head-right .content-head-btn').count(), 1, 'the content header carries the chat button');

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

          // Both header controls must render at button size: a squeezed header once
          // collapsed these slots to zero width and the buttons became invisible.
          const headerProbe = await page.evaluate(() => {
            const read = (selector) => {
              const el = document.querySelector(selector);
              if (!el) return { selector, missing: true };
              const s = getComputedStyle(el);
              const r = el.getBoundingClientRect();
              const parent = el.parentElement;
              const ps = parent ? getComputedStyle(parent) : null;
              const pr = parent ? parent.getBoundingClientRect() : null;
              return {
                selector,
                display: s.display,
                width: s.width,
                minWidth: s.minWidth,
                height: s.height,
                box: [Math.round(r.width), Math.round(r.height)],
                parentDisplay: ps?.display,
                parentWidth: ps?.width,
                parentFlex: ps?.flex,
                parentBox: pr ? [Math.round(pr.width), Math.round(pr.height)] : null,
              };
            };
            return [read('.content-head-left'), read('.content-head-left .sidebar-button')];
          });
          for (const selector of ['.content-head-left .sidebar-button', '.content-head-right #chat-button']) {
            const box = await page.locator(selector).boundingBox();
            assert.ok(
              box && box.width >= 30 && box.height >= 30,
              `${selector} renders at button size (${JSON.stringify(box)}) probe ${JSON.stringify(headerProbe)}`,
            );
          }

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
          assert.ok(shell.headerChildren >= 1, 'the chat header grid is mounted');
          assert.ok(shell.footerChildren >= 1, 'the composer grid fills the footer region');
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

          // An untouched composer rests idle: one row, the field beside its buttons.
          const restState = await page.evaluate(() => ({
            state: document.querySelector('#composer')?.dataset.composerState,
            rows: document.querySelectorAll('#composer .composer-row').length,
            row: document
              .querySelector('#composer .composer-input')
              ?.closest('.composer-row')
              ?.getAttribute('data-composer-row'),
          }));
          assert.equal(restState.state, 'idle', 'the composer mounts idle');
          assert.equal(restState.rows, 1, 'idle shows one row');
          assert.equal(restState.row, '2', 'idle keeps the field in the controls row');

          // Focusing the field promotes it to active, and focus survives the rebuild.
          await page.click('#composer .composer-input');
          await page.waitForFunction(
            () => document.querySelector('#composer')?.dataset.composerState === 'active',
          );
          assert.equal(
            await page.evaluate(() => document.activeElement?.id),
            'chat-input',
            'focus survives the idle to active switch',
          );

          // The composer dock is a 3x3 grid: empty rows collapse, row 3 holds the buttons.
          const composer = await page.evaluate(() => {
            const grid = document.querySelector('#composer .composer-grid');
            const rows = grid ? [...grid.querySelectorAll('.composer-row')] : [];
            const last = rows[rows.length - 1];
            const labels = (cell) =>
              [...(cell?.querySelectorAll('.composer-btn') || [])].map((b) => b.getAttribute('aria-label'));
            return {
              hasGrid: Boolean(grid),
              rowCount: rows.length,
              left: labels(last?.querySelector('.composer-cell-left')),
              right: labels(last?.querySelector('.composer-cell-right')),
              stop: Boolean(document.querySelector('.composer-btn-stop')),
              send: Boolean(document.querySelector('.composer-btn-send')),
              mic: Boolean(document.querySelector('.composer-btn-mic svg rect')),
              square: Boolean(document.querySelector('.composer-btn-stop svg rect')),
              triangle: Boolean(document.querySelector('.composer-btn-send svg path')),
              field: Boolean(document.querySelector('#composer .composer-input')),
              fieldRow: document
                .querySelector('#composer .composer-input')
                ?.closest('.composer-row')
                ?.getAttribute('data-composer-row'),
              fieldCell: document
                .querySelector('#composer .composer-input')
                ?.parentElement?.getAttribute('data-composer-cell'),
              fieldValue: document.querySelector('#composer .composer-input')?.value ?? null,
              fieldClear: (() => {
                const input = document.querySelector('#composer .composer-input');
                const attach = document.querySelector('#composer .composer-btn-attach');
                if (!input || !attach) return false;
                const i = input.getBoundingClientRect();
                const a = attach.getBoundingClientRect();
                return i.bottom <= a.top + 1;
              })(),
            };
          });
          assert.equal(composer.hasGrid, true, 'the composer grid is mounted');
          assert.equal(composer.rowCount, 2, 'the active composer shows the field row and the buttons row');
          assert.equal(composer.field, true, 'the text field is mounted');
          assert.equal(composer.fieldRow, '1', 'the active composer puts the field in row 1');
          assert.equal(composer.fieldCell, 'center', 'the field fills the centre cell');
          assert.equal(composer.fieldValue, '', 'the field starts empty');
          assert.equal(
            composer.fieldClear,
            true,
            'the field sits clear of the buttons instead of overlapping them',
          );
          assert.deepEqual(composer.left, ['Attach files'], 'attach sits in the left cell');
          assert.deepEqual(
            composer.right,
            ['Dictate', 'Stop', 'Send'],
            'mic, stop and send sit in the right cell',
          );
          assert.equal(composer.stop, true, 'stop uses the stop variant');
          assert.equal(composer.send, true, 'send uses the send variant');
          assert.equal(composer.square, true, 'stop draws the square icon');
          assert.equal(composer.triangle, true, 'send draws the triangle icon');
          assert.equal(composer.mic, true, 'mic draws the microphone icon');

          // The bar itself: panel surface, as tall as its largest control, pill radius.
          const bar = await page.evaluate(() => {
            const el = document.querySelector('#composer .composer-surface');
            if (!el) return null;
            const styles = getComputedStyle(el);
            const heights = [...el.querySelectorAll('.composer-btn')].map(
              (b) => b.getBoundingClientRect().height,
            );
            return {
              height: el.getBoundingClientRect().height,
              tallest: Math.max(...heights, 0),
              radius: parseFloat(styles.borderTopLeftRadius),
              background: styles.backgroundColor,
              border: styles.borderTopWidth,
              padding: styles.paddingTop,
              grid: el.querySelector('.composer-grid')?.getBoundingClientRect().height ?? 0,
            };
          });
          assert.ok(bar, 'the composer bar is mounted');
          assert.ok(
            Math.abs(bar.height - bar.grid) <= 1,
            `the bar covers its whole grid (${bar.height} vs ${bar.grid})`,
          );
          assert.ok(
            bar.height + 1 >= bar.tallest,
            `the bar is never shorter than its largest control (${bar.height} vs ${bar.tallest})`,
          );
          assert.ok(
            Math.abs(bar.radius - bar.height / 2) <= 1,
            `the radius is half the bar height (${bar.radius} for ${bar.height})`,
          );
          assert.notEqual(bar.background, 'rgba(0, 0, 0, 0)', 'the bar carries the panel surface');
          assert.equal(bar.border, '0px', 'the bar has no border');
          assert.equal(bar.padding, '0px', 'the bar takes no padding unless the config sets it');

          // The chat header is its own region; desktop gets no mobile-only controls.
          const headControls = await page.evaluate(
            () => document.querySelectorAll('.chat-panel-head .sidebar-button').length,
          );
          assert.equal(headControls, 0, 'the desktop chat header carries no sidebar control');
        } else {
          // Phone: the sidebar is a drawer over the stage.
          assert.equal(await page.locator('#sidebar-button').isVisible(), true, 'sidebar button shows at phone width');
          await page.click('#sidebar-button');
          assert.equal(await page.evaluate(() => document.body.classList.contains('menu-open')), true);
          // The drawer slides in, so wait for the transform to settle before measuring.
          await page.waitForFunction(() => document.querySelector('#sidebar').getBoundingClientRect().x >= 0);
          const drawer = await page.$eval('#sidebar', (node) => node.getBoundingClientRect().x);
          assert.ok(drawer >= 0, `drawer slides into view (x=${drawer})`);
          // Click the backdrop to the right of the open drawer, not through the sidebar.
          await page.click('#sidebar-backdrop', { position: { x: viewport.width - 20, y: 300 } });
          assert.equal(await page.evaluate(() => document.body.classList.contains('menu-open')), false);

          // The header's chat icon switches to the chat page and back to the panel.
          assert.equal(await page.locator('#chat-button').isVisible(), true, 'chat button shows on a phone');
          await page.click('#chat-button');
          await page.waitForFunction(() => window.location.hash === '#/chat');
          await page.waitForSelector('#page-body .chat-surface');
          await page.waitForFunction(() => document.getElementById('page-title').textContent === 'Chat');
          assert.equal(await page.locator('#page-title').textContent(), 'Chat', 'header switches to the chat');
          assert.equal(await page.locator('#chat-button').getAttribute('aria-pressed'), 'true');
          assert.equal(await page.locator('#chat-button').getAttribute('aria-label'), 'Back to the panel');

          // The column and divider stand down on a phone; the chat is its own page.
          assert.equal(await page.locator('#chat-panel').isVisible(), false, 'no chat panel on a phone');
          assert.equal(await page.locator('#chat-divider').isVisible(), false, 'no divider on a phone');

          // The phone chat is the same empty shell, filling the body.
          const phoneShell = await probeShell(page);
          assert.ok(phoneShell, 'chat panel is mounted on a phone');
          assert.ok(phoneShell.headerChildren >= 1, 'the chat header grid is mounted on a phone');
          assert.ok(phoneShell.footerChildren >= 1, 'the composer grid fills the footer region on a phone');
          assert.equal(phoneShell.innerChildren, 0, 'transcript region is empty');
          assert.ok(phoneShell.height > 300, `chat panel fills the height (${phoneShell.height}px)`);

          // The mobile chat header carries the sidebar control from chat-header.json.
          // Scope to the page: the hidden desktop dock mounts its own copy of the shell.
          const phoneHeadControls = await page.evaluate(
            () => document.querySelectorAll('#page-body .chat-panel-head .sidebar-button').length,
          );
          assert.equal(phoneHeadControls, 1, 'the mobile chat header carries the sidebar control');

          // The content header is hidden on the chat route, so the way back is the
          // sidebar: open it from the chat header and pick the sessions entry.
          await page.click('#page-body .chat-panel-head .sidebar-button');
          await page.click('#sidebar-nav [data-href="#/sessions"]');
          await page.waitForFunction(() => window.location.hash === '#/sessions');
          // The route renders asynchronously, so wait for the header to catch up.
          await page.waitForFunction(() => document.getElementById('page-title').textContent === 'All sessions');
          assert.equal(await page.locator('#page-title').textContent(), 'All sessions', 'and back to the panel');
          assert.equal(await page.locator('#chat-button').getAttribute('aria-pressed'), 'false');
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
    assert.equal(await page.locator('.content-head-left .sidebar-button').count(), 1, 'the header still carries the sidebar button');
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

    // Loading the chat page first and pressing back opens the panel instead of leaving.
    await page.goto(`${origin}/web/index.html#/chat`, { waitUntil: 'load' });
    await page.waitForSelector('#page-body .chat-surface');
    await page.evaluate(() => history.back());
    await page.waitForFunction(() => window.location.hash === '#/sessions');
    assert.equal(
      await page.locator('#page-title').textContent(),
      'All sessions',
      'back on the chat page opens the panel',
    );

    assert.deepEqual(errors, [], `no page errors: ${errors.join(' | ')}`);
    await context.close();
  });
});
