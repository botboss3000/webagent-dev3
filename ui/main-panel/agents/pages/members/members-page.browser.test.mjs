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
   const {renderMembers}=await import('/ui/main-panel/agents/pages/members/page.js');
   window.fixture={sessions:[{id:'quiet',title:'Quiet chat',can_join:true,help_pending:false},{id:'help',title:'Needs staff',can_join:true,help_pending:true,help_requested_at:1793440000}],joins:[],denied:false};
   const request=async(url,method='GET',body)=>{
    if(url==='/agents/one/members')return {capability_levels:{order:['member'],levels:{member:{name:'Member'}}},owner_id:'owner',members:[{user_id:'owner',username:'owner',capability_level_id:'member',relationship:'owner'},{user_id:'gone',username:'gone',capability_level_id:'member',relationship:'caller',access_origin:'none'}]};
    if(url==='/agents/one/shared-sessions'){if(fixture.denied)throw Error('No access');return {sessions:fixture.sessions};}
    if(url==='/sessions/help/join'&&method==='POST'){fixture.joins.push(body);throw Error('fixture join stop');}
    throw Error('Unscoped route '+url);
   };
   window.remount=()=>renderMembers(document.querySelector('#host'),request,{agentId:'one'});await remount();
  });
  const sections=page.locator('.members-section');
  assert.deepEqual(await sections.locator('.members-section-title').allTextContents(),['Help requests','All members']);
  const help=sections.first();
  // Only pending requests become rows, one per notification.
  assert.equal(await help.locator('tbody tr').count(),1);
  assert.equal(await help.locator('.members-section-count').textContent(),'1');
  assert.equal(await help.locator('tbody tr a').getAttribute('href'),'/app?session_id=help');
  assert.equal(await help.locator('tbody tr .members-status').textContent(),'Needs a human');
  await help.getByRole('button',{name:'Join'}).click();
  assert.equal(await page.evaluate(()=>fixture.joins.length),1);
  assert.match(await page.getByRole('status').textContent(),/Join failed: fixture join stop/);
  await page.evaluate(async()=>{fixture.denied=true;await remount();});
  assert.equal(await help.locator('tbody tr').count(),0);
  assert.equal(await help.locator('.members-empty').textContent(),'Help requests unavailable: No access');
  assert.equal(await sections.nth(1).locator('tbody tr:visible').count(),1);
  // Users without access are hidden until asked for, and left out of level counts.
  const toggle=sections.nth(1).getByRole('button',{name:'Show inactive (1)'});await toggle.click();
  assert.equal(await sections.nth(1).locator('tbody tr:visible').count(),2);
  await sections.nth(1).getByRole('button',{name:'Hide inactive'}).click();
  assert.equal(await sections.nth(1).locator('tbody tr:visible').count(),1);
  // V2-style level summary counts loaded members; the loading skeleton is gone.
  assert.equal(await page.locator('.members-level-card .members-level-meta').textContent(),'1 member');
  assert.equal(await page.locator('.members-skeleton').count(),0);
  assert.equal(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth),true);
  assert.deepEqual(errors,[]);await context.close();
 }
 console.log('Members help requests: pending-only rows above the members table, join request and failure status, unavailable fallback, touch and desktop widths.');
}finally{await browser?.close();await new Promise(r=>server.close(r));}
