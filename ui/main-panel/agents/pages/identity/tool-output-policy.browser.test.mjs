import http from 'node:http';
import fs from 'node:fs/promises';
import path from 'node:path';
import assert from 'node:assert/strict';
const {chromium}=await import(process.env.PLAYWRIGHT_MODULE||'playwright');
const root=path.resolve(new URL('../../../../../',import.meta.url).pathname);
const server=http.createServer(async(req,res)=>{
 try {
  const url=new URL(req.url,'http://localhost').pathname;
  if(url==='/') {res.setHeader('content-type','text/html');res.end('<!doctype html><meta name="viewport" content="width=device-width,initial-scale=1"><link rel="stylesheet" href="/shared/ui/shared/css/design-system.css"><link rel="stylesheet" href="/shared/ui/main-panel/agents/agents.css"><link rel="stylesheet" href="/ui/shell/app.css"><link rel="stylesheet" href="/ui/shared/option-list/option-list.css"><link rel="stylesheet" href="/ui/shared/models/model-table.css"><link rel="stylesheet" href="/ui/shared/models/model-analytics.css"><link rel="stylesheet" href="/ui/shared/models/save-indicator.css"><link rel="stylesheet" href="/ui/main-panel/agents/pages/abilities/ability-table.css"><link rel="stylesheet" href="/ui/chat/core/confirm-panel.css"><link rel="stylesheet" href="/ui/shared/models/field-tip.css"><link rel="stylesheet" href="/ui/shared/skeleton/skeleton.css"><link rel="stylesheet" href="/ui/shared/controls/controls.css"><link rel="stylesheet" href="/ui/shared/models/reference.css"><main id="host" style="padding:12px;max-width:650px"></main>');return;}
  if(url==='/config/ui/vibrate.json') {res.setHeader('content-type','application/json');res.end(JSON.stringify({enabled:true,duration_ms:17}));return;}
  const file=url.startsWith('/shared/')?path.resolve(root,url.slice(8)):path.resolve(root,'.'+url);
  if(!file.startsWith(root+'/ui/')&&!file.startsWith(path.join(root,'ui')+'/'))throw Error('outside fixture');
  res.setHeader('content-type',file.endsWith('.css')?'text/css':'text/javascript');res.end(await fs.readFile(file));
 } catch {res.statusCode=404;res.end('missing');}
});
await new Promise(r=>server.listen(0,'127.0.0.1',r));let browser;
try {
 browser=await chromium.launch({headless:true,executablePath:process.env.CHROMIUM_PATH||'/snap/bin/chromium',args:['--no-sandbox']});
 for(const touch of [false,true])for(const light of [false,true]) {
  const context=await browser.newContext({hasTouch:touch,isMobile:touch,viewport:touch?{width:390,height:844}:{width:1200,height:900}});
  await context.addInitScript(()=>{window.pulses=[];Object.defineProperty(navigator,'vibrate',{configurable:true,value:n=>{pulses.push(n);return true;}});});
  const page=await context.newPage();page.setDefaultTimeout(5000);const errors=[];page.on('pageerror',e=>{errors.push(e.message);console.error('pageerror: '+e.message);});
  const setup=async(available=true,canManage=true)=>{
   await page.evaluate(async({available,canManage,light})=>{
    document.body.classList.toggle('light-mode',light);
    const {renderCompaction}=await import('/ui/main-panel/agents/pages/identity/compaction.js');
    const {renderToolOutputPolicy}=await import('/ui/main-panel/agents/pages/identity/tool-output-policy.js');
    const {vibrationConfigReady}=await import('/ui/shared/vibrate/vibrate.js');await vibrationConfigReady;
    const stored=JSON.parse(localStorage.getItem('dedup-fixture')||'true');
    const agent={can_manage:canManage,compaction_overridden:true,compaction_thresholds:{trigger_tokens:150000,tail_tokens:20000,ceiling_tokens:250000,summarizer:'self'},compaction_defaults:{trigger_tokens:150000,tail_tokens:20000,summary_tokens:5000,ceiling_tokens:250000,summarizer:'self'},tool_output_policy:{enabled:true,scope:'all_eligible',tools:[],lifetime_calls:6,ordinary_chars:24000,promoted_chars:24000,refresh_on_recall:true,promote_on_edit:true},tool_output_eligible_tools:['file_read']};
    if(available)agent.tool_result_deduplication={enabled:stored};
    window.fixture={writes:[],fail:false,agent};
    document.querySelector('#host').replaceChildren();
    const persist=async patch=>{
     fixture.writes.push(patch);if(fixture.fail)throw Error('stale revision');
     Object.assign(agent,patch);return structuredClone(agent);
    };
    renderCompaction(document.querySelector('#host'),agent,persist);
    renderToolOutputPolicy(document.querySelector('#host'),agent,async patch=>{
     fixture.writes.push(patch);if(fixture.fail)throw Error('stale revision');
     localStorage.setItem('dedup-fixture',JSON.stringify(patch.tool_result_deduplication.enabled));
     return {...agent,...patch};
    });
   },{available,canManage,light});
  };
  await page.goto('http://127.0.0.1:'+server.address().port+'/');await setup();
  // The register owns the disclosure now: these rows render their fields openly and rendering never saves.
  assert.deepEqual(await page.evaluate(()=>fixture.writes),[]);
  // Only the keep-alive rows have switches: the main one and four session states.
  assert.equal(await page.locator('[data-register-section=context-compaction] input[type=checkbox]').count(),5);
  assert.equal(await page.getByLabel('Keep cache warm (experimental)',{exact:true}).isChecked(),false);
  await page.getByLabel('Summary trigger',{exact:true}).fill('10000');
  await page.getByRole('button',{name:'Save compaction thresholds',exact:true}).click();
  await page.getByText('Trigger must exceed the tail plus the summary target (5,000).',{exact:true}).waitFor();
  assert.deepEqual(await page.evaluate(()=>fixture.writes),[]);
  await page.getByLabel('Summary trigger',{exact:true}).fill('160000');
  await page.getByRole('button',{name:'Save compaction thresholds',exact:true}).click();
  await page.getByText('Saved. New model calls use these thresholds.',{exact:true}).waitFor();
  assert.deepEqual(await page.evaluate(()=>fixture.writes),[{compaction_thresholds:{trigger_tokens:160000,tail_tokens:20000,ceiling_tokens:250000,summarizer:'self'}}]);
  await page.getByRole('button',{name:'Use app defaults',exact:true}).click();
  await page.getByText('Saved. New model calls use the app defaults.',{exact:true}).waitFor();
  assert.deepEqual((await page.evaluate(()=>fixture.writes)).at(-1),{reset_compaction_thresholds:true});
  assert.equal(await page.getByLabel('Enable retention',{exact:true}).isDisabled(),true);
  assert.equal(await page.getByLabel('Retention scope',{exact:true}).isDisabled(),true);
  await page.getByText('Under development · Experimental — disabled. Saved lifetime and budget values are preserved for a later strategy revision.',{exact:true}).waitFor();
  if(touch)assert.equal(await page.evaluate(()=>innerWidth===390&&matchMedia('(pointer: coarse)').matches),true);
  await page.evaluate(()=>{fixture.writes=[];pulses.length=0;});
  const toggle=page.getByLabel('Suppress identical tool results',{exact:true});
  assert.equal(await toggle.isChecked(),true);
  if(touch)await page.locator('[data-register-section=duplicate-tool-results] .conn-toggle-wrap').tap();else {await toggle.focus();await page.keyboard.press('Space');}
  await page.getByText('Saved. Future results use this setting; existing references stay unchanged.',{exact:true}).waitFor();
  assert.equal(await toggle.isChecked(),false);
  assert.deepEqual(await page.evaluate(()=>pulses),touch?[17]:[]);
  assert.deepEqual(await page.evaluate(()=>fixture.writes),[{tool_result_deduplication:{enabled:false}}]);
  await page.reload();await setup();assert.equal(await toggle.isChecked(),false);
  await page.evaluate(()=>fixture.fail=true);await page.locator('[data-register-section=duplicate-tool-results] .conn-toggle-wrap').click();
  await page.getByText('Identity changed elsewhere. Reload this page before saving.',{exact:true}).waitFor();
  assert.equal(await toggle.isChecked(),false);
  await setup(false);assert.equal(await toggle.isDisabled(),true);
  assert.equal(await page.getByLabel('Enable retention',{exact:true}).isChecked(),true);
  await page.getByText(/running lifetime\/budget policy is unchanged/).waitFor();
  await page.evaluate(()=>document.querySelector('#identity-tool-result-deduplication-enabled').dispatchEvent(new Event('change')));
  assert.deepEqual(await page.evaluate(()=>fixture.writes),[]);
  await setup(true,false);assert.equal(await toggle.isDisabled(),true);
  assert.equal(await page.getByLabel('Summary trigger',{exact:true}).isDisabled(),true);
  assert.equal(await page.getByRole('button',{name:'Save compaction thresholds',exact:true}).count(),0);
  // The per-tool result-deduplication picker belongs to the shared ability row control,
  // which its own test suite covers; this fixture covers this page's policy only.
  assert.equal(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth),true);
  assert.deepEqual(errors,[]);await context.close();
 }
 console.log('Duplicate-result UI: desktop/touch, light/dark, keyboard focus, persistence, conflict and pending-backend checks passed');
}finally{await browser?.close();await new Promise(r=>server.close(r));}
