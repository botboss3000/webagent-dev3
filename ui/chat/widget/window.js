// @code-index {"version":1,"feature":"chat-widget","aliases":["floating chat","chat launcher","native chat widget"],"purpose":"Connects the native chat surface, floating window, and launcher. Config: data/config/chat.json (runtime /state/config/chat.json, or V3_CHAT_UI_CONFIG_PATH) -> chat_common.window and chat_common.launcher; chat_widget overlays the common profile. Window geometry and launcher draft/position are browser-local preferences.","span_lines":80,"configuration":{"status":"configured","references":[{"path":"data/config/chat.json","pointers":["/chat_common/window","/chat_common/launcher"],"profile_overrides":true}],"opportunities":[]}}
// Purpose: Use fallback geometry whenever a configured or saved value is non-finite.
const finite=(v,d)=>Number.isFinite(Number(v))?Number(v):d;
/** Clamps widget dimensions and position to the viewport using profile min/default sizes; narrow viewports use the full available width. */
export function clampGeometry(value,profile={},viewport={width:innerWidth,height:innerHeight}){
 const cfg=profile.window||{}, margin=8, maxW=Math.max(1,viewport.width-margin*2),maxH=Math.max(1,viewport.height-margin*2);
 const narrow=viewport.width<480;
 const width=narrow?maxW:Math.min(maxW,Math.max(Math.min(maxW,finite(cfg.min_width,300)),finite(value.width,finite(cfg.width,440))));
 const height=Math.min(maxH,Math.max(Math.min(maxH,finite(cfg.min_height,320)),finite(value.height,finite(cfg.height,620))));
 return {width,height,x:narrow?margin:Math.max(margin,Math.min(viewport.width-width-margin,finite(value.x,viewport.width-width-20))),y:Math.max(margin,Math.min(viewport.height-height-margin,finite(value.y,viewport.height-height-20)))};
}
/** Wraps the chat panel in a movable, resizable window with open/minimize/close/dock states. Geometry is restored and saved under storageKey, viewport changes reclamp it, and destroy removes listeners and shell nodes. */
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
 // Purpose: Store current window geometry while ignoring unavailable browser storage.
 const save=()=>{try{localStorage.setItem(storageKey,JSON.stringify(geometry))}catch{}};
 // Purpose: Clamp to profile and viewport bounds before applying and persisting geometry.
 const layout=()=>{geometry=clampGeometry(geometry,profile);Object.assign(shell.style,{left:geometry.x+'px',top:geometry.y+'px',width:geometry.width+'px',height:geometry.height+'px'});save()};
 // Purpose: Add an accessible action button to the window bar.
 const button=(label,text,action)=>{const b=document.createElement('button');b.type='button';b.textContent=text;b.title=label;b.setAttribute('aria-label',label);b.addEventListener('click',action);bar.append(b);return b};
 // Window lifecycle API preserves focus and restores the panel at its dock anchor.
 /** Methods: open displays and focuses the window; minimize/close hide it and restore prior focus; dock restores the panel; setProfile reclamps; isOpen/state expose mode; destroy removes the window. */
 const api={open(){if(disposed)return;if(mode!=='open')savedFocus=document.activeElement;if(panel.parentNode!==content)content.append(panel);shell.hidden=false;mode='open';layout();bar.focus({preventScroll:true})},
 minimize(){shell.hidden=true;mode='minimized';onMinimize();savedFocus?.focus?.({preventScroll:true})},
 close(){shell.hidden=true;mode='closed';onClose();savedFocus?.focus?.({preventScroll:true})},
 dock(){anchor.parentNode?.insertBefore(panel,anchor.nextSibling);shell.hidden=true;mode='docked';onDock()},
 setProfile(next){profile=next||{};shell.dataset.resizable=String(profile.window?.resizable!==false);layout()},
 isOpen(){return mode==='open'},get state(){return mode},
 destroy(){if(disposed)return;api.dock();disposed=true;window.removeEventListener('resize',layout);shell.remove();anchor.remove()}};
 if(!embedded)button('Dock chat','↗',()=>api.dock());button('Minimize chat','−',()=>api.minimize());button('Close chat','×',()=>api.close());
 // Purpose: Bind pointer move/resize gestures; layout clamps and persists resulting geometry.
 function gesture(el,edge){el.addEventListener('pointerdown',e=>{
  if(e.button!==0||e.target.closest('button'))return;
  if(edge?profile.window?.resizable===false:profile.window?.movable===false)return;
  e.preventDefault();el.setPointerCapture(e.pointerId);const start={...geometry},x=e.clientX,y=e.clientY;
   // Purpose: Update geometry from pointer movement or resize delta while preserving the drag start.
  const move=ev=>{const dx=ev.clientX-x,dy=ev.clientY-y;geometry={...start};
   if(!edge){geometry.x+=dx;geometry.y+=dy}else{
    if(edge.includes('e'))geometry.width+=dx;if(edge.includes('s'))geometry.height+=dy;
    if(edge.includes('w')){geometry.width-=dx;geometry.x+=dx}if(edge.includes('n')){geometry.height-=dy;geometry.y+=dy}}
   layout()};
  // Purpose: Detach gesture listeners after pointer release or cancellation.
  const end=()=>{el.removeEventListener('pointermove',move);el.removeEventListener('pointerup',end);el.removeEventListener('pointercancel',end)};
  el.addEventListener('pointermove',move);el.addEventListener('pointerup',end);el.addEventListener('pointercancel',end);
 })}
 gesture(bar,null);
 for(const edge of ['n','s','e','w','ne','nw','se','sw']){const h=document.createElement('div');h.className='wa-chat-resize wa-chat-resize-'+edge;h.setAttribute('aria-hidden','true');shell.append(h);gesture(h,edge)}
 bar.addEventListener('keydown',e=>{const keys={ArrowLeft:[-10,0],ArrowRight:[10,0],ArrowUp:[0,-10],ArrowDown:[0,10]};if(e.target!==bar||!keys[e.key])return;
 if(e.shiftKey?profile.window?.resizable===false:profile.window?.movable===false)return;e.preventDefault();const [x,y]=keys[e.key];if(e.shiftKey){geometry.width+=x;geometry.height+=y}else{geometry.x+=x;geometry.y+=y}layout()});
 window.addEventListener('resize',layout);api.setProfile(profile);return api;
}
/** Builds the launcher, draft input, and corner actions. Draft text is kept in localStorage until a successful send; callbacks own chat actions, and the feature controller handles pickup, drag, focus, and teardown. */
export function createChatLauncher({profile={},onOpen=()=>{},onSend=async()=>{},onAction=async()=>{throw Error('Unknown launcher action')},onPickup=()=>{},storageKey='webagent.widget.draft'}){
 const element=document.createElement('div');element.className='wa-chat-launcher';
 const button=document.createElement('button');button.type='button';button.className='wa-launcher-main';
 const form=document.createElement('form');form.hidden=true;
 const input=document.createElement('input');input.type='text';input.setAttribute('aria-label','Chat message');
 const send=document.createElement('button');send.type='submit';send.textContent='Send';
 const error=document.createElement('span');error.className='wa-chat-launcher-error';error.setAttribute('role','status');
 form.append(input,send);element.append(button,form,error);let state='closed',busy=false;
 try{input.value=localStorage.getItem(storageKey)||''}catch{}
 input.addEventListener('input',()=>{try{localStorage.setItem(storageKey,input.value)}catch{}});
 let features;
 button.addEventListener('click',e=>{if(!features.beforeOpen(e))return;if(state!=='minimized'&&profile.launcher?.mode==='pill'){form.hidden=!form.hidden;if(!form.hidden)input.focus()}else onOpen()});
 form.addEventListener('submit',async e=>{e.preventDefault();if(busy||!input.value.trim())return;busy=true;send.disabled=true;error.textContent='';
 try{await onSend(input.value);input.value='';try{localStorage.removeItem(storageKey)}catch{};onOpen()}catch(e){error.textContent=e.message||'Unable to send. Please try again.'}finally{busy=false;send.disabled=false}});
 /** Methods: show renders open/minimized state, hide closes visibility, setProfile rebuilds configured launcher UI, and destroy removes its feature listeners and element. */
 const api={element,show(next='closed'){state=next;element.dataset.state=state;element.hidden=false;button.replaceChildren();
 if(state==='minimized')button.textContent='Restore chat';else{
  const icon=document.createElementNS('http://www.w3.org/2000/svg','svg');icon.setAttribute('viewBox','0 0 24 24');icon.setAttribute('width','27');icon.setAttribute('height','27');icon.setAttribute('fill','none');icon.setAttribute('stroke','currentColor');icon.setAttribute('stroke-width','2');icon.setAttribute('aria-hidden','true');
  icon.innerHTML=profile.launcher?.icon==='message-circle'?'<path d="M21 11.5a8.5 8.5 0 0 1-8.5 8.5 8.4 8.4 0 0 1-3.8-.9L3 21l1.9-5.7A8.5 8.5 0 1 1 21 11.5Z"/>':'<g class="wla-antenna" style="transform-origin:12px 8px"><path d="M12 8V4H8"/></g><rect width="16" height="12" x="4" y="8" rx="2"/><path d="M2 14h2M20 14h2"/><g class="wla-eye-blink" style="transform-origin:9px 14px"><path d="M9 13v2"/></g><g class="wla-eye-blink" style="transform-origin:15px 14px"><path d="M15 13v2"/></g>';icon.classList.add('wla-bot-svg');button.append(icon);
  if(profile.launcher?.mode==='pill'||profile.launcher?.shape==='pill'){const label=document.createElement('span');label.textContent=profile.launcher?.label||'Chat';button.append(label);}
 }button.setAttribute('aria-label',state==='minimized'?'Restore chat':profile.launcher?.aria_label||'Open chat');button.title=profile.launcher?.label||'Chat';features?.decorate();},hide(){element.hidden=true;features.dismiss()},setProfile(next){profile=next||{};element.dataset.shape=profile.launcher?.shape||'circle';element.dataset.position=({left:'bottom-left',right:'bottom-right'})[profile.launcher?.position]||profile.launcher?.position||'bottom-right';element.style.setProperty('--launcher-accent',profile.launcher?.accent||'var(--brand,#a78bfa)');features?.rebuild(profile);input.placeholder=profile.launcher?.placeholder||'Type a message';api.show(state)},destroy(){features.destroy();element.remove()}};
 features=wireLauncherFeatures({element,button,profile,onAction,onPickup,error,storageKey});api.setProfile(profile);return api;
}

