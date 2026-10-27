import test from 'node:test';
import assert from 'node:assert/strict';
import { describesUploads } from './composer-media.js';
test('uploads are described unless the selected model reads images itself',()=>{
 const models=[{entry_id:null,image_in:false},{entry_id:'claude',image_in:true},{entry_id:'text'}];
 assert.equal(describesUploads({model:{entry_id:'claude'},models}),false);
 assert.equal(describesUploads({model:{entry_id:'text'},models}),true);
 assert.equal(describesUploads({model:{entry_id:null},models}),true);
 assert.equal(describesUploads({model:{},models:[{entry_id:null,image_in:true}]}),false);
 assert.equal(describesUploads({model:{entry_id:'gone'},models}),true);
 assert.equal(describesUploads(null),true);
});
