// Live context-composition refresh: a save repaints only the chart, never the
// abilities page or the browser page. The real page module and request boundary run
// here; only the backend responses are fixtures.
import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs/promises';
import http from 'node:http';

const root = new URL('../../../../../', import.meta.url);

const html = `<!doctype html><html><body><div id="host"></div><script type="module" src="/fixture.js"></script></body></html>`;

const fixture = `
import {renderAbilities} from '/ui/main-panel/agents/pages/abilities/page.js';

const clone = value => structuredClone(value);
let revision = 1;
let compositionReads = 0;
const requests = [];
const abilityRows = () => [
  {id:'base',name:'Base',description:'Core tools and loaders',group:'core',group_name:'Core',
   visibility:'visible',readiness:'ready',installed:true,declared:true,configured:true,
   has_skill:true,skill_visibility:'visible',default:'on',default_mode:'visible',
   general_skills:[{skill_id:'core',title:'Skill',when_to_use:'',mode:'visible'}],
   tools:[{name:'shell',description:'Run commands',policy:'allow',mode:'visible',supported:true,
     default:'auto',has_tool_skill:false}]},
  {id:'web_access',name:'Web Access',description:'Weather and places',group:'web',group_name:'Web',
   visibility:'visible',readiness:'ready',installed:true,declared:true,configured:true,
   has_skill:true,skill_visibility:'visible',default:'on',default_mode:'visible',
   general_skills:[{skill_id:'core',title:'Skill',when_to_use:'',mode:'visible'}],
   tools:[{name:'get_weather',description:'Forecast',policy:'allow',mode:'visible',supported:true,
     default:'auto',has_tool_skill:false}]},
];
const data = () => ({
  authority:{can_manage:true,is_installation_admin:false},
  agent_id:'a',
  revision,
  permissions:{revision:1,ability_permissions:{},tool_permissions:{}},
  capability_levels:{revision:1,order:['operator'],levels:{operator:{name:'Operator'}}},
  abilities:abilityRows(),
});
const composition = tokens => ({
  tokens, estimated:true, revision:'stamp-' + revision,
  breakdown:{system_prompt:{tokens:400},skills:{tokens:300},tools:{tokens:tokens-700}},
  sources:{
    system_prompt:[{label:'Base instructions',tokens:400}],
    skills:[{label:'Visible ability guidance',tokens:300}],
    tools:[{label:'shell',tokens:tokens-700}],
  },
});
const request = async (url, method = 'GET', body) => {
  requests.push({url, method});
  // The effective read carries level_id as a query string.
  const path = url.split('?')[0];
  if (method === 'GET') {
    if (path.endsWith('/context-composition/revision')) return {revision: 'stamp-' + revision};
    if (path.endsWith('/context-composition')) {
      compositionReads += 1;
      return clone(composition(compositionReads === 1 ? 1000 : 1400));
    }
    if (path.endsWith('/abilities/effective')) return clone(data());
    if (path.endsWith('/abilities')) return clone(data());
    if (path.endsWith('/slots')) return {slots:[{slot_name:'system',content:'Configured prompt'}]};
    if (path.endsWith('/repository-connections')) return {load_failed:true};
    if (path.endsWith('/source-control/sites')) return {load_failed:true};
    if (path.endsWith('/ability-members')) return {members:[{user_id:'u1',username:'User One',relationship:'member'}]};
    throw new Error('Unexpected GET ' + url);
  }
  const visibility = path.match(/\\/abilities\\/([^/]+)\\/visibility$/);
  if (method === 'PUT' && visibility) {
    if (body.expected_revision !== revision) { const error = Error('revision changed'); error.status = 409; throw error; }
    revision += 1;
    return {revision};
  }
  throw new Error('Unexpected ' + method + ' ' + url);
};
window.fixture = {
  requests,
  bump: () => { revision += 1; },
  reads: () => compositionReads,
  abilityReads: () => requests.filter(item => item.method === 'GET' && /\\/abilities$/.test(item.url)).length,
  ready: renderAbilities(document.getElementById('host'), request, {agentId:'a'})
    .catch(error => { window.fixture.error = String(error?.stack || error); }),
};
`;

function serve() {
  const server = http.createServer(async (req, res) => {
    const path = new URL(req.url, 'http://x').pathname;
    if (path === '/') { res.setHeader('content-type', 'text/html'); res.end(html); return; }
    if (path === '/fixture.js') { res.setHeader('content-type', 'text/javascript'); res.end(fixture); return; }
    if (!path.startsWith('/ui/')) { res.statusCode = 404; res.end(); return; }
    try {
      const body = await fs.readFile(new URL('.' + path, root));
      res.setHeader('content-type', path.endsWith('.css') ? 'text/css' : 'text/javascript');
      res.end(body);
    } catch { res.statusCode = 404; res.end(); }
  });
  return new Promise(resolve => server.listen(0, '127.0.0.1', () => resolve(server)));
}

