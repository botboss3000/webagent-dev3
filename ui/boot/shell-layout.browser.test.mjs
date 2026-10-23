import fs from 'node:fs';
import path from 'node:path';
import http from 'node:http';
import assert from 'node:assert/strict';
import {fileURLToPath} from 'node:url';
const {chromium}=await import(process.env.PLAYWRIGHT_MODULE||'playwright');
const root=path.resolve(path.dirname(fileURLToPath(import.meta.url)),'../..');
const shared=[process.env.SHARED_UI_ROOT,path.resolve(root,'../ui'),path.join(root,'build-service/shared-ui/ui')].filter(Boolean).find(p=>fs.existsSync(path.join(p,'shared/css/app1.css')));
assert(shared,'shared styles must be available for the real shell cascade');
// Mount actual shell and chat markup/styles; omit authentication and model activity.
const html=fs.readFileSync(path.join(root,'ui/index.html'),'utf8')
 .replace(/<script\b[^>]*>[\s\S]*?<\/script>/g,'')
 .replace('class="is-booting"','')
 .replace('<div id="app" hidden>','<div id="app" data-mobile-view="chat">')
 .replace('<div id="chat-panel" class="chat-surface"></div>','<div id="chat-panel" class="chat-surface">'+fs.readFileSync(path.join(root,'ui/chat/chat.html'),'utf8')+'</div>')
 .replace('</body>','<script src="/v3-ui/boot/boot.js"></script><script src="/v3-ui/boot/viewport.js"></script></body>');
const server=http.createServer((req,res)=>{
 const url=new URL(req.url,'http://localhost'),rel=url.pathname;
 if(rel==='/app'){res.setHeader('content-type','text/html');res.end(html);return;}
 const base=rel.startsWith('/v3-ui/')?path.join(root,'ui'):rel.startsWith('/ui/')?shared:null;
 const target=base&&path.resolve(base,rel.replace(/^\/(v3-ui|ui)\//,''));
 if(!target||!target.startsWith(base+path.sep)||!fs.existsSync(target)){res.writeHead(404);res.end();return;}
 res.setHeader('content-type',target.endsWith('.css')?'text/css':target.endsWith('.js')?'text/javascript':'text/plain');res.end(fs.readFileSync(target));
});
await new Promise(r=>server.listen(0,'127.0.0.1',r));
let browser;
try{
 browser=await chromium.launch({headless:true,executablePath:process.env.CHROMIUM_PATH,args:['--no-sandbox']});
 for(const cfg of [{width:390,height:844},{width:390,height:500},{width:740,height:390},{width:1280,height:900}]){
  const context=await browser.newContext({viewport:cfg,isMobile:cfg.width<=800,hasTouch:cfg.width<=800});
  const page=await context.newPage(),cdp=await context.newCDPSession(page);
  for(const inset of [false,true]){
   const safe=inset?{top:24,right:12,bottom:34,left:12}:{top:0,right:0,bottom:0,left:0};
   await cdp.send('Emulation.setSafeAreaInsetsOverride',{insets:safe});
   await page.goto('http://127.0.0.1:'+server.address().port+'/app');
   await page.evaluate(()=>document.getElementById('boot-curtain')?.remove());
   for(const light of [false,true]){
    await page.evaluate(light=>document.body.classList.toggle('light-mode',light),light);
    const measure=()=>page.evaluate(()=>{
     const rect=id=>{const r=document.getElementById(id).getBoundingClientRect();return{top:r.top,bottom:r.bottom,left:r.left,right:r.right,width:r.width,height:r.height}};
     return{app:rect('app'),stage:rect('stage'),chat:rect('chat-panel'),footer:rect('chat-input-area'),main:rect('main-panel')};
    });
    await page.waitForFunction(()=>getComputedStyle(document.documentElement).backgroundColor===getComputedStyle(document.body).backgroundColor);
    const g=await measure();
    assert.equal(g.app.height,cfg.height);
    if(cfg.width<=800){
     assert(Math.abs(g.chat.top-g.stage.top)<1,'chat starts at the stage top');
     assert(Math.abs(g.chat.bottom-g.stage.bottom)<1,'mobile chat must fill the stage; a desktop percentage width must not cap its height');
     assert(g.stage.bottom<=cfg.height-safe.bottom+1,'stage respects bottom safe area');
     assert(g.footer.bottom>=g.stage.bottom-30,'footer must dock near the stage bottom, not merely stay within the screen');
     assert(g.footer.bottom<=g.stage.bottom,'footer clears bottom system controls');
     await page.evaluate(()=>document.getElementById('app').dataset.mobileView='main');
     const main=await measure();assert(Math.abs(main.main.bottom-main.stage.bottom)<=4,'main-panel retains its existing small card gutter');
     await page.evaluate(()=>document.getElementById('app').dataset.mobileView='chat');
    }else assert(Math.abs(g.chat.width-g.stage.width*.45)<1,'desktop split keeps its width');
   }
  }
  for(const owner of ['body','html']) for(const [color,expected] of [['#00a040','rgb(0, 160, 64)'],['#000000','rgb(0, 0, 0)'],['#ffffff','rgb(255, 255, 255)']]){
   await page.evaluate(({owner,color})=>{document.body.classList.remove('light-mode');document.querySelector(owner).style.setProperty('--bg-0',color);},{owner,color});
   await page.waitForFunction(()=>getComputedStyle(document.documentElement).backgroundColor===getComputedStyle(document.body).backgroundColor);
   assert.equal(await page.evaluate(()=>getComputedStyle(document.documentElement).backgroundColor),expected);
   await page.evaluate(owner=>document.querySelector(owner).style.removeProperty('--bg-0'),owner);
  }
  if(cfg.width===390&&cfg.height===844){
   await page.evaluate(()=>{document.getElementById('chat-input').focus();Object.defineProperty(visualViewport,'height',{configurable:true,value:500});visualViewport.dispatchEvent(new Event('resize'));});
   const keyboard=await page.evaluate(()=>({app:document.getElementById('app').getBoundingClientRect().height,chat:document.getElementById('chat-panel').getBoundingClientRect().bottom,stage:document.getElementById('stage').getBoundingClientRect().bottom,footer:document.getElementById('chat-input-area').getBoundingClientRect().bottom}));
   assert.equal(keyboard.app,500);assert(Math.abs(keyboard.chat-keyboard.stage)<1);assert(keyboard.footer>=keyboard.stage-30);assert(keyboard.footer<=500);
   await page.evaluate(()=>{delete visualViewport.height;visualViewport.dispatchEvent(new Event('resize'));});
   assert.equal(await page.locator('#app').evaluate(e=>e.getBoundingClientRect().height),844);
  }
  await context.close();
 }
 console.log('PASS shell height: full mobile chat, bottom-docked footer, unchanged main panel and desktop split; both themes, safe areas, short/rotated viewports and keyboard restore');
}finally{await browser?.close();await new Promise(r=>server.close(r));}
