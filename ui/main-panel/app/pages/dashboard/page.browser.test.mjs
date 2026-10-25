import fs from 'node:fs/promises';
import http from 'node:http';
import path from 'node:path';
import assert from 'node:assert/strict';
const {chromium}=await import(process.env.PLAYWRIGHT_MODULE||'playwright');
const root=path.resolve(new URL('../../../../..',import.meta.url).pathname);
let mode='live',calls=0,delay=0;const windows=[];
const sample=()=>({schema_version:1,sampled_at_unix_ms:Date.now(),metrics:{
 cpu:{available:true,logical_processors:8,utilization_available:mode!=='warm',utilization_percent:mode==='warm'?null:24.5},
 memory:{available:true,total_bytes:8589934592,used_bytes:2147483648,utilization_percent:25,swap:{available:true,total_bytes:2147483648,free_bytes:1073741824,used_bytes:1073741824}},
 disks:{available:mode!=='missing',filesystems:[{mount:'/',usage:{available:true,available_bytes:10737418240,total_bytes:21474836480,used_bytes:5368709120,utilization_percent:25}}]},
 network:{available:true,interfaces:[
  {name:'lo',loopback:true,rx_bytes:1e9,tx_bytes:1e9,rx_errors:0,tx_errors:0,rx_dropped:0,tx_dropped:0,rates_available:true,rx_bytes_per_second:1e9,tx_bytes_per_second:1e9},
  {name:'eth0',loopback:false,rx_bytes:1073741824,tx_bytes:536870912,rx_errors:1,tx_errors:0,rx_dropped:2,tx_dropped:0,rates_available:mode!=='warm',rx_bytes_per_second:mode==='warm'?null:2048,tx_bytes_per_second:mode==='warm'?null:1024}]},
 uptime:{available:true,seconds:176400}
}});
const server=http.createServer(async(req,res)=>{
 const url=new URL(req.url,'http://localhost');
 if(url.pathname==='/api/v3/admin/system/summary'){
  calls++;res.setHeader('content-type','application/json');
  if(delay)await new Promise(resolve=>setTimeout(resolve,delay));
  if(mode==='denied'){res.statusCode=403;res.end('{}');return;}
  if(mode==='invalid'){res.end('{"schema_version":99}');return;}
  res.end(JSON.stringify(sample()));return;
 }
 if(url.pathname==='/api/v3/admin/system/dashboard'){
  windows.push(url.searchParams.get('window'));
  const point={at:1700000000,cpu:24.5,memory:25,db_ms:1.5,input_tokens:20,output_tokens:5,reported_calls:1,known_cost_usd:.12,active_runs:1};
  res.setHeader('content-type','application/json');
  res.end(JSON.stringify({schema_version:1,window:url.searchParams.get('window'),scope:{sessions_truncated:false},history:{plot_interval_seconds:120,points:[point,{...point,at:1700000120,cpu:30}]},
   database:{available:true,measurement:'Read-only probes',window_p95_ms:1.5,probes:[{name:'application',available:true,journal_mode:'wal',latency_ms:1.5}]},
   devices:{available:true,scope:'process namespace',devices:[{name:'eth0',kind:'network',state:'up'}]},
   git:{available:true,scope:'source checkout',changes:[],truncated:false},
   loop:{available:true,model_samples:2,tool_samples:1,other_samples:0,total_samples:3,measurement:'Sampled phase occupancy'},
   throughput:{available:true,output_tokens_per_second:1,calls_per_second:.1,interval_seconds:30,measurement:'Provider reported'},
   usage:{input_tokens:20,output_tokens:5,reported_calls:1,unreported_calls:0,unknown_cost_calls:0,known_cost_usd:.12,sessions_without_usage:0,session_count:1,tools:[{name:'file_read',calls:1}],agents:[{agent:'fixture',tokens:25}]},
   runtime:{load_average:'0.1 0.2 0.3',active_runs:1,concurrency:0,open_descriptors:24}
  }));return;
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
  mode='live';delay=2000;
  await page.goto('http://127.0.0.1:'+server.address().port+'/fixture');
  const cpu=page.locator('[data-card=cpu]'),system=page.locator('[data-card=system_resources]');
  await page.waitForFunction(()=>document.querySelectorAll('.dashboard-tile').length===26);
  assert.deepEqual(await page.evaluate(()=>[...new Set([...document.querySelectorAll('.dashboard-tile')].map(tile=>tile.dataset.state))]),['loading']);
  assert.equal(await cpu.locator('.dashboard-pending').evaluate(el=>getComputedStyle(el,'::before').animationName),'dashboard-spin');
  assert.notEqual(await cpu.evaluate(el=>getComputedStyle(el).borderColor),await page.evaluate(()=>{const probe=document.createElement('div');probe.className='dashboard-tile';probe.dataset.state='unavailable';document.querySelector('.dashboard-grid').append(probe);const color=getComputedStyle(probe).borderColor;probe.remove();return color;}));
  delay=0;
  await page.waitForFunction(()=>document.querySelector('[data-card=cpu]')?.dataset.state==='live');
  assert.match(await cpu.innerText(),/24.5%/);
  await page.waitForFunction(()=>document.querySelector('[data-card=agents_panel]')?.dataset.state==='live');
  assert.match(await page.locator('[data-card=agents_panel]').innerText(),/Fixture agent/);
  assert.match(await page.locator('[data-card=tokens]').innerText(),/Reported input tokens/);
  assert.match(await page.locator('[data-card=cost]').innerText(),/0.12 USD/);
  assert.match(await page.locator('[data-card=active_runs]').innerText(),/Current account across agents/);
  assert.match(await page.locator('[data-card=security]').innerText(),/current account/i);
  assert.equal(await page.locator('[data-card=user_mgmt] a').getAttribute('href'),'/app?tab=app&appView=users');
  assert.match(await page.locator('[data-card=db_latency]').innerText(),/Read-only probes/);
  assert.match(await page.locator('[data-card=memory]').innerText(),/25.0%/);
  assert.match(await page.locator('[data-card=memory]').innerText(),/swap 1.00 GiB \/ 2.00 GiB/);
  assert.match(await page.locator('[data-card=storage]').innerText(),/10.00 GiB/);
  assert.match(await page.locator('[data-card=storage]').innerText(),/5.00 GiB used of 20.00 GiB \(25.0%\)/);
  assert.match(await page.locator('[data-card=uptime]').innerText(),/2d 1h 0m/);
  const network=page.locator('[data-card=network]');
  assert.equal(await network.getAttribute('data-state'),'live');
  assert.match(await network.innerText(),/In 2.0 KiB\/s · Out 1.0 KiB\/s/);
  assert.match(await network.innerText(),/1 non-loopback interface · received 1.00 GiB · sent 0.50 GiB · errors 1 · dropped 2/);
  const health=page.locator('[data-card=health_board]');
  assert.equal(await health.getAttribute('data-state'),'live');
  for(const text of [/API readiness\s+Ready/,/CPU\s+24.5%/,/RAM\s+25.0%/,/Disk used on \/\s+25.0%/,/Database pools\s+1 \/ 1 responding/])assert.match(await health.innerText(),text);
  assert.equal(await page.locator('.dashboard-tile').count(),26);
  assert.equal(await system.getAttribute('data-state'),'partial');
  assert.equal(await system.locator('.dashboard-field[data-available="true"]').count(),8);
  assert.equal(await page.locator('[data-card=metric_chart] svg').count(),1);
  assert.equal(await page.locator('[data-card=metric_chart] svg polyline').count(),1);
  assert.equal(await page.locator('[data-card=db_latency_chart] svg').count(),1);
  for(const id of ['devices','db_mode','db_latency','loop_split','token_rate','git_changes'])assert.equal(await page.locator('[data-card='+id+']').getAttribute('data-state'),'live');
  assert.match(await page.locator('[data-card=devices]').innerText(),/eth0/);
  assert.match(await page.locator('[data-card=tool_usage]').innerText(),/file_read/);
  await page.getByLabel('Dashboard time window',{exact:true}).selectOption('5m');
  await page.waitForFunction(()=>document.querySelector('[data-card=metric_chart] svg'));
  assert.ok(windows.includes('5m'));
  await page.getByLabel('History chart metric',{exact:true}).selectOption('memory');
  assert.equal(await page.locator('[data-card=metric_chart] svg').getAttribute('aria-label'),'RAM sampled history in %');
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
  assert.equal(await network.getAttribute('data-state'),'unavailable');assert.match(await network.innerText(),/Warming up/);
  assert.equal(await health.getAttribute('data-state'),'partial');assert.match(await health.innerText(),/CPU\s+Unavailable/);
  mode='missing';
  await page.waitForFunction(()=>document.querySelector('[data-card=storage]')?.dataset.state==='unavailable');
  assert.match(await page.locator('[data-card=storage]').innerText(),/Unavailable/);
  mode='denied';
  await page.waitForFunction(()=>document.querySelector('[data-card=cpu] .dashboard-pending')?.textContent==='Administrator access required');
  assert.equal(await cpu.locator('.dashboard-value').innerText(),'—');
  assert.equal(await system.locator('[data-available="true"]').count(),0);
  assert.equal(await network.locator('.dashboard-value').innerText(),'—');
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
 console.log('PASS dashboard: neutral loading spinner, live values, network/swap/disk detail, combined health, warmup, unavailable, 403 clearing, invalid response, retry, focus, partial red, themes, mobile, hidden/dispose/detach');
}finally{await browser?.close();await new Promise(resolve=>server.close(resolve));}
