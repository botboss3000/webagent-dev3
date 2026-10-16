// Browser regression covers independent triggers, completion, private authority and session teardown.
const {test}=require('node:test'),assert=require('node:assert/strict');
const fs=require('node:fs'),path=require('node:path'),http=require('node:http'),crypto=require('node:crypto');
const {chromium}=require(process.env.PLAYWRIGHT_MODULE||'playwright');
const ROOT=process.env.SOURCE_ROOT||path.resolve(__dirname,'../../..');
/** Serve the actual modules with a bounded synthetic journal, never application state. */
async function fixture(t,{mobile=false,dark=false,reduced=false}={}) {
 const inputs=[];
 // Capture the complete local UI dependency tree so test results reject concurrent changes.
 function collect(dir){for(const item of fs.readdirSync(dir,{withFileTypes:true})){const p=path.join(dir,item.name);if(item.isDirectory())collect(p);else if(/\.(js|css|json|cjs)$/.test(p))inputs.push(p);}}
 collect(path.join(ROOT,'ui'));
 const hashes=()=>inputs.map(p=>[p,crypto.createHash('sha256').update(fs.readFileSync(p)).digest('hex')]);const before=hashes();
 const server=http.createServer((req,res)=>{
  const url=new URL(req.url,'http://localhost');
  if(url.pathname==='/'){res.setHeader('content-type','text/html');return res.end('<!doctype html><meta name="viewport" content="width=device-width,initial-scale=1"><style>:root{--fg-3:#777;--fg-2:#444;--accent:#369;--bg-tint:#eee;--bg-2:#fff;--border-width:1px;--border:#aaa;--r-sm:6px}body{background:'+(dark?'#151515':'white')+'}</style><main id="messages"><div data-public-id="q">Question</div></main>');}
  const file=path.resolve(ROOT,'ui',url.pathname.replace(/^\/v3-ui\//,''));
  if(!file.startsWith(path.join(ROOT,'ui')+path.sep)){res.writeHead(403);return res.end();}
  fs.readFile(file,(e,b)=>{res.writeHead(e?404:200,{'content-type':file.endsWith('.js')?'text/javascript':file.endsWith('.css')?'text/css':'text/plain'});res.end(e?'missing':b);});
 });
 await new Promise(r=>server.listen(0,'127.0.0.1',r));
 const browser=await chromium.launch({headless:true,...(process.env.BROWSER_EXECUTABLE?{executablePath:process.env.BROWSER_EXECUTABLE}:{}),args:['--no-sandbox']});
 const context=await browser.newContext({viewport:mobile?{width:390,height:844}:{width:1100,height:800},isMobile:mobile,hasTouch:mobile,reducedMotion:reduced?'reduce':'no-preference'});
 const page=await context.newPage();const errors=[];page.on('pageerror',e=>errors.push(e.message));
 await page.route('**/api/v3/sessions/*/activity?*',route=>route.fulfill({json:{events:[{session_id:'s',run_id:'r',event_id:'e',seq:1,kind:'agent_step_end',payload:{asst_id:'a',content:'A private update'}}],segments:[{id:'g',run_id:'r',boundary_item_id:'q',first_seq:1,last_seq:1,closed:false}],cursor:1,has_more:false}}));
 t.after(async()=>{await browser.close();await new Promise(r=>server.close(r));assert.deepEqual(hashes(),before);assert.deepEqual(errors,[]);});
 await page.goto('http://127.0.0.1:'+server.address().port+'/');
 await page.evaluate(async()=>{
  window.api=await import('/v3-ui/chat/elements/index.js');window.admin=true;window.flags={show_activity_groups:true};window.app={currentSessionId:'s'};
  window.view=api.createChatElements({app,isAdmin:()=>admin,chatUiFlag:(k,f)=>flags[k]??f});
  window.render=(state='running',id='s')=>{app.currentSessionId=id;view.render({sessionId:id,runs:[{id:'r',state}]},document.querySelector('#messages'));};render();
 });
 return page;
}
for(const opts of [{},{mobile:true,dark:true,reduced:true}])test('chat element lifecycle '+JSON.stringify(opts),async t=>{
 const p=await fixture(t,opts);
 const group=p.locator('.v2-activity-group'),dots=p.locator('.chat-typing-ellipsis');
 await group.waitFor();assert.match(await group.locator('summary').first().textContent(),/^1 update · Working/);
 assert.equal(await group.locator('.v2-mini-row').count(),0,'transcript stays lazy');assert.equal(await dots.isVisible(),true);
 await p.locator('#chat-element-style-typing-ellipsis').evaluate(link=>link.sheet?true:new Promise(r=>link.onload=r));
 assert.equal(await dots.locator('span').first().evaluate(n=>getComputedStyle(n).animationName),opts.reduced?'none':'chat-typing-dot');
 await group.locator('summary').first().click();await group.locator('.v2-mini-row').waitFor();assert.match(await group.textContent(),/A private update/);
 await group.locator('summary').first().click();assert.equal(await group.locator('.v2-activity-body').isVisible(),false);
 await group.locator('summary').first().focus();await p.keyboard.press('Enter');await group.locator('.v2-activity-body').waitFor({state:'visible'});assert.equal(await group.locator('.v2-activity-body').isVisible(),true);
 await p.evaluate(()=>render('completed'));assert.equal(await dots.isVisible(),false);assert.equal(await group.locator('summary').first().textContent(),'1 update');assert.equal(await group.locator('.v2-mini-row').count(),1);
 for(const [state,label] of [['finished','1 update'],['failed','1 update · Failed'],['cancelled','1 update · Stopped'],['queued','1 update · Queued…']]){await p.evaluate(state=>render(state),state);assert.equal(await group.locator('summary').first().textContent(),label);}
 await p.evaluate(()=>{flags.chat_elements={typing_ellipsis:{enabled:false}};render();});assert.equal(await dots.isVisible(),false);
 await p.evaluate(()=>{admin=false;flags.chat_elements={};render();});assert.equal(await group.count(),0);assert.equal(await dots.isVisible(),true);
 await p.evaluate(()=>render('running','next'));assert.equal(await p.locator('.v2-mini-row').count(),0);assert.equal(await dots.count(),1);
 await p.evaluate(()=>view.disconnect());assert.equal(await dots.count(),0);
});
test('mini transcript can be opened by an alternate ellipsis trigger',async t=>{
 const p=await fixture(t);await p.evaluate(async()=>{
  view.disconnect();const {createMiniTranscript}=await import('/v3-ui/chat/elements/mini-transcript/index.js');const {createTypingEllipsis}=await import('/v3-ui/chat/elements/typing-ellipsis/index.js');
  window.transcript=createMiniTranscript();transcript.update({rows:[],events:[]});window.indicator=createTypingEllipsis({onActivate:()=>transcript.toggle()});indicator.update([{state:'running'}]);document.querySelector('#messages').append(indicator.element,transcript.element);
 });
 await p.locator('.chat-typing-ellipsis').click();assert.equal(await p.locator('.v2-activity-body').isVisible(),true);
 await p.locator('.chat-typing-ellipsis').click();assert.equal(await p.locator('.v2-activity-body').isVisible(),false);
 await p.evaluate(()=>{indicator.dispose();transcript.dispose();});assert.equal(await p.locator('.v2-activity-body').count(),0);
});
