// Browser regression covers independent triggers, completion, private authority and session teardown.
const {test}=require('node:test'),assert=require('node:assert/strict');
const fs=require('node:fs'),path=require('node:path'),http=require('node:http'),crypto=require('node:crypto');
const {chromium}=require(process.env.PLAYWRIGHT_MODULE||'playwright');
const ROOT=process.env.SOURCE_ROOT||path.resolve(__dirname,'../../..');
const CSP="default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data: blob:; media-src 'self' data: blob:; connect-src 'self'; frame-ancestors 'none'; base-uri 'self'; form-action 'self'";
/** Serve the actual modules with a bounded synthetic journal, never application state. */
async function fixture(t,{mobile=false,dark=false,reduced=false,counts={}}={}) {
 const inputs=[];
 // Capture the complete local UI dependency tree so test results reject concurrent changes.
 function collect(dir){for(const item of fs.readdirSync(dir,{withFileTypes:true})){const p=path.join(dir,item.name);if(item.isDirectory())collect(p);else if(/\.(js|css|json|cjs)$/.test(p))inputs.push(p);}}
 collect(path.join(ROOT,'ui'));
 const hashes=()=>inputs.map(p=>[p,crypto.createHash('sha256').update(fs.readFileSync(p)).digest('hex')]);const before=hashes();
 const server=http.createServer((req,res)=>{
  const url=new URL(req.url,'http://localhost');
  if(url.pathname==='/'){res.setHeader('content-type','text/html');res.setHeader('content-security-policy',CSP);return res.end('<!doctype html><meta name="viewport" content="width=device-width,initial-scale=1"><style>:root{--fg-3:#777;--fg-2:#444;--accent:#369;--bg-tint:#eee;--bg-2:#fff;--border-width:1px;--border:#aaa;--r-sm:6px}body{background:'+(dark?'#151515':'white')+'}</style><main id="messages"><div data-public-id="q">Question</div></main>');}
  const file=path.resolve(ROOT,'ui',url.pathname.replace(/^\/ui\//,''));
  if(!file.startsWith(path.join(ROOT,'ui')+path.sep)){res.writeHead(403);return res.end();}
  fs.readFile(file,(e,b)=>{res.writeHead(e?404:200,{'content-type':file.endsWith('.js')?'text/javascript':file.endsWith('.css')?'text/css':'text/plain'});res.end(e?'missing':b);});
 });
 await new Promise(r=>server.listen(0,'127.0.0.1',r));
 const browser=await chromium.launch({headless:true,...(process.env.BROWSER_EXECUTABLE?{executablePath:process.env.BROWSER_EXECUTABLE}:{}),args:['--no-sandbox']});
 const context=await browser.newContext({viewport:mobile?{width:390,height:844}:{width:1100,height:800},isMobile:mobile,hasTouch:mobile,reducedMotion:reduced?'reduce':'no-preference'});
 const page=await context.newPage();const errors=[];page.on('pageerror',e=>{errors.push(e.message);console.error('pageerror: '+e.message);});
 // Summaries carry counts only; a group's events come from its run log when it is expanded.
 await page.route('**/api/v3/sessions/*/activity?*',route=>route.fulfill({json:{segments:[{id:'g',run_id:'r',boundary_item_id:'q',first_seq:1,last_seq:1,closed:false,latest:true,last_of_run:true,counts:{compactions:0,modes:0,responses:0,retries:0,subagents:0,tools:0,updates:1,...counts}}],cursor:1}}));
 await page.route('**/api/v3/sessions/*/runs/*/logs?*',route=>route.fulfill({json:{events:[{session_id:'s',run_id:'r',event_id:'e',seq:1,kind:'agent_step_end',payload:{asst_id:'a',content:'A private update'}}],cursor:1,has_more:false}}));
 t.after(async()=>{await browser.close();await new Promise(r=>server.close(r));assert.deepEqual(hashes(),before);assert.deepEqual(errors,[]);});
 await page.goto('http://127.0.0.1:'+server.address().port+'/');
 await page.evaluate(async()=>{
  window.api=await import('/ui/chat/elements/index.js');window.admin=true;window.flags={show_activity_groups:true};window.app={currentSessionId:'s'};
  window.view=api.createChatElements({app,isAdmin:()=>admin,chatUiFlag:(k,f)=>flags[k]??f});
  window.render=(state='running',id='s')=>{app.currentSessionId=id;view.render({sessionId:id,runs:[{id:'r',state}]},document.querySelector('#messages'));};render();
 });
 return page;
}
for(const opts of [{},{mobile:true,dark:true,reduced:true}])test('chat element lifecycle '+JSON.stringify(opts),async t=>{
 const p=await fixture(t,opts);
 const group=p.locator('.v2-activity-group'),dots=p.locator('.chat-typing-ellipsis');
 await group.waitFor();assert.match(await group.locator('summary').first().textContent(),/^1 message · Working/);
 assert.equal(await group.locator('summary').first().evaluate(n=>getComputedStyle(n).listStyleType),'none','no disclosure triangle');
 assert.equal(await group.locator('.v2-mini-row').count(),0,'transcript stays lazy');assert.equal(await dots.isVisible(),true);
 await p.locator('#chat-element-style-typing-ellipsis').evaluate(link=>link.sheet?true:new Promise(r=>link.onload=r));
 assert.equal(await dots.locator('span').first().evaluate(n=>getComputedStyle(n).animationName),opts.reduced?'none':'chat-typing-dot');
 await group.locator('summary').first().click();await group.locator('.v2-mini-row').first().waitFor();assert.match(await group.textContent(),/A private update/);
 await group.locator('summary').first().click();assert.equal(await group.locator('.v2-activity-body').isVisible(),false);
 await group.locator('summary').first().focus();await p.keyboard.press('Enter');await group.locator('.v2-activity-body').waitFor({state:'visible'});assert.equal(await group.locator('.v2-activity-body').isVisible(),true);
 await p.evaluate(()=>render('completed'));assert.equal(await dots.isVisible(),false);assert.equal(await group.locator('summary').first().textContent(),'1 message');assert.equal(await group.locator('.v2-mini-row:not(.tc-raw)').count(),1,'one message row beside the diagnostics row');
 for(const [state,label] of [['finished','1 message'],['failed','1 message · Failed'],['cancelled','1 message · Stopped'],['queued','1 message · Queued…']]){await p.evaluate(state=>render(state),state);assert.equal(await group.locator('summary').first().textContent(),label);}
 await p.evaluate(()=>{flags.chat_elements={typing_ellipsis:{enabled:false}};render();});assert.equal(await dots.isVisible(),false);
 await p.evaluate(()=>{admin=false;flags.chat_elements={};render();});assert.equal(await group.count(),0);assert.equal(await dots.isVisible(),true);
 await p.evaluate(()=>render('running','next'));assert.equal(await p.locator('.v2-mini-row').count(),0);assert.equal(await dots.count(),1);
 await p.evaluate(()=>view.disconnect());assert.equal(await dots.count(),0);
});
for(const opts of [{},{mobile:true,dark:true}])test('sub agent spawns read as sub agents apart from the tool count, once with notices '+JSON.stringify(opts),async t=>{
 const p=await fixture(t,{...opts,counts:{subagents:3,tools:18,updates:0}});
 const group=p.locator('.v2-activity-group');
 await group.waitFor();
 assert.equal(await group.locator('summary').first().textContent(),'18 tool calls, 3 sub-agents · Working…');
 // The same three sub agents report back; their notice row must not add a second count.
 await p.evaluate(()=>{
  const row=document.createElement('article');row.dataset.publicId='notice';row.dataset.subagentNotices='3';row.dataset.noticeKinds=JSON.stringify({'sub-agent':3});
  for(const title of ['Sub-agent · running slow','Sub-agent · finished']){const entry=document.createElement('details');entry.className='v3-subagent-row';const summary=document.createElement('summary');summary.textContent=title;entry.append(summary);row.append(entry);}
  document.querySelector('#messages').append(row);render();
 });
 assert.equal(await p.locator('.v3-activity-cluster > summary').textContent(),'18 tool calls, 3 sub-agents · Working…');
 assert.equal(await p.locator('.v3-activity-cluster > summary .tc-head-chev').count(),1,'merged header keeps its chevron');
 // One panel: expanding the cluster loads its members and shows their rows and notices together.
 await p.locator('.v3-activity-cluster > summary').click();
 const body=p.locator('.v3-activity-cluster > .v2-activity-body');
 await body.waitFor({state:'visible'});
 // Member rows load from the run log when the cluster opens; wait for that page, not a paint.
 await p.waitForFunction(()=>document.querySelector('.v3-activity-cluster > .v2-activity-body')?.textContent.includes('A private update'));
 assert.match(await body.textContent(),/A private update/);
 assert.match(await body.textContent(),/Sub-agent · running slow/);
 assert.equal(await p.locator('.v3-activity-cluster .v2-activity-body').count(),1,'one transcript, no nested panel');
 assert.equal(await body.locator('.v2-activity-group').count(),0,'no nested activity group');
 assert.equal(await p.locator('#messages > .v2-activity-group:not(.v3-activity-cluster)').isVisible(),false,'member stays hidden');
});
test('an idle activity poll that finds no changes leaves the transcript DOM untouched',async t=>{
 const p=await fixture(t);let polls=0;
 // Later routes win: cursor polls report no new summaries.
 await p.route('**/api/v3/sessions/*/activity?*after=*',route=>{polls++;return route.fulfill({json:{segments:[],cursor:1}});});
 const group=p.locator('.v2-activity-group');await group.waitFor();
 const settle=Date.now()+8000;while(polls<1 && Date.now()<settle)await p.waitForTimeout(100);
 await p.waitForTimeout(300);
 await p.evaluate(()=>{window.mutations=0;new MutationObserver(list=>{window.mutations+=list.length;}).observe(document.querySelector('#messages'),{subtree:true,childList:true,attributes:true,characterData:true});});
 const start=polls,deadline=Date.now()+8000;
 while(polls<start+2 && Date.now()<deadline)await p.waitForTimeout(100);
 assert.ok(polls>=start+2,'two idle polls ran');
 assert.equal(await p.evaluate(()=>window.mutations),0,'idle polls leave the transcript DOM untouched');
 assert.match(await group.locator('summary').first().textContent(),/^1 message · Working/);
});
test('mini transcript can be opened by an alternate ellipsis trigger',async t=>{
 const p=await fixture(t);await p.evaluate(async()=>{
  view.disconnect();const {createMiniTranscript}=await import('/ui/chat/elements/mini-transcript/index.js');const {createTypingEllipsis}=await import('/ui/chat/elements/typing-ellipsis/index.js');
  window.transcript=createMiniTranscript();transcript.update({rows:[],events:[]});window.indicator=createTypingEllipsis({onActivate:()=>transcript.toggle()});indicator.update([{state:'running'}]);document.querySelector('#messages').append(indicator.element,transcript.element);
 });
 await p.locator('.chat-typing-ellipsis').click();assert.equal(await p.locator('.v2-activity-body').isVisible(),true);
 await p.locator('.chat-typing-ellipsis').click();assert.equal(await p.locator('.v2-activity-body').isVisible(),false);
 await p.evaluate(()=>{indicator.dispose();transcript.dispose();});assert.equal(await p.locator('.v2-activity-body').count(),0);
});

test('each agent loop is its own step, even when the loop sent no message',async t=>{
 const p=await fixture(t);
 const heads=await p.evaluate(async()=>{
  view.disconnect();const {createMiniTranscript}=await import('/ui/chat/elements/mini-transcript/index.js');
  const call=(id,turn)=>({id:'tool:'+id,category:'tools',label:'file_read · done',events:[
   {kind:'tool_call',run_id:'r',turn,payload:{tool:'file_read',tool_call_id:id}},
   {kind:'tool_result',run_id:'r',turn:null,payload:{tool_call_id:id,output:'ok'}}]});
  const note={id:'message:n',category:'updates',label:'Update',text:'Checking the tests next.',
   events:[{kind:'agent_step_end',run_id:'r',turn:3,payload:{asst_id:'n',content:'Checking the tests next.'}}]};
  const transcript=createMiniTranscript();document.querySelector('#messages').append(transcript.element);
  transcript.update({rows:[call('a',1),call('b',1),call('c',2),note,call('d',3)],events:[]});transcript.open();
  return [...transcript.element.querySelectorAll('.tc-step:not([data-kind="system"]) > .tc-step-head')].map(n=>n.textContent);
 });
 // Turn 1 made two calls, turn 2 one, and turn 3 a message with its call.
 assert.deepEqual(heads,['Step 12 tool calls','Step 21 tool call','Step 31 tool call']);
});

test('back closes the last opened call of the active group before that group',async t=>{
 const p=await fixture(t);
 await p.evaluate(async()=>{
  view.disconnect();
  // app.js hands every popstate to consumeBack; the fixture stands in for that listener.
  const {consumeBack}=await import('/ui/shared/history-back/history-back.js');
  window.addEventListener('popstate',()=>consumeBack());
  const {createActivityGroup}=await import('/ui/chat/elements/activity-group/index.js');
  const {createMiniTranscript}=await import('/ui/chat/elements/mini-transcript/index.js');
  const call=id=>({id:'tool:'+id,category:'tools',label:'file_read · done',events:[
   {kind:'tool_call',run_id:'r',turn:1,payload:{tool:'file_read',tool_call_id:id}},
   {kind:'tool_result',run_id:'r',turn:null,payload:{tool_call_id:id,output:'ok'}}]});
  window.group=createActivityGroup({transcript:createMiniTranscript()});
  document.querySelector('#messages').replaceChildren(group.element);
  group.update({id:'g',state:'completed',counts:{tools:2},rows:[call('a'),call('b')],events:[]});
  window.__lenBefore=history.length;
  group.element.open=true;
 });
 // The tool row's own disclosure, not the raw diagnostics nested inside it.
 const calls=p.locator('.v2-mini-tool > details.v2-mini-tool-disclosure > summary');
 await calls.first().waitFor();
 // An open layer owns one real entry, so a device press pops it instead of leaving the app.
 await p.waitForFunction(()=>history.length===window.__lenBefore+1);
 await calls.first().click();
 await p.waitForFunction(()=>document.querySelectorAll('.v2-mini-tool > details.v2-mini-tool-disclosure[open]').length===1);
 // The open call must own a second entry: re-pushing inside popstate is ignored on Android.
 await p.waitForFunction(()=>history.length===window.__lenBefore+2&&history.state?.webagentBack===true);
 // consumeBack is the same press app.js sends on popstate.
 const presses=await p.evaluate(async()=>{
  const {consumeBack}=await import('/ui/shared/history-back/history-back.js');
  const state=()=>({group:group.element.open,
   tools:document.querySelectorAll('.v2-mini-tool > details.v2-mini-tool-disclosure[open]').length});
  const before=state();
  const first=consumeBack();const afterFirst=state();
  const second=consumeBack();const afterSecond=state();
  return {before,first,afterFirst,second,afterSecond};
 });
 assert.deepEqual(presses.before,{group:true,tools:1});
 assert.equal(presses.first,true,'the first press is consumed');
 assert.deepEqual(presses.afterFirst,{group:true,tools:0},'one press closes the call and leaves the group open');
 assert.equal(presses.second,true,'the next press is consumed');
 assert.deepEqual(presses.afterSecond,{group:false,tools:0},'the next press closes the group');
});

test('broker metadata renders inside expanded tool details with inert values',async t=>{
 const p=await fixture(t);
 await p.evaluate(async()=>{
  view.disconnect();const {createTranscriptRow,patchTranscriptRow}=await import('/ui/chat/elements/mini-transcript/rows.js');
  const context={broker_id:'broker-example-123',summary:'<img src=x onerror=alert(1)>',selection:'created',state:'open',provenance:{session_id:'00000000-0000-4000-8000-000000000001'}};
  window.brokerRow={id:'broker-tool',category:'tools',label:'file_write',events:[{kind:'tool_call',payload:{arguments:{broker_id:'new'}}},{kind:'tool_result',payload:{output:JSON.stringify({status:'success',output:{broker_context:context,broker_job_id:'job-example',broker_release_id:'release-example'}})}}]};
  window.brokerNode=createTranscriptRow(brokerRow);document.querySelector('#messages').append(brokerNode);
  window.patchBroker=patchTranscriptRow;
 });
 await p.locator('.v2-mini-tool-disclosure > summary').click();
 const details=p.locator('.v2-mini-broker');await details.waitFor({state:'visible'});
 assert.match(await details.textContent(),/Broker ID: broker-example-123/);
 assert.match(await details.textContent(),/Broker Job ID: job-example/);assert.match(await details.textContent(),/Broker Release ID: release-example/);
 assert.equal(await details.locator('img').count(),0);
 assert.match(await details.textContent(),/<img src=x onerror=alert\(1\)>/);
 await p.evaluate(()=>{brokerRow.events.push({kind:'tool_result',payload:{output:'ordinary result'}});patchBroker(brokerNode,brokerRow);});
 assert.equal(await details.isVisible(),false);
});


test('bundled DOMPurify renders chat and mini Markdown under deployed CSP',async t=>{
 const p=await fixture(t);
 await p.addScriptTag({url:'/ui/vendor/marked.min.js'});
 await p.addScriptTag({url:'/ui/vendor/purify.min.js'});
 await p.evaluate(async()=>{
  view.disconnect();
  const {createMessage}=await import('/ui/chat/core/message-view.js');
  const {createTranscriptRow}=await import('/ui/chat/elements/mini-transcript/rows.js');
  const text='**Safe bold** [link](https://example.com)\n\n<span style="color:red">Styled text</span><img src=x onerror="window.__sanitizerXss=true"><script>window.__sanitizerXss=true</script>';
  window.chatNode=createMessage({id:'chat-markdown',seq:1,run_id:'r',role:'assistant',content:text});
  window.miniNode=createTranscriptRow({id:'mini-markdown',category:'messages',label:'Agent',text,events:[{kind:'agent_step_end',created_at:'2026-01-01T00:00:00Z',payload:{}}]});
  document.querySelector('#messages').replaceChildren(chatNode,miniNode);
 });
 assert.equal(await p.evaluate(()=>DOMPurify.version),'3.4.17');
 assert.equal(await p.evaluate(()=>window.__sanitizerXss),undefined);
 // Mini message rows start collapsed like tool rows; open it to read the rendered Markdown.
 await p.locator('.v2-mini-message .v2-mini-tool-disclosure > summary').click();
 for(const selector of ['.chat-bubble .md-body','.v2-mini-message .v2-mini-content']){
  const body=p.locator(selector);await body.waitFor();
  assert.equal(await body.locator('strong').textContent(),'Safe bold');
  assert.match(await body.textContent(),/Styled text/);
  assert.equal(await body.locator('script,[onerror],[style]').count(),0);
  const link=body.locator('a').first();assert.equal(await link.getAttribute('target'),'_blank');assert.equal(await link.getAttribute('rel'),'noopener noreferrer');
 }
});
test('agent reply screenshots use the attachment tile row and scroll sideways at phone width',async t=>{
 const p=await fixture(t,{mobile:true});
 await p.evaluate(async()=>{
  view.disconnect();
  history.replaceState(null,'','/?session_id=s');
  const css=document.createElement('link');css.rel='stylesheet';css.href='/ui/chat/chat.css';document.head.append(css);
  await new Promise(resolve=>{css.onload=resolve;css.onerror=resolve;});
  const {createMessage}=await import('/ui/chat/core/message-view.js');
  const files=Array.from({length:12},(_,i)=>({id:'00000000-0000-4000-8000-0000000000'+String(i).padStart(2,'0'),name:'screenshot-'+i+'.png',media_type:'image/png'}));
  window.reply=createMessage({id:'assistant:r',seq:2,run_id:'r',role:'assistant',content:'Checked both widths.',attachments:files});
  window.progress=createMessage({id:'working:r:auto:1:0',seq:1,run_id:'r',role:'assistant',content:'Reading the page.'});
  document.querySelector('#messages').replaceChildren(progress,reply);
 });
 const row=p.locator('.chat-bubble.agent .chat-attachments');
 assert.equal(await row.count(),1);
 assert.equal(await row.locator('a.chat-attachment-image img').count(),12);
 assert.equal(await row.locator('a').first().getAttribute('href'),'/api/v3/sessions/s/attachments/00000000-0000-4000-8000-000000000000/content');
 const box=await row.evaluate(node=>({wrap:getComputedStyle(node).flexWrap,overflow:getComputedStyle(node).overflowX,scrolls:node.scrollWidth>node.clientWidth,fits:node.getBoundingClientRect().right<=innerWidth}));
 assert.deepEqual(box,{wrap:'nowrap',overflow:'auto',scrolls:true,fits:true});
});

test('scrolling past a mini transcript end continues in the transcript behind it',async t=>{
 const p=await fixture(t);
 await p.evaluate(async()=>{
  view.disconnect();
  // Stand-in for #chat-messages: the panel sits inside a scroller that can move both ways.
  const stage=document.createElement('div');stage.style.cssText='position:absolute;inset:0;overflow:auto';
  stage.id='stage';document.body.append(stage);
  const {createMiniTranscript}=await import('/ui/chat/elements/mini-transcript/index.js');
  const call=index=>({id:'tool:'+index,category:'tools',label:'file_read · done',events:[
   {kind:'tool_call',run_id:'r',turn:1,payload:{tool:'file_read',tool_call_id:'t'+index}},
   {kind:'tool_result',run_id:'r',turn:null,payload:{tool_call_id:'t'+index,output:'ok'}}]});
  window.panel=createMiniTranscript();stage.append(panel.element);
  panel.update({rows:Array.from({length:16},(_,index)=>call(index)),events:[]});panel.open();
  const tail=document.createElement('div');tail.style.height='1200px';stage.append(tail);
  // The centre of the panel's visible part, so the wheel lands on the panel itself.
  window.aim=()=>{const r=panel.element.getBoundingClientRect();
   const top=Math.max(r.top,0),bottom=Math.min(r.bottom,innerHeight);
   return {x:r.left+r.width/2,y:(top+bottom)/2,visible:bottom-top};};
  window.settled=()=>({stage:stage.scrollTop,panel:panel.element.scrollTop});
 });
 const loosen=await p.evaluate(()=>getComputedStyle(panel.element).overscrollBehaviorY);
 assert.equal(loosen,'auto','the panel lets a finished gesture continue in the transcript behind it');
 // Down: the panel is already at its end, so the wheel moves the transcript behind it.
 const down=await p.evaluate(()=>{stage.scrollTop=0;panel.element.scrollTop=panel.element.scrollHeight;return aim();});
 assert.ok(down.visible>200,'the whole panel is on screen');
 await p.mouse.move(down.x,down.y);await p.mouse.wheel(0,300);await p.waitForTimeout(300);
 assert.ok((await p.evaluate(()=>settled())).stage>0,'wheel past the panel end scrolls the transcript behind it');
 // Up: same the other way, with room left above so the panel stays on screen.
 const up=await p.evaluate(()=>{stage.scrollTop=60;panel.element.scrollTop=0;return {...aim(),stage:stage.scrollTop};});
 assert.ok(up.visible>100,'the panel start is on screen');
 await p.mouse.move(up.x,up.y);await p.mouse.wheel(0,-300);await p.waitForTimeout(300);
 const end=await p.evaluate(()=>settled());
 assert.ok(end.stage<up.stage,'wheel past the panel start scrolls the transcript behind it');
 assert.equal(end.panel,0,'the panel stays at its own start');
});
