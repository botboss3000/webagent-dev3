const {test}=require('node:test');
const assert=require('node:assert/strict');
const fs=require('node:fs'),path=require('node:path'),vm=require('node:vm');
class E {
 constructor(tag='div'){this.tag=tag;this.children=[];this.attributes={};this.dataset={};this.hidden=false;this.isConnected=true;this.textContent='';}
 append(...items){for(const item of items){item.parent=this;this.children.push(item);}}
 replaceChildren(...items){this.children=[];this.append(...items);}
 get lastChild(){return this.children.at(-1);}
 insertBefore(node,before){node.parent=this;const i=this.children.indexOf(before);if(i<0)this.append(node);else this.children.splice(i,0,node);}
 setAttribute(k,v){this.attributes[k]=v;}
 querySelector(s){return this.children.find(c=>s.startsWith('.')?(c.className||'').split(' ').includes(s.slice(1)):c.tag===s)||this.children.map(c=>c.querySelector(s)).find(Boolean)||null;}
 remove(){this.isConnected=false;if(this.parent)this.parent.children=this.parent.children.filter(c=>c!==this);}
}
const flat=n=>[n.textContent,...n.children.map(flat)].join(' ');
const tick=()=>new Promise(setImmediate);
async function moduleAt(file,host){
 const document={createElement:t=>new E(t),createTextNode:text=>Object.assign(new E('#text'),{textContent:text})};
 const context=vm.createContext({document,crypto:require('node:crypto').webcrypto,Set,Map});
 const mod=new vm.SourceTextModule(fs.readFileSync(path.join(__dirname,'../ui/chat/core/',file),'utf8'),{context});
 await mod.link(()=>{});await mod.evaluate();return {api:mod.namespace,root:{querySelector:()=>host}};
}
test('messaging approval exposes destination and remembers only an explicit checkbox across polling',async()=>{
 const host=new E(),{api,root}=await moduleAt('chat-notifications.js',host);
 const ui=api.mountChatNotifications({root,guard:fn=>fn}),calls=[];
 const approval={id:'approve-one',name:'message_other_session',new_intent_confirmation:true,messaging_can_remember:true,messaging_binding:{source_title:'Source',target_title:'Destination',target_session_id:'target',target_agent_id:'agent',delivery:'steer'}};
 const show=()=>ui.showTool({approval,run:{id:'run'},expanded:new Set(),decide:(...args)=>calls.push(args)});
 show();assert.match(flat(host),/Source.*Destination.*steer/);
 let checkbox=host.querySelector('input');assert.equal(checkbox.checked,false);checkbox.checked=true;checkbox.onchange();
 ui.clearTools();show();checkbox=host.querySelector('input');assert.equal(checkbox.checked,true);
 host.querySelector('.chat-notification-approve').onclick();
 assert.equal(calls[0][0],'approve');assert.equal(calls[0][1].remember_messaging_route,true);
 ui.clearTools();approval.messaging_can_remember=false;show();assert.equal(host.querySelector('input'),null);
 host.querySelector('.chat-notification-approve').onclick();assert.deepEqual(JSON.parse(JSON.stringify(calls[1][1])),{});
 ui.destroy();assert.equal(host.children.length,0);
});
test('route revocation is revision fenced and retries retain request identity',async()=>{
 const host=new E(),{api,root}=await moduleAt('messaging-routes.js',host),calls=[],errors=[];
 const route={key:'route-key',revision:7,target_title:'Recipient',scope:{target_session_id:'target'}};
 let fail=true;
 const request=async(...args)=>{calls.push(args);if(args[1]==='POST'){if(fail)throw Error('connection lost');return {};}return {routes:[route]};};
 const ui=api.mountMessagingRoutes({root,request,guard:fn=>async()=>{try{await fn();}catch(e){errors.push(e.message);}},note:e=>errors.push(e)});
 ui.open('source',false);assert.equal(host.children[0].hidden,true);
 ui.open('source',true);const disclosure=host.children[0];disclosure.open=true;disclosure.ontoggle();await tick();
 assert.match(flat(host),/Recipient/);
 const revoke=host.querySelector('button');await revoke.onclick();await revoke.onclick();
 const writes=calls.filter(c=>c[1]==='POST');assert.equal(writes.length,2);
 assert.equal(writes[0][2].expected_revision,7);assert.equal(writes[0][2].client_request_id,writes[1][2].client_request_id);
 fail=false;await revoke.onclick();assert.equal(errors.length,2);ui.destroy();assert.equal(host.children.length,0);
});
test('late route reads cannot populate a newly selected conversation',async()=>{
 const host=new E(),{api,root}=await moduleAt('messaging-routes.js',host);let resolve;
 const ui=api.mountMessagingRoutes({root,request:()=>new Promise(r=>resolve=r),guard:fn=>fn,note:()=>{}});
 ui.open('old',true);const disclosure=host.children[0];disclosure.open=true;disclosure.ontoggle();await tick();
 ui.open('new',true);resolve({routes:[{key:'old',revision:1,target_title:'Old destination',scope:{target_session_id:'old'}}]});await tick();
 assert.equal(disclosure.children[1].children.length,0);assert.equal(disclosure.open,false);ui.destroy();
});
