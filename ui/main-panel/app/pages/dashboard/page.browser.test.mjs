import fs from 'node:fs/promises';
import http from 'node:http';
import path from 'node:path';
import assert from 'node:assert/strict';
const {chromium}=await import(process.env.PLAYWRIGHT_MODULE||'playwright');
const root=path.resolve(new URL('../../../../..',import.meta.url).pathname);
let mode='live',calls=0;
const sample=()=>({schema_version:1,sampled_at_unix_ms:Date.now(),metrics:{
 cpu:{available:true,logical_processors:8,utilization_available:mode!=='warm',utilization_percent:mode==='warm'?null:24.5},
 memory:{available:true,total_bytes:8589934592,used_bytes:2147483648,utilization_percent:25},
 disks:{available:mode!=='missing',filesystems:[{mount:'/',usage:{available:true,available_bytes:10737418240}}]},
 uptime:{available:true,seconds:176400}
}});
const server=http.createServer(async(req,res)=>{
 const url=new URL(req.url,'http://localhost');
 if(url.pathname==='/api/v3/admin/system/summary'){
  calls++;res.setHeader('content-type','application/json');
  if(mode==='denied'){res.statusCode=403;res.end('{}');return;}
  if(mode==='invalid'){res.end('{"schema_version":99}');return;}
  res.end(JSON.stringify(sample()));return;
 }
 const activity={
  '/api/v3/models/agents':{agents:[{id:'fixture',name:'Fixture agent',status:'active'}]},
  '/api/v3/sessions/stats':{sessions:[{agent_id:'fixture',message_count:3,usage:{input_tokens:20,output_tokens:5,reported_calls:1,unreported_calls:0,known_cost_usd:.12,unknown_cost_calls:0}}],next_offset:null},
  '/api/v3/admin/access/accounts':{accounts:[{role:'administrator',status:'active'}]},
  '/api/v3/auth/sessions':{sessions:[{current:true}]},
  '/api/v3/status':{model:'fixture-model',runs:[{id:'run',status:'running',context_controls:{trigger_tokens:100,tail_tokens:20}}]},
  '/health/ready':{ready:true}
 };
 if(activity[url.pathname]){res.setHeader('content-type','application/json');res.end(JSON.stringify(activity[url.pathname]));return;}
 if(url.pathname==='/fixture'){
  res.setHeader('content-type','text/html');res.end('<!doctype html><meta name="viewport" content="width=device-width,initial-scale=1"><style>body{margin:8px;background:var(--bg-elev);color:var(--fg-1)}</style><section id="dashboard"></section><script type="module">import {mountDashboard} from "/ui/main-panel/app/pages/dashboard/page.js";window.mount=()=>mountDashboard(document.querySelector("#dashboard"));window.dispose=await window.mount();</script>');return;
 }
 try{
  const file=path.resolve(root,'.'+url.pathname);if(!file.startsWith(root+'/'))throw Error('outside');
  res.setHeader('content-type',file.endsWith('.js')?'text/javascript':file.endsWith('.css')?'text/css':'application/json');
  res.end(await fs.readFile(file));
 }catch{res.statusCode=404;res.end('{}');}
});
await new Promise(resolve=>server.listen(0,'127.0.0.1',resolve));
let browser;
try{
 browser=await chromium.launch({executablePath:process.env.CHROMIUM_PATH||'/snap/bin/chromium',headless:true,args:['--no-sandbox']});
 for(const mobile of [false,true]){
  const context=await browser.newContext({viewport:mobile?{width:390,height:844}:{width:1280,height:900},isMobile:mobile,hasTouch:mobile});
  const page=await context.newPage();const errors=[];page.on('pageerror',e=>errors.push(e.message));
  mode='live';
  await page.goto('http://127.0.0.1:'+server.address().port+'/fixture');
  const cpu=page.locator('[data-card=cpu]'),system=page.locator('[data-card=system_resources]');
  await page.waitForFunction(()=>document.querySelector('[data-card=cpu]')?.dataset.state==='live');
  assert.match(await cpu.innerText(),/24.5%/);
  await page.waitForFunction(()=>document.querySelector('[data-card=agents_panel]')?.dataset.state==='live');
  assert.match(await page.locator('[data-card=agents_panel]').innerText(),/Fixture agent/);
  assert.match(await page.locator('[data-card=tokens]').innerText(),/Reported input tokens/);
  assert.match(await page.locator('[data-card=cost]').innerText(),/\$0.1200/);
  assert.match(await page.locator('[data-card=active_runs]').innerText(),/Default agent only/);
  assert.match(await page.locator('[data-card=security]').innerText(),/Current account only/);
  assert.equal(await page.locator('[data-card=user_mgmt] a').getAttribute('href'),'/app?tab=app&appView=users');
  assert.match(await page.locator('[data-card=db_latency]').innerText(),/Backend work needed/);
  assert.match(await page.locator('[data-card=memory]').innerText(),/25.0%/);
  assert.match(await page.locator('[data-card=storage]').innerText(),/10.00 GiB/);
  assert.match(await page.locator('[data-card=uptime]').innerText(),/2d 1h 0m/);
  assert.equal(await page.locator('.dashboard-tile').count(),25);
  assert.equal(await system.getAttribute('data-state'),'partial');
  assert.equal(await system.locator('.dashboard-field[data-available="true"]').count(),4);
  assert.match(await page.locator('[data-card=metric_chart]').innerText(),/Backend work needed/);
  for(const theme of ['dark','light']){
   await page.evaluate(theme=>{document.documentElement.style.setProperty('--fg-1',theme==='dark'?'#eee':'#111');document.documentElement.style.setProperty('--bg-elev',theme==='dark'?'#222':'#fff');},theme);
   assert.notEqual(await cpu.evaluate(el=>getComputedStyle(el).color),await system.evaluate(el=>getComputedStyle(el).color));
   assert.equal(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth),true);
  }
  if(mobile)assert.equal(await page.evaluate(()=>matchMedia('(pointer:coarse)').matches&&matchMedia('(hover:none)').matches),true);
  await page.getByRole('button',{name:'Edit layout',exact:true}).click();
  const move=cpu.getByRole('button',{name:'Move earlier: CPU',exact:true});
  await move.focus();mode='warm';
  await page.waitForFunction(()=>document.querySelector('[data-card=cpu]')?.dataset.state==='unavailable');
  assert.match(await cpu.innerText(),/Warming up/);assert.equal(await move.evaluate(el=>el===document.activeElement),true);
  mode='missing';
  await page.waitForFunction(()=>document.querySelector('[data-card=storage]')?.dataset.state==='unavailable');
  assert.match(await page.locator('[data-card=storage]').innerText(),/Unavailable/);
  mode='denied';
  await page.waitForFunction(()=>document.querySelector('[data-card=cpu] .dashboard-pending')?.textContent==='Administrator access required');
  assert.equal(await cpu.locator('.dashboard-value').innerText(),'—');
  assert.equal(await system.locator('[data-available="true"]').count(),0);
  mode='invalid';
  await page.waitForFunction(()=>document.querySelector('[data-card=cpu] .dashboard-pending')?.textContent==='Invalid system metrics response');
  mode='live';
  await page.waitForFunction(()=>document.querySelector('[data-card=cpu]')?.dataset.state==='live');
  await page.evaluate(()=>document.querySelector('#dashboard').hidden=true);
  const hiddenCalls=calls;await page.waitForTimeout(5300);assert.equal(calls,hiddenCalls);
  await page.evaluate(()=>document.querySelector('#dashboard').hidden=false);
  await page.waitForFunction(()=>document.querySelector('[data-card=cpu]')?.dataset.state==='live');
  await page.evaluate(()=>window.dispose());
  const stopped=calls;await page.waitForTimeout(5300);assert.equal(calls,stopped);
  await page.evaluate(()=>window.mount());await page.waitForFunction(()=>document.querySelector('[data-card=cpu]')?.dataset.state==='live');
  await page.evaluate(()=>document.querySelector('#dashboard').remove());
  const detached=calls;await page.waitForTimeout(5300);assert.equal(calls,detached);
  assert.deepEqual(errors,[]);await context.close();
 }
 console.log('PASS dashboard: live values, warmup, unavailable, 403 clearing, invalid response, retry, focus, partial red, themes, mobile, hidden/dispose/detach');
}finally{await browser?.close();await new Promise(resolve=>server.close(resolve));}
