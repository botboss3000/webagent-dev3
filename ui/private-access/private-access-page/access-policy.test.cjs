const {test}=require('node:test');
const assert=require('node:assert/strict');
const fs=require('node:fs');
const path=require('node:path');
const vm=require('node:vm');
const source=fs.readFileSync(path.join(__dirname,'../../app.js'),'utf8');
const policyFunction=source.match(/^async function loadGuestPolicy\(\).*$/m)[0];

test('approval notice is hidden in markup and its hidden attribute overrides the grid',()=>{
  assert.match(fs.readFileSync(path.join(__dirname,'private-access-page.html'),'utf8'),/<div class="private-access-approval" role="note" hidden>/);
  assert.match(fs.readFileSync(path.join(__dirname,'private-access-page.css'),'utf8'),/\.private-access-approval\[hidden\],[^{]+\{ display: none; \}/);
});

test('approval stays hidden while loading, for open registration, and after failures',async()=>{
  const approval={hidden:false},guest={hidden:true};
  let resolve,reject;
  const context={document:{querySelector:()=>approval},$:()=>guest,api:()=>new Promise((yes,no)=>{resolve=yes;reject=no;})};
  vm.runInNewContext(policyFunction+';globalThis.load=loadGuestPolicy;',context);
  let pending=context.load();
  assert.equal(approval.hidden,true);
  resolve({access_mode:'public_registered',guest_access:true});await pending;
  assert.equal(approval.hidden,true);assert.equal(guest.hidden,false);
  pending=context.load();resolve({access_mode:'admin_approval',guest_access:false});await pending;
  assert.equal(approval.hidden,false);assert.equal(guest.hidden,true);
  pending=context.load();assert.equal(approval.hidden,true);
  reject(Error('offline'));await pending;
  assert.equal(approval.hidden,true);assert.equal(guest.hidden,true);
  pending=context.load();resolve({});await pending;
  assert.equal(approval.hidden,true);
});
