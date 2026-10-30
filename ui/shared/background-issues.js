// Tracks independent background failures; navigation changes visibility, never recovery.
export function createBackgroundIssues({request,render=()=>{},now=()=>Date.now(),uuid=()=>crypto.randomUUID(),setIntervalFn=setInterval,clearIntervalFn=clearInterval,setTimeoutFn=setTimeout,clearTimeoutFn=clearTimeout}) {
  const operations=new Set(['usage','message_statistics','read_ack','manager_status','session_controls','shared_controls','chat_sync','status_poll','sessions_list','activity_display','media_display','transcript_position','shared_snapshot','history_refresh']);
  const labels={usage:'Usage unavailable',message_statistics:'Message statistics unavailable',read_ack:'Read acknowledgment failed',manager_status:'Manager status unavailable',session_controls:'Session controls unavailable',shared_controls:'Shared controls unavailable',chat_sync:'Chat synchronization unavailable',status_poll:'App status unavailable',sessions_list:'Conversation list unavailable',activity_display:'Activity display unavailable',media_display:'Media display unavailable',transcript_position:'Transcript positioning unavailable',shared_snapshot:'Shared conversation snapshot unavailable',history_refresh:'Conversation history refresh unavailable'};
  const active=new Map(),cleared=new Map(),queue=[];
  let selected='',config={enabled:true,show_toasts:true,cleared_toast_seconds:3},timer=null,retry=null,sending=null,started=false,epoch=0,configBusy=false,configReady=false,reserved=0;
  const key=(operation,session)=>operation+'|'+(session||'');
  function paint(){render(config.show_toasts?[...active.values(),...cleared.values()].filter(issue=>!issue.session_id||issue.session_id===selected).map(issue=>issue.message+(issue.clearedUntil?' (Cleared '+Math.max(1,Math.ceil((issue.clearedUntil-now())/1000))+')':'')):[]);}
  // Cleared display state never extends an active episode or delays its resolution log.
  function removeCleared(id){const issue=cleared.get(id);if(issue){clearIntervalFn(issue.timer);cleared.delete(id);}}
  // Reserve one queue slot for each accepted opening's eventual resolution. Never evict an opening.
  function release(issue){if(issue.reserved){issue.reserved=false;reserved--;}}
  function discard(issue){issue.captured=false;release(issue);for(let index=queue.length-1;index>=0;index--)if(queue[index].issue===issue)queue.splice(index,1);}
  function clearCapture(){for(const item of queue)item.issue.captured=false;for(const issue of active.values()){issue.captured=false;issue.reserved=false;}queue.length=0;reserved=0;}
  function event(issue,event){
    if(event==='issue_opened'){
      issue.captured=false;
      if(!config.enabled||!started||queue.length+reserved+2>100)return;
      issue.captured=true;issue.reserved=true;reserved++;
    }else{
      if(!issue.captured||!config.enabled||!started){release(issue);return;}
      release(issue);
    }
    const item={issue_id:issue.issue_id,event,operation:issue.operation,repeat_count:issue.repeat_count,duration_ms:Math.min(31536000000,Math.max(0,now()-issue.opened))};
    if(issue.session_id)item.session_id=issue.session_id;if(Number.isInteger(issue.status))item.status=issue.status;
    queue.push({body:item,issue,attempt:0});void flush();
  }
  // Stable episode/event identity makes an ACK-lost replay idempotent at the receiver.
  async function flush(){
    if(sending||!started||!configReady||retry!==null||!queue.length)return;
    const token={},turn=epoch,item=queue[0];sending=token;
    try{const result=await request('/diagnostics/background/events','POST',item.body);if(turn===epoch){if(result?.accepted===false&&item.body.event==='issue_opened')discard(item.issue);else if(queue[0]===item)queue.shift();}}
    catch(error){
      if(turn!==epoch)return;
      if(error.status===401||error.status===403||error.status===404)clearCapture();
      else if(++item.attempt>=3){if(item.body.event==='issue_opened')discard(item.issue);else if(queue[0]===item)queue.shift();}
      else retry=setTimeoutFn(()=>{retry=null;void flush();},1000*item.attempt);
    }finally{if(sending===token){sending=null;if(started&&retry===null&&queue.length)void flush();}}
  }
  async function refreshConfig(){if(!started||configBusy)return;configBusy=true;const turn=epoch;try{const value=await request('/diagnostics/background/config');if(turn!==epoch)return;config={enabled:value.enabled!==false,show_toasts:value.show_toasts!==false,cleared_toast_seconds:Number.isInteger(value.cleared_toast_seconds)&&value.cleared_toast_seconds>=0?value.cleared_toast_seconds:3};configReady=true;if(!config.enabled)clearCapture();paint();void flush();}catch{}finally{if(turn===epoch)configBusy=false;}}
  return {
    fail(operation,error,session=''){if(!started)return;if(!operations.has(operation))throw Error('Unknown background operation');const id=key(operation,session),existing=active.get(id);removeCleared(id);if(existing){existing.repeat_count=Math.min(1000000,existing.repeat_count+1);existing.message=labels[operation]+': '+String(error?.message||error||'Background operation failed');paint();return existing.issue_id;}if(active.size>=128){const oldest=active.keys().next().value;release(active.get(oldest));active.delete(oldest);}const issue={issue_id:uuid(),operation,session_id:session,message:labels[operation]+': '+String(error?.message||error||'Background operation failed'),status:error?.status,repeat_count:1,opened:now()};active.set(id,issue);event(issue,'issue_opened');paint();return issue.issue_id;},
    resolve(operation,session=''){const id=key(operation,session),issue=active.get(id);if(!issue)return;active.delete(id);event(issue,'issue_resolved');if(!config.cleared_toast_seconds){paint();return;}if(cleared.size>=128)removeCleared(cleared.keys().next().value);const display={...issue,clearedUntil:now()+config.cleared_toast_seconds*1000};display.timer=setIntervalFn(()=>{if(now()>=display.clearedUntil)removeCleared(id);paint();},1000);cleared.set(id,display);paint();},
    select(session=''){selected=session;paint();},
    start(){if(started)return;started=true;void refreshConfig();timer=setIntervalFn(()=>void refreshConfig(),5000);},
    stop(){started=false;++epoch;if(timer!==null)clearIntervalFn(timer);if(retry!==null)clearTimeoutFn(retry);for(const id of cleared.keys())removeCleared(id);timer=retry=null;configBusy=false;configReady=false;clearCapture();sending=null;config={enabled:true,show_toasts:true,cleared_toast_seconds:3};active.clear();selected='';paint();},
    refreshConfig,
    // Useful to bounded lifecycle tests; production callers use the same serialized flush.
    flush
  };
}
