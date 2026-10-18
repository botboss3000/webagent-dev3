import http from 'node:http';
import fs from 'node:fs/promises';
import path from 'node:path';
import assert from 'node:assert/strict';
const {chromium}=await import(process.env.PLAYWRIGHT_MODULE||'playwright');
const root=path.resolve(new URL('../../../../../',import.meta.url).pathname);
const server=http.createServer(async(req,res)=>{
 try {
  const url=new URL(req.url,'http://localhost').pathname;
  if(url==='/') {res.setHeader('content-type','text/html');res.end('<!doctype html><meta name="viewport" content="width=device-width,initial-scale=1"><link rel="stylesheet" href="/shared/ui/shared/css/design-system.css"><link rel="stylesheet" href="/shared/ui/main-panel/agents/agents.css"><link rel="stylesheet" href="/ui/app.css"><link rel="stylesheet" href="/ui/shared/models/reference.css"><main id="host" style="padding:12px;max-width:650px"></main><div id="tools"></div>');return;}
  if(url==='/ui/vibrate.json') {res.setHeader('content-type','application/json');res.end(JSON.stringify({enabled:true,duration_ms:17}));return;}
  const file=url.startsWith('/shared/')?path.resolve(root,'..',url.slice(8)):path.resolve(root,'.'+url);
  if(!file.startsWith(root+'/ui/')&&!file.startsWith(path.resolve(root,'../ui')+'/'))throw Error('outside fixture');
  res.setHeader('content-type',file.endsWith('.css')?'text/css':'text/javascript');res.end(await fs.readFile(file));
 } catch {res.statusCode=404;res.end('missing');}
});
await new Promise(r=>server.listen(0,'127.0.0.1',r));let browser;
try {
 browser=await chromium.launch({headless:true,executablePath:process.env.CHROMIUM_PATH||'/snap/bin/chromium',args:['--no-sandbox']});
 for(const touch of [false,true])for(const light of [false,true]) {
  const context=await browser.newContext({hasTouch:touch,isMobile:touch,viewport:touch?{width:390,height:844}:{width:1200,height:900}});
  await context.addInitScript(()=>{window.pulses=[];Object.defineProperty(navigator,'vibrate',{configurable:true,value:n=>{pulses.push(n);return true;}});});
  const page=await context.newPage();page.setDefaultTimeout(5000);const errors=[];page.on('pageerror',e=>errors.push(e.message));
  const setup=async(available=true,canManage=true)=>{
   await page.evaluate(async({available,canManage,light})=>{
    document.body.classList.toggle('light-mode',light);
    const {renderToolOutputPolicy}=await import('/ui/main-panel/agents/pages/identity/tool-output-policy.js');
    const {vibrationConfigReady}=await import('/ui/shared/vibrate.js');await vibrationConfigReady;
    const stored=JSON.parse(localStorage.getItem('dedup-fixture')||'true');
    const agent={can_manage:canManage,tool_output_policy:{enabled:true,scope:'code_index',tools:[],lifetime_calls:6,ordinary_chars:24000,promoted_chars:24000,refresh_on_recall:true,promote_on_edit:true},tool_output_eligible_tools:['file_read']};
    if(available)agent.tool_result_deduplication={enabled:stored};
    window.fixture={writes:[],fail:false,agent};
    document.querySelector('#host').replaceChildren();
    renderToolOutputPolicy(document.querySelector('#host'),agent,async patch=>{
     fixture.writes.push(patch);if(fixture.fail)throw Error('stale revision');
     localStorage.setItem('dedup-fixture',JSON.stringify(patch.tool_result_deduplication.enabled));
     return {...agent,...patch};
    });
   },{available,canManage,light});
  };
  await page.goto('http://127.0.0.1:'+server.address().port+'/');await setup();
  const toggle=page.getByLabel('Suppress identical tool results',{exact:true});
  assert.equal(await toggle.isChecked(),true);
  if(touch)await toggle.tap();else {await toggle.focus();await page.keyboard.press('Space');}
  await page.getByText('Saved. Future results use this setting; existing references stay unchanged.',{exact:true}).waitFor();
  assert.equal(await toggle.isChecked(),false);
  assert.deepEqual(await page.evaluate(()=>pulses),touch?[17]:[]);
  assert.deepEqual(await page.evaluate(()=>fixture.writes),[{tool_result_deduplication:{enabled:false}}]);
  await page.reload();await setup();assert.equal(await toggle.isChecked(),false);
  await page.evaluate(()=>fixture.fail=true);await toggle.click();
  await page.getByText('Identity changed elsewhere. Reload this page before saving.',{exact:true}).waitFor();
  assert.equal(await toggle.isChecked(),false);
  await setup(false);assert.equal(await toggle.isDisabled(),true);
  assert.equal(await page.getByLabel('Enable retention',{exact:true}).isChecked(),true);
  await page.getByText(/running lifetime\/budget policy is unchanged/).waitFor();
  await page.evaluate(()=>document.querySelector('#identity-tool-result-deduplication-enabled').dispatchEvent(new Event('change')));
  assert.deepEqual(await page.evaluate(()=>fixture.writes),[]);
  await setup(true,false);assert.equal(await toggle.isDisabled(),true);
  await page.evaluate(async()=>{
   const {_buildToolRow}=await import('/ui/shared/ability-row-controls.js');
   const host=document.querySelector('#tools');host.replaceChildren();fixture.inclusions=[];
   host.append(_buildToolRow({name:'file_read',permMode:'auto',mode:'visible',result_deduplication:true},
    {canEdit:true,showPerm:true,onResultDeduplicationChange:async(name,included)=>{fixture.inclusions.push({name,included});}}));
  });
  await page.locator('#tools .ac-tri').click();
  const inclusion=page.getByRole('menuitemcheckbox',{name:'Suppress identical results'});
  assert.equal(await inclusion.getAttribute('aria-checked'),'true');
  await inclusion.focus();await page.keyboard.press('Enter');
  await page.waitForFunction(()=>document.querySelector('[role="menuitemcheckbox"]')?.getAttribute('aria-checked')==='false');
  assert.equal(await inclusion.evaluate(node=>node===document.activeElement),true);
  assert.deepEqual(await page.evaluate(()=>fixture.inclusions),[{name:'file_read',included:false}]);
  await page.keyboard.press('Escape');assert.equal(await inclusion.count(),0);
  assert.equal(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth),true);
  assert.deepEqual(errors,[]);await context.close();
 }
 console.log('Duplicate-result UI: desktop/touch, light/dark, keyboard focus, persistence, conflict and pending-backend checks passed');
}finally{await browser?.close();await new Promise(r=>server.close(r));}
