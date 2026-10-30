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
 showModal(){this.open=true;}
 close(){this.open=false;}
 focus(){}
 remove(){this.isConnected=false;if(this.parent)this.parent.children=this.parent.children.filter(c=>c!==this);}
}
const flat=n=>[n.textContent,...n.children.map(flat)].join(' ');
const tick=()=>new Promise(setImmediate);
async function moduleAt(file,host){
 const document={createElement:t=>new E(t),createTextNode:text=>Object.assign(new E('#text'),{textContent:text})};
 const context=vm.createContext({document,crypto:require('node:crypto').webcrypto,Set,Map,URL});
 vm.runInContext(fs.readFileSync(path.join(__dirname,file),'utf8').replaceAll('export function','function'),context);
 return {api:context,root:{querySelector:()=>host}};
}
test('empty or revoked routes never show an idle permission card; real approvals remain visible',async()=>{
 const host=new E(),{api,root}=await moduleAt('chat-action-panel.js',host);let routes=[];
 const ui=api.mountChatActionPanel({root,guard:fn=>fn,request:async()=>({routes}),note:()=>{}});
 ui.messagingRoutes.open('new-chat',true);assert.equal(host.children[2].children[0].hidden,true);await ui.messagingRoutes.refresh();
 assert.equal(host.children[2].hidden,true);assert.equal(host.children[2].children[0].hidden,true);
 ui.showTool({approval:{id:'approval',name:'message_other_session',messaging_binding:{target_title:'Destination'},arguments:{}},run:{id:'run'},expanded:new Set(),decide:()=>{}});
 assert.equal(host.children[1].children.length,1);assert.equal(host.children[1].children[0].hidden,false);
 routes=[{revoked:true}];ui.messagingRoutes.open('existing',true);await ui.messagingRoutes.refresh();assert.equal(host.children[2].hidden,true);
 ui.destroy();
});
test('saved routes remain revocable and revoking the last route hides the entire card',async()=>{
 const host=new E(),{api,root}=await moduleAt('chat-action-panel.js',host);let revoked=false;
 const calls=[],route={key:'route',revision:3,target_title:'Destination',scope:{target_session_id:'target'}};
 const ui=api.mountChatActionPanel({root,guard:fn=>fn,request:async(...args)=>{calls.push(args);if(args[1]==='POST')revoked=true;return {routes:revoked?[]:[route]};},note:()=>{}});
 ui.messagingRoutes.open('source',true);await ui.messagingRoutes.refresh();const slot=host.children[2],card=slot.children[0];assert.equal(slot.hidden,true);assert.equal(card.hidden,true);
 ui.showTool({approval:{id:'approval',name:'message_other_session',messaging_binding:{target_title:'Destination'},arguments:{}},run:{id:'run'},expanded:new Set(),decide:()=>{}});assert.equal(slot.hidden,false);assert.equal(card.hidden,false);
 await card.querySelector('button').onclick();await card.querySelector('.chat-action-deny').onclick();
 assert.equal(calls.find(c=>c[1]==='POST')[2].expected_revision,3);assert.equal(slot.hidden,true);assert.equal(card.hidden,true);
 ui.destroy();
});
test('late and failed route reads do not reveal a stale card or suppress approval controls',async()=>{
 const host=new E(),{api,root}=await moduleAt('chat-action-panel.js',host);let resolve;const errors=[];
 const ui=api.mountChatActionPanel({root,guard:fn=>fn,request:()=>new Promise(r=>resolve=r),note:e=>errors.push(e)});
 ui.messagingRoutes.open('old',true);const pending=ui.messagingRoutes.refresh();ui.messagingRoutes.disconnect();resolve({routes:[{key:'old',target_title:'Old',scope:{target_session_id:'old'}}]});await pending;
 assert.equal(host.children[2].hidden,true);assert.equal(host.children[2].children[0].hidden,true);ui.destroy();
 const f=await moduleAt('chat-action-panel.js',new E());const broken=f.api.mountChatActionPanel({root:f.root,guard:fn=>fn,request:async()=>{throw Error('offline');},note:e=>errors.push(e)});
 broken.messagingRoutes.open('new',true);await assert.rejects(broken.messagingRoutes.refresh(),/offline/);assert.equal(f.root.querySelector().children[2].hidden,true);broken.destroy();
});
test('an older refresh cannot undo the latest saved-route visibility',async()=>{
 const host=new E(),{api,root}=await moduleAt('chat-action-panel.js',host),pending=[];
 const ui=api.mountChatActionPanel({root,guard:fn=>fn,request:()=>new Promise(resolve=>pending.push(resolve)),note:()=>{}});
 ui.messagingRoutes.open('source',true);const old=ui.messagingRoutes.refresh(),latest=ui.messagingRoutes.refresh();
 pending[1]({routes:[]});await latest;pending[0]({routes:[{key:'old',scope:{target_session_id:'target'}}]});await old;
 assert.equal(host.children[2].hidden,true);assert.equal(host.children[2].children[0].hidden,true);ui.destroy();
});
test('saved permissions are hidden without pending approvals, including nested route cards',async()=>{
 const host=new E(),{api,root}=await moduleAt('chat-action-panel.js',host);
 const ui=api.mountChatActionPanel({root,guard:fn=>fn,request:async()=>({routes:[{key:'saved',scope:{target_session_id:'target'}}]}),note:()=>{}});
 ui.messagingRoutes.open('source',true);await ui.messagingRoutes.refresh();const slot=host.children[2],card=slot.children[0],list=card.children[1];
 assert.equal(card.hidden,true);assert.equal(list.children[0].hidden,true);
 ui.showTool({approval:{id:'tool',name:'file_write',arguments:{}},run:{id:'run'},expanded:new Set(),decide:()=>{}});
 assert.equal(card.hidden,false);assert.equal(list.children[0].hidden,false);
 ui.clearTools();assert.equal(slot.hidden,true);assert.equal(card.hidden,true);assert.equal(list.children[0].hidden,true);
 ui.destroy();
});
test('paused and stopped runs stay quiet while observer and recovery notices retain guarded actions',async()=>{
 const host=new E(),{api,root}=await moduleAt('chat-action-panel.js',host),calls=[];let guarded=0;
 const ui=api.mountChatActionPanel({root,guard:fn=>(...args)=>{guarded++;return fn(...args);}});
 for(const status of ['paused','stopped'])for(const canControl of [true,false])assert.equal(ui.showRun({run:{id:status,status,error:'Cancelled by user'},canControl}),undefined);
 assert.equal(host.children[3].children.length,0);
 const waiting=ui.showRun({run:{id:'waiting',status:'waiting_approval',pending:{arguments:{secret:'hidden'},can_decide:false}},canControl:false});assert.match(flat(waiting),/Waiting for the authorized user/);assert.doesNotMatch(flat(waiting),/hidden/);
 const recovery=ui.showRun({run:{id:'uncertain',status:'paused',uncertain:true},canControl:true,expanded:new Set(),renderRecovery:(run,{button,decide})=>{const body=new E();body.append(button('Authorize retry',()=>decide('retry','known')));return body;},recover:(...args)=>calls.push(args)});
 assert.equal(recovery.querySelector('.chat-action-approve').className,'chat-action-action chat-action-approve');recovery.querySelector('button').onclick();assert.deepEqual(calls[0],['retry','known']);assert.equal(guarded,1);assert.doesNotMatch(flat(recovery),/Resume run/);
 for(const status of ['completed','running','queued','failed'])assert.equal(ui.showRun({run:{id:status,status}}),undefined);
 ui.clearRuns();assert.equal(host.children[3].children.length,0);ui.destroy();assert.equal(host.children.length,0);
});
test('send failure card survives run refreshes; Retry and Dismiss clear it',async()=>{
 const host=new E(),{api,root}=await moduleAt('chat-action-panel.js',host);let retried=0;
 const ui=api.mountChatActionPanel({root,guard:fn=>fn,request:async()=>({routes:[]}),note:()=>{}});
 ui.showSendError({message:'Model unavailable',retry:async()=>{retried++;}});
 assert.match(flat(host),/Message not sent/);assert.match(flat(host),/Model unavailable/);
 ui.clearRuns();assert.match(flat(host),/Message not sent/);
 const buttons=node=>[...(node.tag==='button'?[node]:[]),...node.children.flatMap(buttons)];
 await buttons(host).find(b=>b.textContent==='Retry').onclick();assert.equal(retried,1);assert.doesNotMatch(flat(host),/Message not sent/);
 ui.showSendError({message:'again',retry:async()=>{}});buttons(host).find(b=>b.textContent==='Dismiss').onclick();assert.doesNotMatch(flat(host),/Message not sent/);
});
test('credential card follows its session request: link not vault name, draft kept, saved and dismissed server-side',async()=>{
 const host=new E(),{api,root}=await moduleAt('chat-action-panel.js',host);const calls=[];
 const ui=api.mountChatActionPanel({root,guard:fn=>fn,request:async(...args)=>{calls.push(args);return {};},note:()=>{}});
 const all=(n,tag)=>[...(n.tag===tag?[n]:[]),...n.children.flatMap(c=>all(c,tag))];
 const detail={name:'source_x',origin:'https://api.github.com',requestId:'a'.repeat(64),revision:0,label:'GitHub token',createUrl:'https://github.com/settings/personal-access-tokens/new'};
 ui.showCredential(detail,'sid');const slot=host.children.at(-1);
 assert.ok(flat(slot).includes('GitHub token'));assert.ok(!flat(slot).includes('source_x'));
 const link=all(slot,'a')[0];assert.equal(link.href,detail.createUrl);assert.equal(link.target,'_blank');assert.equal(link.textContent,'github.com/settings/personal-access-tokens/new');
 const button=label=>all(slot,'button').find(b=>b.textContent===label);
 assert.equal(slot.querySelector('.chat-action-secure').attributes['aria-label'],'Private vault input');
 const note=slot.querySelector('.chat-action-note');assert.match(flat(note),/Only you see this value/);assert.ok(note.querySelector('.chat-action-secure'),'the note carries the shield');assert.equal(note.hidden,true);
 button('i').onclick();assert.equal(note.hidden,false);assert.equal(button('i').attributes['aria-expanded'],'true');
 button('i').onclick();assert.equal(note.hidden,true,'a second tap collapses the note');
 all(slot,'input')[0].value='typed';ui.showCredential({...detail},'sid');assert.equal(all(slot,'input')[0].value,'typed');
 await button('Save').onclick();
 assert.deepEqual(calls[0].slice(0,2),['/core/vault/sid/source_x','POST']);assert.equal(calls[0][2].value,'typed');assert.equal(calls[0][2].request_id,detail.requestId);
 assert.ok(flat(slot).includes('GitHub token saved'));ui.showCredential(null,'sid');assert.ok(flat(slot).includes('GitHub token saved'));
 ui.clearCredential();assert.equal(slot.children.length,0);
 ui.showCredential({...detail,requestId:'b'.repeat(64)},'sid');await button('Dismiss').onclick();
 assert.deepEqual(calls[1].slice(0,2),['/core/vault/sid/source_x','DELETE']);assert.equal(calls[1][2].request_id,'b'.repeat(64));assert.equal(slot.children.length,0);
 ui.showCredential(detail,'sid');ui.showCredential(null,'other');assert.equal(slot.children.length,0);
 ui.destroy();
});
test('shared notices float without a visible composer, replace by key and close after a successful action',async()=>{
 const body=new E('body'),byId={};
 const all=()=>{const out=[],walk=n=>{for(const c of n.children){out.push(c);walk(c);}};walk(body);return out;};
 const document={body,createElement:t=>new E(t),getElementById:id=>byId[id]||all().find(n=>n.id===id)||null,
  querySelectorAll:s=>all().filter(n=>s==='[data-notification-key]'&&n.dataset.notificationKey)};
 const context=vm.createContext({document,setTimeout,clearTimeout});
 vm.runInContext(fs.readFileSync(path.join(__dirname,'chat-action-panel.js'),'utf8').replaceAll('export function','function'),context);
 let undone=0,fail=true;
 context.showNotification({key:'usage',title:'First',timeout:0});
 context.showNotification({key:'usage',title:'Cleared',message:'Hidden',timeout:0,actions:[{label:'Undo',run:async()=>{if(fail)throw Error('Nothing to undo');undone++;}}]});
 const stack=body.children[0];assert.equal(stack.id,'app-notifications');assert.equal(stack.children.length,1);
 const card=stack.children[0],[undo,dismiss]=card.querySelector('.chat-action-actions').children;
 await undo.onclick();assert.equal(card.querySelector('.chat-action-message').textContent,'Nothing to undo');assert.equal(stack.children.length,1);
 fail=false;await undo.onclick();assert.equal(undone,1);assert.equal(stack.children.length,0);
 context.showNotification({title:'Other',timeout:0});dismiss.onclick();stack.children[0].querySelector('.chat-action-actions').children[0].onclick();assert.equal(stack.children.length,0);
});
