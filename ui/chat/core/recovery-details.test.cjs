const {test}=require('node:test');
const assert=require('node:assert/strict'),fs=require('node:fs'),path=require('node:path'),vm=require('node:vm');
function render(pending,extra={}){
 const nodes=[];const document={createElement(tag){const node={tag,children:[],textContent:'',append(...items){this.children.push(...items);},addEventListener(){},setAttribute(){}};nodes.push(node);return node;}};
 const source=fs.readFileSync(path.join(__dirname,'recovery-details.js'),'utf8').replace('export function','function');
 const context=vm.createContext({document,Set});vm.runInContext(source,context);
 const decisions=[];const root=context.createRecoveryDetails({id:'run-1',pending,...extra},{expanded:new Set(),button:(label,click)=>({textContent:label,click}),decide:(...args)=>decisions.push(args)});
 return {root,nodes,text:nodes.map(n=>n.textContent).join('\n'),decisions};
}
test('specific failure label and stage replace misleading transport label',()=>{
 const f=render({name:'build_work',interruption:{reason:'transport_error'},failure:{version:1,stage:'authorization',code:'legacy_authorization_rejected',delivery:'not_sent',elapsed_ms:4}});
 assert.match(f.text,/Broker authorization rejected/);assert.match(f.text,/Checking authorization/);assert.match(f.text,/Request not sent/);assert.doesNotMatch(f.text,/Connection interrupted/);
 const actions=f.root.children.at(-1).children;assert.deepEqual(actions.map(x=>x.textContent),['Accept known result','Authorize retry','Abandon run']);actions[1].click();assert.deepEqual(f.decisions,[['retry',undefined]]);
});
test('legacy records remain readable and unknown diagnostic text is never rendered',()=>{
 const legacy=render({name:'shell',interruption:{reason:'transport_error'}});assert.match(legacy.text,/Connection interrupted/);assert.match(legacy.text,/Detailed failure evidence unavailable/);
 const invalid=render({failure:{version:1,code:'SECRET_CANARY',stage:'SECRET_CANARY',delivery:'SECRET_CANARY'}});assert.doesNotMatch(invalid.text,/SECRET_CANARY/);
});
test('response validation and execution failure have distinct labels',()=>{
 for(const [stage,code,label]of[['response_validation','malformed_response','Invalid tool response'],['execution_task','task_panicked','Execution task failed']]){
  const f=render({failure:{version:1,stage,code,delivery:'response_received',elapsed_ms:12}});assert.ok(f.text.includes(label));assert.doesNotMatch(f.text,/Connection interrupted/);
 }
});

test('parallel failure metadata retains the tool and call reference without pending arguments',()=>{
 const f=render(null,{tool_failure:{version:1,stage:'response_read',code:'disconnected_response',tool:'build_work',call_id:'parallel-call-2',delivery:'possibly_sent',elapsed_ms:9}});
 assert.match(f.text,/build_work/);assert.match(f.text,/Call: parallel-call-2/);
});
