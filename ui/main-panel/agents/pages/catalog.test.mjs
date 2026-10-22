import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs/promises';
const source=await fs.readFile(new URL('./catalog.js',import.meta.url),'utf8');
const {loadAgentPages}=await import('data:text/javascript;base64,'+Buffer.from(source.replace(/export \{ renderAgentDirectory \} from '[^']+';/,'').replaceAll('import.meta.url',JSON.stringify(new URL('./catalog.js',import.meta.url).href))).toString('base64'));
const catalog=JSON.parse(await fs.readFile(new URL('./catalog.json',import.meta.url),'utf8'));
async function load(value){globalThis.fetch=async()=>({ok:true,json:async()=>value});return loadAgentPages();}
test('typed catalog separates configuration from common pages',async()=>{
 const pages=await load(catalog);
 assert.deepEqual(pages.filter(p=>p.type==='agent_configuration').map(p=>p.id),['identity','model','abilities','members']);
 assert.equal(pages.find(p=>p.type==='directory').id,'config');
 assert.equal(pages.find(p=>p.type==='sessions').id,'sessions');
});
test('invalid catalog types, versions, duplicates and scope fail closed',async()=>{
 for(const change of [
 c=>c.version=1,
 c=>c.pages[1].type='unknown',
 c=>c.pages.push({...c.pages[1]}),
 c=>c.pages=c.pages.filter(p=>p.id!=='sessions'),
 c=>c.pages[0].type='agent_configuration',
 c=>c.pages[1].panelId=''
 ]){const c=structuredClone(catalog);change(c);await assert.rejects(load(c), /catalog|scope/);}
});
