const {test}=require('node:test'),assert=require('node:assert/strict');
const fs=require('node:fs'),path=require('node:path'),http=require('node:http');
const {chromium}=require(process.env.PLAYWRIGHT_MODULE||'playwright');
const ROOT=process.env.SOURCE_ROOT||path.resolve(__dirname,'../../..'),UI=path.join(ROOT,'ui');
// A paged reply panel walks its steps, skips, takes "Something else…", stages files from a free-text
// field, hides the composer while a panel field is focused and sends one answer message.
for(const mobile of [false,true])for(const light of [false,true])test(`reply panel ${mobile?'mobile':'desktop'} ${light?'light':'dark'}`,async t=>{
 const html='<!doctype html><meta name="viewport" content="width=device-width,initial-scale=1"><link rel="stylesheet" href="/app.css"><link rel="stylesheet" href="/chat/chat.css"><style>:root{--fg-1:#eee;--fg-2:#ddd;--fg-3:#aaa;--bg-0:#000;--bg-1:#222;--bg-elev:#181818;--brand:#48f;--accent:#48f;--danger:#e44;--border:#666;--border-width:1px}body{margin:0;background:#111;font-family:system-ui}body.light-mode{background:white;--fg-1:#222;--fg-2:#333;--fg-3:#666;--bg-0:#fff;--bg-1:#eee;--bg-elev:#fafafa}#chat-panel{max-width:650px;margin:auto}#chat-input-area{position:static!important}</style><main id="chat-panel">'+fs.readFileSync(path.join(UI,'chat/chat.html'),'utf8')+'</main>';
 const server=http.createServer((req,res)=>{const rel=new URL(req.url,'http://localhost').pathname;if(rel==='/'){res.setHeader('content-type','text/html');return res.end(html);}const p=path.resolve(UI,rel.replace(/^\/ui\//,'').replace(/^\//,''));if(!p.startsWith(UI+path.sep)){res.writeHead(403);return res.end();}fs.readFile(p,(e,b)=>{res.writeHead(e?404:200,{'content-type':p.endsWith('.js')?'text/javascript':p.endsWith('.css')?'text/css':'text/plain'});res.end(e?'missing':b);});});
 await new Promise(r=>server.listen(0,'127.0.0.1',r));const browser=await chromium.launch({headless:true,...(process.env.BROWSER_EXECUTABLE?{executablePath:process.env.BROWSER_EXECUTABLE}:{}),args:['--no-sandbox']});
 t.after(async()=>{await browser.close();await new Promise(r=>server.close(r));});
 const context=await browser.newContext({viewport:mobile?{width:390,height:844}:{width:1200,height:1000},isMobile:mobile,hasTouch:mobile});const p=await context.newPage();await p.goto('http://127.0.0.1:'+server.address().port);
 await p.evaluate(async light=>{
  document.body.classList.toggle('light-mode',light);window.sent=[];window.staged=[];
  const {buildReplyForm,answerText}=await import('/chat/core/reply-panel.js');window.answerText=answerText;
  const fields=[{name:'layout',label:'Layout',type:'radio',options:['Cards','List'],other:true,required:true},{name:'extras',label:'Extras',type:'multi',options:['Skip labels','Step counter']},{name:'notes',label:'Notes',type:'textarea',attach:true},{name:'name',label:'Name',type:'text',required:true}];
  const panel=document.createElement('div');panel.className='v3-reply-panel';
  const dismiss=document.createElement('button');dismiss.type='button';dismiss.textContent='Dismiss';
  panel.append(buildReplyForm({fields,paged:true,dismiss,addFiles:async files=>{staged.push(...files.map(file=>file.name));return staged.length;},send:async answers=>{sent.push(answers);}}));
  const done=document.createElement('div');done.className='v3-reply-panel';done.id='answered';
  done.append(buildReplyForm({fields,paged:true,state:'answered',values:{layout:'List',name:'Bo'},dismiss:document.createElement('button')}));
  document.getElementById('chat-messages-inner').append(panel,done);
 },light);
 const live=p.locator('.v3-reply-panel:not(#answered)');
 const step=()=>live.locator('.v3-reply-step').innerText();
 const shown=name=>live.getByRole('button',{name,exact:true}).isVisible();
 assert.equal(await step(),'Step 1 of 4');
 assert.deepEqual([await shown('Back'),await shown('Skip'),await shown('Continue'),await shown('Submit'),await shown('Dismiss')],[false,false,true,false,true]);
 // The single-choice mark is a round 20 px circle; the checkbox mark is a rounded square.
 const marks=await live.locator('.v3-reply-mark').evaluateAll(nodes=>nodes.map(n=>{const s=getComputedStyle(n);return [n.previousElementSibling.type,s.width,s.height,s.borderTopLeftRadius];}));
 assert.deepEqual(marks.find(m=>m[0]==='radio'),['radio','20px','20px','50%']);assert.deepEqual(marks.find(m=>m[0]==='checkbox'),['checkbox','20px','20px','5px']);
 await live.getByRole('button',{name:'Continue'}).click();
 assert.equal(await live.locator('.v3-reply-error:visible').innerText(),'Choose one option.');assert.equal(await step(),'Step 1 of 4');
 await live.locator('.v3-reply-option',{hasText:'Something else…'}).click();
 const other=live.locator('.v3-reply-other');assert.equal(await other.isVisible(),true);await other.fill('Tiles');
 await live.getByRole('button',{name:'Continue'}).click();assert.equal(await step(),'Step 2 of 4');
 assert.deepEqual([await shown('Back'),await shown('Skip')],[true,true]);
 await live.getByRole('button',{name:'Skip'}).click();assert.equal(await step(),'Step 3 of 4');
 // A focused panel field hides the composer so the keyboard serves the panel; it returns on blur.
 const composer=()=>p.evaluate(()=>getComputedStyle(document.getElementById('composer')).display);
 assert.notEqual(await composer(),'none');
 await live.getByRole('textbox',{name:'Notes'}).fill('Keep it short');assert.equal(await composer(),'none');
 await live.locator('input[type=file]').setInputFiles([{name:'a.txt',mimeType:'text/plain',buffer:Buffer.from('a')},{name:'b.txt',mimeType:'text/plain',buffer:Buffer.from('b')}]);
 await p.waitForFunction(()=>staged.length===2);assert.equal(await live.locator('.v3-reply-files').innerText(),'2 files will be sent with your answer');
 await p.evaluate(()=>document.activeElement.blur());assert.notEqual(await composer(),'none');
 await live.getByRole('button',{name:'Continue'}).click();assert.equal(await step(),'Step 4 of 4');
 assert.deepEqual([await shown('Skip'),await shown('Continue'),await shown('Submit'),await shown('Dismiss')],[false,false,true,true]);
 await live.getByRole('button',{name:'Submit'}).click();assert.equal(await live.locator('.v3-reply-error:visible').innerText(),'Name is required.');assert.equal(await p.evaluate(()=>sent.length),0);
 await live.getByRole('textbox',{name:'Name'}).fill('Ana');await live.getByRole('button',{name:'Submit'}).click();
 await p.waitForFunction(()=>sent.length===1);
 assert.equal(await p.evaluate(()=>answerText('Panel setup',sent[0])),'Panel setup\nLayout: Tiles\nExtras: skipped\nNotes: Keep it short\nName: Ana');
 assert.deepEqual(await p.evaluate(()=>staged),['a.txt','b.txt']);
 // An answered panel shows every question read-only with no buttons.
 const answered=p.locator('#answered');assert.equal(await answered.locator('button').count(),0);
 assert.equal(await answered.locator('input:not([disabled]),textarea:not([disabled])').count(),0);
 assert.equal(await answered.getByRole('radio',{name:'List'}).isChecked(),true);
 assert.equal(await p.evaluate(()=>document.documentElement.scrollWidth<=innerWidth),true);
 if(process.env.EVIDENCE_DIR)await p.screenshot({path:path.join(process.env.EVIDENCE_DIR,`reply-panel-${mobile?'mobile':'desktop'}-${light?'light':'dark'}.png`),fullPage:true});
});
