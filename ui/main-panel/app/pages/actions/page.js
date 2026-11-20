import { el, request } from '../users/support.js';
import { mountActions } from '../../../../shared/actions/actions-list.js';
// App → Actions: every user's recorded actions across all agents, uncertain ones first; an administrator settles any.
export async function mountAppActions(host){
  host.replaceChildren(el('h2','','Actions'),el('p','ac-hint','Every user\'s recorded actions across all agents: publishing, pushing and other tool calls that must not happen twice. Mark an uncertain one done or failed after checking what really happened.'));
  const box=el('div','');host.append(box);
  await mountActions(box,request,'/admin/actions',{showSession:true,showUser:true,resolvePath:(id,action)=>'/admin/actions/'+encodeURIComponent(action.requester)+'/'+encodeURIComponent(id)+'/resolve'});
}
