const {test}=require('node:test');
const assert=require('node:assert/strict');
const fs=require('node:fs'),path=require('node:path'),vm=require('node:vm');
class E {
 constructor(tag){this.tag=tag;this.children=[];this.attributes={};this.dataset={};this.textContent='';this.open=false;}
 append(...items){this.children.push(...items);}
 setAttribute(k,v){this.attributes[k]=v;}
 addEventListener(name,fn){this.onclose=fn;}
 showModal(){this.open=true;}
 close(){this.open=false;this.onclose?.();}
 remove(){document.body.children=document.body.children.filter(c=>c!==this);}
 focus(){document.activeElement=this;}
 all(){return [this,...this.children.flatMap(c=>c.all())];}
}
const document={body:new E('body'),createElement:tag=>new E(tag),querySelector:()=>null};
async function load(){const context=vm.createContext({document,location:{host:'dev.webagent.live'}});
 const mod=new vm.SourceTextModule(fs.readFileSync(path.join(__dirname,'credential-panel.js'),'utf8'),{context});await mod.link(()=>{});await mod.evaluate();return mod.namespace;}
test('sign-in panel collects no secrets and closes before starting the popup flow',async()=>{
 const {openSignInPanel}=await load();const calls=[];document.body.children=[];
 openSignInPanel({agent:'Helper',message:'Use the popup.',identity:'',signIn:()=>calls.push(document.body.children.length),guest:null});
 const dialog=document.body.children[0],nodes=dialog.all(),buttons=nodes.filter(n=>n.tag==='button');
 assert.equal(dialog.open,true);assert.equal(nodes.some(n=>n.tag==='input'||n.tag==='form'),false);
 assert.deepEqual(buttons.map(b=>b.textContent),['Sign in','Cancel']);assert.equal(document.activeElement,buttons[0]);
 buttons[0].onclick();assert.deepEqual(calls,[0]);assert.equal(dialog.open,false);
});
test('guest choice appears only when offered',async()=>{
 const {openSignInPanel}=await load();let guests=0;document.body.children=[];
 openSignInPanel({agent:'Helper',message:'Use the popup.',identity:'Guest',signIn:()=>{},guest:()=>guests++});
 const nodes=document.body.children[0].all(),buttons=nodes.filter(n=>n.tag==='button');
 assert.deepEqual(buttons.map(b=>b.textContent),['Sign in','Continue as guest','Cancel']);
 assert.ok(nodes.some(n=>n.textContent==='Signed in as Guest'));buttons[1].onclick();assert.equal(guests,1);
});
