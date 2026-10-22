const {test}=require('node:test'),assert=require('node:assert/strict');
const fs=require('node:fs'),path=require('node:path'),vm=require('node:vm');
/** Minimal ancestry-aware DOM fixture; nested message identities must share one delivery owner. */
class Element {
  constructor(tag='div'){this.tag=tag;this.children=[];this.dataset={};this.className='';this.textContent='';this.attributes={};}
  append(...nodes){for(const node of nodes){node.remove();node.parentElement=this;this.children.push(node);}}
  remove(){if(this.parentElement)this.parentElement.children=this.parentElement.children.filter(node=>node!==this);this.parentElement=null;}
  replaceChildren(...nodes){for(const node of [...this.children])node.remove();this.append(...nodes);}
  matches(selector){return selector==='[data-public-id]'?!!this.dataset.publicId:selector==='button'?this.tag==='button':selector.startsWith('.')&&this.className.split(' ').includes(selector.slice(1));}
  closest(selector){return this.matches(selector)?this:this.parentElement?.closest(selector)||null;}
  querySelectorAll(selector){return this.children.flatMap(node=>[...(node.matches(selector)?[node]:[]),...node.querySelectorAll(selector)]);}
  querySelector(selector){return this.querySelectorAll(selector)[0]||null;}
  contains(node){return node===this||this.children.some(child=>child.contains(node));}
  setAttribute(key,value){this.attributes[key]=value;}
  get childNodes(){return this.children;}
}
/** Exercise the real queue renderer with both the transcript row and its nested bubble identity. */
test('nested bubble identities keep one delivery label and one set of actions below the gutter',async()=>{
  const root=new Element(),row=new Element(),bubble=new Element(),gutter=new Element(),host=new Element();
  row.dataset.publicId=bubble.dataset.publicId='user:fixture';gutter.className='turn-gutter';row.append(bubble,gutter);root.append(row,host);
  const context=vm.createContext({document:{activeElement:null,createElement:tag=>new Element(tag)},Set,Number});
  const module=new vm.SourceTextModule(fs.readFileSync(path.join(__dirname,'followup-queue.js'),'utf8'),{context});
  await module.link(()=>new vm.SourceTextModule(fs.readFileSync(path.join(__dirname,'chat-notifications.js'),'utf8'),{context}));await module.evaluate();
  const queue=module.namespace.mountFollowupQueue({root,host,client:{},getSession:()=> 'fixture',guard:fn=>fn,note(){},keyId:()=> 'fixture',forgetKey(){},onSnapshot(){}});
  for(const [revision,status,actions]of [[1,'pending',4],[2,'delivered',0],[3,'delivered',0]]){
    queue.update({queue:[{id:'entry',message_id:'user:fixture',revision,choice:'queue',status}],queue_state:{revision}});
    assert.equal(root.querySelectorAll('.followup-delivery-status').length,1);
    assert.equal(root.querySelectorAll('button').length,actions);
    assert.equal(bubble.querySelector('.followup-delivery'),null);
    assert.equal(row.children[1],gutter);
    assert.equal(row.children[2].className,'followup-delivery');
  }
  queue.destroy();assert.equal(root.querySelector('.followup-delivery'),null);
});
