import fs from 'node:fs/promises';
import path from 'node:path';
import http from 'node:http';
import assert from 'node:assert/strict';
const {chromium}=await import(process.env.PLAYWRIGHT_MODULE||'playwright');
const root=path.resolve(new URL('../..',import.meta.url).pathname);
let shared=path.resolve(root,'../ui');
try{await fs.access(path.join(shared,'shared/css/app1.css'));}catch{shared=path.join(root,'build-service/shared-ui/ui');}
const pages=[];
for(const id of ['app','agents','builds','account','session-metrics']){
 const meta=JSON.parse(await fs.readFile(path.join(root,'ui/main-panel',id,'page.json'),'utf8'));
 const asset=name=>name?'/v3-ui/main-panel/'+id+'/'+name:null;
 pages.push({...meta,entry:asset(meta.entry),html:asset(meta.html),css:(meta.css||[]).map(asset)});
}
pages.push({id:'automation',label:'Automations',entry:'/api/v3/modules/automation/assets/page.js',html:'/api/v3/modules/automation/assets/page.html',css:['/api/v3/modules/automation/assets/page.css']});
let protectedRequests=0;
const server=http.createServer(async(req,res)=>{
 const url=new URL(req.url,'http://localhost');
 res.setHeader('content-type','application/json');
 if(url.pathname==='/api/v3/pages/catalog'){res.end(JSON.stringify({main:pages}));return;}
 if(url.pathname==='/api/v3/auth/setup-status'){res.end('{"initialized":true}');return;}
 if(url.pathname==='/api/v3/auth/access-mode'){res.end('{"guest_access":false}');return;}
 if(url.pathname.startsWith('/api/v3/modules/'))protectedRequests++;
 if(url.pathname.startsWith('/api/')){res.statusCode=401;res.end('{"error":"Sign in required"}');return;}
 let asset;
 if(url.pathname==='/'||url.pathname==='/app')asset=path.join(root,'ui/index.html');
 else if(url.pathname.startsWith('/v3-ui/'))asset=path.join(root,'ui',url.pathname.slice(7));
 else if(url.pathname.startsWith('/ui/'))asset=path.join(shared,url.pathname.slice(4));
 try{
  if(!asset)throw Error('missing');
  const mime={'.js':'text/javascript','.mjs':'text/javascript','.css':'text/css','.html':'text/html','.json':'application/json','.svg':'image/svg+xml','.woff2':'font/woff2'};
  res.setHeader('content-type',mime[path.extname(asset)]||'application/octet-stream');
  res.end(await fs.readFile(asset));
 }catch{res.statusCode=404;res.end('{}');}
});
await new Promise(resolve=>server.listen(0,'127.0.0.1',resolve));
let browser;
try{
 browser=await chromium.launch({executablePath:process.env.CHROMIUM_PATH||'/snap/bin/chromium',headless:true,args:['--no-sandbox']});
 for(const viewport of [{width:390,height:844},{width:1280,height:720}]){
  const page=await browser.newPage({viewport});page.setDefaultTimeout(15000);
  const errors=[];page.on('pageerror',e=>errors.push(e.message));
  for(const route of ['/','/app','/?tab=agents&agentView=identity&appView=files&agent=shared_default','/?tab=automation']){
   await page.goto('http://127.0.0.1:'+server.address().port+route);
   await page.getByRole('button',{name:'Sign in',exact:true}).waitFor();
   await page.locator('#boot-curtain').waitFor({state:'detached',timeout:2000});
   assert.equal(await page.locator('#app').isVisible(),false);
   assert.equal(await page.locator('#app-restricted-overlay').isVisible(),true);
   await page.getByRole('button',{name:'Sign in',exact:true}).click();
   await page.getByRole('textbox',{name:'Username',exact:true}).waitFor();
   assert.equal(await page.getByLabel('Password',{exact:true}).isVisible(),true);
   assert.equal(protectedRequests,0,'Signed-out boot requested authenticated package assets');
   assert.deepEqual(errors,[]);
  }
  await page.close();
 }
 console.log('PASS real signed-out shell: root, /app, stale bookmark and module deep link show sign-in at phone/desktop widths without requesting protected assets');
}finally{await browser?.close();server.close();}
