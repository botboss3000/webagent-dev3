import fs from 'node:fs/promises';
import http from 'node:http';
import path from 'node:path';
import assert from 'node:assert/strict';
const {chromium}=await import(process.env.PLAYWRIGHT_MODULE||'playwright');
const root=path.resolve(new URL('../../..',import.meta.url).pathname);
const source=await fs.readFile(new URL('./app-page.js',import.meta.url),'utf8');
const adapted=source.replace(/^import .* from '\.\/pages\/(?:users|user-policies|page-access|settings|functions|modules|models|files)\/[^']+';$/gm,line=>line.replace(/from '[^']+'/,"from '/stubs.js'"));
let preferences={page_ids:['config'],revision:0,configured:false};
const stubs="export const mountUsers=async(host)=>{document.body.dataset.usersMounts=String(Number(document.body.dataset.usersMounts||0)+1);host.textContent='Users fixture pending';await new Promise(()=>{});},mountPolicies=async()=>{},mountPageAccess=async()=>{},mountAppSettings=()=>{},mountFunctions=()=>{},mountModules=async()=>{},mountAppModels=async()=>()=>{},startView=async()=>{},stopView=()=>{},resetExplorerForRemount=()=>{};";
const server=http.createServer(async(req,res)=>{
 const url=new URL(req.url,'http://localhost');res.setHeader('content-type','application/json');
 if(url.pathname==='/api/v3/auth/me'){res.end(JSON.stringify({is_admin:true,user_id:'fixture',csrf_token:'fixture'}));return;}
 if(url.pathname==='/api/v3/user/app-page-favorites'){
  if(req.method==='PUT'){let raw='';for await(const chunk of req)raw+=chunk;const input=JSON.parse(raw);assert.equal(req.headers['x-csrf-token'],'fixture');if(input.expected_revision!==preferences.revision){res.statusCode=409;res.end('{"error":"conflict"}');return;}preferences={...input,revision:preferences.revision+1,configured:true};}
  res.end(JSON.stringify(preferences));return;
 }
 if(url.pathname==='/api/v3/file-manager'){res.end('{}');return;}
 if(url.pathname==='/stubs.js'){res.setHeader('content-type','text/javascript');res.end(stubs);return;}
 if(url.pathname==='/app'){res.setHeader('content-type','text/html');res.end('<!doctype html><meta name="viewport" content="width=device-width,initial-scale=1"><link rel="stylesheet" href="/v3-ui/main-panel/app/app-page.css"><style>.agent-detail-content{display:flex;flex-direction:column;height:520px;min-height:0}.agent-detail-body{flex:1;min-height:0;overflow-y:auto}</style><main id="tab-app"></main><script type="module">import {start} from "/ui/main-panel/app/app-page.js";await start(document.getElementById("tab-app"));</script>');return;}
 try{const asset=path.resolve(root,'.'+url.pathname.replace(/^\/v3-ui\//,'/ui/'));if(!asset.startsWith(root+'/'))throw Error('outside');res.setHeader('content-type',asset.endsWith('.js')?'text/javascript':asset.endsWith('.css')?'text/css':asset.endsWith('.json')?'application/json':'text/html');res.end(asset.endsWith('/app/app-page.js')?adapted:await fs.readFile(asset));}catch{res.statusCode=404;res.end('{}');}
});
await new Promise(resolve=>server.listen(0,'127.0.0.1',resolve));
let browser;
try{
 browser=await chromium.launch({executablePath:process.env.CHROMIUM_PATH||'/snap/bin/chromium',headless:true,args:['--no-sandbox']});
 for(const viewport of [{width:1200,height:850},{width:390,height:844}]){
  const page=await browser.newPage({viewport});const errors=[];page.on('pageerror',error=>errors.push(error.message));
  await page.goto('http://127.0.0.1:'+server.address().port+'/app?tab=app&appView=config');
  await page.getByRole('button',{name:'Star File Manager',exact:true}).waitFor();
  // App pages grow past the shared 520px detail cap instead of clipping their content.
  const layout=await page.evaluate(()=>{const body=document.querySelector('#app-config-page'),filler=document.createElement('div');filler.style.height='1500px';body.append(filler);const result={content:document.querySelector('#tab-app .agent-detail-content').getBoundingClientRect().height,overflow:getComputedStyle(body).overflowY};filler.remove();return result;});
  assert.ok(layout.content>1500,'App detail content height '+layout.content);assert.equal(layout.overflow,'visible');
  // A hidden panel whose request never finishes cannot delay initial Config.
  assert.equal(await page.locator('body').getAttribute('data-users-mounts'),null);
  await page.getByRole('button',{name:'Open Users',exact:true}).click();
  await page.getByText('Users fixture pending',{exact:true}).waitFor();
  await page.locator('#app-tab-config').click();
  await page.getByRole('button',{name:'Open Users',exact:true}).click();
  assert.equal(await page.locator('body').getAttribute('data-users-mounts'),'1');
  await page.locator('#app-tab-config').click();
  const star=page.getByRole('button',{name:'Star File Manager',exact:true});
  if(await star.getAttribute('aria-pressed')!=='true')await star.click();
  await page.waitForFunction(()=>document.querySelector('#app-tab-files')&&!document.querySelector('#app-tab-files').hidden);
  await page.reload();await page.locator('#app-tab-files').waitFor({state:'visible'});
  await page.locator('#app-tab-files').focus();await page.keyboard.press('Alt+ArrowRight');
  await page.waitForFunction(()=>document.querySelector('[role=tablist]').lastElementChild.id==='app-tab-files');
  await page.reload();await page.locator('#app-tab-files').waitFor({state:'visible'});
  assert.equal(await page.locator('[role=tablist] > button:not([hidden])').last().getAttribute('id'),'app-tab-files');
  await page.locator('#app-tab-files').click();await page.locator('#file-manager').waitFor({state:'visible'});
  assert.equal(await page.locator('#app-files-page').evaluate(el=>getComputedStyle(el).overflowY),'hidden');
  assert.equal(new URL(page.url()).searchParams.get('appView'),'files');
  await page.locator('#app-tab-config').click();await page.getByRole('button',{name:'Star File Manager',exact:true}).click();
  await page.reload();await page.getByRole('button',{name:'Star File Manager',exact:true}).waitFor();
  assert.equal(await page.locator('#app-tab-files').isVisible(),false);assert.deepEqual(errors,[]);
  await page.close();
 }
 console.log('PASS App shell: full-height pages, stars, database adapter, reload, keyboard ordering, unstar, routes and moved File Manager markup at desktop/mobile widths');
}finally{await browser?.close();server.close();}
