const {test}=require('node:test'),assert=require('node:assert/strict'),fs=require('node:fs'),path=require('node:path'),vm=require('node:vm');
const tick=()=>new Promise(setImmediate);
async function moduleAt(file,globals={}){const context=vm.createContext({...globals});const module=new vm.SourceTextModule(fs.readFileSync(path.join(__dirname,file),'utf8'),{context});await module.link(()=>{throw Error('Unexpected dependency')});await module.evaluate();return module.namespace;}
test('restored and resized widget stays within viewport',async()=>{const {clampGeometry}=await moduleAt('window.js');const r=clampGeometry({x:-900,y:9000,width:9999,height:9999},{},{width:1024,height:768});assert.equal(r.x,8);assert.equal(r.y,8);assert.equal(r.width,1008);assert.equal(r.height,752);});
test('narrow widget uses available width; profile enforces minimums',async()=>{const {clampGeometry}=await moduleAt('window.js');const narrow=clampGeometry({width:440},{},{width:360,height:640});assert.equal(narrow.width,344);assert.equal(narrow.x,8);const r=clampGeometry({width:10,height:10},{window:{min_width:350,min_height:400}},{width:1000,height:800});assert.equal(r.width,350);assert.equal(r.height,400);});
test('widget overrides inherit common formatting and replace row arrays',async()=>{const {mergeProfile}=await moduleAt('window.js');const merged=mergeProfile({messages:{font:17,color:'red'},rows:['base']},{messages:{color:'blue'},rows:['widget']});assert.equal(merged.messages.font,17);assert.equal(merged.messages.color,'blue');assert.deepEqual(Array.from(merged.rows),['widget']);});
test('authenticated SSE handles split Unicode, named events and closes',async()=>{
 let controller,options;const stream=new ReadableStream({start(c){controller=c}});const {createFetchStream}=await moduleAt('../core/fetch-stream.js',{TextDecoder,AbortController,queueMicrotask,setTimeout,clearTimeout,fetch:()=>{throw Error('Unexpected default fetch')}});
 const events=[];const source=createFetchStream('/events',{headers:()=>({Authorization:'Bearer test-only'}),fetchFn:async(url,o)=>{options=o;return {ok:true,body:stream}}});source.onmessage=e=>events.push(e.data);source.addEventListener('session_updated',e=>events.push('meta:'+e.data));await tick();
 const bytes=new TextEncoder().encode('id: 8\r\ndata: {"text":"🙂"}\r\n\r\nevent: session_updated\ndata: {}\n\n');for(const b of bytes)controller.enqueue(new Uint8Array([b]));await tick();
 assert.deepEqual(events,['{"text":"🙂"}','meta:{}']);assert.equal(options.credentials,'omit');assert.equal(options.headers.Authorization,'Bearer test-only');source.close();assert.equal(options.signal.aborted,true);
});
test('stream reconnect sends last-event ID without query credentials',async()=>{
 let attempt=0,timer;const options=[];const {createFetchStream}=await moduleAt('../core/fetch-stream.js',{TextDecoder,AbortController,queueMicrotask,fetch(){},setTimeout:fn=>{timer=fn;return 1},clearTimeout(){}});
 const source=createFetchStream('/events?after=0',{headers:()=>({Authorization:'Bearer test-only'}),fetchFn:async(url,o)=>{options.push({url,...o});if(++attempt===1)return {ok:true,body:new ReadableStream({start(c){c.enqueue(new TextEncoder().encode('id: 9\ndata: {}\n\n'));c.close()}})};return {ok:true,body:new ReadableStream({start(){}})}}});await tick();assert.equal(typeof timer,'function');timer();await tick();assert.equal(options[1].headers['Last-Event-ID'],'9');assert.equal(options[1].url,'/events?after=0');source.close();
});

test('head loader waits for the body and respects an already mounted widget',()=>{
 let ready;const document={body:null,currentScript:{dataset:{agent:'fixture'}},addEventListener(name,fn,options){assert.equal(name,'DOMContentLoaded');assert.equal(options.once,true);ready=fn;}};const window={};vm.runInNewContext(fs.readFileSync(path.join(__dirname,'../../embed/loader.js'),'utf8'),{document,window});assert.equal(typeof ready,'function');assert.equal(window.WebAgentWidget,undefined);const existing={};window.WebAgentWidget=existing;ready();assert.equal(window.WebAgentWidget,existing);
});

test('embedded window keeps its iframe mounted once across open, minimize and close',async()=>{
 let attachments=0,panel;class Node{
 constructor(){this.children=[];this.parentNode=null;this.style={};this.dataset={};}
 append(...nodes){for(const n of nodes){if(n===panel)attachments++;n.remove();this.children.push(n);n.parentNode=this;}}
 before(n){if(this.parentNode)this.parentNode.insertBefore(n,this);}
 insertBefore(n,next){n.remove();const i=this.children.indexOf(next);this.children.splice(i<0?this.children.length:i,0,n);n.parentNode=this;}
 remove(){if(this.parentNode){const a=this.parentNode.children;a.splice(a.indexOf(this),1);this.parentNode=null;}}
 setAttribute(){}addEventListener(){}focus(){} }
 const body=new Node();panel=new Node();const document={body,activeElement:body,createElement:()=>new Node(),createComment:()=>new Node()};const window={innerWidth:1000,innerHeight:800,addEventListener(){},removeEventListener(){}};
 const {createFloatingChat}=await moduleAt('window.js',{document,window,innerWidth:1000,innerHeight:800,localStorage:{getItem:()=>null,setItem(){}}});const shell=createFloatingChat({panel,embedded:true});assert.equal(attachments,1);shell.open();shell.minimize();shell.open();shell.close();shell.open();assert.equal(attachments,1);assert.equal(panel.parentNode.className,'wa-chat-window-content');shell.destroy();
});

