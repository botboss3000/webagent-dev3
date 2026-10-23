// Mini app code is never inserted into the app DOM or given the app origin.
export function visualUrl(id, revision, current, light = false) {
  if(typeof id!=='string'||!/^[a-f0-9]{8}-[a-f0-9]{4}-[a-f0-9]{4}-[a-f0-9]{4}-[a-f0-9]{12}$/.test(id)||!Number.isSafeInteger(revision)||revision<1)return null;
  const url=new URL('/api/v3/core/visuals/'+id+'/view',current);url.searchParams.set('revision',String(revision));url.searchParams.set('theme',light?'light':'dark');return url.href;
}
function theme(frame){
  const style=getComputedStyle(document.body),light=document.body.classList.contains('light-mode');
  frame.contentWindow?.postMessage({type:'webagent:visual-theme',theme:light?'light':'dark',colors:{bg:style.backgroundColor,text:style.color}},'*');
}
export function mountVisualFrame(spec){
  const url=visualUrl(spec.data?.visual_id,spec.data?.revision,location.href,document.body.classList.contains('light-mode'));if(!url)return null;
  const frame=document.createElement('iframe');frame.src=url;frame.title=spec.title||'Interactive visual';frame.dataset.coreVisual='true';
  frame.setAttribute('sandbox','allow-scripts');frame.referrerPolicy='no-referrer';frame.loading='lazy';frame.style.width='100%';frame.style.height='600px';frame.style.border='1px solid var(--border, #334155)';frame.style.borderRadius='8px';frame.addEventListener('load',()=>theme(frame));return frame;
}
export function syncVisualThemes(root){for(const frame of root.querySelectorAll('iframe[data-core-visual]'))theme(frame);}
