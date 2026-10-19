// Browser regressions for the real dropdown modules, component JSON and shared presentation.
const {test}=require('node:test'),assert=require('node:assert/strict');
const fs=require('node:fs'),path=require('node:path'),http=require('node:http');
const {chromium}=require(process.env.PLAYWRIGHT_MODULE||'playwright');
const UI=path.resolve(__dirname,'../../..'),shared=path.resolve(UI,'../../ui');
/** Serve real isolated source to a networkless browser with deterministic private-session fixtures. */
async function fixture(t,{mobile=false}={}) {
 const html='<!doctype html><meta name="viewport" content="width=device-width,initial-scale=1"><link rel="stylesheet" href="/shared/shared/css/design-system.css"><link rel="stylesheet" href="/shared/shared/css/app1.css"><link rel="stylesheet" href="/v3-ui/chat/chat.css"><style>body{margin:0}#chat-panel{width:100%;height:100vh}#chat-header{position:fixed;top:20px;left:12px;width:340px}#session-dropdown{width:320px;min-height:40px}#session-dropdown-trigger{display:flex;width:100%;align-items:center}</style><main id="chat-panel">'+fs.readFileSync(path.join(UI,'chat/chat.html'),'utf8')+'</main>';
 const server=http.createServer((req,res)=>{
  const u=new URL(req.url,'http://localhost').pathname;
  if(u==='/'){res.setHeader('content-type','text/html');return res.end(html);}
  const root=u.startsWith('/shared/')?shared:UI;
  const p=path.resolve(root,u.replace(/^\/(?:v3-ui|shared)\//,''));
  if(!p.startsWith(root+path.sep)){res.writeHead(403);return res.end();}
  fs.readFile(p,(err,b)=>{res.writeHead(err?404:200,{'content-type':p.endsWith('.js')?'text/javascript':p.endsWith('.css')?'text/css':p.endsWith('.json')?'application/json':'text/plain'});res.end(err?'missing':b);});
 });
 await new Promise(r=>server.listen(0,'127.0.0.1',r));
 const browser=await chromium.launch({headless:true,args:['--no-sandbox']});
 const context=await browser.newContext({viewport:mobile?{width:390,height:844}:{width:1280,height:900},hasTouch:mobile,isMobile:mobile});const page=await context.newPage();
 t.after(async()=>{await browser.close();await new Promise(r=>server.close(r));});
 await page.goto('http://127.0.0.1:'+server.address().port+'/');
 await page.evaluate(async()=>{
  const {mountSessionDropdown}=await import('/v3-ui/chat/controls/session-dropdown/controller.js');
  const {createSessionList}=await import('/v3-ui/chat/controls/session-dropdown/list.js');
  const cfg=await import('/v3-ui/chat/controls/session-dropdown/config.js');await cfg.loadSessionDropdownConfig();window.cfg=cfg;
  window.config=cfg.sessionDropdownConfig('chat_desktop');window.selectedCalls=[];window.writes=[];window.refreshes=0;
  const root=document.querySelector('#session-dropdown');
  window.items=Array.from({length:18},(_,i)=>({id:'s'+i,title:'Session '+i,agent_id:'shared_default',last_active_at:123,metadata_revision:1,status:'active'}));
  window.controller=mountSessionDropdown(root,{refresh:async()=>{refreshes++;list.update(items,window.fixtureSelected||'s0');},select:id=>selectedCalls.push(id),guard:fn=>fn,resetSearch:()=>list.reset()});
  window.list=createSessionList(root,{guard:fn=>fn,refresh:async()=>{refreshes++;},select:id=>selectedCalls.push(id),client:{sessions:async status=>({sessions:status==='active'?items:[{...items[0],id:'bin',status:'recycled'}]}),patch:async(id,body)=>{writes.push({id,body});return {session:{...items[0],title:body.title}};}},screenSettings:()=>{const d=document.createElement('details');d.className='display-settings';d.innerHTML='<summary>Screen display</summary>';return d;}});
  list.configure(config);list.update(items,'s0');
 });
 return page;
}
/** Await the controller's deferred refresh rather than inspecting its loading skeleton. */
async function openList(page){await page.locator('#session-dropdown-trigger').click();await page.locator('.session-dropdown-rows .session-row').first().waitFor();}
/** Apply a full profile to exercise live renderer options without modifying source files. */
async function configure(page,changes){await page.evaluate(changes=>{for(const [k,v]of Object.entries(changes))config[k]={...config[k],...v};list.configure(config);},changes);}
test('JSON owns trigger, rows, independent menu actions, optional More and footer visibility',async t=>{
 const p=await fixture(t);assert.equal(await p.locator('#session-dropdown-kebab').isVisible(),false);
 await p.locator('#session-dropdown-trigger').click({button:'right'});
 assert.equal(await p.locator('.session-row-actions button').count(),9);assert.equal(await p.locator('.display-settings').count(),1);assert.equal(await p.evaluate(()=>selectedCalls.length),0);
 await p.keyboard.press('Escape');assert.equal(await p.locator('.session-row-actions').count(),0);
 await configure(p,{menu:{pinning:false,auto_rename:false,open_in_widget:false,refresh:false,start_scratchpad:false,delete:false,agent_id:false,session_id:false,always_on_display:false},footer:{enabled:false},trigger:{agent_icon:false,session_name:false,status:false,row_actions:true}});
 assert.equal(await p.locator('#session-dropdown-icon').isVisible(),false);assert.equal(await p.locator('#session-dropdown-label').isVisible(),false);assert.equal(await p.locator('#session-dropdown-status').isVisible(),false);
 await p.locator('#session-dropdown-kebab').click();assert.deepEqual(await p.locator('.session-row-actions button').allTextContents(),[' Rename session']);
 await p.keyboard.press('Escape');await openList(p);assert.equal(await p.locator('.session-manage-row').count(),0);
 await configure(p,{menu:{default_rows:3,agent_icons:false,session_name:false,status:false,row_actions:true},footer:{enabled:true,search:false,recycle_bin:false,hidden_sessions:false,batch_delete:true}});
 assert.equal(await p.locator('.session-row-agent-icon').count(),1);assert.equal(await p.locator('.session-manage-row button').count(),1);assert.equal(await p.locator('.session-manage-search-input').count(),0);
 assert.equal(await p.locator('.session-row-kebab:visible').count(),19);
 const geometry=await p.locator('.session-dropdown-rows').evaluate(e=>({height:e.clientHeight,scroll:e.scrollHeight,three:[...e.children].slice(0,3).reduce((s,r)=>s+r.getBoundingClientRect().height,0)}));
 assert(geometry.scroll>geometry.height);assert(Math.abs(geometry.height-geometry.three)<2,JSON.stringify(geometry));
 await configure(p,{menu:{max_height:'160px'}});await p.waitForTimeout(420);assert(await p.locator('#session-dropdown-menu').evaluate(e=>e.getBoundingClientRect().height)<=162);assert(await p.locator('.session-manage-row').isVisible());
 await configure(p,{menu:{max_height:'min(66vh, 640px)'}});
 await configure(p,{trigger:{agent_icon:true,session_name:true,status:true,row_actions:false},menu:{session_name:true,row_actions:false}});
 await p.locator('.session-row[data-id="s3"]').click({button:'right'});await p.locator('.session-row-actions button').click();
 const input=p.locator('.session-row-title-input');await input.fill('Renamed by fixture');await input.press('Enter');
 assert.equal(await p.evaluate(()=>writes[0].body.title),'Renamed by fixture');assert.equal(await p.evaluate(()=>selectedCalls.length),0);
});
test('keyboard action menu, footer selections and hiding an active footer reset transient state',async t=>{
 const p=await fixture(t);await p.locator('#session-dropdown-trigger').focus();await p.keyboard.press('Shift+F10');await p.locator('.session-row-actions').waitFor();await p.keyboard.press('Escape');
 await openList(p);await p.getByRole('button',{name:'Enable checkbox mode',exact:true}).click();await p.locator('.session-row[data-id="s2"]').click();assert.equal(await p.evaluate(()=>selectedCalls.length),0);
 await configure(p,{footer:{enabled:false}});assert.equal(await p.locator('.session-row-checkbox').count(),0);
 await configure(p,{footer:{enabled:true}});await p.getByRole('button',{name:'Recycle bin',exact:true}).click();await p.locator('.session-row.recycled').waitFor();
 await configure(p,{menu:{restore:false,purge:false}});assert.equal(await p.locator('.session-row.recycled button').count(),0);
 await configure(p,{footer:{enabled:false}});assert.equal(await p.locator('.session-row.recycled').count(),0);
});
test('long press opens once, suppresses navigation, cancels on movement and teardown on mobile',async t=>{
 const p=await fixture(t,{mobile:true});await configure(p,{gestures:{long_press_ms:250}});
 const trigger=p.locator('#session-dropdown-trigger');
 await trigger.dispatchEvent('pointerdown',{pointerType:'touch',pointerId:1,isPrimary:true,clientX:20,clientY:30});await p.waitForTimeout(320);
 assert.equal(await p.locator('.session-row-actions').count(),1);
 await trigger.dispatchEvent('pointerup',{pointerType:'touch',pointerId:1});await trigger.dispatchEvent('click');assert.equal(await p.locator('#session-dropdown-menu').isVisible(),false);
 await p.keyboard.press('Escape');await p.waitForTimeout(1000);
 await trigger.dispatchEvent('pointerdown',{pointerType:'touch',pointerId:2,isPrimary:true,clientX:20,clientY:30});await p.locator('body').dispatchEvent('pointermove',{pointerType:'touch',pointerId:2,clientX:80,clientY:30});await p.waitForTimeout(320);assert.equal(await p.locator('.session-row-actions').count(),0);
 await openList(p);const row=p.locator('.session-row[data-id="s1"]');await row.dispatchEvent('pointerdown',{pointerType:'touch',pointerId:3,isPrimary:true,clientX:30,clientY:90});await p.waitForTimeout(320);assert.equal(await p.locator('.session-row-actions').count(),1);await row.dispatchEvent('click');assert.equal(await p.evaluate(()=>selectedCalls.length),0);
 await p.keyboard.press('Escape');await row.dispatchEvent('pointerdown',{pointerType:'touch',pointerId:4,isPrimary:true,clientX:30,clientY:90});await p.evaluate(()=>{list.destroy();controller.destroy();});await p.waitForTimeout(320);assert.equal(await p.locator('.session-row-actions').count(),0);
});
test('configuration deep-merges surface overlays without consulting legacy chat settings',async t=>{
 const p=await fixture(t);await p.route('**/session-dropdown.json',route=>route.fulfill({contentType:'application/json',body:JSON.stringify({common:{menu:{rename:true,delete:true},footer:{enabled:true}},mobile:{menu:{delete:false},footer:{enabled:false}}})}));
 const value=await p.evaluate(async()=>{await cfg.loadSessionDropdownConfig();return cfg.sessionDropdownConfig('chat_mobile');});assert.deepEqual(value,{menu:{rename:true,delete:false},footer:{enabled:false}});
 await p.route('**/session-dropdown.json',route=>route.fulfill({status:503,body:'unavailable'}));assert.deepEqual(await p.evaluate(async()=>{await cfg.loadSessionDropdownConfig();return cfg.sessionDropdownConfig('chat_mobile');}),value);
});

test('renamed Main Chat keeps rename and hides delete across trigger, menu and batch selection',async t=>{
 const p=await fixture(t);await p.evaluate(()=>{items[0]={...items[0],title:'Renamed coordinator',main_chat:true,can_delete:false};list.update(items,'s0');});
 await configure(p,{trigger:{delete:true,row_actions:true},menu:{rename:true,delete:true,purge:true,row_actions:true}});
 assert.equal(await p.locator('#session-dropdown-delete').isVisible(),false);
 await p.locator('#session-dropdown-kebab').click();assert.equal(await p.getByRole('button',{name:'Recycle session',exact:true}).count(),0);assert.equal(await p.getByRole('button',{name:'Rename session',exact:true}).count(),1);await p.keyboard.press('Escape');
 await openList(p);await p.getByRole('button',{name:'Enable checkbox mode',exact:true}).click();assert.equal(await p.locator('.session-row[data-id="s0"] .session-row-checkbox').isDisabled(),true);await p.locator('.session-row[data-id="s0"]').click();assert.equal(await p.evaluate(()=>writes.length),0);
});

for (const mobile of [false,true]) test(`Main Chat outranks pins and keeps a wavy halo after rename and selection (${mobile?'mobile':'desktop'})`,async t=>{
 const p=await fixture(t,{mobile});await p.evaluate(()=>{
  window.fixtureSelected='main';
  items=[{...items[0],id:'pin',pinned:true,last_active_at:900},{...items[1],id:'recent',title:'Main Chat',last_active_at:999},{...items[2],id:'main',title:'Renamed coordinator',main_chat:true,last_active_at:1}];list.update(items,'main');
 });await openList(p);
 assert.deepEqual(await p.locator('.session-dropdown-rows .session-row').evaluateAll(rows=>rows.map(r=>r.dataset.id)),['main','pin','recent']);
 for(const light of [false,true]){
  await p.evaluate(light=>document.body.classList.toggle('light-mode',light),light);
  const ring=await p.locator('[data-id="main"] .session-row-agent-icon').evaluate(n=>{const s=getComputedStyle(n,'::after');return {mask:s.maskImage||s.webkitMaskImage,width:s.width,height:s.height,pointer:s.pointerEvents,background:s.backgroundColor};});
  assert.match(ring.mask,/data:image\/svg\+xml/);assert.equal(ring.pointer,'none');assert.notEqual(ring.background,'rgba(0, 0, 0, 0)');assert.ok(parseFloat(ring.width)>18);assert.ok(parseFloat(ring.height)>18);
  assert.equal(await p.locator('[data-id="pin"] .session-row-agent-warning').count(),1);
  assert.equal(await p.locator('#session-dropdown-icon.session-row-agent-main').count(),1);
 }
 await p.evaluate(()=>{items=items.map(item=>item.id==='main'?{...item,pinned:true,title:'Renamed again'}:item);list.update(items,'main');});
 assert.equal(await p.locator('[data-id="main"] .session-row-agent-warning').count(),0);
 assert.equal(await p.locator('[data-id="main"] .session-row-agent-main').count(),1);
 await p.evaluate(()=>list.update(items,'pin'));assert.equal(await p.locator('#session-dropdown-icon.session-row-agent-main').count(),0);assert.equal(await p.locator('#session-dropdown-icon.session-row-agent-warning').count(),1);
});
