import http from 'node:http';
import fs from 'node:fs/promises';
import path from 'node:path';
import assert from 'node:assert/strict';
const {chromium}=await import(process.env.PLAYWRIGHT_MODULE||'playwright');
const root=path.resolve(new URL('../../../../../',import.meta.url).pathname);
const server=http.createServer(async(req,res)=>{
 try{
  const u=new URL(req.url,'http://localhost').pathname;
  if(u==='/'){res.setHeader('content-type','text/html');res.end('<!doctype html><meta name="viewport" content="width=device-width,initial-scale=1"><link rel="stylesheet" href="/shared/ui/shared/css/design-system.css"><link rel="stylesheet" href="/shared/ui/main-panel/agents/agents.css"><link rel="stylesheet" href="/ui/app.css"><link rel="stylesheet" href="/ui/shared/models/reference.css"><main id="host" style="padding:12px;max-width:650px"></main>');return;}
  if(u==='/ui/vibrate.json'){res.setHeader('content-type','application/json');res.end('{"enabled":true,"duration_ms":17}');return;}
  const file=u.startsWith('/shared/')?path.resolve(root,'..',u.slice(8)):path.resolve(root,'.'+u);
  if(!file.startsWith(root+'/ui/')&&!file.startsWith(path.resolve(root,'../ui')+'/'))throw Error('outside');
  res.setHeader('content-type',file.endsWith('.css')?'text/css':'text/javascript');res.end(await fs.readFile(file));
 }catch{res.statusCode=404;res.end('missing');}
});
await new Promise(r=>server.listen(0,'127.0.0.1',r));let browser;
try{
 browser=await chromium.launch({headless:true,executablePath:process.env.CHROMIUM_PATH,args:['--no-sandbox']});
 for(const touch of [false,true])for(const light of [false,true]){
  const context=await browser.newContext({hasTouch:touch,isMobile:touch,viewport:touch?{width:390,height:844}:{width:1200,height:900}});
  const page=await context.newPage();page.setDefaultTimeout(6000);const errors=[];page.on('pageerror',e=>errors.push(e.message));
  await page.goto('http://127.0.0.1:'+server.address().port+'/');
  await page.evaluate(async light=>{
   document.body.classList.toggle('light-mode',light);
   const {renderIdentity}=await import('/ui/main-panel/agents/pages/identity/page.js');
   const {vibrationConfigReady}=await import('/ui/shared/vibrate.js');await vibrationConfigReady;
   window.pulses=[];Object.defineProperty(navigator,'vibrate',{configurable:true,value:n=>{pulses.push(n);return true;}});
   const agent={name:'Fixture',description:'',revision:3,can_manage:true,default_execution_mode:'ask',execution_modes:[{id:'ask',label:'Ask',permission_policy:'read_only',description:'Ask first',prompt:'Ask prompt',builtin:true},{id:'auto',label:'Auto',permission_policy:'write',description:'Act',prompt:'Auto prompt',builtin:true}],shared_chat:{shareable:false,admin_view:false,admin_join:false,help:false,initial_active:true},compaction_thresholds:{trigger_tokens:150000,tail_tokens:40000},compaction_defaults:{trigger_tokens:150000,tail_tokens:40000},manager_incoming_message:{enabled:false,criteria:''},message_visibility:{mode:'manual',guidance:'Saved guidance',fixed_protocol:'Fixed protocol'},closing_message:{notification_guidance:'Saved closing'},closing_protocol:{flags:['complete'],notification_max_words:5}};
   const widget={revision:5,config:{enabled:false,guest_access:false,allowed_origins:['https://example.com']}};
   window.fixture={agent,widget,writes:[],fail:false};
   const request=async(url,method='GET',body)=>{
    if(url==='/models/agents/one/main-chat')throw Error('Main Chat outside fixture');
    if(url==='/agents/one/shared-sessions')return {sessions:[]};
    const embed=url==='/agents/one/embed';if(!embed&&url!=='/models/agents/one/identity')throw Error('Unscoped route '+url);
    if(method==='GET')return structuredClone(embed?widget:agent);
    fixture.writes.push({url,body:structuredClone(body)});if(fixture.fail){fixture.fail=false;throw Error('stale revision');}
    if(embed){if(body.expected_revision!==widget.revision)throw Error('wrong widget revision');Object.assign(widget,{revision:widget.revision+1,config:structuredClone(body.config)});return structuredClone(widget);}
    if(body.revision!==agent.revision)throw Error('wrong agent revision');const {revision,...patch}=body;Object.assign(agent,structuredClone(patch));agent.revision++;return structuredClone(agent);
   };
   window.remount=async()=>{await renderIdentity(document.querySelector('#host'),request,{agentId:'one'});};await remount();
  },light);
  const titles=['Website chat widget','Shared conversations','Chat modes','Review incoming messages','Working-message promotion','Final response'];
  for(const title of titles){
   const btn=page.getByRole('button',{name:'Expand '+title,exact:true});assert.equal(await btn.getAttribute('aria-expanded'),'false');
   await btn.press('Enter');assert.equal(await page.getByRole('button',{name:'Collapse '+title,exact:true}).getAttribute('aria-expanded'),'true');
  }
  assert.equal(await page.evaluate(()=>fixture.writes.length),0);
  assert.equal(await page.locator('[data-identity-section="shared-conversations"] > .conn-card-header input').count(),0);
  assert.equal(await page.locator('[data-identity-section="working-message-promotion"] > .conn-card-header select').count(),0);
  const reviewRow=page.locator('[data-identity-section="review-incoming-messages"]');
  await reviewRow.locator('.conn-toggle-wrap').click();
  assert.equal(await page.getByLabel('Enable incoming-message review',{exact:true}).isChecked(),false);
  assert.match(await reviewRow.innerText(),/Enter an acceptance prompt/);
  assert.equal(await page.evaluate(()=>fixture.writes.length),0);
  await page.getByLabel('Acceptance prompt',{exact:true}).fill('Accept relevant requests');
  await page.getByRole('button',{name:'Save prompt',exact:true}).click();await page.waitForFunction(()=>fixture.agent.manager_incoming_message.criteria==='Accept relevant requests');
  assert.equal(await page.evaluate(()=>fixture.agent.manager_incoming_message.enabled),false);
  await page.getByRole('button',{name:'Collapse Review incoming messages',exact:true}).click();
  await reviewRow.locator('.conn-toggle-wrap').click();await page.waitForFunction(()=>fixture.agent.manager_incoming_message.enabled);
  assert.equal(await page.getByRole('button',{name:'Expand Review incoming messages',exact:true}).getAttribute('aria-expanded'),'false');
  await page.evaluate(()=>fixture.fail=true);await reviewRow.locator('.conn-toggle-wrap').click();
  await page.getByRole('button',{name:'Collapse Review incoming messages',exact:true}).waitFor();assert.equal(await page.getByLabel('Enable incoming-message review',{exact:true}).isChecked(),true);assert.match(await reviewRow.innerText(),/Save failed: stale revision/);
  await page.locator('[data-identity-section="website-chat-widget"] .conn-toggle-wrap').click();
  assert.equal(await page.evaluate(()=>fixture.widget.config.enabled),false);
  await page.getByRole('button',{name:'Save widget settings',exact:true}).click();await page.waitForFunction(()=>fixture.widget.config.enabled);assert.equal(await page.evaluate(()=>fixture.writes.at(-1).body.expected_revision),5);
  await page.getByLabel('Participants may invite others',{exact:true}).check();await page.getByRole('button',{name:'Save shared-chat policy',exact:true}).click();await page.waitForFunction(()=>fixture.agent.shared_chat.shareable);
  await page.locator('[data-field="default_execution_mode"]').selectOption('auto');await page.waitForFunction(()=>fixture.agent.default_execution_mode==='auto');
  await page.locator('[data-mode-id="ask"] summary').click();await page.locator('[data-mode-id="ask"] [data-mode-field="prompt"]').fill('Updated prompt');await page.locator('[data-mode-id="ask"] [data-mode-card-save]').click();await page.waitForFunction(()=>fixture.agent.execution_modes[0].prompt==='Updated prompt');
  await page.getByLabel('Working-message promotion',{exact:true}).selectOption('none');await page.waitForFunction(()=>fixture.agent.message_visibility.mode==='none');assert.equal(await page.getByLabel('Additional promotion guidance',{exact:true}).isVisible(),false);
  await page.getByLabel('Working-message promotion',{exact:true}).selectOption('manual');await page.waitForFunction(()=>fixture.agent.message_visibility.mode==='manual');
  await page.getByLabel('Notification guidance',{exact:true}).fill('Updated closing');await page.locator('[data-identity-section="final-response"]').getByRole('button',{name:'Save',exact:true}).click();await page.waitForFunction(()=>fixture.agent.closing_message.notification_guidance==='Updated closing');
  await page.evaluate(()=>remount());for(const title of titles)assert.equal(await page.getByRole('button',{name:'Expand '+title,exact:true}).getAttribute('aria-expanded'),'false');
  await page.getByRole('button',{name:'Expand Chat modes',exact:true}).click();assert.equal(await page.locator('[data-field="default_execution_mode"]').inputValue(),'auto');
  await page.getByRole('button',{name:'Expand Final response',exact:true}).click();assert.equal(await page.getByLabel('Notification guidance',{exact:true}).inputValue(),'Updated closing');
  if(touch){const before=await page.evaluate(()=>pulses.length);await page.getByRole('button',{name:'Expand Shared conversations',exact:true}).tap();assert.equal(await page.evaluate(()=>pulses.length),before+1);}
  await page.evaluate(async()=>{fixture.agent.can_manage=false;await remount();});
  assert.equal(await page.locator('[data-identity-section="website-chat-widget"]').count(),0);
  for(const title of titles.slice(1))await page.getByRole('button',{name:'Expand '+title,exact:true}).click();
  assert.equal(await page.locator('[data-field="default_execution_mode"]').isDisabled(),true);
  for(const label of ['Enable incoming-message review','Working-message promotion','Notification guidance'])assert.equal(await page.getByLabel(label,{exact:true}).isDisabled(),true);
  assert.equal(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth),true);
  if(touch)assert.equal(await page.evaluate(()=>innerWidth===390&&matchMedia('(pointer: coarse)').matches),true);
  assert.deepEqual(errors,[]);await context.close();
 }
 console.log('Identity configuration rows: six features, keyboard-only expansion without writes, explicit widget save, revision fences, review validation/rollback, shared policy, modes, promotion, closing, remount persistence, read-only, touch, both themes.');
}finally{await browser?.close();await new Promise(r=>server.close(r));}
