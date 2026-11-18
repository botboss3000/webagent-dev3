const {test}=require('node:test'),assert=require('node:assert/strict');
const fs=require('node:fs'),path=require('node:path'),http=require('node:http');
const {chromium}=require(process.env.PLAYWRIGHT_MODULE||'playwright');
const ROOT=process.env.SOURCE_ROOT||path.resolve(__dirname,'../../..'),UI=path.join(ROOT,'ui');
// A paged reply panel pages with chevrons around "N of M", clears a radio on a second tap, swaps
// Skip/Continue, takes "Something else…", keeps its pick list flush so the marks line up with the
// comment field below, carries a comment typed under a pick list into the answer, stages files from
// + inside a two-row text box, keeps Dismiss as a chip at the top right, hides the composer while a
// panel field is focused and sends one answer message.
for(const mobile of [false,true])for(const light of [false,true])test(`reply panel ${mobile?'mobile':'desktop'} ${light?'light':'dark'}`,async t=>{
 const html='<!doctype html><meta name="viewport" content="width=device-width,initial-scale=1"><link rel="stylesheet" href="/shell/app.css"><link rel="stylesheet" href="/chat/chat.css"><style>:root{--fg-1:#eee;--fg-2:#ddd;--fg-3:#aaa;--bg-0:#000;--bg-1:#222;--bg-elev:#181818;--brand:#48f;--accent:#48f;--danger:#e44;--border:#666;--border-width:1px}body{margin:0;background:#111;font-family:system-ui}body.light-mode{background:white;--fg-1:#222;--fg-2:#333;--fg-3:#666;--bg-0:#fff;--bg-1:#eee;--bg-elev:#fafafa}#chat-panel{max-width:650px;margin:auto}#chat-input-area{position:static!important}</style><main id="chat-panel">'+fs.readFileSync(path.join(UI,'chat/chat.html'),'utf8')+'</main>';
 const server=http.createServer((req,res)=>{const rel=new URL(req.url,'http://localhost').pathname;if(rel==='/'){res.setHeader('content-type','text/html');return res.end(html);}const p=path.resolve(UI,rel.replace(/^\/ui\//,'').replace(/^\//,''));if(!p.startsWith(UI+path.sep)){res.writeHead(403);return res.end();}fs.readFile(p,(e,b)=>{res.writeHead(e?404:200,{'content-type':p.endsWith('.js')?'text/javascript':p.endsWith('.css')?'text/css':'text/plain'});res.end(e?'missing':b);});});
 await new Promise(r=>server.listen(0,'127.0.0.1',r));const browser=await chromium.launch({headless:true,...(process.env.BROWSER_EXECUTABLE?{executablePath:process.env.BROWSER_EXECUTABLE}:{}),args:['--no-sandbox']});
 t.after(async()=>{await browser.close();await new Promise(r=>server.close(r));});
 const context=await browser.newContext({viewport:mobile?{width:390,height:844}:{width:1200,height:1000},isMobile:mobile,hasTouch:mobile});const p=await context.newPage();await p.goto('http://127.0.0.1:'+server.address().port);
 await p.evaluate(async light=>{
  document.body.classList.toggle('light-mode',light);window.sent=[];window.staged=[];
  const {buildReplyForm,answerText,answeredPanelRuns}=await import('/chat/core/reply-panel.js');window.answerText=answerText;window.answeredPanelRuns=answeredPanelRuns;
  const fields=[{name:'layout',label:'Layout',type:'radio',options:['Cards','List'],other:true,required:true},{name:'extras',label:'Extras',type:'multi',options:['Skip labels','Step counter']},{name:'notes',label:'Notes',type:'textarea',attach:true},{name:'name',label:'Name',type:'text',required:true}];
  // Mirror the transcript: the panel's content holds the bold title, then the form.
  const card=(id,form)=>{const panel=document.createElement('div');panel.className='v3-reply-panel';if(id)panel.id=id;const body=document.createElement('div');body.className='message-content';const title=document.createElement('strong');title.textContent='Panel setup';body.append(title,form);panel.append(body);return panel;};
  const dismiss=document.createElement('button');dismiss.type='button';dismiss.textContent='Dismiss';
  const live=card('',buildReplyForm({fields,paged:true,dismiss,addFiles:async files=>{staged.push(...files.map(file=>file.name));return staged.length;},send:async answers=>{sent.push(answers);}}));
  const done=card('answered',buildReplyForm({fields,paged:true,state:'answered',values:{layout:'List',name:'Bo'},comments:{layout:'chosen for clarity'},dismiss:document.createElement('button')}));
  done.classList.add('is-answered');document.getElementById('chat-messages-inner').append(live,done);
 },light);
 const live=p.locator('.v3-reply-panel:not(#answered)');
 const step=()=>live.locator('.v3-reply-step').innerText();
 const shown=name=>live.getByRole('button',{name,exact:true}).isVisible();
 const nav=async()=>[await shown('Previous question'),await shown('Next question'),await shown('Skip'),await shown('Continue'),await shown('Submit'),await shown('Dismiss')];
 assert.equal(await step(),'1 of 4');
 assert.equal(await live.getByRole('button',{name:'Back'}).count(),0);
 assert.deepEqual(await nav(),[false,true,false,true,false,true]);
 // Dismiss is a short chip above the first question, at the card's top right.
 const chip=await live.locator('.v3-reply-dismiss').boundingBox(),q=await live.locator('.v3-reply-q').first().boundingBox(),body=await live.locator('.message-content').boundingBox();
 assert.ok(chip.y<q.y&&chip.height<=30,'dismiss chip sits above the question');assert.ok(body.x+body.width-(chip.x+chip.width)<=12,'dismiss chip sits at the right edge');
 // The single-choice mark is a round 20 px circle; the checkbox mark is a rounded square.
 const marks=await live.locator('.v3-reply-mark').evaluateAll(nodes=>nodes.map(n=>{const s=getComputedStyle(n);return [n.previousElementSibling.type,s.width,s.height,s.borderTopLeftRadius];}));
 assert.deepEqual(marks.find(m=>m[0]==='radio'),['radio','20px','20px','50%']);assert.deepEqual(marks.find(m=>m[0]==='checkbox'),['checkbox','20px','20px','5px']);
 // The pick list has no box of its own, and its marks line up with the comment field below it.
 const listStyle=await live.locator('.v3-reply-options').first().evaluate(n=>{const s=getComputedStyle(n);return [s.borderTopWidth,s.borderLeftWidth,s.borderTopLeftRadius];});
 assert.deepEqual(listStyle,['0px','0px','0px']);
 assert.ok(Math.abs((await live.locator('.v3-reply-mark').first().boundingBox()).x-(await live.locator('.v3-reply-comment').first().boundingBox()).x)<=1,'marks align with the comment field');
 // A comment typed under a pick list goes out with the answer.
 await live.getByRole('textbox',{name:'Layout: comments'}).fill('chosen for clarity');
 await live.getByRole('button',{name:'Continue'}).click();
 assert.equal(await live.locator('.v3-reply-error:visible').innerText(),'Choose one option.');assert.equal(await step(),'1 of 4');
 // Tapping the chosen radio again clears it.
 const list=live.getByRole('radio',{name:'List'});
 await live.locator('.v3-reply-option',{hasText:'List'}).click();assert.equal(await list.isChecked(),true);
 await live.locator('.v3-reply-option',{hasText:'List'}).click();assert.equal(await list.isChecked(),false);
 await live.locator('.v3-reply-option',{hasText:'Something else…'}).click();
 const other=live.locator('.v3-reply-other');assert.equal(await other.isVisible(),true);await other.fill('Tiles');
 await live.getByRole('button',{name:'Continue'}).click();assert.equal(await step(),'2 of 4');
 // An empty optional page offers Skip; a selection turns it back into Continue.
 assert.deepEqual(await nav(),[true,true,true,false,false,true]);
 await live.locator('.v3-reply-option',{hasText:'Skip labels'}).click();assert.deepEqual([await shown('Skip'),await shown('Continue')],[false,true]);
 await live.locator('.v3-reply-option',{hasText:'Skip labels'}).click();assert.deepEqual([await shown('Skip'),await shown('Continue')],[true,false]);
 await live.getByRole('button',{name:'Previous question'}).click();assert.equal(await step(),'1 of 4');
 await live.getByRole('button',{name:'Next question'}).click();assert.equal(await step(),'2 of 4');
 await live.getByRole('button',{name:'Next question'}).click();assert.equal(await step(),'3 of 4');
 // + sits inside the text box, on its own row below the full-width text.
 const box=await live.locator('.v3-reply-input:has(.v3-reply-attach)').boundingBox(),area=await live.getByRole('textbox',{name:'Notes'}).boundingBox(),plus=await live.locator('.v3-reply-attach').boundingBox();
 assert.ok(plus.y>=area.y+area.height-1,'+ is below the text');assert.ok(plus.x>=box.x&&plus.x+plus.width<=box.x+box.width&&plus.y+plus.height<=box.y+box.height+1,'+ is inside the box');
 assert.ok(area.width>=box.width-4,'text spans the box');
 // A focused panel field hides the composer so the keyboard serves the panel; it returns on blur.
 const composer=()=>p.evaluate(()=>getComputedStyle(document.getElementById('composer')).display);
 assert.notEqual(await composer(),'none');
 await live.getByRole('textbox',{name:'Notes'}).fill('Keep it short');assert.equal(await composer(),'none');
 await live.locator('input[type=file]').setInputFiles([{name:'a.txt',mimeType:'text/plain',buffer:Buffer.from('a')},{name:'b.txt',mimeType:'text/plain',buffer:Buffer.from('b')}]);
 await p.waitForFunction(()=>staged.length===2);assert.equal(await live.locator('.v3-reply-files').innerText(),'2 files will be sent with your answer');
 await p.evaluate(()=>document.activeElement.blur());assert.notEqual(await composer(),'none');
 await live.getByRole('button',{name:'Continue'}).click();assert.equal(await step(),'4 of 4');
 assert.deepEqual(await nav(),[true,false,false,false,true,true]);
 await live.getByRole('button',{name:'Submit'}).click();assert.equal(await live.locator('.v3-reply-error:visible').innerText(),'Name is required.');assert.equal(await p.evaluate(()=>sent.length),0);
 await live.getByRole('textbox',{name:'Name'}).fill('Ana');await live.getByRole('button',{name:'Submit'}).click();
 await p.waitForFunction(()=>sent.length===1);
 assert.equal(await p.evaluate(()=>answerText('Panel setup',sent[0])),'Panel setup\nLayout: Tiles\nLayout — comment: chosen for clarity\nExtras: skipped\nNotes: Keep it short\nName: Ana');
 assert.equal(await p.evaluate(()=>sent[0].find(item=>item.name==='layout').comment),'chosen for clarity');
 assert.deepEqual(await p.evaluate(()=>staged),['a.txt','b.txt']);
 // An answered panel shows every recorded answer read-only and greyed out, with Complete where Submit was.
 const answered=p.locator('#answered');assert.equal(await answered.locator('button').count(),0);
 assert.equal(await answered.locator('input:not([disabled]),textarea:not([disabled])').count(),0);
 assert.equal(await answered.getByRole('radio',{name:'List'}).isChecked(),true);
 assert.equal(await answered.locator('.v3-reply-state').count(),0);
 assert.equal(await answered.locator('.v3-reply-complete').innerText(),'Complete');
 const complete=await answered.locator('.v3-reply-complete').boundingBox(),field=await answered.locator('.v3-reply-field').last().boundingBox(),options=await answered.locator('.v3-reply-options').first().boundingBox();
 assert.ok(complete.y>=field.y+field.height-1,'Complete sits below the answers');
 assert.ok(Math.abs(options.x+options.width-(complete.x+complete.width))<=1,'Complete sits where Submit was, at the footer right');
 assert.ok(complete.height>=44);
 const greyed=await answered.locator('.v3-reply-options,.v3-reply-comment,.v3-reply-input').evaluateAll(nodes=>nodes.map(node=>Number(getComputedStyle(node).opacity)));
 assert.ok(greyed.length>=2&&greyed.every(value=>value>0&&value<1),'recorded picks and comments are greyed out');
 assert.deepEqual(await p.evaluate(()=>[...window.answeredPanelRuns([{component:{lifecycle:{message_run_id:'run-1'}}},{component:{lifecycle:{}}},{}])]),['run-1']);
 assert.equal(await p.evaluate(()=>document.documentElement.scrollWidth<=innerWidth),true);
 if(process.env.EVIDENCE_DIR)await p.screenshot({path:path.join(process.env.EVIDENCE_DIR,`reply-panel-${mobile?'mobile':'desktop'}-${light?'light':'dark'}.png`),fullPage:true});
});
