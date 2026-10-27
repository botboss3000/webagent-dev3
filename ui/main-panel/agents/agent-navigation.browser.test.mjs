const {chromium}=await import(process.env.PLAYWRIGHT_MODULE||'playwright');
import fs from 'node:fs/promises';
import http from 'node:http';
import assert from 'node:assert/strict';
const source=await fs.readFile(process.env.AGENT_NAVIGATION_SOURCE||new URL('../../app.js',import.meta.url),'utf8');
// Run the shell's actual card, route, and remount functions; replace only backend/page collaborators.
// Keep extraction bounded to its owning feature and fail directly if the source seam moves.
const slice=(start,end)=>{
 const first=source.indexOf(start),last=source.indexOf(end,first);
 assert.ok(first>=0&&last>first,'Navigation fixture source boundary is unavailable: '+start);
 return source.slice(first,last);
};
const shell=slice('function renderAgentCards(){','async function loadAgentRoster(){')
 +slice('function disposeSessionsPage(){',"for(const page of [configPage,...agentPages]){")
 +(source.includes('async function mountAgentModels(){')?slice('async function mountAgentModels(){','// A rejected sign-in'):'');
const setup=`
const $=id=>document.getElementById(id);
let agentId='one',sessionsScope=true,createScope=false,sessionsPage=null,sessionsGeneration=0,selectionTable=null,agentSelectionQueue=Promise.resolve();
const agentRoster=[{id:'one',name:'One',can_manage:true},{id:'two',name:'Two',can_manage:true}];
const configPage={id:'config',panelId:'config-page'},sessionsDescriptor={id:'sessions',title:'Sessions',panelId:'sessions-section'},createDescriptor={id:'create',title:'New agent',panelId:'create-agent-page'};
const agentPages=['model','identity','abilities','members'].map(id=>({id,panelId:id+'-page'})),agentTabOrder=['config','model','identity','abilities','members'];
const accountMe={is_admin:false},activeMainPage='agents',api=async()=>({}),chat={sessionId:'keep'};
let explicitAssignment=false;
const note=text=>{if(text)window.fixture.errors.push(text);};
const guard=fn=>async event=>{event?.preventDefault();try{await fn(event);note('');}catch(error){note(error.message);}};
const renderIdentity=async(host,request,{agentId})=>{fixture.identityRenders.push(agentId);host.replaceChildren();await new Promise(r=>setTimeout(r,10));const name=document.createElement('p');name.textContent='Identity '+agentId;host.append(name);};
const renderAbilities=async(host,request,{agentId})=>{host.textContent='Abilities '+agentId;};
const renderMembers=async(host,request,{agentId})=>{host.textContent='Members '+agentId;};
const renderCreateAgent=(host,request,{onCreated})=>{fixture.createRenders++;host.textContent='Create form';fixture.create=()=>onCreated({id:'three',name:'Three',assignment_required:true});};
const loadAgentRoster=async()=>{agentRoster.push({id:'three',name:'Three',can_manage:true});renderAgentCards();};
window.fixture={createRenders:0,guest(){accountMe.role='guest';renderAgentCards();},errors:[],mounts:[],destroyed:[],sessionsDestroyed:0,identityRenders:[],appAdminWithoutAgentAuthority(){accountMe.is_admin=true;agentRoster.find(agent=>agent.id===agentId).can_manage=false;}};
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
const html=`<!doctype html><main id="tab-agents"><div id="agents-grid"><div class="agents-squares"></div><div id="agents-detail-region" hidden><div class="agent-row"></div><div class="agent-card-tabs"></div><div id="config-page">Config</div><div id="model-page"><div class="ac-model-table"></div><div id="selection-model-table"></div><div id="settings"></div></div><div id="identity-page"></div><div id="abilities-page"></div><div id="members-page"></div></div><div id="sessions-region"><section id="sessions-section"></section></div><div id="create-region" hidden><section id="create-agent-page"></section></div></div></main><textarea id="draft">Unsaved draft</textarea><script type="module" src="/navigation.js"></script>`;
const module=setup+shell+`
for(const id of ['config','model','identity','abilities','members']){
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
 await page.locator('#agent-tab-members').click();assert.equal(await page.locator('#members-page').innerText(),'Members one');
 await page.evaluate(()=>fixture.appAdminWithoutAgentAuthority());await page.locator('#agent-tab-identity').click();
 assert.equal(new URL(page.url()).searchParams.get('agentView'),'abilities');assert.equal(await page.locator('#identity-page').isVisible(),false);
 // New agent is its own scope; the created agent opens on the page it needs next.
 await page.getByRole('button',{name:'New agent',exact:true}).click();
 await page.waitForFunction(()=>!document.querySelector('#create-region').hidden);
 assert.equal(await page.locator('#agents-detail-region').isVisible(),false);
 assert.equal(new URL(page.url()).searchParams.get('agentScope'),'create');
 assert.equal(await page.getByRole('button',{name:'New agent',exact:true}).getAttribute('aria-pressed'),'true');
 assert.equal(await page.getByRole('button',{name:'Configure One',exact:true}).getAttribute('aria-pressed'),'false');
 await page.getByRole('button',{name:'Sessions',exact:true}).click();
 assert.equal(await page.locator('#create-region').isVisible(),false);
 await page.getByRole('button',{name:'New agent',exact:true}).click();
 assert.equal(await page.evaluate(()=>fixture.createRenders),1);
 await page.evaluate(()=>fixture.create());
 await page.waitForFunction(()=>document.querySelector('#selection-model-table').textContent==='Models three');
 const created=new URL(page.url()).searchParams;
 assert.equal(created.get('agent'),'three');assert.equal(created.get('agentView'),'model');assert.equal(created.has('agentScope'),false);
 assert.equal(await page.locator('#create-region').isVisible(),false);
 assert.equal(await page.getByRole('button',{name:'Configure Three',exact:true}).getAttribute('aria-pressed'),'true');
 await page.evaluate(()=>fixture.guest());
 assert.equal(await page.getByRole('button',{name:'New agent',exact:true}).count(),0);
 assert.deepEqual(await page.evaluate(()=>fixture.errors),[]);
 console.log('PASS New agent scope, creation handoff and guest hiding; agent Operator config without app admin; app admin without agent authority stays read-only; agent cards: Sessions exit, same/different agent, keyboard, model/identity rebinding, rapid clicks, preserved draft/conversation, no document navigation');
}finally{await browser?.close();server.close();}