test('lost popup notification recovers with one in-flight redemption',async()=>{
 let tick,resolve,count=0,connected=0,cleared=0;const {watchAuthorization}=await moduleAt('../../embed/auth-handoff.js');const watcher=watchAuthorization({redeem:()=>{count++;return new Promise(r=>resolve=r)},connected:()=>connected++,ended:()=>assert.fail('Unexpected cancellation'),isClosed:()=>false,isExpired:()=>false,setIntervalFn:fn=>{tick=fn;return 1},clearIntervalFn:()=>cleared++});for(let i=0;i<10;i++)tick();const fast=watcher.complete();await tickAsync();assert.equal(count,1);resolve({});await fast;tick();assert.equal(connected,1);assert.equal(cleared,1);assert.equal(count,1);
 function tickAsync(){return new Promise(setImmediate)}
});
test('cancelled popup stops polling after final failed redemption',async()=>{
 let tick,ended=0,count=0;const {watchAuthorization}=await moduleAt('../../embed/auth-handoff.js');watchAuthorization({redeem:async()=>{count++;throw Error('Not authorized')},connected:()=>assert.fail('Unexpected connection'),ended:()=>ended++,isClosed:()=>true,isExpired:()=>false,setIntervalFn:fn=>{tick=fn;return 1},clearIntervalFn(){}});tick();await new Promise(setImmediate);tick();assert.equal(count,1);assert.equal(ended,1);
});

test('launcher corner configuration dispatches scoped actions and pickup without opening chat',async()=>{
 class Node{
  constructor(tag){this.tagName=tag;this.children=[];this.dataset={};this.style={setProperty(){}};this.events={};this.attributes={};this.hidden=false;this.parentNode=null;this.classList={add(){},toggle(){},remove(){}};}
  append(...nodes){for(const n of nodes){this.children.push(n);n.parentNode=this;}}
  replaceChildren(...nodes){this.children=[];this.append(...nodes)}
  setAttribute(k,v){this.attributes[k]=v}addEventListener(k,fn){this.events[k]=fn}removeEventListener(){}
  getBoundingClientRect(){return {left:900,top:700,width:58,height:58}}
  querySelector(){return null}contains(n){return this===n||this.children.some(c=>c.contains(n))}
  focus(){}remove(){}setPointerCapture(){}releasePointerCapture(){}
 }
 const document={createElement:tag=>new Node(tag),createElementNS:(_,tag)=>new Node(tag),addEventListener(){},removeEventListener(){},elementFromPoint:()=>({tagName:'H2',id:'example',innerText:'Selected page heading',closest:()=>null})};
 const window={addEventListener(){},removeEventListener(){}};
 const calls=[],samples=[];let opened=0;
 const {createChatLauncher}=await moduleAt('window.js',{document,window,innerWidth:1200,innerHeight:900,matchMedia:()=>({matches:true}),localStorage:{getItem:()=>null},setTimeout:()=>1,clearTimeout(){},requestAnimationFrame:()=>1});
 const profile={launcher:{corner_buttons:{enabled:true,top_left:{type:'sessions',icon:'list',tooltip:'Show sessions'},top_right:{type:'new_session',icon:'plus'},bottom_left:{type:'element_pickup',icon:'crosshair'},bottom_right:{enabled:false,type:'open_all'}}}};
 const launcher=createChatLauncher({profile,onAction:type=>calls.push(type),onPickup:value=>samples.push(value),onOpen:()=>opened++});
 const main=launcher.element.children[0],menu=launcher.element.children.find(n=>n.className==='wa-launcher-corners');
 assert.equal(menu.children.length,3);
 await menu.children[0].onclick({stopPropagation(){}});await menu.children[1].onclick({stopPropagation(){}});
 assert.deepEqual(calls,['sessions','new_session']);assert.equal(opened,0);
 await menu.children[2].onclick({stopPropagation(){}});assert.equal(JSON.parse(samples[0]).text,'Selected page heading');
 main.events.pointerdown({button:0,clientX:900,clientY:700,pointerId:1,pointerType:'touch'});launcher.element.events.focusin();assert.equal(menu.hidden,true);main.events.pointerup({pointerId:1});main.events.click({detail:1});assert.equal(opened,0);assert.equal(menu.hidden,false);
 main.events.click({detail:1,pointerType:'touch'});assert.equal(opened,1);
 launcher.setProfile({launcher:{corner_buttons:{enabled:false}}});assert.equal(menu.children.length,0);
 launcher.destroy();
});