// V2 launcher interaction port. Page samples remain ordinary untrusted message context.
/** Owns launcher corner actions, pickup, hover, drag, and animation listeners. Picked page content is forwarded as untrusted context. */
function wireLauncherFeatures({element,button,profile,onAction,onPickup,error,storageKey}){
 const menu=document.createElement('div');menu.className='wa-launcher-corners';menu.hidden=true;element.append(menu);
 const dot=document.createElement('span');dot.className='wa-pickup-dot';button.append(dot);
 let config=profile,showTimer,hideTimer,drag=null,dragged=false,lastPointerTouch=false,touchFocus=false,pickup=false,lastSample=0,disposed=false,animation,position;
 const motion=matchMedia('(prefers-reduced-motion: reduce)');
 const icons={list:'<path d="M8 6h13M8 12h13M8 18h13M3 6h.01M3 12h.01M3 18h.01"/>',plus:'<path d="M12 5v14M5 12h14"/>',crosshair:'<circle cx="12" cy="12" r="8"/><path d="M12 2v4M12 18v4M2 12h4M18 12h4"/>','maximize-2':'<path d="M15 3h6v6M9 21H3v-6M21 3l-7 7M3 21l7-7"/>',x:'<path d="m6 6 12 12M18 6 6 18"/>'};
 // Purpose: Cancel pending corner-menu hover timers.
 const clear=()=>{clearTimeout(showTimer);clearTimeout(hideTimer);showTimer=hideTimer=null};
 // Purpose: Reveal configured corner actions when the launcher is available.
 function reveal(){clear();if(menu.children.length&&!element.hidden)menu.hidden=false}
 // Purpose: Hide the corner menu and cancel delayed transitions.
 function dismiss(){clear();menu.hidden=true}
 // Purpose: Delay menu reveal by the configured hover interval.
 const show=()=>{clearTimeout(hideTimer);if(!showTimer)showTimer=setTimeout(()=>{showTimer=null;reveal()},Math.max(0,Number(config.launcher?.corner_buttons?.hover_delay_ms)||200))};
 // Purpose: Keep a dragged launcher inside viewport margins.
 function place(x,y){const r=element.getBoundingClientRect(),edge=18;position={x:Math.max(edge,Math.min(innerWidth-r.width-edge,x)),y:Math.max(edge,Math.min(innerHeight-r.height-edge,y))};Object.assign(element.style,{left:position.x+'px',top:position.y+'px',right:'auto',bottom:'auto'})}
 // Purpose: Send a bounded text-only sample from the user-selected page element as untrusted context.
 function sample(){
  if(!pickup)return;
  if(Date.now()-lastSample<150)return;lastSample=Date.now();
  const r=button.getBoundingClientRect(),target=document.elementFromPoint(r.left+r.width/2,r.top-6);
  if(!target||element.contains(target)||target.closest('input,textarea,select,[contenteditable],[data-private],[data-sensitive]'))return;
  const text=(target.innerText||target.textContent||'').trim().slice(0,1500);
  const value=JSON.stringify({source:'Untrusted page element selected by user',tag:target.tagName.toLowerCase(),id:target.id||'',text});
  Promise.resolve(onPickup(value)).catch(e=>{error.textContent=e.message});
 }
 // Purpose: Toggle user-controlled element pickup and clear its context when disabled.
 function toggle(btn){pickup=!pickup;btn.classList.toggle('active',pickup);btn.setAttribute('aria-pressed',String(pickup));dot.classList.toggle('active',pickup);if(pickup)sample();else Promise.resolve(onPickup('')).catch(e=>{error.textContent=e.message})}
 // Purpose: Rebuild corner actions from the current profile, preserving pickup only while its action remains enabled and clearing it when disabled.
 function rebuild(next){
  config=next;menu.replaceChildren();dismiss();
  const cfg=config.launcher?.corner_buttons;if(pickup&&(!cfg||cfg.enabled===false||!Object.values(cfg).some(c=>c?.type==='element_pickup'&&c.enabled!==false))){pickup=false;dot.classList.remove('active');Promise.resolve(onPickup('')).catch(e=>{error.textContent=e.message})}if(!cfg||cfg.enabled===false)return;
  for(const [key,cls] of Object.entries({top_left:'tl',top_right:'tr',bottom_left:'bl',bottom_right:'br'})){
   const c=cfg[key];if(!c||c.enabled===false)continue;
   const b=document.createElement('button');b.type='button';b.className='wa-launcher-corner '+cls;b.title=c.tooltip||c.type;b.setAttribute('aria-label',b.title);b.dataset.cornerType=c.type;
   const svg=document.createElementNS('http://www.w3.org/2000/svg','svg');svg.setAttribute('viewBox','0 0 24 24');svg.setAttribute('fill','none');svg.setAttribute('stroke','currentColor');svg.setAttribute('stroke-width','2');svg.setAttribute('aria-hidden','true');svg.innerHTML=icons[c.icon]||icons[c.type==='new_session'?'plus':'list'];b.append(svg);
   if(c.type==='element_pickup'){b.setAttribute('aria-pressed',String(pickup));b.classList.toggle('active',pickup)}
   b.onclick=async e=>{e.stopPropagation();error.textContent='';if(c.type==='element_pickup'){toggle(b);return}b.disabled=true;try{await onAction(c.type);dismiss()}catch(e){error.textContent=e.message}finally{b.disabled=false}};menu.append(b);
  }
 }
 element.addEventListener('pointerenter',show);element.addEventListener('pointerleave',()=>{clearTimeout(showTimer);showTimer=null;hideTimer=setTimeout(dismiss,300)});
 element.addEventListener('focusin',()=>{if(!touchFocus)reveal()});element.addEventListener('focusout',e=>{if(!element.contains(e.relatedTarget))dismiss()});
 // Purpose: Dismiss corner actions when pointer input lands outside the launcher.
 const outside=e=>{if(!e.composedPath().includes(element))dismiss()};document.addEventListener('pointerdown',outside);
 element.addEventListener('keydown',e=>{if(e.key==='Escape')dismiss()});
 button.addEventListener('pointerdown',e=>{touchFocus=lastPointerTouch=e.pointerType==='touch';if(e.button!==0||config.launcher?.draggable===false)return;const r=element.getBoundingClientRect();lastPointerTouch=e.pointerType==='touch';drag={x:e.clientX,y:e.clientY,left:r.left,top:r.top,moved:false,touch:lastPointerTouch};button.setPointerCapture(e.pointerId)});
 button.addEventListener('pointermove',e=>{if(!drag)return;const dx=e.clientX-drag.x,dy=e.clientY-drag.y;if(!drag.moved&&Math.hypot(dx,dy)<5)return;drag.moved=true;dismiss();element.classList.add('dragging');place(drag.left+dx,drag.top+dy);sample()});
 // Purpose: End dragging and persist the launcher position.
 const finish=e=>{touchFocus=false;if(!drag)return;dragged=drag.moved;drag=null;element.classList.remove('dragging');try{button.releasePointerCapture(e.pointerId)}catch{};if(dragged){try{localStorage.setItem(storageKey+'.position',JSON.stringify(position))}catch{}}};
 button.addEventListener('pointerup',finish);button.addEventListener('pointercancel',finish);
 // Purpose: Re-clamp the saved launcher position after viewport resize.
 const resize=()=>{if(position)place(position.x,position.y)};window.addEventListener('resize',resize);
 try{const saved=JSON.parse(localStorage.getItem(storageKey+'.position')||'null');if(Number.isFinite(saved?.x)&&Number.isFinite(saved?.y))requestAnimationFrame(()=>{if(!disposed)place(saved.x,saved.y)})}catch{}
 // Keep the exact Lucide bot geometry and V2 animation wrappers.
 // Purpose: Animate launcher accents only while visible, idle, and motion is allowed.
 function animate(){if(disposed)return;const svg=button.querySelector('.wla-bot-svg');if(svg&&!motion.matches&&!element.hidden&&!drag){const eyes=svg.querySelectorAll('.wla-eye-blink');for(const e of eyes)e.classList.add('blinking');setTimeout(()=>{for(const e of eyes)e.classList.remove('blinking')},220);const antenna=svg.querySelector('.wla-antenna');if(antenna){antenna.style.transition='transform 650ms ease';antenna.style.transform='rotate(-14deg)';setTimeout(()=>{antenna.style.transform='rotate(0deg)'},650)}}animation=setTimeout(animate,3200)}
 animation=setTimeout(animate,1500);
 // Purpose: Apply the pointer-follow effect except during reduced motion or active dragging.
 const magnet=e=>{if(motion.matches||drag)return;const r=button.getBoundingClientRect(),dx=e.clientX-r.left-r.width/2,dy=e.clientY-r.top-r.height/2,d=Math.hypot(dx,dy),s=d?3.5*Math.max(0,1-d/150)/d:0;button.style.setProperty('--launcher-icon-x',dx*s+'px');button.style.setProperty('--launcher-icon-y',dy*s+'px')};document.addEventListener('pointermove',magnet,{passive:true});
 rebuild(profile);
 // Methods: rebuild applies profile actions, dismiss hides the menu, beforeOpen handles drag/touch gating and pickup, decorate installs the marker, and destroy releases listeners.
 return {rebuild,dismiss,beforeOpen(e){if(dragged){dragged=false;return false}if(e.detail!==0&&(e.pointerType==='touch'||lastPointerTouch)&&menu.hidden&&menu.children.length){reveal();return false}sample();return true},decorate(){button.append(dot)},destroy(){disposed=true;clear();clearTimeout(animation);document.removeEventListener('pointerdown',outside);document.removeEventListener('pointermove',magnet);window.removeEventListener('resize',resize)}};
}

/** Recursively overlays object-valued widget profile fields; arrays and scalar values replace the base. */
export function mergeProfile(base,override){const result={...base};for(const [key,value] of Object.entries(override||{}))result[key]=value&&typeof value==='object'&&!Array.isArray(value)?mergeProfile(base?.[key]||{},value):value;return result;}
