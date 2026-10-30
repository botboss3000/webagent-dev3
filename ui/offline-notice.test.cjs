const test=require('node:test');
const assert=require('node:assert/strict');
const fs=require('node:fs');
const vm=require('node:vm');

test('connection-failure notices clear on the next server response; other notices stay',()=>{
 const notice={textContent:''},context=vm.createContext({$:()=>notice,createBackgroundIssues:()=>({})});
 const source=fs.readFileSync(__dirname+'/app.js','utf8'),block=source.slice(source.indexOf("let noticeSource='"),source.indexOf('async function api('));
 vm.runInContext(block+';this.shell={note,clearNoticeFrom,OFFLINE};',context);const {note,clearNoticeFrom,OFFLINE}=context.shell;
 note(OFFLINE);assert.equal(notice.textContent,OFFLINE);clearNoticeFrom('network');assert.equal(notice.textContent,'');
 note('Conversation unavailable: '+OFFLINE);clearNoticeFrom('network');assert.equal(notice.textContent,'');
 note('Saved','save');clearNoticeFrom('network');assert.equal(notice.textContent,'Saved');
});
