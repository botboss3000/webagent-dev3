// Browser regressions exercise the actual V3 config, pill renderer and viewport script.
const {test}=require('node:test'),assert=require('node:assert/strict');
const fs=require('node:fs'),path=require('node:path'),http=require('node:http'),crypto=require('node:crypto');
const {chromium}=require(process.env.PLAYWRIGHT_MODULE||'playwright');
const ROOT=process.env.SOURCE_ROOT||path.resolve(__dirname,'../../..');
const UI=path.join(ROOT,'ui');
const layout=(position='center',carousel=false)=>({rows:[{[position]:carousel?{carousel:['textarea']}:{controls:['textarea']}},{left:{controls:['attach']},center:{controls:['stats','stop','continue']},right:{controls:['mic_send']}}],controls:{textarea:{font_size:'16px',min_height:'40px'},attach:{enabled:true},stop:{enabled:true},continue:{enabled:true}}});
const config=pill=>({chat_common:{chat_header:{rows:[]},active_footer:{chat_pill:pill},idle_footer:{chat_pill:pill}}});
async function fixture(t,{fallback=false,widget=false}={}){
 const files=['chat/controls/chat-config.js','chat/controls/chat-pill-config.js','boot/viewport.js','chat/chat.html','chat/controls/footer-handle.js'];
 const hashes=()=>Object.fromEntries(files.map(p=>[p,crypto.createHash('sha256').update(fs.readFileSync(path.join(UI,p))).digest('hex')]));
 const before=hashes();
 const html='<!doctype html><meta name="viewport" content="width=device-width,initial-scale=1,interactive-widget=resizes-content"><style>body{margin:0}#chat-panel{width:100%;height:var(--app-height,100vh)}textarea{font-size:16px}</style><main id="chat-panel">'+fs.readFileSync(path.join(UI,'chat/chat.html'),'utf8')+'</main>';
 const server=http.createServer((req,res)=>{let rel=new URL(req.url,'http://localhost').pathname;if(rel==='/'){res.setHeader('content-type','text/html');return res.end(html);}rel=rel.replace(/^\/v3-ui\//,'');const p=path.resolve(UI,rel);if(!p.startsWith(UI+path.sep)){res.writeHead(403);return res.end();}fs.readFile(p,(e,b)=>{res.writeHead(e?404:200,{'content-type':p.endsWith('.js')?'text/javascript':p.endsWith('.css')?'text/css':'text/plain'});res.end(e?'missing':b);});});
 await new Promise(r=>server.listen(0,'127.0.0.1',r));
 const browser=await chromium.launch({headless:true,...(process.env.BROWSER_EXECUTABLE?{executablePath:process.env.BROWSER_EXECUTABLE}:{}),args:['--no-sandbox']});
 const context=await browser.newContext({viewport:{width:390,height:844},isMobile:true,hasTouch:true});const page=await context.newPage();
 t.after(async()=>{await browser.close();await new Promise(r=>server.close(r));assert.deepEqual(hashes(),before,'unchanged browser fixture inputs');});
 await page.addInitScript(({fallback,widget})=>{
  if(fallback)Element.prototype.moveBefore=undefined;
  window.__CHAT_PORTAL__=widget;
  window.__observers=[];const Native=ResizeObserver;
  window.ResizeObserver=class extends Native{constructor(fn){super(fn);window.__observers.push(this);}disconnect(){this.disconnected=true;super.disconnect();}};
 },{fallback,widget});
 await page.goto('http://127.0.0.1:'+server.address().port+'/');
 await page.evaluate(async cfg=>{window.initialPanelHtml=document.querySelector('#chat-panel').innerHTML;window.api=await import('/v3-ui/chat/controls/chat-config.js');window.pillApi=await import('/v3-ui/chat/controls/chat-pill-config.js');await window.api.initChatConfig({config:cfg});window.input=document.querySelector('#chat-input');window.rows=document.querySelector('.chat-pill-layout-rows');window.inputParent=input.parentNode;window.events={blur:0,focus:0};input.addEventListener('blur',()=>events.blur++);input.addEventListener('focus',()=>events.focus++);},config(layout()));
 return page;
}
async function settle(page){await page.evaluate(()=>new Promise(r=>requestAnimationFrame(()=>requestAnimationFrame(r))));}
async function focusDraft(page){await page.locator('#chat-input').fill('retained draft');await page.evaluate(()=>{input.setSelectionRange(2,7,'backward');events.blur=events.focus=0;window.inputParent=input.parentNode;});}
async function stable(page){return page.evaluate(()=>({same:input===document.querySelector('#chat-input'),rows:rows===document.querySelector('.chat-pill-layout-rows'),parent:inputParent===input.parentNode,focused:document.activeElement===input,value:input.value,start:input.selectionStart,end:input.selectionEnd,direction:input.selectionDirection,...events}));}
test('height-only resizes preserve DOM, draft, selection and focus without blur/refocus',async t=>{
 const p=await fixture(t);await focusDraft(p);
 for(const height of [550,700,844,560,844]){await p.setViewportSize({width:390,height});await p.evaluate(()=>{for(let i=0;i<8;i++)window.dispatchEvent(new Event('resize'));});await settle(p);}
 assert.deepEqual(await stable(p),{same:true,rows:true,parent:true,focused:true,value:'retained draft',start:2,end:7,direction:'backward',blur:0,focus:0});
});
test('styling-only config and control state updates keep rows and editor connected',async t=>{
 const p=await fixture(t);await focusDraft(p);const next=layout();next.controls.textarea.font_size='19px';
 await p.evaluate(async cfg=>{await api.initChatConfig({config:cfg});document.querySelector('#chat-stop-btn').dataset.run='run-1';api.applyChatConfig();},config(next));
 assert.equal((await stable(p)).parent,true);assert.equal((await stable(p)).blur,0);
 assert.equal(await p.locator('#chat-input').evaluate(e=>e.style.getPropertyValue('--chat-pill-font-size')),'19px');
 assert.equal(await p.locator('#chat-stop-btn').evaluate(e=>e.hidden),false);
 await p.evaluate(()=>{delete document.querySelector('#chat-stop-btn').dataset.run;input.value='';input.dispatchEvent(new Event('input',{bubbles:true}));document.querySelector('#chat-input-row').dataset.attachmentCount='1';api.applyChatConfig();});
 assert.equal(await p.locator('#chat-send').evaluate(e=>e.style.display),'');assert.equal(await p.locator('#chat-continue-btn').evaluate(e=>e.hidden),true);
});
test('supported state-preserving movement keeps focus on genuine structural changes',async t=>{
 const p=await fixture(t);if(!await p.evaluate(()=>typeof Element.prototype.moveBefore==='function'))return t.skip('browser lacks moveBefore; fallback is covered separately');
 await focusDraft(p);await p.evaluate(async cfg=>api.initChatConfig({config:cfg}),config(layout('left',true)));await settle(p);
 const s=await stable(p);assert.equal(s.parent,false);assert.equal(s.focused,true);assert.equal(s.blur,0);assert.equal(s.focus,0);assert.equal(s.start,2);assert.equal(s.end,7);
 assert.equal(await p.locator('#chat-input').evaluate(e=>e.parentNode.className),'chat-pill-zone-carousel-strip');
});
test('fallback applies only the latest pending structural change after natural blur',async t=>{
 const p=await fixture(t,{fallback:true});await focusDraft(p);
 await p.evaluate(async configs=>{for(const cfg of configs)await api.initChatConfig({config:cfg});},[config(layout('left',true)),config(layout('right'))]);
 assert.equal((await stable(p)).parent,true);assert.equal((await stable(p)).focused,true);assert.equal((await stable(p)).blur,0);
 await p.locator('#chat-input').blur();await settle(p);
 assert.equal(await p.locator('#chat-input').evaluate(e=>e.parentNode.classList.contains('chat-pill-layout-zone-right')),true);
 assert.equal(await p.evaluate(()=>events.focus),0);assert.equal(await p.locator('#chat-input').inputValue(),'retained draft');
});
test('carousel observers are reused and disconnected when the zone is removed',async t=>{
 const p=await fixture(t);await p.evaluate(async cfg=>api.initChatConfig({config:cfg}),config(layout('center',true)));await settle(p);
 const count=await p.evaluate(()=>__observers.length);await p.evaluate(async cfg=>api.initChatConfig({config:cfg}),config(layout('center',true)));await settle(p);
 assert.equal(await p.evaluate(()=>__observers.length),count);
 await p.evaluate(async cfg=>api.initChatConfig({config:cfg}),config(layout()));await settle(p);assert.equal(await p.evaluate(()=>__observers.filter(o=>o.disconnected).length>=1),true);
});
test('remount cancels pending layout and observes the new panel',async t=>{
 const p=await fixture(t,{fallback:true});await focusDraft(p);await p.evaluate(async cfg=>api.initChatConfig({config:cfg}),config(layout('left')));
 await p.evaluate(async cfg=>{const old=document.querySelector('#chat-panel');window.oldInput=input;window.oldParent=input.parentNode;const fresh=document.createElement('main');fresh.id='chat-panel';fresh.innerHTML=initialPanelHtml;old.replaceWith(fresh);await api.initChatConfig({config:cfg,root:fresh});window.input=document.querySelector('#chat-input');},config(layout()));await settle(p);
 assert.equal(await p.evaluate(()=>oldInput.parentNode===oldParent),true);assert.equal(await p.evaluate(()=>document.activeElement===input),false);
 assert.equal(await p.evaluate(()=>__observers.some(o=>o.disconnected)),true);
});
test('desktop and widget profile changes retain rows when their structure is identical',async t=>{
 const p=await fixture(t,{widget:true});await focusDraft(p);await p.setViewportSize({width:1200,height:800});await settle(p);assert.equal((await stable(p)).parent,true);
 await p.evaluate(()=>{window.__CHAT_PORTAL__=false;api.refreshChatSurface();});await settle(p);assert.equal((await stable(p)).rows,true);assert.equal((await stable(p)).blur,0);
});
test('dismissal survives programmatic focus and viewport jitter until editor interaction',async t=>{
 const p=await fixture(t);await p.evaluate(async()=>{window.vh=500;window.ih=844;Object.defineProperty(window,'innerHeight',{configurable:true,get:()=>ih});Object.defineProperty(window.visualViewport,'height',{configurable:true,get:()=>vh});await new Promise((ok,no)=>{const s=document.createElement('script');s.src='/v3-ui/boot/viewport.js';s.onload=ok;s.onerror=no;document.head.append(s);});input.focus();});
 assert.equal(await p.evaluate(()=>document.body.classList.contains('kb-open')),true);
 await p.evaluate(()=>{vh=844;visualViewport.dispatchEvent(new Event('resize'));vh=500;visualViewport.dispatchEvent(new Event('resize'));document.dispatchEvent(new FocusEvent('focusin',{bubbles:true}));input.dispatchEvent(new FocusEvent('focusin',{bubbles:true}));});
 assert.equal(await p.evaluate(()=>document.body.classList.contains('kb-open')),false);
 await p.locator('#chat-input').tap();
 assert.equal(await p.evaluate(()=>document.body.classList.contains('kb-open')),true);
});

test('same-zone reorder also defers the focused editor on fallback browsers',async t=>{
 const p=await fixture(t,{fallback:true});await focusDraft(p);const next=layout();next.rows[0].center.controls=['stats','textarea','attach'];next.rows[1].left.controls=[];next.rows[1].center.controls=['stop','continue'];
 await p.evaluate(async cfg=>api.initChatConfig({config:cfg}),config(next));assert.equal((await stable(p)).blur,0);assert.equal((await stable(p)).parent,true);
 await p.locator('#chat-input').blur();await settle(p);assert.equal(await p.locator('#chat-input').evaluate(e=>e.nextSibling.id),'chat-attach-btn');assert.equal(await p.evaluate(()=>events.focus),0);
});
test('empty-editor active/idle transitions retain DOM across themes and width changes',async t=>{
 const p=await fixture(t);for(const theme of ['light','dark']){
  await p.emulateMedia({colorScheme:theme});await p.locator('#chat-input').focus();await p.evaluate(()=>{input.value='';input.dispatchEvent(new Event('input',{bubbles:true}));events.blur=events.focus=0;window.inputParent=input.parentNode;});
  await p.setViewportSize({width:844,height:390});await settle(p);const s=await stable(p);assert.equal(s.focused,true);assert.equal(s.rows,true);assert.equal(s.blur,0);
  await p.locator('#chat-input').blur();await settle(p);assert.equal(await p.locator('#chat-input-area').evaluate(e=>e.classList.contains('footer-mode-idle')),true);
  await p.setViewportSize({width:390,height:844});await settle(p);
 }
});


/** Pin the real composer to the fixture panel bottom and load the actual viewport owner. */
async function motionFixture(t,settings={enabled:true,duration_ms:140}) {
 const p=await fixture(t);await p.evaluate(async()=>{document.querySelector('#chat-panel').style.position='relative';document.querySelector('#chat-input-area').style.cssText='position:absolute;bottom:0';window.motionVh=844;window.motionScale=1;Object.defineProperty(visualViewport,'height',{configurable:true,get:()=>motionVh});Object.defineProperty(visualViewport,'scale',{configurable:true,get:()=>motionScale});await new Promise((ok,no)=>{const s=document.createElement('script');s.src='/v3-ui/boot/viewport.js';s.onload=ok;s.onerror=no;document.head.append(s);});});
 const cfg=config(layout());cfg.chat_common.composer={keyboard_motion:settings};await p.evaluate(async cfg=>api.initChatConfig({config:cfg}),cfg);await focusDraft(p);return p;
}
test('keyboard movement starts at the rendered position while app height changes immediately',async t=>{
 const p=await motionFixture(t);
 for(const height of [500,844]){
  const state=await p.evaluate(height=>{const row=document.querySelector('#chat-input-row'),before=row.getBoundingClientRect().top;motionVh=height;visualViewport.dispatchEvent(new Event('resize'));const a=row.getAnimations()[0];a?.pause();return{before,after:row.getBoundingClientRect().top,height:document.documentElement.style.getPropertyValue('--app-height'),duration:a?.effect.getTiming().duration,opacity:getComputedStyle(row).opacity,frames:a?.effect.getKeyframes()};},height);
  assert.equal(state.height,height+'px');assert.equal(state.duration,140);assert(Math.abs(state.before-state.after)<.5,'visible composer starts where it was');assert.equal(state.opacity,'1');assert(state.frames.every(f=>f.opacity===undefined));
  await p.evaluate(()=>{for(const node of document.querySelectorAll('#chat-input-row,#chat-footer-row,#chat-footer-handle'))for(const a of node.getAnimations())a.finish();});await settle(p);
 }
 const s=await stable(p);assert.equal(s.focused,true);assert.equal(s.blur,0);assert.equal(s.focus,0);assert.equal(s.value,'retained draft');assert.equal(s.start,2);assert.equal(s.end,7);
});
test('interrupted keyboard motion retargets from the current position and preserves CSS transforms',async t=>{
 const p=await motionFixture(t,{enabled:true,duration_ms:160});
 const result=await p.evaluate(()=>{const row=document.querySelector('#chat-input-row');row.style.transform='scale(1.05)';motionVh=500;visualViewport.dispatchEvent(new Event('resize'));const first=row.getAnimations()[0];first.pause();first.currentTime=80;const before=row.getBoundingClientRect().top;motionVh=440;visualViewport.dispatchEvent(new Event('resize'));const next=row.getAnimations()[0];next?.pause();return{before,after:row.getBoundingClientRect().top,count:row.getAnimations().length,duration:next?.effect.getTiming().duration,transform:row.style.transform,height:document.documentElement.style.getPropertyValue('--app-height')};});
 assert(Math.abs(result.before-result.after)<.5);assert.equal(result.count,1);assert.equal(result.duration,160);assert.equal(result.transform,'scale(1.05)');assert.equal(result.height,'440px');assert.equal((await stable(p)).focused,true);
});
test('motion completes, and reduced motion or disabling cancels translations immediately',async t=>{
 const p=await motionFixture(t);await p.evaluate(()=>{motionVh=500;visualViewport.dispatchEvent(new Event('resize'));});await p.waitForFunction(()=>document.querySelector('#chat-input-row').getAnimations().length===0);assert.equal(await p.locator('#chat-input-row').evaluate(e=>getComputedStyle(e).translate),'none');
 await p.evaluate(()=>{motionVh=844;visualViewport.dispatchEvent(new Event('resize'));for(const a of document.querySelector('#chat-input-row').getAnimations())a.pause();});await p.emulateMedia({reducedMotion:'reduce'});await p.waitForFunction(()=>document.querySelector('#chat-input-row').getAnimations().length===0);assert.equal(await p.locator('#chat-input-row').evaluate(e=>e.getAnimations().length),0);
 await p.emulateMedia({reducedMotion:'no-preference'});await p.evaluate(()=>{motionVh=500;visualViewport.dispatchEvent(new Event('resize'));window.__chatKeyboardMotionConfigure({enabled:false});});assert.equal(await p.locator('#chat-input-row').evaluate(e=>e.getAnimations().length),0);assert.equal((await stable(p)).focused,true);
});
test('desktop, widget, zoom, chrome jitter and zero duration bypass keyboard motion',async t=>{
 const p=await motionFixture(t);
 for(const mode of ['jitter','zoom','widget','zero','desktop']){
  const count=await p.evaluate(mode=>{window.__chatKeyboardMotionConfigure({enabled:true,duration_ms:mode==='zero'?0:140});motionScale=mode==='zoom'?1.5:1;window.__CHAT_PORTAL__=mode==='widget';motionVh=mode==='jitter'?810:500;visualViewport.dispatchEvent(new Event('resize'));return document.querySelector('#chat-input-row').getAnimations().length;},mode);
  if(mode==='desktop'){await p.setViewportSize({width:1200,height:844});await p.evaluate(()=>{motionVh=844;visualViewport.dispatchEvent(new Event('resize'));});}
  else assert.equal(count,0,mode);
 }
 assert.equal(await p.locator('#chat-input-row').evaluate(e=>e.getAnimations().length),0);
});

test('live configuration updates existing automatic bubble corners without navigation or losing drafts',async t=>{
 const p=await fixture(t);await p.addStyleTag({path:path.join(UI,'chat/chat.css')});
 let live=config(layout()),bad=false,requests=0;live.chat_common.live_reload={enabled:true,interval_ms:1000};
 live.chat_common.message_bubbles={font_size_px:17,user:{radius_px:'auto',padding_top_px:8,padding_bottom_px:4,padding_left_px:14,padding_right_px:14,line_height:1.5,border:'1px solid blue'},agent:{radius_px:'auto',padding_top_px:8,padding_bottom_px:4,padding_left_px:4,padding_right_px:14,line_height:1.5}};
 await p.route('**/api/v3/auth/ui-config',route=>{requests++;return route.fulfill(bad?{status:503,body:'unavailable'}:{json:{chat_ui:live}});});
 await p.evaluate(()=>{
  for(const role of ['user','agent']){
   const row=document.createElement('div');row.className='message-row '+role;
   const bubble=document.createElement('article');bubble.className='chat-bubble v3-config-bubble '+role;bubble.id='geometry-'+role;
   bubble.style.cssText='display:block;width:max-content';const body=document.createElement('div');body.className='bubble-body';body.textContent='A short message';bubble.append(body);row.append(bubble);document.getElementById('chat-messages-inner').append(row);
  }
 });
 await focusDraft(p);let navigations=0;p.on('framenavigated',()=>navigations++);
 await p.evaluate(()=>api.initChatConfig());
 await p.waitForFunction(()=>Number.parseFloat(getComputedStyle(document.getElementById('geometry-user')).borderTopLeftRadius)>19);
 const metrics=()=>p.evaluate(()=>Object.fromEntries(['user','agent'].map(role=>{const e=document.getElementById('geometry-'+role),s=getComputedStyle(e);return [role,{height:e.getBoundingClientRect().height,radius:Number.parseFloat(s.borderTopLeftRadius),bottomLeft:s.borderBottomLeftRadius,font:s.fontSize}];})));
 let m=await metrics();assert.ok(Math.abs(m.user.radius-m.user.height/2)<0.1);assert.ok(Math.abs(m.agent.radius-m.agent.height/2)<0.1);
 live=JSON.parse(JSON.stringify(live));live.chat_common.message_bubbles.font_size_px=24;
 Object.assign(live.chat_common.message_bubbles.user,{padding_top_px:10,padding_bottom_px:6,border:'2px solid blue',radius_bottom_left_px:3});
 Object.assign(live.chat_common.message_bubbles.agent,{padding_top_px:5,padding_bottom_px:7,line_height:1.25});
 await p.waitForFunction(()=>getComputedStyle(document.getElementById('geometry-user')).fontSize==='24px'&&Number.parseFloat(getComputedStyle(document.getElementById('geometry-user')).borderTopLeftRadius)===28);
 m=await metrics();assert.equal(m.user.radius,28);assert.equal(m.user.bottomLeft,'3px');assert.equal(m.agent.radius,21);assert.ok(Math.abs(m.user.radius-m.user.height/2)<0.1);assert.ok(Math.abs(m.agent.radius-m.agent.height/2)<0.1);
 const state=await stable(p);assert.equal(state.same,true);assert.equal(state.parent,true);assert.equal(state.focused,true);assert.equal(state.value,'retained draft');assert.equal(state.start,2);assert.equal(state.end,7);assert.equal(state.blur,0);assert.equal(navigations,0);
 await p.evaluate(()=>{const e=document.getElementById('geometry-user');e.style.maxWidth='140px';e.querySelector('.bubble-body').textContent='A long wrapped message '.repeat(8);});
 m=await metrics();assert.ok(m.user.height>56);assert.equal(m.user.radius,28);
 const failed=p.waitForResponse(r=>r.url().includes('/api/v3/auth/ui-config')&&r.status()===503);bad=true;await failed;await settle(p);assert.equal((await metrics()).user.radius,28);
 bad=false;live=JSON.parse(JSON.stringify(live));live.chat_common.message_bubbles.user.radius_px=6;live.chat_common.message_bubbles.user.radius_bottom_left_px=null;
 await p.waitForFunction(()=>getComputedStyle(document.getElementById('geometry-user')).borderTopLeftRadius==='6px');assert.equal((await metrics()).user.bottomLeft,'6px');assert.equal(navigations,0);assert.ok(requests>=4);
});

/** Exercise configured fill against real shared important rules, including clears and live adoption. */
test('JSON-only bubble backgrounds cover absent transparent solid alpha themes profiles and live clearing',async t=>{
 const p=await fixture(t);
 const shared=[path.join(ROOT,'build-service/shared-ui/ui/shared/css/index.css'),path.resolve(ROOT,'../ui/shared/css/index.css')].find(fs.existsSync);
 assert.ok(shared,'real shared cascade input exists');
 await p.addStyleTag({path:path.join(UI,'chat/chat.css')});await p.addStyleTag({path:shared});
 await p.evaluate(()=>{
  document.documentElement.style.setProperty('--bg-elev','rgb(200, 180, 160)');
  document.documentElement.style.setProperty('--user-bubble-bg','red');document.documentElement.style.setProperty('--agent-bubble-bg','blue');
  for(const role of ['user','agent']){
   const row=document.createElement('div');row.className='message-row '+role;
   const e=document.createElement('article');e.id='paint-'+role;e.className='chat-bubble v3-config-bubble '+role;e.innerHTML='<div class="bubble-body">Configured bubble</div>';row.append(e);document.getElementById('chat-messages-inner').append(row);
  }
  window.paintNodes=['user','agent'].map(role=>document.getElementById('paint-'+role));
 });
 const read=()=>p.evaluate(()=>paintNodes.map(e=>{const s=getComputedStyle(e);return {bg:s.backgroundColor,image:s.backgroundImage,filter:s.backdropFilter,opacity:s.opacity,same:e===document.getElementById(e.id)};}));
 const cases=[
  ['no message config',undefined,['rgba(0, 0, 0, 0)','rgba(0, 0, 0, 0)']],
  ['no role config',{},['rgba(0, 0, 0, 0)','rgba(0, 0, 0, 0)']],
  ...[null,'','   ',0,false,{},[],'url(example.png)','not-a-color','transparent'].map(v=>['clear '+v,{user:{bg_dark:v,bg_light:v},agent:{bg_dark:v,bg_light:v}},['rgba(0, 0, 0, 0)','rgba(0, 0, 0, 0)']]),
  ['solid independent',{user:{bg_dark:'#123456',bg_light:'#abcdef'},agent:{bg_dark:'red',bg_light:'blue'}},null],
  ['alpha independent',{user:{bg_dark:'rgba(18, 52, 86, 0.4)',bg_light:'#12345680'},agent:{bg_dark:'#abcdef00',bg_light:'rgba(10, 20, 30, 0.75)'}},null],
  ['one theme only',{user:{bg_dark:'red'},agent:{bg_light:'blue'}},null],
  ['clear prior colors',{user:{},agent:{}},['rgba(0, 0, 0, 0)','rgba(0, 0, 0, 0)']]
 ];
 for(const surface of ['desktop','mobile','widget']){
  await p.setViewportSize({width:surface==='mobile'?390:1200,height:844});
  await p.evaluate(surface=>{window.__CHAT_PORTAL__=surface==='widget';document.getElementById('chat-panel').style.width=surface==='mobile'?'390px':'1100px';},surface);
  for(const light of [false,true]){
   await p.evaluate(light=>document.body.classList.toggle('light-mode',light),light);
   for(const [name,bubbles,constant]of cases){
    const cfg=config(layout());cfg.chat_common.message_bubbles=bubbles;
    await p.evaluate(async cfg=>api.initChatConfig({config:cfg}),cfg);await settle(p);
    const expected=constant|| (name==='solid independent'?(light?['rgb(171, 205, 239)','rgb(0, 0, 255)']:['rgb(18, 52, 86)','rgb(255, 0, 0)']):name==='alpha independent'?(light?['rgba(18, 52, 86, 0.5)','rgba(10, 20, 30, 0.75)']:['rgba(18, 52, 86, 0.4)','rgba(171, 205, 239, 0)']):(light?['rgba(0, 0, 0, 0)','rgb(0, 0, 255)']:['rgb(255, 0, 0)','rgba(0, 0, 0, 0)']));
    const actual=await read();assert.deepEqual(actual.map(x=>x.bg),expected,surface+' '+light+' '+name);
    assert.ok(actual.every(x=>x.image==='none'&&x.filter==='none'&&x.opacity==='1'&&x.same),JSON.stringify({surface,light,name,actual}));
   }
   const cfg=config(layout());cfg.chat_common.message_bubbles={user:{bg_dark:'red',bg_light:'red'},agent:{bg_dark:'blue',bg_light:'blue'}};
   cfg['chat_'+surface]={message_bubbles:{user:{bg_dark:'transparent',bg_light:'transparent'}}};
   await p.evaluate(async cfg=>api.initChatConfig({config:cfg}),cfg);assert.deepEqual((await read()).map(x=>x.bg),['rgba(0, 0, 0, 0)','rgb(0, 0, 255)'],'surface overlay is role-local');
  }
 }
 let live=config(layout());live.chat_common.live_reload={enabled:true,interval_ms:1000};live.chat_common.message_bubbles={user:{bg_dark:'red',bg_light:'red'},agent:{bg_dark:'blue',bg_light:'blue'}};
 await p.route('**/api/v3/auth/ui-config',route=>route.fulfill({json:{chat_ui:live}}));
 await p.evaluate(()=>api.initChatConfig());assert.deepEqual((await read()).map(x=>x.bg),['rgb(255, 0, 0)','rgb(0, 0, 255)']);
 live=JSON.parse(JSON.stringify(live));delete live.chat_common.message_bubbles;
 await p.waitForFunction(()=>getComputedStyle(document.getElementById('paint-user')).backgroundColor==='rgba(0, 0, 0, 0)'&&getComputedStyle(document.getElementById('paint-agent')).backgroundColor==='rgba(0, 0, 0, 0)');
 assert.ok((await read()).every(x=>x.same));
});


test('OS dismissal makes empty composer idle, preserves active drafts and reactivates on trusted tap',async t=>{
 const p=await fixture(t);await p.evaluate(async()=>{window.vh=844;Object.defineProperty(visualViewport,'height',{configurable:true,get:()=>vh});await new Promise((ok,no)=>{const s=document.createElement('script');s.src='/v3-ui/boot/viewport.js';s.onload=ok;s.onerror=no;document.head.append(s);});});
 for(const draft of ['', 'retained draft']){
  await p.locator('#chat-input').fill(draft);await p.locator('#chat-input').tap();await p.evaluate(()=>{vh=500;visualViewport.dispatchEvent(new Event('resize'));events.blur=events.focus=0;});
  assert.equal(await p.locator('#chat-input-area').evaluate(e=>e.classList.contains('footer-mode-active')),true);
  await p.evaluate(()=>{vh=844;visualViewport.dispatchEvent(new Event('resize'));});assert.equal(await p.locator('#chat-input-area').evaluate(e=>e.classList.contains('footer-mode-idle')),draft==='');
  const state=await stable(p);assert.equal(state.focused,true);assert.equal(state.blur,0);assert.equal(state.focus,0);assert.equal(state.value,draft);
  await p.evaluate(()=>{vh=500;visualViewport.dispatchEvent(new Event('resize'));input.focus();input.dispatchEvent(new FocusEvent('focusin',{bubbles:true}));});assert.equal(await p.locator('#chat-input-area').evaluate(e=>e.classList.contains('footer-mode-idle')),draft==='');assert.equal(await p.evaluate(()=>document.body.classList.contains('kb-open')),false);
  await p.locator('#chat-input').tap();assert.equal(await p.locator('#chat-input-area').evaluate(e=>e.classList.contains('footer-mode-active')),true);
  await p.locator('#chat-input').blur();await p.evaluate(()=>{vh=844;visualViewport.dispatchEvent(new Event('resize'));});
 }
});
