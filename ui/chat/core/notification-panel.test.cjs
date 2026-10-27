const {test}=require('node:test');
const assert=require('node:assert/strict');
const fs=require('node:fs'),path=require('node:path'),vm=require('node:vm');
class E {
 constructor(tag='div'){this.tag=tag;this.children=[];this.attributes={};this.dataset={};this.hidden=false;this.isConnected=true;this.textContent='';this.listeners={};}
 append(...items){for(const item of items){item.parent=this;this.children.push(item);}}
 replaceChildren(...items){this.children=[];this.append(...items);}
 get lastChild(){return this.children.at(-1);}
 insertBefore(node,before){node.parent=this;const i=this.children.indexOf(before);if(i<0)this.append(node);else this.children.splice(i,0,node);}
 setAttribute(k,v){this.attributes[k]=v;}
 addEventListener(name,fn){this.listeners[name]=fn;}
 querySelector(s){return this.children.find(c=>s.startsWith('.')?(c.className||'').split(' ').includes(s.slice(1)):c.tag===s)||this.children.map(c=>c.querySelector(s)).find(Boolean)||null;}
 remove(){this.isConnected=false;if(this.parent)this.parent.children=this.parent.children.filter(c=>c!==this);}
}
const flat=n=>[n.textContent,...n.children.map(flat)].join(' ');
const tick=()=>new Promise(setImmediate);
async function moduleAt(file,host){
 const document={createElement:t=>new E(t),createTextNode:text=>Object.assign(new E('#text'),{textContent:text})};
 const context=vm.createContext({document,crypto:require('node:crypto').webcrypto,Set,Map});
 vm.runInContext(fs.readFileSync(path.join(__dirname,file),'utf8').replaceAll('export function','function'),context);
 return {api:context,root:{querySelector:()=>host}};
}
test('empty or revoked routes never show an idle permission card; real approvals remain visible',async()=>{
 const host=new E(),{api,root}=await moduleAt('notification-panel.js',host);let routes=[];
 const ui=api.mountNotificationPanel({root,guard:fn=>fn,request:async()=>({routes}),note:()=>{}});
 ui.messagingRoutes.open('new-chat',true);assert.equal(host.children[2].children[0].hidden,true);await ui.messagingRoutes.refresh();
 assert.equal(host.children[2].hidden,true);assert.equal(host.children[2].children[0].hidden,true);
 ui.showTool({approval:{id:'approval',name:'message_other_session',messaging_binding:{target_title:'Destination'},arguments:{}},run:{id:'run'},expanded:new Set(),decide:()=>{}});
 assert.equal(host.children[1].children.length,1);assert.equal(host.children[1].children[0].hidden,false);
 routes=[{revoked:true}];ui.messagingRoutes.open('existing',true);await ui.messagingRoutes.refresh();assert.equal(host.children[2].hidden,true);
 ui.destroy();
});
test('saved routes remain revocable and revoking the last route hides the entire card',async()=>{
 const host=new E(),{api,root}=await moduleAt('notification-panel.js',host);let revoked=false;
 const calls=[],route={key:'route',revision:3,target_title:'Destination',scope:{target_session_id:'target'}};
 const ui=api.mountNotificationPanel({root,guard:fn=>fn,request:async(...args)=>{calls.push(args);if(args[1]==='POST')revoked=true;return {routes:revoked?[]:[route]};},note:()=>{}});
 ui.messagingRoutes.open('source',true);await ui.messagingRoutes.refresh();const slot=host.children[2],card=slot.children[0];assert.equal(slot.hidden,true);assert.equal(card.hidden,true);
 ui.showTool({approval:{id:'approval',name:'message_other_session',messaging_binding:{target_title:'Destination'},arguments:{}},run:{id:'run'},expanded:new Set(),decide:()=>{}});assert.equal(slot.hidden,false);assert.equal(card.hidden,false);
 await card.querySelector('button').onclick();await card.querySelector('.chat-notification-deny').onclick();
 assert.equal(calls.find(c=>c[1]==='POST')[2].expected_revision,3);assert.equal(slot.hidden,true);assert.equal(card.hidden,true);
 ui.destroy();
});
test('late and failed route reads do not reveal a stale card or suppress approval controls',async()=>{
 const host=new E(),{api,root}=await moduleAt('notification-panel.js',host);let resolve;const errors=[];
 const ui=api.mountNotificationPanel({root,guard:fn=>fn,request:()=>new Promise(r=>resolve=r),note:e=>errors.push(e)});
 ui.messagingRoutes.open('old',true);const pending=ui.messagingRoutes.refresh();ui.messagingRoutes.disconnect();resolve({routes:[{key:'old',target_title:'Old',scope:{target_session_id:'old'}}]});await pending;
 assert.equal(host.children[2].hidden,true);assert.equal(host.children[2].children[0].hidden,true);ui.destroy();
 const f=await moduleAt('notification-panel.js',new E());const broken=f.api.mountNotificationPanel({root:f.root,guard:fn=>fn,request:async()=>{throw Error('offline');},note:e=>errors.push(e)});
 broken.messagingRoutes.open('new',true);await assert.rejects(broken.messagingRoutes.refresh(),/offline/);assert.equal(f.root.querySelector().children[2].hidden,true);broken.destroy();
});
test('an older refresh cannot undo the latest saved-route visibility',async()=>{
 const host=new E(),{api,root}=await moduleAt('notification-panel.js',host),pending=[];
 const ui=api.mountNotificationPanel({root,guard:fn=>fn,request:()=>new Promise(resolve=>pending.push(resolve)),note:()=>{}});
 ui.messagingRoutes.open('source',true);const old=ui.messagingRoutes.refresh(),latest=ui.messagingRoutes.refresh();
 pending[1]({routes:[]});await latest;pending[0]({routes:[{key:'old',scope:{target_session_id:'target'}}]});await old;
 assert.equal(host.children[2].hidden,true);assert.equal(host.children[2].children[0].hidden,true);ui.destroy();
});
test('saved permissions are hidden without pending approvals, including nested route cards',async()=>{
 const host=new E(),{api,root}=await moduleAt('notification-panel.js',host);
 const ui=api.mountNotificationPanel({root,guard:fn=>fn,request:async()=>({routes:[{key:'saved',scope:{target_session_id:'target'}}]}),note:()=>{}});
 ui.messagingRoutes.open('source',true);await ui.messagingRoutes.refresh();const slot=host.children[2],card=slot.children[0],list=card.children[1];
 assert.equal(card.hidden,true);assert.equal(list.children[0].hidden,true);
 ui.showTool({approval:{id:'tool',name:'file_write',arguments:{}},run:{id:'run'},expanded:new Set(),decide:()=>{}});
 assert.equal(card.hidden,false);assert.equal(list.children[0].hidden,false);
 ui.clearTools();assert.equal(slot.hidden,true);assert.equal(card.hidden,true);assert.equal(list.children[0].hidden,true);
 ui.destroy();
});
test('paused and stopped runs stay quiet while observer and recovery notices retain guarded actions',async()=>{
 const host=new E(),{api,root}=await moduleAt('notification-panel.js',host),calls=[];let guarded=0;
 const ui=api.mountNotificationPanel({root,guard:fn=>(...args)=>{guarded++;return fn(...args);}});
 for(const status of ['paused','stopped'])for(const canControl of [true,false])assert.equal(ui.showRun({run:{id:status,status,error:'Cancelled by user'},canControl}),undefined);
 assert.equal(host.children[3].children.length,0);
 const waiting=ui.showRun({run:{id:'waiting',status:'waiting_approval',pending:{arguments:{secret:'hidden'},can_decide:false}},canControl:false});assert.match(flat(waiting),/Waiting for the authorized user/);assert.doesNotMatch(flat(waiting),/hidden/);
 const recovery=ui.showRun({run:{id:'uncertain',status:'paused',uncertain:true},canControl:true,expanded:new Set(),renderRecovery:(run,{button,decide})=>{const body=new E();body.append(button('Authorize retry',()=>decide('retry','known')));return body;},recover:(...args)=>calls.push(args)});
 assert.equal(recovery.querySelector('.chat-notification-approve').className,'chat-notification-action chat-notification-approve');recovery.querySelector('button').onclick();assert.deepEqual(calls[0],['retry','known']);assert.equal(guarded,1);assert.doesNotMatch(flat(recovery),/Resume run/);
 for(const status of ['completed','running','queued','failed'])assert.equal(ui.showRun({run:{id:status,status}}),undefined);
 ui.clearRuns();assert.equal(host.children[3].children.length,0);ui.destroy();assert.equal(host.children.length,0);
});
test('send failure card survives run refreshes; Retry and Dismiss clear it',async()=>{
 const host=new E(),{api,root}=await moduleAt('notification-panel.js',host);let retried=0;
 const ui=api.mountNotificationPanel({root,guard:fn=>fn,request:async()=>({routes:[]}),note:()=>{}});
 ui.showSendError({message:'Model unavailable',retry:async()=>{retried++;}});
 assert.match(flat(host),/Message not sent/);assert.match(flat(host),/Model unavailable/);
 ui.clearRuns();assert.match(flat(host),/Message not sent/);
 const buttons=node=>[...(node.tag==='button'?[node]:[]),...node.children.flatMap(buttons)];
 await buttons(host).find(b=>b.textContent==='Retry').onclick();assert.equal(retried,1);assert.doesNotMatch(flat(host),/Message not sent/);
 ui.showSendError({message:'again',retry:async()=>{}});buttons(host).find(b=>b.textContent==='Dismiss').onclick();assert.doesNotMatch(flat(host),/Message not sent/);
});
