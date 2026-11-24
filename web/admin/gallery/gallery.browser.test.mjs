// Browser fixture for the component gallery: it must list every component folder
// on disk, with a live specimen for each, in both themes.

import test from 'node:test';
import assert from 'node:assert/strict';
import { createServer } from 'node:http';
import { readFile, readdir } from 'node:fs/promises';
import { fileURLToPath } from 'node:url';
import path from 'node:path';

const HERE = path.dirname(fileURLToPath(import.meta.url));
const WEB = path.resolve(HERE, '../..');
const COMPONENTS = path.join(WEB, 'components');

const TYPES = {
  '.html': 'text/html; charset=utf-8',
  '.css': 'text/css; charset=utf-8',
  '.js': 'text/javascript; charset=utf-8',
};

function startServer() {
  const server = createServer(async (request, response) => {
    const url = new URL(request.url, 'http://127.0.0.1');
    const send = (status, body, type = 'application/json; charset=utf-8') => {
      response.writeHead(status, { 'content-type': type, 'cache-control': 'no-store' });
      response.end(body);
    };
    if (url.pathname === '/api/v3/auth/me') {
      return send(200, JSON.stringify({ is_admin: true, user_id: 'fixture', csrf_token: 'fixture' }));
    }
    if (!url.pathname.startsWith('/web/')) return send(404, '{"error":"not found"}');
    const file = path.join(WEB, url.pathname.slice('/web/'.length));
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

test('the gallery lists exactly the component folders on disk', async () => {
  const source = await readFile(path.join(COMPONENTS, 'components.js'), 'utf8');
  const manifest = [...source.matchAll(/^\s{4}id: '([a-z-]+)',$/gm)].map((match) => match[1]);
  const folders = (await readdir(COMPONENTS, { withFileTypes: true }))
    .filter((entry) => entry.isDirectory())
    .map((entry) => entry.name)
    .sort();

  assert.ok(manifest.length >= 10, `manifest lists components (${manifest.length})`);
  assert.deepEqual([...manifest].sort(), folders, 'every component folder is in the manifest and nothing extra');

  const { chromium } = await import(process.env.PLAYWRIGHT_MODULE || 'playwright');
  const { server, origin } = await startServer();
  const browser = await chromium.launch({
    headless: true,
    executablePath: process.env.CHROMIUM_PATH,
    args: ['--no-sandbox'],
  });
  try {
    for (const [width, light] of [[1280, false], [1280, true], [390, false], [390, true]]) {
      const context = await browser.newContext({
        viewport: { width, height: width === 390 ? 844 : 900 },
        hasTouch: width === 390,
        isMobile: width === 390,
      });
      const page = await context.newPage();
      const errors = [];
      page.on('pageerror', (error) => errors.push(String(error)));
      page.setDefaultTimeout(8000);

      await page.goto(`${origin}/web/admin/gallery/index.html`, { waitUntil: 'load' });
      if (light) await page.evaluate(() => document.body.classList.add('light-mode'));
      await page.waitForSelector('.gallery-card');

      const rendered = await page.$$eval('.gallery-card', (nodes) => nodes.map((node) => node.dataset.component));
      assert.deepEqual([...rendered].sort(), folders, `gallery rendered every component at ${width}px`);

      const count = await page.$eval('#gallery-count', (node) => node.textContent);
      assert.equal(Number(count), folders.length, 'the header count matches the inventory');

      const missing = await page.$$eval('.gallery-body', (nodes) =>
        nodes.filter((node) => /No specimen yet/.test(node.textContent)).length,
      );
      assert.equal(missing, 0, 'every component shows a specimen');

      const overflow = await page.evaluate(() => document.documentElement.scrollWidth - document.documentElement.clientWidth);
      assert.ok(overflow <= 1, `no horizontal overflow at ${width}px (${overflow})`);
      assert.deepEqual(errors, [], `no page errors: ${errors.join(' | ')}`);
      await context.close();
    }
  } finally {
    await browser.close();
    await new Promise((resolve) => server.close(resolve));
  }
});
