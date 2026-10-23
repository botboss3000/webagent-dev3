import test from 'node:test';
import assert from 'node:assert/strict';
import { visualUrl } from './visual-frame.js';
test('only private same-origin UUID views can be mounted',()=>{
 const id='12345678-1234-4234-9234-123456789abc';
 assert.equal(new URL(visualUrl(id,2,'https://example.test/app?session_id=private',true)).pathname,'/api/v3/core/visuals/'+id+'/view');
 assert.equal(new URL(visualUrl(id,2,'https://example.test/app',true)).searchParams.get('theme'),'light');
 for(const value of ['https://other.test','../users','javascript:alert(1)',''])assert.equal(visualUrl(value,1,'https://example.test/app'),null);
 assert.equal(visualUrl(id,0,'https://example.test/app'),null);
});
