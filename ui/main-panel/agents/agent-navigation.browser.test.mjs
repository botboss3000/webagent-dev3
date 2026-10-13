const {chromium}=await import(process.env.PLAYWRIGHT_MODULE||'playwright');
import fs from 'node:fs/promises';
import http from 'node:http';
import assert from 'node:assert/strict';
const source=await fs.readFile(process.env.AGENT_NAVIGATION_SOURCE||new URL('../../app.js',import.meta.url),'utf8');
// Run the shell's actual card, route, and remount functions; replace only backend/page collaborators.
const slice=(start,end)=>source.slice(source.indexOf(start),source.indexOf(end,source.indexOf(start)));
const shell=slice('function renderAgentCards(){','async function loadAgentRoster(){')
 +slice('function disposeSessionsPage(){',"for(const page of [configPage,...agentPages]){")
 +(source.includes('async function mountAgentModels(){')?slice('async function mountAgentModels(){','// Registration mirrors'):'');
const setup=`
const $=id=>document.getElementById(id);
let agentId='one',sessionsScope=true,sessionsPage=null,sessionsGeneration=0,selectionTable=null,agentSelectionQueue=Promise.resolve();
const agentRoster=[{id:'one',name:'One'},{id:'two',name:'Two'}];
const configPage={id:'config',panelId:'config-page'},sessionsDescriptor={id:'sessions',title:'Sessions',panelId:'sessions-section'};
const agentPages=['model','identity','abilities'].map(id=>({id,panelId:id+'-page'})),agentTabOrder=['config','model','identity','abilities'];
const accountMe={is_admin:true},activeMainPage='agents',api=async()=>({}),chat={sessionId:'keep'};
let explicitAssignment=false;
const note=text=>{if(text)window.fixture.errors.push(text);};
const guard=fn=>async event=>{event?.preventDefault();try{await fn(event);note('');}catch(error){note(error.message);}};
const renderIdentity=async(host,request,{agentId})=>{fixture.identityRenders.push(agentId);host.replaceChildren();await new Promise(r=>setTimeout(r,10));const name=document.createElement('p');name.textContent='Identity '+agentId;host.append(name);};
const renderAbilities=async(host,request,{agentId})=>{host.textContent='Abilities '+agentId;};
window.fixture={errors:[],mounts:[],destroyed:[],sessionsDestroyed:0,identityRenders:[]};
const mountSelectionTable=async(host,{agentId})=>{
 await new Promise(resolve=>setTimeout(resolve,150));
 fixture.mounts.push(agentId);host.textContent='Models '+agentId;
 return {destroy(){fixture.destroyed.push(agentId);host.replaceChildren();}};
};
const mountSessionsPage=async host=>{
 const footer=document.createElement('footer');footer.id='sessions-footer-dock';$('tab-agents').append(footer);
 return {destroy(){fixture.sessionsDestroyed++;footer.remove();}};
};
`;
const html=`<!doctype html><main id="tab-agents"><div id="agents-grid"><div class="agents-squares"></div><div id="agents-detail-region" hidden><div class="agent-row"></div><div class="agent-card-tabs"></div><div id="config-page">Config</div><div id="model-page"><div class="ac-model-table"></div><div id="selection-model-table"></div><div id="settings"></div></div><div id="identity-page"></div><div id="abilities-page"></div></div><div id="sessions-region"><section id="sessions-section"></section></div></div></main><textarea id="draft">Unsaved draft</textarea><script type="module" src="/navigation.js"></script>`;
const module=setup+shell+`
for(const id of ['config','model','identity','abilities']){
 const tab=document.createElement('button');tab.id='agent-tab-'+id;tab.textContent=id;tab.onclick=guard(()=>showAgentView(id));document.querySelector('.agent-card-tabs').append(tab);
}
await showSessions('replace');`;
const server=http.createServer((req,res)=>{res.setHeader('content-type',req.url==='/navigation.js'?'text/javascript':'text/html');res.end(req.url==='/navigation.js'?module:html);});
await new Promise(r=>server.listen(0,'127.0.0.1',r));
let browser;
try{
 browser=await chromium.launch({executablePath:process.env.CHROMIUM_PATH||'/snap/bin/chromium',headless:true,args:['--no-sandbox']});
 const page=await browser.newPage();page.setDefaultTimeout(5000);
 await page.goto('http://127.0.0.1:'+server.address().port+'/app?tab=agents&agentScope=sessions&agentView=sessions&session_id=keep');
 await page.getByRole('button',{name:'Configure One',exact:true}).waitFor();
 const origin=await page.evaluate(()=>performance.timeOrigin);
 await page.getByRole('button',{name:'Configure One',exact:true}).click();
 await page.waitForFunction(()=>document.querySelector('#agent-tab-config').getAttribute('aria-selected')==='true');
 assert.equal(await page.evaluate(()=>performance.timeOrigin),origin);
 assert.equal(await page.locator('#agents-detail-region').isVisible(),true);
 assert.equal(await page.locator('#sessions-region').isVisible(),false);
 assert.equal(await page.locator('#sessions-footer-dock').count(),0);
 assert.equal(new URL(page.url()).searchParams.has('agentScope'),false);
 assert.equal(new URL(page.url()).searchParams.get('agentView'),'config');
 assert.equal(new URL(page.url()).searchParams.get('session_id'),'keep');
 await page.waitForFunction(()=>fixture.mounts.length===1);
 await page.locator('#agent-tab-model').click();
 assert.equal(await page.locator('#selection-model-table').innerText(),'Models one');
 await page.getByRole('button',{name:'Configure Two',exact:true}).click();
 await page.waitForFunction(()=>document.querySelector('#selection-model-table').textContent==='Models two');
 assert.equal(new URL(page.url()).searchParams.get('agentView'),'model');
 await page.locator('#agent-tab-identity').click();
 await page.waitForFunction(()=>document.querySelector('#identity-page').textContent==='Identity two');
 await page.getByRole('button',{name:'Sessions',exact:true}).click();
 await page.getByRole('button',{name:'Configure One',exact:true}).press('Enter');
 await page.locator('#agent-tab-identity').click();
 await page.waitForFunction(()=>fixture.mounts.length===3&&document.querySelector('#identity-page').textContent.includes('Identity one'));
 assert.deepEqual(await page.evaluate(()=>fixture.identityRenders),['two','one']);
 // Rapid selections finish on the last card with matching model handlers.
 await page.getByRole('button',{name:'Configure Two',exact:true}).click();
 await page.getByRole('button',{name:'Configure One',exact:true}).click();
 await page.waitForFunction(()=>fixture.mounts.length===5);
 assert.equal(new URL(page.url()).searchParams.get('agent'),'one');
 assert.equal(await page.locator('#selection-model-table').innerText(),'Models one');
 assert.equal(await page.evaluate(()=>performance.timeOrigin),origin);
 assert.equal(await page.locator('#draft').inputValue(),'Unsaved draft');
 const state=await page.evaluate(()=>fixture);
 assert.deepEqual(state.errors,[]);
 assert.deepEqual(state.mounts,['one','two','one','two','one']);
 assert.deepEqual(state.destroyed,['one','two','one','two']);
 assert.equal(state.sessionsDestroyed,2);
 console.log('PASS agent cards: Sessions exit, same/different agent, keyboard, model/identity rebinding, rapid clicks, preserved draft/conversation, no document navigation');
}finally{await browser?.close();server.close();}
