import { mountSessions } from './js/sessions-page.js';
export async function mountSessionsPage(host,options){
  const response=await fetch('/v3-ui/main-panel/agents/pages/sessions/sessions.html',{credentials:'same-origin'});
  if(!response.ok)throw Error('Sessions page unavailable');
  const html=await response.text();
  if(!options.isCurrent())return null;
  host.innerHTML=html;
  const renderer=mountSessions(host,options);
  const footer=host.querySelector('#sessions-footer');
  const shell=host.closest('#tab-agents');
  // The reference footer belongs to the page shell, outside the table scrollport.
  if(shell&&footer){footer.id='sessions-footer-dock';shell.append(footer);}
  return {refresh:()=>renderer.refresh(),destroy(){renderer.destroy();footer?.remove();}};
}
