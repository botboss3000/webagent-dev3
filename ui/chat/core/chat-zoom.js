// Chat pinch-to-zoom, ported from v2 ui/chat/js/chat-zoom.js: two-finger pinch (touch) and Ctrl+wheel
// (trackpad pinch / desktop Ctrl+scroll) over the transcript scale only message content, never the app.
// CSS zoom is applied to #chat-messages-inner children through --v3-chat-zoom (chat.css), so header/footer
// clearance padding stays unscaled while scroll geometry follows the content. The level persists in
// localStorage; the floating − / % / + / reset chip is styled by bundled shared ui/shared/css/app1.css.
const ZOOM_KEY='chatZoomLevel',MIN_ZOOM=0.7,MAX_ZOOM=2.5,CHIP_HIDE_MS=2000;
// Heavily damped like v2: a 2x finger spread zooms 1.12x; a ~100px trackpad notch about 10%.
const PINCH_SENSITIVITY=0.12,WHEEL_SENSITIVITY=0.001;

export const clampZoom=value=>Math.min(MAX_ZOOM,Math.max(MIN_ZOOM,value));
/** Level for a pinch whose finger distance is ratio times its starting distance. */
export const pinchZoom=(startZoom,ratio)=>clampZoom(startZoom*(1+PINCH_SENSITIVITY*(ratio-1)));
/** Level after a Ctrl+wheel event; deltaMode 1 (lines) and 2 (pages) are normalized to pixels. */
export const wheelZoom=(zoom,deltaY,deltaMode)=>clampZoom(zoom*Math.exp(-(deltaMode===1?deltaY*16:deltaMode===2?deltaY*100:deltaY)*WHEEL_SENSITIVITY));

/** Mounts transcript zoom gestures and the level chip; the panel destroy lifecycle calls the returned disposer. */
export function mountChatZoom({root,scroller,inner,header}){
  // Without layout-affecting CSS zoom a transform would scale pixels but not scroll geometry, so disable instead.
  if(!('zoom' in document.documentElement.style))return()=>{};
  const abort=new AbortController(),signal=abort.signal;
  let zoom=1,chip=null,hideTimer=0,startDist=0,startZoom=1;
  try{const saved=parseFloat(localStorage.getItem(ZOOM_KEY));if(Number.isFinite(saved))zoom=clampZoom(saved);}catch{}
  const apply=()=>zoom===1?inner.style.removeProperty('--v3-chat-zoom'):inner.style.setProperty('--v3-chat-zoom',String(zoom));
  function showChip(){
    if(!chip){
      chip=document.createElement('div');chip.id='chat-zoom-chip';chip.className='chat-zoom-chip';chip.setAttribute('role','group');chip.setAttribute('aria-label','Chat zoom');
      const icon=name=>'<i data-lucide="'+name+'" style="width:15px;height:15px"></i>';
      chip.innerHTML='<button type="button" class="cz-btn" data-cz="out" title="Zoom out" aria-label="Zoom out">'+icon('minus')+'</button><span class="cz-level" aria-live="polite"></span>'
        +'<button type="button" class="cz-btn" data-cz="in" title="Zoom in" aria-label="Zoom in">'+icon('plus')+'</button><button type="button" class="cz-btn" data-cz="reset" title="Reset to 100%" aria-label="Reset zoom">'+icon('rotate-ccw')+'</button>';
      chip.addEventListener('click',event=>{const action=event.target.closest('button[data-cz]')?.dataset.cz;if(action)set(action==='in'?zoom+0.1:action==='out'?zoom-0.1:1);});
      root.append(chip);window.lucide?.createIcons();
    }
    // The configured header height can change between shows; sit just below it.
    chip.style.top=Math.round((header?.getBoundingClientRect().height||0)+8)+'px';
    chip.querySelector('.cz-level').textContent=Math.round(zoom*100)+'%';
    chip.hidden=false;chip.classList.remove('cz-hide');
    clearTimeout(hideTimer);hideTimer=setTimeout(()=>chip.classList.add('cz-hide'),CHIP_HIDE_MS);
  }
  function set(next){
    next=clampZoom(next);if(next===zoom)return;
    zoom=next;apply();showChip();
    try{localStorage.setItem(ZOOM_KEY,String(zoom));}catch{}
  }
  scroller.addEventListener('wheel',event=>{if(!event.ctrlKey)return;event.preventDefault();set(wheelZoom(zoom,event.deltaY,event.deltaMode));},{passive:false,signal});
  const distance=touches=>Math.hypot(touches[0].clientX-touches[1].clientX,touches[0].clientY-touches[1].clientY);
  scroller.addEventListener('touchstart',event=>{
    if(event.touches.length<2)return;
    // Keep the browser's page pinch from running on top of ours.
    if(event.cancelable)event.preventDefault();
    startDist=distance(event.touches);startZoom=zoom;
  },{passive:false,signal});
  scroller.addEventListener('touchmove',event=>{
    if(event.touches.length<2||!startDist)return;
    if(event.cancelable)event.preventDefault();
    const current=distance(event.touches);if(current)set(pinchZoom(startZoom,current/startDist));
  },{passive:false,signal});
  for(const type of ['touchend','touchcancel'])scroller.addEventListener(type,()=>{startDist=0;},{passive:true,signal});
  apply();
  return()=>{abort.abort();clearTimeout(hideTimer);chip?.remove();inner.style.removeProperty('--v3-chat-zoom');};
}
