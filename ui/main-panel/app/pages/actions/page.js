import { el, request } from '../users/support.js';
import { mountActions } from '../../../../shared/actions/actions-list.js';
// App → Actions: the administrator's recorded actions across every agent, uncertain ones first.
export async function mountAppActions(host){
  host.replaceChildren(el('h2','','Actions'),el('p','ac-hint','Your recorded actions across all agents: publishing, pushing and other tool calls that must not happen twice. Mark an uncertain one done or failed after checking what really happened.'));
  const box=el('div','');host.append(box);
  await mountActions(box,request,'/actions',{showSession:true,resolvePath:id=>'/actions/'+encodeURIComponent(id)+'/resolve'});
}
