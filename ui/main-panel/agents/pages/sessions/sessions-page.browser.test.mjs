const {chromium}=await import(process.env.PLAYWRIGHT_MODULE||'playwright');
import http from 'node:http';import fs from 'node:fs/promises';import path from 'node:path';import assert from 'node:assert/strict';
const root=path.resolve(new URL('../../../../..',import.meta.url).pathname);
const server=http.createServer(async(req,res)=>{try{const file=path.resolve(root,'.'+new URL(req.url,'http://localhost').pathname.replace(/^\/v3-ui\//,'/ui/'));if(!file.startsWith(root+'/'))throw Error('bad');const data=await fs.readFile(file);res.setHeader('content-type',file.endsWith('.js')?'text/javascript':file.endsWith('.css')?'text/css':'text/html');res.end(data);}catch{res.statusCode=404;res.end('Not found');}});
await new Promise(r=>server.listen(0,'127.0.0.1',r));
let browser;try{
 browser=await chromium.launch({executablePath:process.env.CHROMIUM_PATH||'/snap/bin/chromium',headless:true,args:['--no-sandbox']});
 const page=await browser.newPage({viewport:{width:1200,height:850}});const errors=[];page.on('pageerror',e=>errors.push(e.message));
 await page.goto('http://127.0.0.1:'+server.address().port+'/ui/main-panel/agents/pages/sessions/sessions.html');
 const result=await page.evaluate(async()=>{
  const html=await(await fetch('/ui/main-panel/agents/pages/sessions/sessions.html')).text();document.body.innerHTML='<div id="tab-agents" class="active"><div id="sessions-region"><section id="sessions-section"></section></div></div>';
  const style=document.createElement('link');style.rel='stylesheet';style.href='/ui/main-panel/agents/pages/sessions/sessions.css';document.head.append(style);
  const host=document.getElementById('sessions-section');host.innerHTML=html;
  const rows=[{id:'a',agent_id:'one',title:'Alpha',status:'active',metadata_revision:0,created_at:1,last_active_at:10,is_unread:true,unread_count:1,read_through_seq:0,last_public_seq:3,message_count:3,usage:{input_tokens:0,output_tokens:0,reported_calls:1,unreported_calls:0,known_cost_usd:0,unknown_cost_calls:0}},{id:'b',agent_id:'two',title:'Beta',status:'active',metadata_revision:0,created_at:1,last_active_at:9,is_unread:false,message_count:0},{id:'c',agent_id:'one',title:'Recycled',status:'recycled',metadata_revision:0,created_at:1,last_active_at:8,is_unread:false,message_count:0}];
  const writes=[],opens=[],calls=[];let failB=true;
  const request=async(url,method='GET',body)=>{
   calls.push(url);const parsed=new URL(url,location.origin);
   if(url.startsWith('/sessions/stats'))return {sessions:rows.filter(r=>r.status===parsed.searchParams.get('status')),next_offset:null};
   if(url.startsWith('/sessions/search'))return {results:[{session_id:'b',kind:'message',snippet:'needle <script> bad'}],continuation:null};
   const id=decodeURIComponent(parsed.pathname.split('/')[2]),row=rows.find(r=>r.id===id);if(method!=='GET'){writes.push({id,method,body:structuredClone(body)});if(id==='b'&&method==='DELETE'&&failB){const e=Error('fixture network failure');throw e;}if(method==='DELETE')row.status='recycled';if(parsed.pathname.endsWith('/restore'))row.status='active';if(method==='PATCH')Object.assign(row,body);if(method==='PUT')row.is_unread=false;row.metadata_revision++;return {session:structuredClone(row)};}
   throw Error('Unexpected '+url);
  };
  const {mountSessions}=await import('/ui/main-panel/agents/pages/sessions/js/sessions-page.js');
  window.fixture={host,rows,writes,opens,calls,setFailB:v=>{failB=v;},controller:mountSessions(host,{request,userId:'fixture',roster:[{id:'one',name:'Agent One'},{id:'two',name:'Agent Two'}],getSessionId:()=>null,open:id=>opens.push(id),changed:()=>{}})};
  await new Promise(r=>setTimeout(r,300));
  return {titles:[...host.querySelectorAll('.session-row-title-label')].map(n=>n.textContent),display:getComputedStyle(host).display};
 });
 assert.deepEqual(result.titles,['Alpha','Beta']);assert.equal(result.display,'flex');
 assert.equal(await page.locator('td.col-tokens_in').first().innerText(),'0');assert.equal(await page.locator('td.col-cost').first().innerText(),'$0.000000');
 if(await page.locator('[data-sessions-footer-gutter]').getAttribute('aria-pressed')!=='true')await page.locator('[data-sessions-footer-gutter]').click();
 // Keyboard select must not open a chat or write a read cursor.
 await page.locator('.sessions-check-cell').first().focus();await page.keyboard.press('Space');
 assert.equal(await page.evaluate(()=>fixture.opens.length),0);assert.equal(await page.evaluate(()=>fixture.writes.length),0);
 await page.locator('[data-sessions-footer-checkall]').click();
 await page.locator('#sessions-trash-btn').click();await page.waitForTimeout(500);
 assert.equal(await page.evaluate(()=>fixture.writes.length),0);
 await page.locator('#sessions-trash-btn').click();await page.waitForTimeout(300);
 assert.equal(await page.locator('.sessions-check-cell.checked').count(),1);
 const receipt=await page.evaluate(()=>fixture.writes.find(w=>w.id==='b').body.client_request_id);
 await page.evaluate(()=>fixture.setFailB(false));await page.locator('#sessions-trash-btn').click();await page.waitForTimeout(500);await page.locator('#sessions-trash-btn').click();await page.waitForTimeout(300);
 const retries=await page.evaluate(()=>fixture.writes.filter(w=>w.id==='b'&&w.method==='DELETE'));assert.equal(retries.length,2);assert.equal(retries[1].body.client_request_id,receipt);
 // Restore the retained undo context and exercise recycled view.
 await page.locator('#sessions-undo-btn').click();await page.waitForTimeout(500);await page.locator('#sessions-undo-btn').click();await page.waitForTimeout(300);
 await page.locator('[data-sessions-footer-view="recycle-bin"]').click();await page.waitForTimeout(300);
 await page.locator('[data-session-row-more]').first().click();assert.equal(await page.locator('[data-action="delete"]').isDisabled(),true);
 await page.locator('#sessions-heading').click();
 assert.equal(await page.locator('.session-row-actions').count(),0);
 await page.locator('[data-sessions-footer-view="both"]').click();await page.waitForTimeout(300);
 await page.locator('#sessions-search-input').fill('needle');await page.waitForTimeout(500);
 assert.equal(await page.locator('.session-row-title-label').first().innerText(),'Beta');
 assert.equal(await page.evaluate(()=>fixture.calls.some(p=>p.includes('/api/v1'))),false);
 await page.setViewportSize({width:390,height:780});await page.waitForTimeout(100);
 assert.equal(await page.locator('#sessions-table-wrap').evaluate(n=>n.scrollWidth<=n.clientWidth+2),true);
 await page.evaluate(()=>document.body.classList.add('light-mode'));
 await page.locator('#sessions-search-input').fill('');await page.waitForTimeout(250);
 await page.evaluate(()=>fixture.controller.destroy());const before=await page.evaluate(()=>fixture.calls.length);await page.evaluate(()=>window.dispatchEvent(new Event('v3:sessions-changed')));await page.waitForTimeout(100);assert.equal(await page.evaluate(()=>fixture.calls.length),before);

 await page.evaluate(async()=>{
  const host=document.getElementById('sessions-section');host.innerHTML=await(await fetch('/ui/main-panel/agents/pages/sessions/sessions.html')).text();
  const {mountSessions}=await import('/ui/main-panel/agents/pages/sessions/js/sessions-page.js');
  const main={id:'main-protected',title:'Renamed coordinator',agent_id:'one',status:'active',metadata_revision:3,main_chat:true,can_delete:false,message_count:0};
  window.protectedPage=mountSessions(host,{request:async()=>({sessions:[{...main,id:'pin',main_chat:false,can_delete:true,pinned:true,title:'A pinned',last_active_at:900},{...main,id:'recent',main_chat:false,can_delete:true,title:'Main Chat',last_active_at:999},main],next_offset:null}),userId:'protected-fixture',roster:[{id:'one',name:'One'}],getSessionId:()=>null,open:()=>{},changed:()=>{}});
  const {createSessionAdapter}=await import('/ui/main-panel/agents/pages/sessions/js/adapter.js');let writes=0;const adapter=createSessionAdapter({request:async()=>{writes++;},roster:[],userId:'protected',changed:()=>{}});adapter.project(main);let blocked=false;try{await adapter.mutate(main.id,'recycle');}catch(error){blocked=error.message.includes('cannot be deleted');}window.protectedResult={blocked,writes};
 });
 await page.locator('tr[data-session-id="main-protected"]').waitFor();
 assert.deepEqual(await page.locator('#sessions-table-body tr[data-session-id]').evaluateAll(rows=>rows.map(r=>r.dataset.sessionId)),['main-protected','pin','recent']);
 await page.locator('th[data-sort-key="title"]').click();
 assert.deepEqual(await page.locator('#sessions-table-body tr[data-session-id]').evaluateAll(rows=>rows.map(r=>r.dataset.sessionId)),['main-protected','pin','recent']);
 assert.equal(await page.locator('tr[data-session-id="main-protected"] .session-row-agent-main').count(),1);
 assert.equal(await page.locator('tr[data-session-id="pin"] .sess-title-agent-warning').count(),1);
 const mask=await page.locator('tr[data-session-id="main-protected"] .session-row-agent-main').evaluate(n=>getComputedStyle(n,'::after').maskImage);assert.match(mask,/data:image\/svg\+xml/);
assert.equal(await page.locator('tr[data-session-id="main-protected"] .sessions-check-cell').count(),0);
 await page.locator('tr[data-session-id="main-protected"] [data-session-row-more]').click();assert.equal(await page.locator('.session-row-actions [data-action="delete"]').count(),0);assert.equal(await page.locator('.session-row-actions [data-action="rename"]').count(),1);assert.deepEqual(await page.evaluate(()=>protectedResult),{blocked:true,writes:0});await page.evaluate(()=>protectedPage.destroy());
 assert.deepEqual(errors,[]);
 // Exercise the public page mount with a long catalog and its docked footer.
 await page.evaluate(async()=>{
  document.body.innerHTML='<main id="main-panel"><section id="tab-agents" class="active sessions-view"><div id="agents-grid"><div class="agents-top-row">Sessions</div><div id="sessions-region"><div class="agent-row"><div class="agent-card-tabs-only">Sessions</div><div class="agent-detail-panel"><div class="agent-detail-content"><section id="sessions-section"></section></div></div></div></div></div></section></main>';
  const style=document.createElement('style');style.textContent='html,body{height:100%;margin:0}#main-panel{height:100%;overflow:auto}#agents-grid{display:flex;flex-direction:column} .agents-top-row{height:148px} .agent-card-tabs-only{min-height:39px} .sessions-footer{display:flex;min-height:32px} .agent-detail-content{height:520px;flex-direction:column}';
  document.head.append(style);
  const appStyle=document.createElement('link');appStyle.rel='stylesheet';appStyle.href='/ui/app.css';document.head.append(appStyle);
  const {mountSessionsPage}=await import('/ui/main-panel/agents/pages/sessions/page.js');
  const rows=Array.from({length:200},(_,i)=>({id:'long-'+i,title:'Long catalog '+i,agent_id:'one',status:'active',metadata_revision:0,created_at:1,last_active_at:200-i,message_count:0}));
  window.longPage=await mountSessionsPage(document.getElementById('sessions-section'),{isCurrent:()=>true,request:async()=>({sessions:rows,next_offset:null}),userId:'long-fixture',roster:[{id:'one',name:'One'}],getSessionId:()=>null,open:()=>{},changed:()=>{}});
 });
 await page.locator('#sessions-table-body tr').nth(199).waitFor({state:'attached'});
 for(const viewport of [{width:1200,height:720},{width:390,height:780}]){
  await page.setViewportSize(viewport);
  const layout=await page.evaluate(()=>{const table=document.getElementById('sessions-table-wrap'),footer=document.getElementById('sessions-footer-dock'),panel=document.getElementById('main-panel');return {client:table.clientHeight,scroll:table.scrollHeight,tableBottom:table.getBoundingClientRect().bottom,footerTop:footer.getBoundingClientRect().top,footerBottom:footer.getBoundingClientRect().bottom,panelBottom:panel.getBoundingClientRect().bottom,touch:getComputedStyle(table).touchAction,footerParent:footer.parentElement.id};});
  assert.ok(layout.scroll>layout.client);assert.ok(layout.client>100);assert.equal(layout.footerParent,'tab-agents');assert.ok(layout.tableBottom<=layout.footerTop+1);assert.ok(Math.abs(layout.footerBottom-layout.panelBottom)<=2);assert.equal(layout.touch,'auto');
  await page.locator('#sessions-table-wrap').hover();await page.mouse.wheel(0,10000);await page.waitForTimeout(150);
  assert.ok(await page.locator('#sessions-table-wrap').evaluate(e=>e.scrollTop>0));
  assert.ok(await page.locator('#sessions-footer-dock').isVisible());
  // Footer controls still operate from outside the renderer's host.
  await page.locator('#sessions-footer-dock [data-sessions-footer-gutter]').click();
 }
 await page.evaluate(()=>longPage.destroy());assert.equal(await page.locator('#sessions-footer-dock').count(),0);
 assert.deepEqual(errors,[]);
 console.log('PASS reference renderer: zero statistics, selection, confirmation, partial batch, stable retry receipt, restore, disabled permanent deletion, server search, narrow layout, teardown, long catalog scrolling and docked footer');
}finally{await browser?.close();server.close();}
