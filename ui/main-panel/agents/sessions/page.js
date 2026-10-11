import { mountSessions } from './js/sessions-page.js';
export async function mountCommonSessions(host,options){
  const response=await fetch('/v3-ui/main-panel/agents/sessions/sessions.html',{credentials:'same-origin'});
  if(!response.ok)throw Error('Sessions page unavailable');
  const html=await response.text();
  if(!options.isCurrent())return null;
  host.innerHTML=html;
  return mountSessions(host,options);
}
