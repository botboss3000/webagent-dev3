const {test}=require('node:test'),assert=require('node:assert/strict'),fs=require('node:fs'),vm=require('node:vm');
class Node {
 constructor(){this.children=[];this.parentNode=null;this.dataset={};this.value='';this.clientWidth=1000;this.style={setProperty(){}};this.classes=new Set();this.classList={toggle:(key,value)=>value?this.classes.add(key):this.classes.delete(key),add:key=>this.classes.add(key)};}
 append(...nodes){for(const node of nodes){if(node.parentNode)node.parentNode.children=node.parentNode.children.filter(n=>n!==node);this.children.push(node);node.parentNode=this;}}
 replaceChildren(...nodes){for(const node of this.children)node.parentNode=null;this.children=[];this.append(...nodes);}
 setAttribute(){}addEventListener(){}contains(){return false;}closest(){return this;}
 querySelector(selector){if(selector.includes(':not(:empty)'))return this.children.find(n=>n.children.length)||null;return null;}
 querySelectorAll(){return [];}
}
// Model the document event boundary; notification listeners are outside this layout fixture.
async function renderer(surface){
 const nodes={};const get=id=>nodes[id]||(nodes[id]=new Node());
 const context=vm.createContext({document:{getElementById:get,querySelector:()=>new Node(),querySelectorAll:()=>[],createElement:()=>new Node(),addEventListener(){},dispatchEvent(){return true;}},window:{innerWidth:surface==='mobile'?390:1280,__CHAT_PORTAL__:surface==='widget',addEventListener(){}},ResizeObserver:class{observe(){}},setTimeout,Event:class{},Number,Math});
 const module=new vm.SourceTextModule(fs.readFileSync(__dirname+'/chat-config.js','utf8'),{context});await module.link((specifier)=>{const exports=specifier.includes('bubble-paint')?['applyBubblePaint']:specifier.includes('chat-pill-config')?['applyChatPillLayout','disposeChatPillLayout']:['initFooterHandle'];return new vm.SyntheticModule(exports,function(){for(const key of exports)this.setExport(key,()=>{})},{context});});await module.evaluate();
 get('chat-panel').clientWidth=surface==='mobile'?390:1000;
 const controls={share:new Node(),request_help:new Node(),agent_active:new Node()};module.namespace.setSharedChatControls(controls);
 return {api:module.namespace,nodes,get,controls};
}
const inTree=(root,node)=>root===node||root.children.some(child=>inTree(child,node));
for(const surface of ['desktop','mobile','widget'])test(surface+' uses its configured header and idle/active footer locations',async()=>{
 const r=await renderer(surface),profile='chat_'+surface;
 const config={chat_common:{chat_header:{rows:[{left:['share']}]},active_footer:{chat_pill:{},below_pill:{rows:[{right:['request_help']}]}},idle_footer:{chat_pill:{},below_pill:{rows:[{center:['agent_active']}]}}},[profile]:{chat_header:{rows:[{right:['share']}]}}};
 await r.api.initChatConfig({config});
 assert.equal(inTree(r.get('chat-header'),r.controls.share),true);
 assert.equal(inTree(r.get('chat-footer-row'),r.controls.agent_active),true);
 assert.equal(inTree(r.get('chat-footer-row'),r.controls.request_help),false);
 assert.equal(r.get('chat-footer-row').classes.has('idle-collapsed'),false);
 r.get('chat-input').value='A follow-up';r.api.applyChatConfig();
 assert.equal(inTree(r.get('chat-header'),r.controls.share),true);
 assert.equal(inTree(r.get('chat-footer-row'),r.controls.request_help),true);
 assert.equal(inTree(r.get('chat-footer-row'),r.controls.agent_active),false);
 r.get('chat-input').value='';r.api.applyChatConfig();
 assert.equal(inTree(r.get('chat-footer-row'),r.controls.agent_active),true);
});
test('an omitted idle footer does not inherit active-only controls',async()=>{
 const r=await renderer('desktop');await r.api.initChatConfig({config:{chat_common:{chat_header:{rows:[]},active_footer:{below_pill:{rows:[{left:['request_help']}]}},idle_footer:{}}}});
 assert.equal(inTree(r.get('chat-footer-row'),r.controls.request_help),false);
});

test('explicitly disabled idle rows remain hidden despite configured controls',async()=>{const r=await renderer('mobile');await r.api.initChatConfig({config:{chat_common:{chat_header:{rows:[]},idle_footer:{below_pill:{enabled:false,rows:[{left:['agent_active']}]}}}}});assert.equal(r.get('chat-footer-row').hidden,true);});
