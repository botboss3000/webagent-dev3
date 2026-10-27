import http from 'node:http';
import fs from 'node:fs/promises';
import path from 'node:path';
import assert from 'node:assert/strict';
const {chromium}=await import(process.env.PLAYWRIGHT_MODULE||'playwright');
const root=path.resolve(new URL('../../../../../',import.meta.url).pathname);
const server=http.createServer(async(req,res)=>{
 try{
  const u=new URL(req.url,'http://localhost').pathname;
  if(u==='/'){res.setHeader('content-type','text/html');res.end('<!doctype html><meta name="viewport" content="width=device-width,initial-scale=1"><link rel="stylesheet" href="/shared/ui/shared/css/design-system.css"><link rel="stylesheet" href="/shared/ui/main-panel/agents/agents.css"><link rel="stylesheet" href="/ui/app.css"><main id="host" style="padding:12px;max-width:650px"></main>');return;}
  if(u==='/ui/vibrate.json'){res.setHeader('content-type','application/json');res.end('{"enabled":true,"duration_ms":17}');return;}
  const file=u.startsWith('/shared/')?path.resolve(root,'..',u.slice(8)):path.resolve(root,'.'+u);
  if(!file.startsWith(root+'/ui/')&&!file.startsWith(path.resolve(root,'../ui')+'/'))throw Error('outside');
  res.setHeader('content-type',file.endsWith('.css')?'text/css':'text/javascript');res.end(await fs.readFile(file));
 }catch{res.statusCode=404;res.end('missing');}
});
await new Promise(r=>server.listen(0,'127.0.0.1',r));let browser;
try{
 browser=await chromium.launch({headless:true,executablePath:process.env.CHROMIUM_PATH,args:['--no-sandbox']});
 for(const touch of [false,true]){
  const context=await browser.newContext({hasTouch:touch,isMobile:touch,viewport:touch?{width:390,height:844}:{width:1200,height:900}});
  const page=await context.newPage();page.setDefaultTimeout(6000);const errors=[];page.on('pageerror',e=>errors.push(e.message));
  await page.goto('http://127.0.0.1:'+server.address().port+'/');
  await page.evaluate(async()=>{
   const {renderCreateAgent}=await import('/ui/main-panel/agents/pages/create/page.js');
   const {vibrationConfigReady}=await import('/ui/shared/vibrate.js');await vibrationConfigReady;
   window.pulses=[];Object.defineProperty(navigator,'vibrate',{configurable:true,value:n=>{pulses.push(n);return true;}});
   window.calls=[];window.created=[];window.fail=true;
   const request=async(path,method,body)=>{calls.push({path,method,body});if(fail){fail=false;throw Error('Temporarily unavailable');}return {id:'agent-x',name:body.name,assignment_required:true};};
   renderCreateAgent(document.getElementById('host'),request,{onCreated:async value=>{created.push(value);}});
  });
  const submit=page.getByRole('button',{name:'Create agent',exact:true});
  const press=()=>touch?submit.tap():submit.click();
  await page.getByLabel(/^Name/).fill('   ');await press();
  assert.equal(await page.getByRole('status').innerText(),'Enter a name for the agent.');
  assert.equal(await page.evaluate(()=>calls.length),0);
  await page.getByLabel(/^Name/).fill('  Research assistant ');
  await press();
  await page.getByText('Agent not created: Temporarily unavailable').waitFor();
  await press();
  await page.getByText('Created Research assistant.').waitFor();
  const calls=await page.evaluate(()=>calls);
  assert.deepEqual(calls.map(call=>[call.path,call.method,call.body.name,call.body.description]),[['/models/agents','POST','Research assistant',null],['/models/agents','POST','Research assistant',null]]);
  assert.equal(calls[0].body.client_request_id,calls[1].body.client_request_id,'an unchanged retry reuses its request ID');
  assert.deepEqual(await page.evaluate(()=>created),[{id:'agent-x',name:'Research assistant',assignment_required:true}]);
  assert.equal(await page.getByLabel(/^Name/).inputValue(),'','the form resets after creation');
  await page.evaluate(()=>{fail=true;});
  await page.getByLabel(/^Name/).fill('Writer');await press();
  await page.getByText('Agent not created: Temporarily unavailable').waitFor();
  await page.getByLabel(/^Description/).fill('Drafts posts');await press();
  await page.getByText('Created Writer.').waitFor();
  const ids=(await page.evaluate(()=>calls)).map(call=>call.body.client_request_id);
  assert.equal(new Set(ids).size,3,'a new agent and a changed draft each get a new request ID');
  assert.equal((await page.evaluate(()=>calls)).at(-1).body.description,'Drafts posts');
  assert.equal(await submit.evaluate(el=>getComputedStyle(el).webkitTapHighlightColor),'rgba(0, 0, 0, 0)');
  if(touch)assert.ok((await page.evaluate(()=>pulses)).length>0,'touch submit vibrates');
  assert.equal(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth),true,'no horizontal overflow');
  assert.deepEqual(errors,[]);
  await context.close();
 }
 console.log('PASS create agent: required name, trimmed body, idempotent retry, new ID per draft, reset after create, tap highlight, touch vibration, no overflow');
}finally{await browser?.close();server.close();}
