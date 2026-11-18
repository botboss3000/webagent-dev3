/** Resize the docked desktop split, keeping a proportional width across viewport changes.
 * Pointer capture owns each drag; mobile and floating layouts retain their own geometry.
 * Boot configuration is supplied separately by the shell, not persisted as a browser preference.
 */
export function mountChatResize({enabled}){
  const stage=document.getElementById('stage');
  const handle=document.getElementById('chat-resize-handle');
  let fraction=.45,pointer=null,grabOffset=0,pixels=null;
  const available=()=>Math.max(0,stage.getBoundingClientRect().width-handle.getBoundingClientRect().width);
  const limits=width=>({min:Math.min(240,width/2),max:Math.max(width/2,width-280)});
  /** Clamp the actual split and expose chat's percentage to assistive technology. */
  function apply(width=pixels??fraction*available()){
    if(!enabled())return;
    const total=available();
    if(!total)return;
    const {min,max}=limits(total);
    const pixels=Math.max(min,Math.min(max,width));
    stage.style.setProperty('--chat-panel-width',pixels+'px');
    handle.setAttribute('aria-valuemin',String(Math.ceil(min/total*100)));
    handle.setAttribute('aria-valuemax',String(Math.floor(max/total*100)));
    handle.setAttribute('aria-valuenow',String(Math.round(pixels/total*100)));
    handle.setAttribute('aria-valuetext',Math.round(pixels/total*100)+'% chat width');
    return pixels/total;
  }
  /** End a gesture on release, cancellation, lost capture, or a layout change. */
  function stop(){
    const previous=pointer;pointer=null;
    handle.classList.remove('resizing');
    document.body.classList.remove('chat-resizing');
    if(previous!==null&&handle.hasPointerCapture(previous))handle.releasePointerCapture(previous);
  }
  handle.addEventListener('pointerdown',event=>{
    if(!enabled()||event.button!==0||!event.isPrimary)return;
    event.preventDefault();
    const rect=handle.getBoundingClientRect();
    grabOffset=event.clientX-rect.left;
    handle.setPointerCapture(event.pointerId);pointer=event.pointerId;
    handle.focus({preventScroll:true});
    handle.classList.add('resizing');document.body.classList.add('chat-resizing');
  });
  handle.addEventListener('pointermove',event=>{
    handle.style.setProperty('--resize-y',(event.clientY-handle.getBoundingClientRect().top)+'px');
    if(pointer!==event.pointerId)return;
    if(!enabled()){stop();return;}
    const rect=stage.getBoundingClientRect();
    pixels=null;
    fraction=apply(rect.right-event.clientX+grabOffset-handle.getBoundingClientRect().width)??fraction;
  });
  for(const type of ['pointerup','pointercancel','lostpointercapture'])handle.addEventListener(type,stop);
  window.addEventListener('blur',stop);
  handle.addEventListener('keydown',event=>{
    if(!enabled())return;
    const total=available(),{min,max}=limits(total);
    let width=document.getElementById('chat-panel').getBoundingClientRect().width;
    if(event.key==='ArrowLeft')width+=event.shiftKey?50:10;
    else if(event.key==='ArrowRight')width-=event.shiftKey?50:10;
    else if(event.key==='Home')width=min;
    else if(event.key==='End')width=max;
    else return;
    event.preventDefault();pixels=null;fraction=apply(width)??fraction;
  });
  new ResizeObserver(()=>{if(!enabled())stop();else apply();}).observe(stage);
  new MutationObserver(()=>{if(!enabled())stop();else apply();}).observe(handle,{attributes:true,attributeFilter:['hidden']});
  // Layout observers apply after the authenticated shell becomes visible.
  apply();
  return {
    /** Apply validated boot geometry once; zero visibility is owned by the shell. */
    configure({width=45,unit='%'}={}){
      if(!Number.isFinite(width)||width<0||!['px','%'].includes(unit)||(unit==='%'&&width>100))throw Error('Invalid chat panel geometry');
      pixels=width>0&&unit==='px'?width:null;
      fraction=width>0&&unit==='%'?width/100:.45;
      apply();
    }
  };
}
