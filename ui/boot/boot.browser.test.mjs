import fs from 'node:fs/promises';
import http from 'node:http';
import assert from 'node:assert/strict';
const {chromium}=await import(process.env.PLAYWRIGHT_MODULE||'playwright');
const script=await fs.readFile(new URL('./boot.js',import.meta.url),'utf8');
const css=await fs.readFile(new URL('./boot.css',import.meta.url),'utf8');
const server=http.createServer((req,res)=>{
 if(req.url==='/boot.js'){res.setHeader('content-type','text/javascript');res.end(script);return;}
 if(req.url==='/boot.css'){res.setHeader('content-type','text/css');res.end(css);return;}
 res.setHeader('content-type','text/html');
 res.end('<!doctype html><link rel="stylesheet" href="/boot.css"><body class="is-booting"><div id="boot-curtain" role="status" aria-label="Loading"><canvas class="boot-grid"></canvas></div><div id="app" hidden><h1>App shell</h1></div><script src="/boot.js"></script>');
});
await new Promise(resolve=>server.listen(0,'127.0.0.1',resolve));
let browser;
try{
 browser=await chromium.launch({executablePath:process.env.CHROMIUM_PATH||'/snap/bin/chromium',headless:true,args:['--no-sandbox']});
 for(const reducedMotion of ['no-preference','reduce']){
  const page=await browser.newPage({reducedMotion});const errors=[];
  page.on('pageerror',error=>errors.push(error.message));
  await page.addInitScript(()=>{
   const timeout=window.setTimeout.bind(window);
   window.setTimeout=(fn,ms,...args)=>timeout(()=>{fn(...args);if(ms===10000)document.body.dataset.bootTimeout='fired';},ms===10000?50:ms);
  });
  await page.goto('http://127.0.0.1:'+server.address().port+'/');
  await page.waitForFunction(()=>document.body.dataset.bootTimeout==='fired');
  // Expiring the old safety timer must not expose the still-hidden app.
  assert.equal(await page.locator('#boot-curtain').isVisible(),true);
  assert.equal(await page.locator('#app').isVisible(),false);
  assert.equal(await page.evaluate(()=>window.__boot.revealed),false);
  // The app's theme applying mid-boot must not recolour the black scene.
  assert.equal(await page.evaluate(()=>{document.body.classList.add('light-mode');return getComputedStyle(document.getElementById('boot-curtain')).backgroundColor;}),'rgb(0, 0, 0)');
  await page.evaluate(()=>{
   window.readyEvents=0;window.addEventListener('webagent-app-ready',()=>window.readyEvents++);
   document.getElementById('app').hidden=false;window.__boot.shellReady();
  });
  // The app is released at once; the curtain fades over it (reduced motion removes it immediately).
  assert.equal(await page.getByRole('heading',{name:'App shell'}).isVisible(),true);
  if(reducedMotion==='reduce')assert.equal(await page.locator('#boot-curtain').count(),0);
  else{
   assert.equal(await page.locator('#boot-curtain.is-lifting').count(),1);
   // The grid fades out before the black curtain, and its transitionend must not remove the curtain early.
   await page.evaluate(()=>new Promise(resolve=>document.querySelector('.boot-grid').addEventListener('transitionend',resolve,{once:true})));
   assert.equal(await page.evaluate(()=>getComputedStyle(document.getElementById('boot-curtain')).opacity>0.5),true);
  }
  await page.locator('#boot-curtain').waitFor({state:'detached',timeout:2000});
  assert.equal(await page.evaluate(()=>window.readyEvents),0);
  await page.evaluate(()=>{window.__boot.appReady();window.__boot.appReady();});
  assert.equal(await page.evaluate(()=>window.readyEvents),1);
  assert.deepEqual(errors,[]);await page.close();
 }
 console.log('PASS boot: delayed app retains animation, shell reveal and once-only readiness, including reduced motion');
}finally{await browser?.close();server.close();}
