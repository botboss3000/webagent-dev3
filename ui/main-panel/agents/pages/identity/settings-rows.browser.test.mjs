import http from 'node:http';
import fs from 'node:fs/promises';
import path from 'node:path';
import assert from 'node:assert/strict';
const {chromium}=await import(process.env.PLAYWRIGHT_MODULE||'playwright');
const root=path.resolve(new URL('../../../../../',import.meta.url).pathname);
const server=http.createServer(async(req,res)=>{
 try{
  const u=new URL(req.url,'http://localhost').pathname;
  if(u==='/'){res.setHeader('content-type','text/html');res.end('<!doctype html><meta name="viewport" content="width=device-width,initial-scale=1"><link rel="stylesheet" href="/shared/ui/shared/css/design-system.css"><link rel="stylesheet" href="/shared/ui/shared/subpage-list/subpage-list.css"><link rel="stylesheet" href="/shared/ui/main-panel/agents/agents.css"><link rel="stylesheet" href="/shared/ui/main-panel/agents/pages/identity/identity.css"><link rel="stylesheet" href="/ui/shell/app.css"><link rel="stylesheet" href="/ui/shared/option-list/option-list.css"><link rel="stylesheet" href="/ui/shared/models/model-table.css"><link rel="stylesheet" href="/ui/shared/models/model-analytics.css"><link rel="stylesheet" href="/ui/shared/models/save-indicator.css"><link rel="stylesheet" href="/ui/main-panel/agents/pages/abilities/ability-table.css"><link rel="stylesheet" href="/ui/chat/core/confirm-panel.css"><link rel="stylesheet" href="/ui/shared/models/field-tip.css"><link rel="stylesheet" href="/ui/shared/skeleton/skeleton.css"><link rel="stylesheet" href="/ui/shared/controls/controls.css"><link rel="stylesheet" href="/ui/shared/config-rows/config-rows.css"><link rel="stylesheet" href="/ui/shared/models/reference.css"><main id="host" style="padding:12px;max-width:650px"></main>');return;}
  if(u==='/config/ui/vibrate.json'){res.setHeader('content-type','application/json');res.end('{"enabled":true,"duration_ms":17}');return;}
  const file=u.startsWith('/shared/')?path.resolve(root,u.slice(8)):path.resolve(root,'.'+u);
  if(!file.startsWith(root+'/ui/')&&!file.startsWith(path.join(root,'ui')+'/'))throw Error('outside');
  res.setHeader('content-type',file.endsWith('.css')?'text/css':'text/javascript');res.end(await fs.readFile(file));
 }catch{res.statusCode=404;res.end('missing');}
});
await new Promise(r=>server.listen(0,'127.0.0.1',r));let browser;
try{
 browser=await chromium.launch({headless:true,executablePath:process.env.CHROMIUM_PATH,args:['--no-sandbox']});
 for(const touch of [false,true])for(const light of [false,true]){
  const context=await browser.newContext({hasTouch:touch,isMobile:touch,viewport:touch?{width:390,height:844}:{width:1200,height:900}});
  const page=await context.newPage();page.setDefaultTimeout(6000);const errors=[];page.on('pageerror',e=>{errors.push(e.message);console.error('pageerror: '+e.message);});
  await page.goto('http://127.0.0.1:'+server.address().port+'/');
  await page.evaluate(async light=>{
   document.body.classList.toggle('light-mode',light);
   const {renderIdentity}=await import('/ui/main-panel/agents/pages/identity/page.js');
   const {vibrationConfigReady}=await import('/ui/shared/vibrate/vibrate.js');await vibrationConfigReady;
   window.pulses=[];Object.defineProperty(navigator,'vibrate',{configurable:true,value:n=>{pulses.push(n);return true;}});
   // app.js answers a back press through consumeBack; the fixture stands in for that listener.
   const {consumeBack}=await import('/ui/shared/history-back/history-back.js');
   window.__unclaimed=0;window.__alive=true;
   window.addEventListener('popstate',()=>{if(!consumeBack())window.__unclaimed+=1;});
   const agent={name:'Fixture',description:'',revision:3,can_manage:true,member_access:{visitor_access:false,app_login_members:true,conversation_privacy:'operator_reviewable'},capability_levels:{revision:0,order:['operator','member','visitor'],levels:Object.fromEntries([['operator','Operator'],['member','Member'],['visitor','Visitor']].map(([id,name])=>[id,{id,name,revision:1,ability_permissions:{},tool_permissions:{},ability_exposure:{},tool_exposure:{},skill_exposure:{}}]))},default_execution_mode:'ask',execution_modes:[{id:'ask',label:'Ask',description:'Ask first',prompt:'Ask prompt',builtin:true},{id:'auto',label:'Auto',description:'Act',prompt:'Auto prompt',builtin:true}],shared_chat:{shareable:false,admin_view:false,admin_join:false,help:false,initial_active:true},compaction_thresholds:{trigger_tokens:150000,tail_tokens:40000},compaction_defaults:{trigger_tokens:150000,tail_tokens:40000},manager_incoming_message:{enabled:false,criteria:''},message_visibility:{mode:'manual'},closing_message:{notification_guidance:'Saved closing'},closing_protocol:{flags:['complete'],notification_target_words:5},voice_output:{enabled:false,voice:'af_heart'},voice_catalog:[{id:'af_heart',name:'Heart',group:'English'}]};
   const widget={revision:5,config:{enabled:false,guest_access:false,allowed_origins:['https://example.com']}};
   window.fixture={agent,widget,writes:[],fail:false};
   const request=async(url,method='GET',body)=>{
    if(url==='/models/agents/one/main-chat')throw Error('Main Chat outside fixture');
    if(url==='/agents/one/capability-ladder'&&method==='PUT'){fixture.ladder=body;return {capability_levels:body.ladder};}
    const embed=url==='/agents/one/embed';if(!embed&&url!=='/models/agents/one/identity')throw Error('Unscoped route '+url);
    if(method==='GET')return structuredClone(embed?widget:agent);
    fixture.writes.push({url,body:structuredClone(body)});if(fixture.fail){fixture.fail=false;throw Error('stale revision');}
    if(embed){if(body.expected_revision!==widget.revision)throw Error('wrong widget revision');Object.assign(widget,{revision:widget.revision+1,config:structuredClone(body.config)});return structuredClone(widget);}
    if(body.revision!==agent.revision)throw Error('wrong agent revision');const {revision,...patch}=body;Object.assign(agent,structuredClone(patch));agent.revision++;return structuredClone(agent);
   };
   window.remount=async()=>{await renderIdentity(document.querySelector('#host'),request,{agentId:'one'});};await remount();
  },light);
  const titles=['Profile','Member access','Website chat widget','Shared conversations','Main Chat','Context compaction','Duplicate tool results','Lifetime/budget retention','Chat modes','Review incoming messages','Final response','Voice output','Chat customization'];
  const rowFor=title=>page.locator('.spl-row',{hasText:title}).first();
  const openRow=async title=>{
   await rowFor(title).click();
   await page.waitForFunction(()=>{const menu=document.querySelector('.spl-menu');const detail=document.querySelector('.spl-detail');return detail&&!detail.hidden&&menu&&menu.hidden;});
  };
  const backToList=async()=>{
   await page.locator('.spl-back').click();
   await page.waitForFunction(()=>{const menu=document.querySelector('.spl-menu');return document.querySelector('.spl-detail').hidden&&!menu.hidden;});
  };
  const toList=async()=>{if(await page.evaluate(()=>!document.querySelector('.spl-detail').hidden))await backToList();};
  assert.equal(await page.locator('.spl-row').count(),titles.length);
  assert.equal(await page.locator('.spl-menu .spl-title').first().textContent(),'Profile');
  // Each category opens its own panel, back returns focus to its row, and opening never writes.
  for(const title of titles){
   await openRow(title);
   assert.equal(await page.locator('.spl-detail-body').evaluate(node=>node.children.length>0),true);
   await backToList();
   assert.equal(await rowFor(title).evaluate(node=>node===document.activeElement),true);
  }
  assert.equal(await page.evaluate(()=>fixture.writes.length),0);
  assert.equal(await page.locator('.spl-toggle-slot .conn-toggle-wrap').count(),0);
  // Only a phone claims the back press; a desktop open leaves the page's history alone.
  if(!touch){
   await openRow('Chat modes');
   assert.equal(await page.evaluate(()=>Boolean(history.state&&history.state.webagentBack)),false);
   await backToList();
  }
  await openRow('Review incoming messages');
  const reviewPanel=page.locator('.spl-detail-body');
  await page.locator('.spl-toggle-slot .conn-toggle-wrap').click();
  assert.equal(await page.getByLabel('Enable incoming-message review',{exact:true}).isChecked(),false);
  assert.match(await reviewPanel.innerText(),/Enter an acceptance prompt/);
  assert.equal(await page.getByLabel('Acceptance prompt',{exact:true}).evaluate(node=>node===document.activeElement),true);
  assert.equal(await page.evaluate(()=>fixture.writes.length),0);
  await page.getByLabel('Acceptance prompt',{exact:true}).fill('Accept relevant requests');
  await page.getByRole('button',{name:'Save prompt',exact:true}).click();await page.waitForFunction(()=>fixture.agent.manager_incoming_message.criteria==='Accept relevant requests');
  assert.equal(await page.evaluate(()=>fixture.agent.manager_incoming_message.enabled),false);
  await page.locator('.spl-toggle-slot .conn-toggle-wrap').click();await page.waitForFunction(()=>fixture.agent.manager_incoming_message.enabled);
  await page.evaluate(()=>fixture.fail=true);await page.locator('.spl-toggle-slot .conn-toggle-wrap').click();
  await page.waitForFunction(()=>document.querySelector('.spl-toggle-slot input[type=checkbox]').checked===true);
  assert.match(await reviewPanel.innerText(),/Save failed: stale revision/);
  await toList();
  await openRow('Website chat widget');
  await page.locator('.spl-toggle-slot .conn-toggle-wrap').click();
  assert.equal(await page.evaluate(()=>fixture.widget.config.enabled),false);
  await page.getByRole('button',{name:'Save widget settings',exact:true}).click();await page.waitForFunction(()=>fixture.widget.config.enabled);assert.equal(await page.evaluate(()=>fixture.writes.at(-1).body.expected_revision),5);
  await toList();
  await openRow('Shared conversations');
  // The shared switch hides its checkbox; press the visible switch.
  await page.locator('label.conn-toggle-wrap:has([aria-label="Participants may invite others"])').click();await page.getByRole('button',{name:'Save shared-chat policy',exact:true}).click();await page.waitForFunction(()=>fixture.agent.shared_chat.shareable);
  await toList();
  await openRow('Member access');
  assert.equal(await page.getByRole('radio',{name:/^Registered/}).isChecked(),true);
  await page.getByRole('radio',{name:/^Authorized/}).check();await page.waitForFunction(()=>fixture.agent.member_access.app_login_members===false&&fixture.agent.member_access.visitor_access===false);
  await page.getByLabel('Conversation privacy',{exact:true}).selectOption('private');await page.waitForFunction(()=>fixture.agent.member_access.conversation_privacy==='private');
  await toList();
  await openRow('Chat modes');
  await page.locator('[data-field="default_execution_mode"]').selectOption('auto');await page.waitForFunction(()=>fixture.agent.default_execution_mode==='auto');
  await page.locator('[data-mode-id="ask"] summary').click();await page.locator('[data-mode-id="ask"] [data-mode-field="prompt"]').fill('Updated prompt');await page.locator('[data-mode-id="ask"] [data-mode-card-save]').click();await page.waitForFunction(()=>fixture.agent.execution_modes[0].prompt==='Updated prompt');
  await toList();
  await openRow('Final response');
  await page.getByLabel('Notification guidance',{exact:true}).fill('Updated closing');await page.locator('.spl-detail-body').getByRole('button',{name:'Save',exact:true}).click();await page.waitForFunction(()=>fixture.agent.closing_message.notification_guidance==='Updated closing');
  await page.locator('label:has([data-final-key="require_flag"]) .conn-toggle-track').click();await page.waitForFunction(()=>fixture.agent.closing_message.require_flag===false);
  assert.equal(await page.evaluate(()=>fixture.agent.closing_message.notification_guidance),'Updated closing');
  await page.locator('label:has([data-final-key="promote_all"]) .conn-toggle-track').click();await page.waitForFunction(()=>fixture.agent.message_visibility.mode==='auto');
  await page.locator('label:has([data-final-key="promote_all"]) .conn-toggle-track').click();await page.waitForFunction(()=>fixture.agent.message_visibility.mode==='manual');
  await page.locator('label:has([data-final-key="require_notification"]) .conn-toggle-track').click();await page.waitForFunction(()=>fixture.agent.closing_message.enabled===false);
  assert.equal(await page.getByLabel('Notification guidance',{exact:true}).isVisible(),false);
  await page.locator('label:has([data-final-key="require_notification"]) .conn-toggle-track').click();await page.waitForFunction(()=>fixture.agent.closing_message.enabled===true&&fixture.agent.closing_message.require_notification===true);
  await page.evaluate(()=>remount());
  assert.equal(await page.locator('.spl-row').count(),titles.length);
  await openRow('Chat modes');assert.equal(await page.locator('[data-field="default_execution_mode"]').inputValue(),'auto');await toList();
  await openRow('Final response');assert.equal(await page.getByLabel('Notification guidance',{exact:true}).textContent(),'Updated closing');assert.equal(await page.locator('[data-final-key="require_flag"]').isChecked(),false);await toList();
  if(touch){
   const before=await page.evaluate(()=>pulses.length);
   await rowFor('Shared conversations').tap();
   await page.waitForFunction(()=>!document.querySelector('.spl-detail').hidden&&document.querySelector('.spl-menu').hidden);
   assert.equal(await page.evaluate(()=>pulses.length),before+1);
   // The way back is a touch control too: one tap, one request.
   await page.locator('.spl-back').tap();
   await page.waitForFunction(()=>document.querySelector('.spl-detail').hidden&&!document.querySelector('.spl-menu').hidden);
   assert.equal(await page.evaluate(()=>pulses.length),before+2);
   // The phone's back press returns to the list instead of leaving the page.
   await openRow('Chat modes');
   assert.equal(await page.evaluate(()=>Boolean(history.state&&history.state.webagentBack)),true);
   await page.evaluate(()=>history.back());
   await page.waitForFunction(()=>document.querySelector('.spl-detail').hidden&&!document.querySelector('.spl-menu').hidden);
   assert.equal(await page.evaluate(()=>window.__alive===true),true);
   assert.equal(await page.evaluate(()=>window.__unclaimed),0);
  }
  await openRow('Member access');
  await page.getByLabel('Member messages per 24 hours',{exact:true}).fill('5');await page.getByRole('button',{name:'Save limits'}).first().click();
  await page.waitForFunction(()=>fixture.ladder?.ladder.levels.member.daily_turns===5&&fixture.ladder.ladder.levels.visitor.daily_turns===undefined);
  // Switching pages starts the new page at the top; a pinned tab header stays pinned above it.
  await toList();
  await page.evaluate(() => {
   const host = document.querySelector('#host');
   const scroller = document.createElement('div');
   scroller.id = 'main-panel';
   // app.css pins #main-panel to position:relative; the fixture needs a real scroller.
   const style = document.createElement('style');
   style.textContent = '#main-panel{position:fixed!important;inset:0!important;width:auto!important;overflow:auto!important}';
   document.head.append(style);
   const lead = document.createElement('div');
   lead.style.height = '300px';
   const stick = document.createElement('div');
   stick.id = 'fixture-stick';
   stick.style.cssText = 'position:sticky;top:-1px;height:80px;background:var(--bg-0)';
   const panel = document.createElement('div');
   const tail = document.createElement('div');
   tail.style.height = '1200px';
   panel.append(host, tail);
   scroller.append(lead, stick, panel);
   document.body.append(scroller);
  });
  const scrollState = () => page.evaluate(() => {
   const scroller = document.querySelector('#main-panel'), top = scroller.getBoundingClientRect().top;
   const at = selector => document.querySelector(selector).getBoundingClientRect().top - top;
   return {scrollTop: scroller.scrollTop, pageTop: at('#host'), detailTop: at('.spl-detail'), stickTop: at('#fixture-stick')};
  });
  const scrollRowsTo = value => page.evaluate(v => { document.querySelector('#main-panel').scrollTop = v; }, value);
  await scrollRowsTo(100);
  await openRow('Member access');
  assert.equal((await scrollState()).scrollTop, 0);
  await backToList();
  await scrollRowsTo(600);
  const beforeOpen = await scrollState();
  // A plain DOM click keeps the scroll where it is; a pointer click would scroll the row into view first.
  await page.evaluate(() => [...document.querySelectorAll('.spl-row')].find(row => row.textContent.trim().startsWith('Chat modes')).click());
  await page.waitForFunction(()=>!document.querySelector('.spl-detail').hidden&&document.querySelector('.spl-menu').hidden);
  const openedPage = await scrollState();
  assert.ok(Math.abs(beforeOpen.stickTop) <= 1, JSON.stringify({beforeOpen}));
  assert.ok(openedPage.scrollTop > 0 && Math.abs(openedPage.stickTop) <= 1, JSON.stringify({beforeOpen, openedPage}));
  assert.ok(Math.abs(openedPage.pageTop - (openedPage.stickTop + 80)) < 2);
  assert.ok(openedPage.detailTop - openedPage.pageTop >= 0 && openedPage.detailTop - openedPage.pageTop < 20);
  await page.evaluate(() => document.querySelector('.spl-back').click());
  await page.waitForFunction(()=>document.querySelector('.spl-detail').hidden&&!document.querySelector('.spl-menu').hidden);
  const closedPage = await scrollState();
  assert.ok(Math.abs(closedPage.stickTop) <= 1);
  assert.ok(Math.abs(closedPage.pageTop - (closedPage.stickTop + 80)) < 2);
  await page.evaluate(() => { document.body.append(document.querySelector('#host')); document.querySelector('#main-panel').remove(); });
  await toList();
  await page.evaluate(async()=>{fixture.agent.can_manage=false;await remount();});
  assert.equal(await page.locator('[data-register-section="website-chat-widget"]').count(),0);
  assert.equal(await page.locator('.spl-row').count(),titles.length-1);
  await openRow('Chat modes');assert.equal(await page.locator('[data-field="default_execution_mode"]').isDisabled(),true);await toList();
  await openRow('Review incoming messages');assert.equal(await page.getByLabel('Enable incoming-message review',{exact:true}).isDisabled(),true);await toList();
  await openRow('Final response');assert.equal(await page.getByLabel('Notification guidance',{exact:true}).getAttribute('contenteditable'),'false');await toList();
  await openRow('Member access');assert.equal(await page.getByLabel('Conversation privacy',{exact:true}).isDisabled(),true);assert.equal(await page.getByRole('radio',{name:/^Authorized/}).isDisabled(),true);await toList();
  assert.equal(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth),true);
  if(touch)assert.equal(await page.evaluate(()=>innerWidth===390&&matchMedia('(pointer: coarse)').matches),true);
  // A back press the phone did not claim would have reloaded the page instead.
  if(touch)assert.equal(await page.evaluate(()=>window.__unclaimed),0);
  assert.deepEqual(errors,[]);await context.close();
 }
 console.log('Identity settings register: thirteen categories, one panel at a time, focus returns to the row, touch pulse per tap, review validation focus, explicit saves, revision fences, shared policy, modes, promotion, closing, remount persistence, read-only, both themes.');
}finally{await browser?.close();await new Promise(r=>server.close(r));}
