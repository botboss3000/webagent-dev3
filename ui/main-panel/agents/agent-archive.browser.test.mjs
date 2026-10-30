import http from 'node:http';
import fs from 'node:fs/promises';
import path from 'node:path';
import assert from 'node:assert/strict';
const {chromium}=await import(process.env.PLAYWRIGHT_MODULE||'playwright');
const root=path.resolve(new URL('../../../',import.meta.url).pathname);
const html='<!doctype html><meta name="viewport" content="width=device-width,initial-scale=1"><link rel="stylesheet" href="/shared/ui/main-panel/agents/agents.css"><link rel="stylesheet" href="/ui/main-panel/agents/agents.css">'
 +'<main id="tab-agents"><div id="agents-grid" class="carousel"><div class="agents-top-row"><div class="agents-squares"></div><div id="agents-toolbar"><div id="agents-toolbar-left"><button type="button" id="agents-back-btn" style="display:none;">Back</button></div>'
 +'<div id="agents-toolbar-actions"><button type="button" id="agents-restore-btn" style="display:none;"><span>Restore</span></button></div></div></div>'
 +'<div id="agents-detail-region" class="open">Detail</div></div><p id="notice"></p></main>';
const server=http.createServer(async(req,res)=>{
 try{
  const u=new URL(req.url,'http://localhost').pathname;
  if(u==='/'){res.setHeader('content-type','text/html');res.end(html);return;}
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
   const {createAgentArchive}=await import('/ui/main-panel/agents/agent-archive.js');
   const {vibrationConfigReady}=await import('/ui/shared/vibrate.js');await vibrationConfigReady;
   window.pulses=[];Object.defineProperty(navigator,'vibrate',{configurable:true,value:n=>{pulses.push(n);return true;}});
   window.roster=[{id:'shared_default',name:'WebAgent',can_manage:true,revision:1},{id:'a1',name:'Alpha',can_manage:true,revision:3},{id:'a2',name:'Beta',can_manage:false,revision:1},{id:'a3',name:'Gamma',can_manage:true,revision:5}];
   window.archived=[];window.calls=[];window.changes=[];window.failGamma=true;
   const request=async(path,method='GET',body)=>{
    calls.push({path,method,body});
    if(path==='/models/agents?archived=true')return {agents:archived};
    const id=decodeURIComponent(path.split('/')[3]);
    if(path.endsWith('/remove')){if(id==='a3'&&failGamma){failGamma=false;throw Error('Agent changed; reload and try again');}
     const agent=roster.find(item=>item.id===id);roster=roster.filter(item=>item.id!==id);archived.push({...agent,status:'archived',revision:agent.revision+1});return {status:'archived'};}
    if(method==='DELETE'){archived=archived.filter(item=>item.id!==id);return {agent_id:id,deleted:true};}
    const agent=archived.find(item=>item.id===id);archived=archived.filter(item=>item.id!==id);roster.push({...agent,status:'active',revision:agent.revision+1});return {status:'active'};
   };
   const grid=document.getElementById('agents-grid');
   const rerender=()=>{
    const squares=grid.querySelector('.agents-squares');squares.replaceChildren();
    if(archive.open){archive.render(squares);return;}
    for(const agent of roster){const row=document.createElement('div');row.className='agent-row';const card=document.createElement('div');card.className='agent-card agent-square';card.setAttribute('role','button');card.setAttribute('aria-label','Configure '+agent.name);card.textContent=agent.name;card.onclick=()=>calls.push({configure:agent.id});row.append(card);archive.decorate(row,agent);squares.append(row);}
    archive.sync();
   };
   const archive=createAgentArchive({grid,request,roster:()=>roster,rerender,notify:text=>{document.getElementById('notice').textContent=text;},onChanged:async(ids,verb)=>{changes.push([verb,...ids]);},canUseArchive:()=>true});
   window.archive=archive;rerender();
  });
  const press=locator=>touch?locator.tap():locator.click();
  const box=name=>page.getByRole('checkbox',{name:'Select '+name});
  const remove=page.locator('#agents-remove-btn');
  assert.deepEqual(await page.getByRole('checkbox').evaluateAll(boxes=>boxes.map(box=>box.getAttribute('aria-label'))),['Select Alpha','Select Beta','Select Gamma'],'every non-default agent is selectable');
  assert.equal(await remove.isVisible(),false,'Remove appears only with a selection');
  await press(box('Alpha'));await press(box('Gamma'));
  assert.equal(await page.evaluate(()=>calls.some(call=>call.configure)),false,'ticking a square does not open it');
  assert.equal(await box('Alpha').getAttribute('aria-checked'),'true');
  await press(remove);
  await page.waitForFunction(()=>document.getElementById('agents-remove-btn').dataset.state==='warning');
  assert.equal(await page.evaluate(()=>calls.length),0,'the first press only arms Remove');
  await press(remove);
  await page.getByText('Not removed — Gamma: Agent changed; reload and try again').waitFor();
  assert.deepEqual(await page.evaluate(()=>calls.filter(call=>call.path.endsWith('/remove')).map(call=>[call.method,call.path,call.body.expected_revision])),[['POST','/models/agents/a1/remove',3],['POST','/models/agents/a3/remove',5]]);
  assert.deepEqual(await page.evaluate(()=>changes),[['removed','a1']]);
  assert.equal(await box('Gamma').getAttribute('aria-checked'),'true','a failed remove stays selected');
  assert.equal(await box('Alpha').count(),0);
  await press(box('Gamma'));
  assert.equal(await remove.isVisible(),false);
  await press(page.getByRole('button',{name:'Archived',exact:true}));
  await page.getByText('Alpha').waitFor();
  assert.equal(await page.locator('#agents-detail-region').isVisible(),false,'the archived view hides the detail regions');
  const unarchive=page.locator('#agents-unarchive-btn'),permanentDelete=page.locator('#agents-delete-permanent-btn');
  assert.equal(await unarchive.isDisabled(),true);assert.equal(await permanentDelete.isVisible(),false,'Delete appears only with an archived selection');
  await press(page.locator('.agent-card',{hasText:'Alpha'}));
  assert.equal(await box('Alpha').getAttribute('aria-checked'),'true','pressing an archived square selects it');
  await press(unarchive);
  await page.getByText('No archived agents.').waitFor();
  assert.deepEqual(await page.evaluate(()=>calls.filter(call=>call.path.endsWith('/unarchive')).map(call=>[call.method,call.path,call.body.expected_revision])),[['POST','/models/agents/a1/unarchive',4]]);
  assert.deepEqual(await page.evaluate(()=>changes),[['removed','a1'],['unarchived','a1']]);
  await page.evaluate(()=>{const gamma=roster.find(item=>item.id==='a3');roster=roster.filter(item=>item.id!=='a3');archived.push({...gamma,status:'archived',revision:6});});
  await press(page.locator('#agents-back-btn'));await press(page.getByRole('button',{name:'Archived',exact:true}));
  await press(page.locator('.agent-card',{hasText:'Gamma'}));
  await press(permanentDelete);
  await page.waitForFunction(()=>document.getElementById('agents-delete-permanent-btn').dataset.state==='warning');
  assert.equal(await page.evaluate(()=>calls.filter(call=>call.method==='DELETE').length),0,'the first press only arms the permanent delete');
  await press(permanentDelete);
  await page.getByText('No archived agents.').waitFor();
  assert.deepEqual(await page.evaluate(()=>calls.filter(call=>call.method==='DELETE').map(call=>[call.path,call.body.expected_revision])),[['/models/agents/a3',6]]);
  assert.deepEqual(await page.evaluate(()=>changes.at(-1)),['deleted','a3']);
  await press(page.locator('#agents-back-btn'));
  await page.getByRole('checkbox',{name:'Select Alpha'}).waitFor();
  assert.equal(await page.locator('#agents-detail-region').isVisible(),true);
  assert.equal(await remove.evaluate(el=>getComputedStyle(el).webkitTapHighlightColor),'rgba(0, 0, 0, 0)');
  assert.equal(await box('Alpha').evaluate(el=>getComputedStyle(el).webkitTapHighlightColor),'rgba(0, 0, 0, 0)');
  if(touch)assert.ok((await page.evaluate(()=>pulses)).length>0,'touch presses vibrate');
  assert.equal(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth),true,'no horizontal overflow');
  assert.deepEqual(errors,[]);
  await context.close();
 }
 console.log('PASS agent archive: selectable agents, two-step remove with revisions, failures stay selected, archived view hides detail, unarchive, two-step permanent delete, tap highlight, touch vibration, no overflow');
}finally{await browser?.close();server.close();}
