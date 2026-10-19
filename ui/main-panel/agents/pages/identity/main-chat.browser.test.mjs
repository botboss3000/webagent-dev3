import http from 'node:http';
import fs from 'node:fs/promises';
import path from 'node:path';
import assert from 'node:assert/strict';
const {chromium}=await import(process.env.PLAYWRIGHT_MODULE||'playwright');
// The shared toggle intentionally hides its checkbox; exercise the visible label.
async function setMainEnabled(page,value){
 await page.waitForFunction(()=>{const input=document.querySelector('input[aria-label="Enable Main Chat"]');return input&&!input.disabled;});
 const input=page.getByLabel('Enable Main Chat');
 if(await input.isChecked()!==value)await page.locator('label.conn-toggle-wrap').click();
 assert.equal(await input.isChecked(),value);
}

const root=path.resolve(new URL('../../../../../',import.meta.url).pathname),shared=path.resolve(root,'../ui');
const server=http.createServer(async(req,res)=>{try{
 const u=new URL(req.url,'http://localhost').pathname;
 if(u==='/'){res.setHeader('content-type','text/html');res.end('<!doctype html><meta name="viewport" content="width=device-width,initial-scale=1"><link rel="stylesheet" href="/shared/shared/css/design-system.css"><link rel="stylesheet" href="/shared/shared/css/app1.css"><link rel="stylesheet" href="/ui/shared/models/reference.css"><main id="host"></main>');return;}
 const base=u.startsWith('/shared/')?shared:root,file=path.resolve(base,'.'+(base===shared?u.slice(7):u));
 if(!file.startsWith(base+'/'))throw Error('bad');res.setHeader('content-type',file.endsWith('.css')?'text/css':file.endsWith('.json')?'application/json':'text/javascript');res.end(await fs.readFile(file));
}catch{res.statusCode=404;res.end('missing');}});
await new Promise(r=>server.listen(0,'127.0.0.1',r));let browser;
try{
 browser=await chromium.launch({headless:true,executablePath:process.env.CHROMIUM_PATH||'/snap/bin/chromium',args:['--no-sandbox']});
 for(const mobile of [false,true])for(const light of [false,true]){
  const context=await browser.newContext({viewport:mobile?{width:390,height:844}:{width:1200,height:850},hasTouch:mobile,isMobile:mobile});const page=await context.newPage();const errors=[];page.on('pageerror',e=>errors.push(e.message));
  await page.goto('http://127.0.0.1:'+server.address().port+'/');
  await page.evaluate(async light=>{
   document.body.classList.toggle('light-mode',light);
   const {renderMainChat}=await import('/ui/main-panel/agents/pages/identity/main-chat.js');window.mount=renderMainChat;
   const make=id=>({agent_id:id,can_manage:true,revision:7,session_metadata_revision:2,session_id:null,review_status:'disabled',settings:{enabled:false,chat_name:'Main '+id,notes_name:'Notes '+id,write_mode:'new_topics'},skills:{contribute:{override:null,fallback:'Fallback contributor: retain source references.'},review:{override:null,fallback:'Fallback coordinator: read notes and verify conditions.'}}});
   const states={one:make('one'),two:make('two')};window.fixture={states,writes:[],fail:false,activations:{one:0,two:0},demoted:[]};
   fixture.request=async(url,method='GET',body)=>{
    const match=url.match(/^\/models\/agents\/(one|two)\/main-chat$/);if(!match)throw Error('Wrong or unscoped route');const state=states[match[1]];
    if(method==='GET')return structuredClone(state);
    fixture.writes.push({agent:match[1],body:structuredClone(body)});if(fixture.fail){fixture.fail=false;throw Error('response lost');}
    assertBody(body);const wasEnabled=state.settings.enabled;
    if(wasEnabled&&!body.settings.enabled&&body.confirm_deactivation!==true)throw Error('Deactivation confirmation required');
    if(wasEnabled&&!body.settings.enabled){fixture.demoted.push(state.session_id);state.session_id=null;}
    if(!wasEnabled&&body.settings.enabled){fixture.activations[state.agent_id]++;state.session_id='main-'+state.agent_id+'-'+fixture.activations[state.agent_id];}
    state.settings=structuredClone(body.settings);for(const key of ['contribute','review'])state.skills[key].override=body.skill_overrides[key];state.revision++;state.session_metadata_revision++;return {configured:true};
   };
   function assertBody(body){if('context_limit_bytes'in body.settings||'reporting'in body.settings)throw Error('Removed setting sent');}
   await renderMainChat(document.querySelector('#host'),fixture.request,{agentId:'one',canManage:true});
  },light);
  assert.equal(await page.getByRole('button',{name:'Expand Main Chat',exact:true}).getAttribute('aria-expanded'),'false');
  assert.equal(await page.getByLabel('Enable Main Chat').isChecked(),false);
  await page.getByRole('button',{name:'Expand Main Chat',exact:true}).click();
  assert.equal(await page.getByLabel('Main Chat name',{exact:true}).inputValue(),'Main one');
  assert.equal(await page.locator('[data-main-chat-field="context_kib"],[data-main-chat-field="reporting"]').count(),0);
  await page.locator('details[data-main-chat-skill="contribute"] summary').click();await page.locator('details[data-main-chat-skill="review"] summary').click();
  const contribute=page.getByLabel('Contribute to Main Notes override',{exact:true}),review=page.getByLabel('Coordinate Main Notes override',{exact:true});
  assert.equal(await contribute.inputValue(),'');assert.match(await contribute.getAttribute('placeholder'),/Fallback contributor/);
  await contribute.fill('  Agent One tailored contribution instructions.  ');await page.getByLabel('Main Chat name',{exact:true}).fill('Coordinator One');await page.getByLabel('Main Notes name',{exact:true}).fill('Notes for One');await page.getByLabel('Notes write approval').selectOption('every_write');await setMainEnabled(page,true);
  assert.equal(await page.evaluate(()=>fixture.writes.length),0); // Explicit Save, including toggle changes.
  await page.evaluate(()=>fixture.fail=true);await page.getByRole('button',{name:'Save Main Chat',exact:true}).click();await page.getByRole('button',{name:'Retry save',exact:true}).waitFor();assert.equal(await contribute.isDisabled(),true);
  await page.getByRole('button',{name:'Retry save',exact:true}).click();await page.getByText('Main Chat saved. Updated skills apply on the next model call.',{exact:true}).waitFor();
  const writes=await page.evaluate(()=>fixture.writes);assert.equal(writes.length,2);assert.deepEqual(writes[0],writes[1]);assert.equal(writes[1].agent,'one');assert.equal(writes[1].body.expected_revision,7);assert.equal(writes[1].body.expected_session_revision,2);assert.equal(writes[1].body.skill_overrides.review,null);assert.equal(writes[1].body.skill_overrides.contribute,'  Agent One tailored contribution instructions.  ');
  assert.equal(await page.getByRole('link',{name:'Open Main Chat',exact:true}).getAttribute('href'),'/app?session_id=main-one-1');
  await contribute.fill('');await page.getByRole('button',{name:'Save Main Chat',exact:true}).click();await page.getByText('Main Chat saved. Updated skills apply on the next model call.',{exact:true}).waitFor();assert.equal(await page.evaluate(()=>fixture.states.one.skills.contribute.override),null);assert.match(await contribute.getAttribute('placeholder'),/Fallback contributor/);
  await page.evaluate(()=>{fixture.states.one.skills.review.fallback=null;});await page.getByRole('button',{name:'Reload settings',exact:true}).click();await page.locator('details[data-main-chat-skill="review"] .main-chat-skill-warning').waitFor();
  assert.equal(await page.getByLabel('Enable Main Chat').isEnabled(),true);assert.equal(await page.getByRole('button',{name:'Save Main Chat',exact:true}).isEnabled(),true);
  await review.fill('Agent One coordinator override');assert.equal(await page.locator('.main-chat-skill-warning:visible').count(),0);await page.getByRole('button',{name:'Save Main Chat',exact:true}).click();await page.getByText('Main Chat saved. Updated skills apply on the next model call.',{exact:true}).waitFor();
  const beforeDisable=await page.evaluate(()=>fixture.writes.length);await setMainEnabled(page,false);await page.getByRole('button',{name:'Save Main Chat',exact:true}).click();
  const confirmation=page.getByRole('alertdialog',{name:'Deactivate Main Chat?',exact:true});await confirmation.waitFor();assert.match(await confirmation.innerText(),/become a normal session/);assert.match(await confirmation.innerText(),/create a new Main Chat/);assert.equal(await page.evaluate(()=>fixture.writes.length),beforeDisable);
  await page.getByRole('button',{name:'Keep Main Chat',exact:true}).click();assert.equal(await page.getByLabel('Enable Main Chat').isChecked(),true);assert.equal(await page.evaluate(()=>fixture.writes.length),beforeDisable);
  await setMainEnabled(page,false);await page.getByRole('button',{name:'Save Main Chat',exact:true}).click();await confirmation.waitFor();await page.getByRole('button',{name:'Confirm deactivation',exact:true}).click();await page.waitForFunction(()=>fixture.states.one.settings.enabled===false);
  assert.deepEqual(await page.evaluate(()=>fixture.demoted),['main-one-1']);assert.equal(await page.evaluate(()=>fixture.writes.at(-1).body.confirm_deactivation),true);assert.equal(await page.evaluate(()=>fixture.states.one.session_id),null);assert.equal(await page.evaluate(()=>fixture.states.one.skills.review.override),'Agent One coordinator override');
  await setMainEnabled(page,true);await page.getByRole('button',{name:'Save Main Chat',exact:true}).click();await page.waitForFunction(()=>fixture.states.one.session_id==='main-one-2');assert.equal(await page.getByRole('link',{name:'Open Main Chat',exact:true}).getAttribute('href'),'/app?session_id=main-one-2');assert.deepEqual(await page.evaluate(()=>fixture.demoted),['main-one-1']);
  await page.evaluate(async()=>{document.querySelector('#host').replaceChildren();await mount(document.querySelector('#host'),fixture.request,{agentId:'two',canManage:true});});await page.getByRole('button',{name:'Expand Main Chat',exact:true}).click();assert.equal(await page.getByLabel('Main Chat name',{exact:true}).inputValue(),'Main two');assert.equal(await page.getByLabel('Enable Main Chat').isChecked(),false);assert.equal(await contribute.inputValue(),'');assert.equal(await page.evaluate(()=>fixture.states.one.skills.review.override),'Agent One coordinator override');
  await page.evaluate(async()=>{fixture.states.two.can_manage=false;document.querySelector('#host').replaceChildren();await mount(document.querySelector('#host'),fixture.request,{agentId:'two',canManage:false});});await page.getByRole('button',{name:'Expand Main Chat',exact:true}).click();assert.equal(await page.getByRole('button',{name:'Save Main Chat',exact:true}).isVisible(),false);assert.equal(await page.getByLabel('Enable Main Chat').isDisabled(),true);assert.equal(await contribute.getAttribute('readonly'),'');const before=await page.evaluate(()=>fixture.writes.length);await page.evaluate(()=>document.querySelector('form').requestSubmit());assert.equal(await page.evaluate(()=>fixture.writes.length),before);
  assert.equal(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth),true);if(mobile)assert.equal(await page.evaluate(()=>innerWidth===390&&matchMedia('(pointer: coarse)').matches),true);assert.deepEqual(errors,[]);await context.close();
 }
 console.log('Agent Main Chat Identity fixtures: collapsed row, explicit saves, agent isolation, independent skills, fallback notices, override clearing, exact retries, read-only administration, shared context, both themes and mobile');
}finally{await browser?.close();await new Promise(r=>server.close(r));}
