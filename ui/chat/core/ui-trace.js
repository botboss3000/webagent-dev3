'use strict';
// Development diagnostics: while Diagnostics' ui_trace category is enabled, keeps a bounded
// structural timeline around the chat composer (viewport sizes, focus moves, focus/blur calls,
// composer DOM changes) and reports it when the composer loses focus or the OS keyboard
// closes. Never records typed text, key values, URLs or credentials.
const WINDOW_MS=5000,MAX=300,AFTERMATH_MS=300,MIN_GAP_MS=5000,USER_TAP_MS=400;
const UUID=/^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;
const NAMED_KEYS=new Set(['Enter','Backspace','Delete','Escape','Tab','ArrowUp','ArrowDown','ArrowLeft','ArrowRight','Home','End','PageUp','PageDown','Unidentified','Process']);
/** Restricts recorded text to the server's structural alphabet and length. */
const clean=text=>String(text).replace(/[^A-Za-z0-9 #._:/()=>,[\]-]/g,'').slice(0,200)||undefined;
/** Describes an element by tag, id and up to three classes. */
export function describe(node){
  if(node===document)return 'document';if(node===window)return 'window';
  if(!node||node.nodeType!==1)return undefined;
  const classes=typeof node.className==='string'?node.className.trim().split(/\s+/).filter(Boolean).slice(0,3):[];
  return clean(node.tagName.toLowerCase()+(node.id?'#'+node.id:'')+classes.map(name=>'.'+name).join(''));
}
/** Keeps the nearest stack frames as "function (path:line:column)" with the origin removed. */
export function frames(stack){
  return clean(String(stack||'').split('\n').slice(2,6).map(line=>line.trim().replace(/^at /,'')
    .replace(/https?:\/\/[^/\s)]+\/(v3-ui\/)?/,'').replace(/\?[^:)\s]*/,'')).join(' > '));
}

/** Starts collection when the category is enabled; resolves to a disposer. */
export async function mountUiTrace({request,getSessionId=()=>''}){
  let enabled=false;
  try{enabled=(await request('/diagnostics/ui-trace/config'))?.enabled===true;}catch{}
  if(!enabled)return ()=>{};
  const abort=new AbortController(),{signal}=abort,log=[],vv=window.visualViewport;
  let lastSent=-Infinity,lastOutsideTap=-Infinity,pending=0,keyboardOpen=document.body.classList.contains('kb-open');
  const input=()=>document.getElementById('chat-input');
  function push(kind,{target,detail}={}){
    const now=performance.now();
    log.push({at:now,kind,target:clean(target??'')||undefined,detail:clean(detail??'')||undefined,active:describe(document.activeElement),
      inner_height:Math.round(window.innerHeight),viewport_height:vv?Math.round(vv.height):undefined,viewport_top:vv?Math.round(Math.max(0,vv.offsetTop)):undefined});
    while(log.length>MAX||(log.length&&now-log[0].at>WINDOW_MS))log.shift();
  }
  function report(trigger){
    const now=performance.now();
    if(pending||now-lastSent<MIN_GAP_MS||now-lastOutsideTap<USER_TAP_MS)return;
    // Include the immediate aftermath (layout, keyboard class) before sending.
    pending=setTimeout(()=>{
      pending=0;lastSent=performance.now();
      const end=performance.now(),sid=getSessionId();
      const entries=log.filter(entry=>end-entry.at<=WINDOW_MS).map(({at,...entry})=>({before_ms:Math.round(end-at),...entry}));
      if(!entries.length)return;
      request('/diagnostics/ui-trace/events','POST',{trace_id:crypto.randomUUID(),trigger,session_id:UUID.test(sid||'')?sid:null,
        width:Math.round(window.innerWidth),height:Math.round(window.innerHeight),entries}).catch(()=>{});
    },AFTERMATH_MS);
  }
  window.addEventListener('resize',()=>push('resize'),{signal});
  vv?.addEventListener('resize',()=>push('viewport_resize'),{signal});
  vv?.addEventListener('scroll',()=>push('viewport_scroll'),{signal});
  document.addEventListener('focusin',event=>push('focusin',{target:describe(event.target),detail:'from '+(describe(event.relatedTarget)||'none')}),{capture:true,signal});
  document.addEventListener('focusout',event=>{
    push('focusout',{target:describe(event.target),detail:'to '+(describe(event.relatedTarget)||'none')});
    if(event.target===input())report('composer_focus_lost');
  },{capture:true,signal});
  document.addEventListener('pointerdown',event=>{
    push('pointerdown',{target:describe(event.target),detail:event.isTrusted?'trusted':'synthetic'});
    if(event.isTrusted&&!event.target.closest?.('#chat-input-area'))lastOutsideTap=performance.now();
  },{capture:true,signal});
  // Key categories only: a printable key is recorded as "character", never its value.
  document.addEventListener('keydown',event=>push('keydown',{target:describe(event.target),detail:NAMED_KEYS.has(event.key)?event.key:'character'}),{capture:true,signal});
  document.addEventListener('input',event=>push('input',{target:describe(event.target),detail:event.inputType}),{capture:true,signal});
  document.addEventListener('submit',event=>push('submit',{target:describe(event.target)}),{capture:true,signal});
  document.addEventListener('visibilitychange',()=>push('visibility',{detail:document.visibilityState}),{signal});
  const observer=new MutationObserver(records=>{
    const editor=input();
    for(const record of records){
      if(record.target===document.body&&record.attributeName==='class'){
        const open=document.body.classList.contains('kb-open');
        if(open!==keyboardOpen){keyboardOpen=open;push('keyboard_class',{detail:open?'open':'closed'});if(!open&&editor&&document.activeElement===editor)report('composer_keyboard_closed');}
        continue;
      }
      if(!editor)continue;
      if(record.type==='attributes'&&record.target.contains?.(editor))push('attribute',{target:describe(record.target),detail:record.attributeName});
      else if(record.type==='childList'&&[...record.removedNodes,...record.addedNodes].some(node=>node===editor||node.contains?.(editor)))
        push('child_list',{target:describe(record.target),detail:record.removedNodes.length?'removed':'added'});
    }
  });
  observer.observe(document.body,{subtree:true,childList:true,attributes:true,attributeFilter:['class','hidden','style','readonly','disabled','inert']});
  // Programmatic focus changes are the usual cause of an unexpected keyboard close.
  const {blur,focus}=HTMLElement.prototype;
  HTMLElement.prototype.blur=function(){if(this===document.activeElement)push('blur_call',{target:describe(this),detail:frames(new Error().stack)});return blur.call(this);};
  HTMLElement.prototype.focus=function(...args){if(document.activeElement===input()&&this!==input())push('focus_call',{target:describe(this),detail:frames(new Error().stack)});return focus.apply(this,args);};
  return ()=>{abort.abort();observer.disconnect();clearTimeout(pending);HTMLElement.prototype.blur=blur;HTMLElement.prototype.focus=focus;};
}
