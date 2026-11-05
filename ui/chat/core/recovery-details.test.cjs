const {test}=require('node:test');
const assert=require('node:assert/strict'),fs=require('node:fs'),path=require('node:path'),vm=require('node:vm');
function render(pending,extra={}){
 const nodes=[];const document={createElement(tag){const node={tag,children:[],textContent:'',append(...items){this.children.push(...items);},addEventListener(){},setAttribute(){}};nodes.push(node);return node;}};
 const source=fs.readFileSync(path.join(__dirname,'recovery-details.js'),'utf8').replace('export function','function');
 const context=vm.createContext({document,Set});vm.runInContext(source,context);
 const decisions=[];const root=context.createRecoveryDetails({id:'run-1',pending,...extra},{expanded:new Set(),button:(label,click)=>({textContent:label,click}),decide:(...args)=>decisions.push(args)});
 return {root,nodes,text:nodes.map(n=>n.textContent).join('\n'),decisions};
}
const flat=node=>[node,...(node.children||[]).flatMap(flat)];
const buttons=f=>flat(f.root).filter(n=>typeof n.click==='function');
test('choices are visible without expanding, and diagnostics keep specific labels',()=>{
 const f=render({name:'build_work',call_id:'c1',id:'pend-1',interruption:{reason:'transport_error'},failure:{version:1,stage:'authorization',code:'legacy_authorization_rejected',delivery:'not_sent',elapsed_ms:4}});
 assert.deepEqual(buttons(f).map(b=>b.textContent),['Run again','Use my result','End run']);
 assert.equal(f.root.children.some(n=>n.tag==='details'&&n.className==='recovery-diagnostics'),true);
 assert.match(f.text,/Broker authorization rejected/);assert.match(f.text,/Checking authorization/);assert.match(f.text,/Request not sent/);
 assert.match(f.text,/Interaction: pend-1/);
 const [again,mine,end]=buttons(f);assert.equal(mine.disabled,true);
 again.click();end.click();assert.deepEqual(f.decisions,[['retry',undefined,undefined],['abandon','']]);
});
test('legacy records remain readable and unknown diagnostic text is never rendered',()=>{
 const legacy=render({name:'shell',interruption:{reason:'transport_error'}});assert.match(legacy.text,/Connection interrupted/);assert.match(legacy.text,/Detailed failure evidence unavailable/);
 const invalid=render({failure:{version:1,code:'SECRET_CANARY',stage:'SECRET_CANARY',delivery:'SECRET_CANARY'}});assert.doesNotMatch(invalid.text,/SECRET_CANARY/);
});
test('a run with no open call offers resume and end only',()=>{
 const f=render(null,{tool_failure:{version:1,stage:'response_read',code:'cancelled',tool:'dev_browser_action',call_id:'parallel-call-2',delivery:'possibly_sent',elapsed_ms:9}});
 assert.deepEqual(buttons(f).map(b=>b.textContent),['Resume run','End run']);
 assert.match(f.text,/No call needs a decision/);assert.match(f.text,/Last failed call: parallel-call-2/);
 buttons(f)[0].click();assert.deepEqual(f.decisions,[['retry','']]);
});
test('each uncertain parallel call shows its tool, action, arguments and IDs and resolves by call ID',()=>{
 const f=render(null,{uncertain_calls:[
  {call_id:'call-a',name:'dev_browser_action',arguments:{action:'navigate',path:'/app'},interaction_id:'pend-a',transport_request_id:'req-a'},
  {call_id:'call-b',name:'dev_browser_action',arguments:{action:'read'},interaction_id:'pend-b'}]});
 assert.match(f.text,/dev_browser_action · navigate/);assert.match(f.text,/"path": "\/app"/);assert.match(f.text,/2 tool calls were interrupted/);
 assert.match(f.text,/Call: call-a · Interaction: pend-a · Request: req-a/);assert.match(f.text,/Call: call-b · Interaction: pend-b/);
 assert.deepEqual(buttons(f).map(b=>b.textContent),['Run again','Use my result','Run again','Use my result','End run']);
 buttons(f)[2].click();assert.deepEqual(f.decisions,[['retry',undefined,'call-b']]);
});
