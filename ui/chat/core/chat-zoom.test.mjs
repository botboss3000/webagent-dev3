import test from 'node:test';
import assert from 'node:assert/strict';
import { clampZoom, pinchZoom, wheelZoom } from './chat-zoom.js';
test('zoom levels are damped and clamped to 70%-250%',()=>{
 assert.equal(clampZoom(0.1),0.7);
 assert.equal(clampZoom(9),2.5);
 assert.ok(Math.abs(pinchZoom(1,2)-1.12)<1e-9);
 assert.ok(pinchZoom(1,0.5)<1);
 assert.equal(pinchZoom(2.5,4),2.5);
 assert.ok(wheelZoom(1,-100,0)>1&&wheelZoom(1,100,0)<1);
 assert.equal(wheelZoom(1,-1,1),wheelZoom(1,-16,0));
});