// Fail with the page's own error text instead of a bare Playwright timeout.
async function waitForChart(page, text) {
  try {
    await page.waitForFunction(value => document.body.textContent.includes(value), text);
  } catch (error) {
    const detail = await page.evaluate(() => ({
      error: window.fixture?.error || '',
      body: (document.getElementById('host')?.textContent || document.body.textContent || '').slice(0, 300),
    }));
    throw new Error(error.message + ' | ' + JSON.stringify(detail));
  }
}

test('a save repaints only the chart, not the abilities page', async () => {
  const server = await serve();
  let browser;
  try {
    const {chromium} = await import(process.env.PLAYWRIGHT_MODULE || 'playwright');
    browser = await chromium.launch({executablePath: process.env.CHROMIUM_PATH || '/snap/bin/chromium', headless: true, args: ['--no-sandbox']});
    const page = await browser.newPage();
    page.setDefaultTimeout(8000);
    await page.goto('http://127.0.0.1:' + server.address().port + '/');
    await waitForChart(page, '1,000 tokens');
    const baseline = await page.evaluate(() => ({
      origin: performance.timeOrigin,
      abilityReads: window.fixture.abilityReads(),
      marker: (document.getElementById('host').dataset.marker = 'kept'),
    }));
    // The ability switch saves through the real page handler.
    await page.evaluate(() => document.querySelector('[data-ability-id="web_access"] input.conn-toggle').click());
    await waitForChart(page, '1,400 tokens');
    const after = await page.evaluate(() => ({
      origin: performance.timeOrigin,
      abilityReads: window.fixture.abilityReads(),
      marker: document.getElementById('host').dataset.marker,
      rows: document.querySelectorAll('.v3-ability').length,
      toggle: document.querySelector('[data-ability-id="web_access"] input.conn-toggle')?.checked,
      toggleType: document.querySelector('[data-ability-id="web_access"] input.conn-toggle')?.type,
      exposure: document.querySelector('.v3-ability[data-ability-id="web_access"]')?.dataset.exposure,
      puts: window.fixture.requests.filter(item => item.method === 'PUT').map(item => item.url),
      reads: window.fixture.reads(),
    }));
    assert.equal(after.origin, baseline.origin, 'browser page must not reload');
    assert.equal(after.marker, 'kept', 'abilities page must not re-render');
    assert.equal(after.abilityReads, baseline.abilityReads, 'abilities data must not be re-read');
    assert.equal(after.rows, 2);
    assert.equal(after.exposure, 'off', 'ability exposure: ' + JSON.stringify(after));
    assert.equal(after.toggle, false, 'ability toggle state: ' + JSON.stringify(after));
    assert.ok(after.reads >= 2, 'composition must be re-read after the save');
    assert.equal(await page.evaluate(() => window.fixture.requests.filter(item => item.method === 'PUT').length), 1);
  } finally {
    await browser?.close();
    await new Promise(resolve => server.close(resolve));
  }
});

test('a repaint waits while the chart is focused, then catches up', async () => {
  const server = await serve();
  let browser;
  try {
    const {chromium} = await import(process.env.PLAYWRIGHT_MODULE || 'playwright');
    browser = await chromium.launch({executablePath: process.env.CHROMIUM_PATH || '/snap/bin/chromium', headless: true, args: ['--no-sandbox']});
    const page = await browser.newPage();
    page.setDefaultTimeout(8000);
    await page.goto('http://127.0.0.1:' + server.address().port + '/');
    await waitForChart(page, '1,000 tokens');
    await page.evaluate(() => {
      document.querySelector('[data-context-composition-chart]').focus();
      document.dispatchEvent(new Event('visibilitychange'));
    });
    await page.waitForTimeout(350);
    assert.equal(await page.evaluate(() => document.body.textContent.includes('1,000 tokens')), true,
      'the focused chart must keep its current bars');
    await page.evaluate(() => document.querySelector('[data-context-composition-chart]').blur());
    await waitForChart(page, '1,400 tokens');
  } finally {
    await browser?.close();
    await new Promise(resolve => server.close(resolve));
  }
});

test('a revision poll re-reads the estimate only when the stamp moves', async () => {
  const server = await serve();
  let browser;
  try {
    const {chromium} = await import(process.env.PLAYWRIGHT_MODULE || 'playwright');
    browser = await chromium.launch({executablePath: process.env.CHROMIUM_PATH || '/snap/bin/chromium', headless: true, args: ['--no-sandbox']});
    const page = await browser.newPage();
    page.setDefaultTimeout(8000);
    await page.goto('http://127.0.0.1:' + server.address().port + '/');
    await waitForChart(page, '1,000 tokens');
    // Two poll ticks with an unchanged stamp must not re-read the estimate.
    await page.waitForTimeout(2600);
    assert.equal(await page.evaluate(() => window.fixture.reads()), 1);
    // A moved stamp is re-read once, and the following ticks stay quiet.
    await page.evaluate(() => window.fixture.bump());
    await waitForChart(page, '1,400 tokens');
    await page.waitForTimeout(2200);
    assert.equal(await page.evaluate(() => window.fixture.reads()), 2);
  } finally {
    await browser?.close();
    await new Promise(resolve => server.close(resolve));
  }
});
