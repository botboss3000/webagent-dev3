import http from 'node:http';
import fs from 'node:fs/promises';
import path from 'node:path';
import assert from 'node:assert/strict';
const {chromium}=await import(process.env.PLAYWRIGHT_MODULE||'playwright');
const root=path.resolve(new URL('../../../../../',import.meta.url).pathname);
const server=http.createServer(async(req,res)=>{try{if(req.url==='/'){res.setHeader('content-type','text/html');res.end('<!doctype html><meta name="viewport" content="width=device-width,initial-scale=1"><main id="host"></main>');return;}const file=path.resolve(root,'.'+new URL(req.url,'http://localhost').pathname);if(!file.startsWith(root+'/ui/'))throw Error('bad');res.setHeader('content-type','text/javascript');res.end(await fs.readFile(file));}catch{res.statusCode=404;res.end('missing');}});
await new Promise(r=>server.listen(0,'127.0.0.1',r));let browser;
try{
 browser=await chromium.launch({headless:true,executablePath:process.env.CHROMIUM_PATH||'/snap/bin/chromium',args:['--no-sandbox']});
 for(const mobile of [false,true]){
  const page=await browser.newPage({viewport:mobile?{width:390,height:844}:{width:1200,height:850}});const errors=[];page.on('pageerror',e=>errors.push(e.message));
  await page.goto('http://127.0.0.1:'+server.address().port+'/');
  await page.evaluate(async()=>{
   const {renderMainChat}=await import('/ui/main-panel/agents/pages/identity/main-chat.js');
   const state={revision:7,session_metadata_revision:2,session_id:'stable-main',review_status:'active',settings:{enabled:true,chat_name:'Main Chat',notes_name:'Main Notes',context_limit_bytes:262144,write_mode:'new_topics',reporting:'meaningful'}};
   window.fixture={state,writes:[],fail:true};
   const request=async(url,method='GET',body)=>{if(url!=='/main-chat')throw Error('wrong route');if(method==='GET')return structuredClone(state);fixture.writes.push(structuredClone(body));if(fixture.fail){fixture.fail=false;throw Error('response lost');}state.settings=structuredClone(body.settings);state.revision++;state.session_metadata_revision++;return {configured:true,session_id:state.session_id};};
   await renderMainChat(document.querySelector('#host'),request);
  });
  assert.equal(await page.getByLabel('Main Chat name',{exact:true}).inputValue(),'Main Chat');
  await page.getByLabel('Main Chat name',{exact:true}).fill('Coordinator');await page.getByLabel('Main Notes name',{exact:true}).fill('My Notes');
  await page.getByLabel('Notes write approval').selectOption('every_write');await page.getByLabel('Context limit (KiB)').fill('128');await page.getByLabel('Enable Main Chat').uncheck();
  await page.getByRole('button',{name:'Save Main Chat',exact:true}).click();await page.getByRole('button',{name:'Retry save'}).waitFor();
  assert.equal(await page.getByLabel('Main Chat name',{exact:true}).isDisabled(),true);
  await page.getByRole('button',{name:'Retry save'}).click();await page.getByText('Main Chat saved',{exact:true}).waitFor();
  const writes=await page.evaluate(()=>fixture.writes);assert.equal(writes.length,2);assert.deepEqual(writes[0],writes[1]);assert.equal(writes[1].expected_revision,7);assert.equal(writes[1].expected_session_revision,2);assert.equal(writes[1].settings.context_limit_bytes,131072);assert.equal(writes[1].settings.enabled,false);assert.equal(writes[1].settings.write_mode,'every_write');
  assert.equal(await page.getByRole('link',{name:'Open Main Chat',exact:true}).getAttribute('href'),'/app?session_id=stable-main');
  assert.equal(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth),true);assert.deepEqual(errors,[]);await page.close();
 }
 console.log('Main Chat Identity: names, toggle, policy, metadata CAS, exact retry and responsive fixtures passed');
}finally{await browser?.close();await new Promise(r=>server.close(r));}
