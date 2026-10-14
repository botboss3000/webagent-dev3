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
