const finite=(v,d)=>Number.isFinite(Number(v))?Number(v):d;
export function clampGeometry(value,profile={},viewport={width:innerWidth,height:innerHeight}){
 const cfg=profile.window||{}, margin=8, maxW=Math.max(1,viewport.width-margin*2),maxH=Math.max(1,viewport.height-margin*2);
 const narrow=viewport.width<480;
 const width=narrow?maxW:Math.min(maxW,Math.max(Math.min(maxW,finite(cfg.min_width,300)),finite(value.width,finite(cfg.width,440))));
 const height=Math.min(maxH,Math.max(Math.min(maxH,finite(cfg.min_height,320)),finite(value.height,finite(cfg.height,620))));
 return {width,height,x:narrow?margin:Math.max(margin,Math.min(viewport.width-width-margin,finite(value.x,viewport.width-width-20))),y:Math.max(margin,Math.min(viewport.height-height-margin,finite(value.y,viewport.height-height-20)))};
}
export function createFloatingChat({panel,profile={},storageKey='webagent.widget.geometry',onMinimize=()=>{},onClose=()=>{},onDock=()=>{},embedded=false,container=document.body}){
 const anchor=document.createComment('chat dock position');panel.before(anchor);
 const shell=document.createElement('section');shell.className='wa-chat-window';shell.hidden=true;shell.setAttribute('aria-label','Chat window');
 const bar=document.createElement('div');bar.className='wa-chat-window-bar';bar.tabIndex=0;bar.setAttribute('aria-label','Move chat window with arrow keys; hold Shift to resize');
 const grip=document.createElement('span');grip.textContent='⋮⋮';grip.setAttribute('aria-hidden','true');bar.append(grip);
 const content=document.createElement('div');content.className='wa-chat-window-content';shell.append(bar,content);container.append(shell);
 // Reparenting a live iframe reloads its document and loses in-memory authentication.
 if(embedded)content.append(panel);
 let mode='closed',geometry={},savedFocus=null,disposed=false;
 try{const saved=JSON.parse(localStorage.getItem(storageKey)||'{}');if(saved&&typeof saved==='object')geometry=saved}catch{}
 if(!Number.isFinite(geometry.x)&&profile.launcher?.position?.endsWith('left'))geometry.x=20;
 if(!Number.isFinite(geometry.y)&&profile.launcher?.position?.startsWith('top'))geometry.y=20;
 const save=()=>{try{localStorage.setItem(storageKey,JSON.stringify(geometry))}catch{}};
 const layout=()=>{geometry=clampGeometry(geometry,profile);Object.assign(shell.style,{left:geometry.x+'px',top:geometry.y+'px',width:geometry.width+'px',height:geometry.height+'px'});save()};
 const button=(label,text,action)=>{const b=document.createElement('button');b.type='button';b.textContent=text;b.title=label;b.setAttribute('aria-label',label);b.addEventListener('click',action);bar.append(b);return b};
 const api={open(){if(disposed)return;if(mode!=='open')savedFocus=document.activeElement;if(panel.parentNode!==content)content.append(panel);shell.hidden=false;mode='open';layout();bar.focus({preventScroll:true})},
 minimize(){shell.hidden=true;mode='minimized';onMinimize();savedFocus?.focus?.({preventScroll:true})},
 close(){shell.hidden=true;mode='closed';onClose();savedFocus?.focus?.({preventScroll:true})},
 dock(){anchor.parentNode?.insertBefore(panel,anchor.nextSibling);shell.hidden=true;mode='docked';onDock()},
 setProfile(next){profile=next||{};shell.dataset.resizable=String(profile.window?.resizable!==false);layout()},
 isOpen(){return mode==='open'},get state(){return mode},
 destroy(){if(disposed)return;api.dock();disposed=true;window.removeEventListener('resize',layout);shell.remove();anchor.remove()}};
 if(!embedded)button('Dock chat','↗',()=>api.dock());button('Minimize chat','−',()=>api.minimize());button('Close chat','×',()=>api.close());
 function gesture(el,edge){el.addEventListener('pointerdown',e=>{
  if(e.button!==0||e.target.closest('button'))return;
  if(edge?profile.window?.resizable===false:profile.window?.movable===false)return;
  e.preventDefault();el.setPointerCapture(e.pointerId);const start={...geometry},x=e.clientX,y=e.clientY;
  const move=ev=>{const dx=ev.clientX-x,dy=ev.clientY-y;geometry={...start};
   if(!edge){geometry.x+=dx;geometry.y+=dy}else{
    if(edge.includes('e'))geometry.width+=dx;if(edge.includes('s'))geometry.height+=dy;
    if(edge.includes('w')){geometry.width-=dx;geometry.x+=dx}if(edge.includes('n')){geometry.height-=dy;geometry.y+=dy}}
   layout()};
  const end=()=>{el.removeEventListener('pointermove',move);el.removeEventListener('pointerup',end);el.removeEventListener('pointercancel',end)};
  el.addEventListener('pointermove',move);el.addEventListener('pointerup',end);el.addEventListener('pointercancel',end);
 })}
 gesture(bar,null);
 for(const edge of ['n','s','e','w','ne','nw','se','sw']){const h=document.createElement('div');h.className='wa-chat-resize wa-chat-resize-'+edge;h.setAttribute('aria-hidden','true');shell.append(h);gesture(h,edge)}
 bar.addEventListener('keydown',e=>{const keys={ArrowLeft:[-10,0],ArrowRight:[10,0],ArrowUp:[0,-10],ArrowDown:[0,10]};if(e.target!==bar||!keys[e.key])return;
 if(e.shiftKey?profile.window?.resizable===false:profile.window?.movable===false)return;e.preventDefault();const [x,y]=keys[e.key];if(e.shiftKey){geometry.width+=x;geometry.height+=y}else{geometry.x+=x;geometry.y+=y}layout()});
 window.addEventListener('resize',layout);api.setProfile(profile);return api;
}
export function createChatLauncher({profile={},onOpen=()=>{},onSend=async()=>{},storageKey='webagent.widget.draft'}){
 const element=document.createElement('div');element.className='wa-chat-launcher';
 const button=document.createElement('button');button.type='button';
 const form=document.createElement('form');form.hidden=true;
 const input=document.createElement('input');input.type='text';input.setAttribute('aria-label','Chat message');
 const send=document.createElement('button');send.type='submit';send.textContent='Send';
 const error=document.createElement('span');error.className='wa-chat-launcher-error';error.setAttribute('role','status');
 form.append(input,send);element.append(button,form,error);let state='closed',busy=false;
 try{input.value=localStorage.getItem(storageKey)||''}catch{}
 input.addEventListener('input',()=>{try{localStorage.setItem(storageKey,input.value)}catch{}});
 button.addEventListener('click',()=>{if(state!=='minimized'&&profile.launcher?.mode==='pill'){form.hidden=!form.hidden;if(!form.hidden)input.focus()}else onOpen()});
 form.addEventListener('submit',async e=>{e.preventDefault();if(busy||!input.value.trim())return;busy=true;send.disabled=true;error.textContent='';
 try{await onSend(input.value);input.value='';try{localStorage.removeItem(storageKey)}catch{};onOpen()}catch(e){error.textContent=e.message||'Unable to send. Please try again.'}finally{busy=false;send.disabled=false}});
 const api={element,show(next='closed'){state=next;element.dataset.state=state;element.hidden=false;button.replaceChildren();
 if(state==='minimized')button.textContent='Restore chat';else{
  const icon=document.createElementNS('http://www.w3.org/2000/svg','svg');icon.setAttribute('viewBox','0 0 24 24');icon.setAttribute('width','24');icon.setAttribute('height','24');icon.setAttribute('fill','none');icon.setAttribute('stroke','currentColor');icon.setAttribute('stroke-width','2');icon.setAttribute('aria-hidden','true');
  icon.innerHTML=profile.launcher?.icon==='message-circle'?'<path d="M21 11.5a8.5 8.5 0 0 1-8.5 8.5 8.4 8.4 0 0 1-3.8-.9L3 21l1.9-5.7A8.5 8.5 0 1 1 21 11.5Z"/>':'<rect x="3" y="7" width="18" height="14" rx="3"/><path d="M12 7V3M8 12h.01M16 12h.01M8 17h8"/>';button.append(icon);
  if(profile.launcher?.mode==='pill'||profile.launcher?.shape==='pill'){const label=document.createElement('span');label.textContent=profile.launcher?.label||'Chat';button.append(label);}
 }button.setAttribute('aria-label',state==='minimized'?'Restore chat':profile.launcher?.aria_label||'Open chat');button.title=profile.launcher?.label||'Chat';},hide(){element.hidden=true},setProfile(next){profile=next||{};element.dataset.shape=profile.launcher?.shape||'circle';element.dataset.position=({left:'bottom-left',right:'bottom-right'})[profile.launcher?.position]||profile.launcher?.position||'bottom-right';element.style.background=profile.launcher?.accent||'';input.placeholder=profile.launcher?.placeholder||'Type a message';api.show(state)},destroy(){element.remove()}};
 api.setProfile(profile);return api;
}

export function mergeProfile(base,override){const result={...base};for(const [key,value] of Object.entries(override||{}))result[key]=value&&typeof value==='object'&&!Array.isArray(value)?mergeProfile(base?.[key]||{},value):value;return result;}
