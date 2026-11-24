import http from 'node:http';
import fs from 'node:fs/promises';
import path from 'node:path';
import assert from 'node:assert/strict';
const {chromium}=await import(process.env.PLAYWRIGHT_MODULE||'playwright');
// BRIDGE-SHARING: a bridge model holding a role carries a standing warning on the agent Model
// page, stronger on an agent other people use; other models carry none. Phone and desktop.
const root=path.resolve(new URL('../../../',import.meta.url).pathname);
const server=http.createServer(async(req,res)=>{
 try{
  const u=new URL(req.url,'http://localhost').pathname;
  if(u==='/'){res.setHeader('content-type','text/html');res.end('<!doctype html><meta name="viewport" content="width=device-width,initial-scale=1"><link rel="stylesheet" href="/ui/shared/css/design-system.css"><link rel="stylesheet" href="/ui/shared/models/model-table.css"><link rel="stylesheet" href="/ui/shared/models/reference.css"><main id="host" style="padding:12px;max-width:650px"></main>');return;}
  if(u.startsWith('/config/')){res.setHeader('content-type','application/json');res.end('{}');return;}
  const file=path.resolve(root,'.'+u);
  if(!file.startsWith(path.join(root,'ui')+'/'))throw Error('outside');
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
  const mount=(standard,shared)=>page.evaluate(async([standard,shared])=>{
   const {mountModelTable}=await import('/ui/shared/models/model-table.js');
   const host=document.getElementById('host');window.table?.destroy?.();host.replaceChildren();
   const presets={opencode_bridge:{name:'OpenCode Bridge',base_url:'bridge://opencode',validated:true,efforts:[],models:[]},
    deepseek:{name:'DeepSeek',base_url:'https://api.deepseek.com',validated:true,efforts:[],models:[]}};
   const roster=[{entry_id:'b',provider:'opencode_bridge',model:'opencode-go/fixture',display_name:'Fixture bridge'},
    {entry_id:'d',provider:'deepseek',model:'deepseek-flash',display_name:'Fixture direct'}];
   const adapter={loadPresets:async()=>presets,loadConfig:async()=>({roster,role_assignments:{standard}}),
    bridgeClients:async()=>({clients:[]}),fetchModelUsage:async()=>({}),saveRoster:async()=>({})};
   window.table=mountModelTable(host,{adapter,sharedAgent:shared,agentLocalRoster:true,fetchFn:async()=>new Response('{}')});
   await window.table.reload();
  },[standard,shared]);
  const standing=()=>page.locator('.ac-model-bridge-sharing-standing');
  await mount('b',false);
  assert.equal(await standing().count(),1,'a bridge model in a role shows the warning');
  assert.match(await standing().textContent(),/In use: Fixture bridge\. Bridge models are recommended for personal use only/);
  assert.equal(await standing().evaluate(el=>el.classList.contains('is-shared')),false);
  await mount('d',false);
  assert.equal(await standing().count(),0,'no warning while only a non-bridge model holds a role');
  await mount('b',true);
  assert.equal(await standing().evaluate(el=>el.classList.contains('is-shared')),true,'stronger on a shared agent');
  assert.match(await standing().textContent(),/This agent is open to other people or website visitors/);
  // Stacked in the page at both widths: no sideways scrolling, the text wraps inside.
  const box=await standing().boundingBox();
  assert.ok(box.width<=page.viewportSize().width-12,'warning fits the width');
  assert.ok(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth),'no horizontal scroll');
  assert.deepEqual(errors,[]);
  await context.close();
 }
}finally{await browser?.close();server.close();}
