const test=require('node:test');
const assert=require('node:assert/strict');
test('storage notice shows once, only when near the limit',async()=>{
  const {createStorageNotice}=await import('./storage-notice.js');
  const shown=[];
  const near=createStorageNotice({request:async()=>({near_limit:true,used_bytes:850*1024*1024,limit_bytes:1024*1024*1024}),show:n=>shown.push(n)});
  await near();await near();
  assert.equal(shown.length,1);
  assert.match(shown[0].message,/850 MB of 1024 MB \(83%\)/);
  assert.equal(shown[0].timeout,0);
  const fine=createStorageNotice({request:async()=>({near_limit:false,used_bytes:1,limit_bytes:null}),show:n=>shown.push(n)});
  await fine();
  const broken=createStorageNotice({request:async()=>{throw new Error('offline');},show:n=>shown.push(n)});
  await broken();
  assert.equal(shown.length,1);
});
