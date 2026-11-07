const test=require('node:test');
const assert=require('node:assert/strict');
const fs=require('node:fs');
const path=require('node:path');

// app.js's static imports are found one level at a time, so without a list the browser
// waits a round trip per import depth. index.html preloads the whole graph instead.
// Rerun this test after changing imports; a failure prints the replacement block.
function startupModules(){
 const imports=/(?:^|[;\s])(?:import|export)\s[^;]*?from\s*['"](\.[^'"]+)['"]|(?:^|[;\s])import\s*['"](\.[^'"]+)['"]/gm;
 const seen=new Set(),queue=['app.js'];
 while(queue.length){
  const file=queue.shift();if(seen.has(file))continue;seen.add(file);
  const source=fs.readFileSync(path.join(__dirname,file),'utf8').replace(/\/\*[\s\S]*?\*\//g,'').replace(/^\s*\/\/.*$/gm,'');
  for(const match of source.matchAll(imports))queue.push(path.posix.normalize(path.posix.join(path.posix.dirname(file),(match[1]||match[2]).split('?')[0])));
 }
 seen.delete('app.js');
 return [...seen].sort();
}

test('index.html preloads exactly the modules app.js imports at start-up',()=>{
 const html=fs.readFileSync(path.join(__dirname,'index.html'),'utf8');
 const listed=[...html.matchAll(/<link rel="modulepreload" href="\/v3-ui\/([^"]+)">/g)].map(m=>m[1]);
 const expected=startupModules();
 assert.deepEqual(listed,expected,'Replace the modulepreload block in index.html with:\n'+expected.map(f=>`<link rel="modulepreload" href="/v3-ui/${f}">`).join('\n'));
});
