import http from 'node:http';
import fs from 'node:fs/promises';
import path from 'node:path';
import assert from 'node:assert/strict';
const {chromium}=await import(process.env.PLAYWRIGHT_MODULE||'playwright');
const root=path.resolve(new URL('../../../',import.meta.url).pathname);
const server=http.createServer(async(req,res)=>{
 try{
  const u=new URL(req.url,'http://localhost').pathname;
  if(u==='/'){res.setHeader('content-type','text/html');res.end('<!doctype html><meta name="viewport" content="width=device-width,initial-scale=1"><link rel="stylesheet" href="/ui/shared/css/design-system.css"><link rel="stylesheet" href="/ui/shared/option-list/option-list.css"><link rel="stylesheet" href="/ui/shared/models/model-table.css"><link rel="stylesheet" href="/ui/shared/models/model-analytics.css"><link rel="stylesheet" href="/ui/shared/models/save-indicator.css"><link rel="stylesheet" href="/ui/main-panel/agents/pages/abilities/ability-table.css"><link rel="stylesheet" href="/ui/chat/core/confirm-panel.css"><link rel="stylesheet" href="/ui/shared/models/field-tip.css"><link rel="stylesheet" href="/ui/shared/skeleton/skeleton.css"><link rel="stylesheet" href="/ui/shared/controls/controls.css"><link rel="stylesheet" href="/ui/shared/config-rows/config-rows.css"><link rel="stylesheet" href="/ui/shared/subpage-list/subpage-list.css"><main id="host" style="padding:12px;max-width:650px"></main>');return;}
  if(u==='/config/ui/vibrate.json'){res.setHeader('content-type','application/json');res.end('{"enabled":true,"impact":{"enabled":true,"duration_ms":10},"release":{"enabled":true,"duration_ms":17}}');return;}
  const file=path.resolve(root,'.'+u);
  if(!file.startsWith(path.join(root,'ui')+'/'))throw Error('outside');
  res.setHeader('content-type',file.endsWith('.css')?'text/css':'text/javascript');res.end(await fs.readFile(file));
 }catch{res.statusCode=404;res.end('missing');}
});
await new Promise(r=>server.listen(0,'127.0.0.1',r));let browser;
const wait=ms=>new Promise(r=>setTimeout(r,ms));
try{
 browser=await chromium.launch({headless:true,executablePath:process.env.CHROMIUM_PATH,args:['--no-sandbox']});
 for(const touch of [false,true]){
  const context=await browser.newContext({hasTouch:touch,isMobile:touch,viewport:touch?{width:390,height:844}:{width:1200,height:900}});
  const page=await context.newPage();page.setDefaultTimeout(6000);const errors=[];page.on('pageerror',e=>errors.push(e.message));
  await page.goto('http://127.0.0.1:'+server.address().port+'/');

  // Rows that save as you go: a resolved save keeps the value, a failed one reverts and says why.
  const saved=await page.evaluate(async()=>{
   const rows=await import('/ui/shared/config-rows/config-rows.js');
   const host=document.getElementById('host');const list=rows.createConfigList(host,{label:'Fixture'});
   window.saves=[];window.failNext=false;
   const onSave=key=>async value=>{await new Promise(r=>setTimeout(r,20));if(window.failNext){window.failNext=false;throw Error('Server said no');}saves.push([key,value]);};
   window.toggle=rows.createToggleRow(list,{title:'Enabled',hint:'Turns it on',value:false,onSave:onSave('enabled')});
   window.select=rows.createSelectRow(list,{title:'Mode',options:[['a','A'],['b','B']],value:'a',onSave:onSave('mode')});
   window.text=rows.createTextRow(list,{title:'Name',value:'one',maxLength:20,onSave:onSave('name')});
   window.number=rows.createNumberRow(list,{title:'Limit',value:9,min:0,max:10,onSave:onSave('limit')});
   window.choice=rows.createChoiceRow(list,{title:'Access',options:[{value:'open',title:'Open'},{value:'closed',title:'Closed',hint:'Nobody'}],value:'open',onSave:onSave('access')});
   window.long=rows.createLongTextRow(list,{title:'Prompt',value:'',defaultText:'The default prompt',maxLength:12,onSave:onSave('prompt')});
   window.read=rows.createReadTextRow(list,{title:'Skill',value:'# Skill text'});
   return {rows:list.children.length};
  });
  assert.equal(saved.rows,7);
  await page.locator('.conn-toggle-wrap').first().click();await wait(500);
  assert.deepEqual(await page.evaluate(()=>saves.at(-1)),['enabled',true]);
  await page.evaluate(()=>{window.failNext=true;});
  await page.locator('.conn-toggle-wrap').first().click();await wait(700);
  assert.equal(await page.evaluate(()=>toggle.getValue()),true,'a failed save reverts the switch');
  assert.match(await page.locator('.cfr-error').first().textContent(),/Server said no/);
  await page.selectOption('select[aria-label="Mode"]','b');await wait(300);
  assert.deepEqual(await page.evaluate(()=>saves.at(-1)),['mode','b']);
  await page.fill('input[aria-label="Name"]','two');await page.locator('input[aria-label="Name"]').blur();await wait(300);
  assert.deepEqual(await page.evaluate(()=>saves.at(-1)),['name','two']);
  await page.click('button[aria-label="Increase Limit"]');await wait(300);
  assert.deepEqual(await page.evaluate(()=>saves.at(-1)),['limit',10]);
  assert.equal(await page.locator('button[aria-label="Increase Limit"]').isDisabled(),true,'the stepper stops at max');
  await page.locator('.ctl-choice',{hasText:'Closed'}).click();await wait(300);
  assert.deepEqual(await page.evaluate(()=>saves.at(-1)),['access','closed']);
  const editor=page.locator('.rte-doc[aria-label="Prompt"]');
  assert.equal(await editor.getAttribute('data-placeholder'),'The default prompt','an empty field shows the default');
  await editor.click();await page.keyboard.type('abcdefghijklmnop');await wait(1200);
  assert.deepEqual(await page.evaluate(()=>saves.at(-1)),['prompt','abcdefghijkl'],'typing stops at maxLength');
  assert.equal(await page.locator('.rte-count').textContent(),'12 / 12');
  assert.equal(await page.locator('.rte-doc[aria-label="Skill"]').getAttribute('contenteditable'),'false');

  // A form saves its rows together.
  const form=await page.evaluate(async()=>{
   const rows=await import('/ui/shared/config-rows/config-rows.js');
   const host=document.getElementById('host');host.replaceChildren();const list=rows.createConfigList(host);
   window.formSaves=[];
   const form=rows.createConfigForm(host,{onSave:async values=>{formSaves.push(values);},onReset:async()=>{}, resetLabel:'Reset'});
   rows.createToggleRow(list,{title:'Flag',value:true,key:'flag',form});
   rows.createTextRow(list,{title:'Label',value:'x',key:'label',form});
   form.attach();
   return {disabled:host.querySelector('.cfr-form-bar .ac-btn-primary').disabled};
  });
  assert.equal(form.disabled,true,'nothing to save yet');
  await page.fill('input[aria-label="Label"]','y');
  assert.equal(await page.locator('.cfr-form-state').textContent(),'Unsaved changes');
  await page.click('.cfr-form-bar .ac-btn-primary');await wait(200);
  assert.deepEqual(await page.evaluate(()=>formSaves.at(-1)),{flag:true,label:'y'});
  assert.equal(await page.locator('.cfr-form-state').textContent(),'Saved');

  // A page pushed inside a category page: back and Escape leave one level at a time.
  await page.evaluate(async()=>{
   const {createSubpageList,createSubpage}=await import('/ui/shared/subpage-list/subpage-list.js');
   const {consumeBack}=await import('/ui/shared/history-back/history-back.js');
   window.addEventListener('popstate',()=>consumeBack());
   const host=document.getElementById('host');host.replaceChildren();
   window.register=createSubpageList(host,{label:'Groups',categories:[{id:'group',title:'Group'}]});
   const section=createSubpage(register.stage,{id:'group',title:'Group'});
   section.body.textContent='Group page';
   register.present();
   window.popped=0;
  });
  await page.click('.spl-row[data-register-for="group"]');await wait(350);
  const pushed=await page.evaluate(()=>{const body=document.createElement('div');body.textContent='Ability page';return register.push({title:'Ability',body,onPop:()=>{popped+=1;}});});
  assert.equal(pushed,true);await wait(300);
  assert.equal(await page.locator('.spl-back-title').textContent(),'Ability');
  assert.equal(await page.evaluate(()=>register.depth()),2);
  await page.click('.spl-back');await wait(300);
  assert.equal(await page.locator('.spl-back-title').textContent(),'Group','back leaves the pushed page only');
  assert.equal(await page.evaluate(()=>popped),1);
  await page.evaluate(()=>{const body=document.createElement('div');body.textContent='Again';register.push({title:'Ability',body});});await wait(300);
  await page.keyboard.press('Escape');await wait(300);
  assert.equal(await page.evaluate(()=>register.depth()),1,'Escape leaves one level');
  if(touch){
   await page.evaluate(()=>{const body=document.createElement('div');register.push({title:'Ability',body});});await wait(300);
   await page.goBack();await wait(400);
   assert.equal(await page.evaluate(()=>register.depth()),1,'a phone back press leaves one level');
   await page.goBack();await wait(400);
   assert.equal(await page.locator('.spl-detail').isHidden(),true,'the next back press returns to the list');
  }
  assert.deepEqual(errors,[]);
  await context.close();
 }
}finally{await browser?.close();server.close();}
