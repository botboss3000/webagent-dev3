// @code-index {"version":1,"feature":"chat-elements","aliases":["chat elements","activity group","typing ellipsis","mini transcript"],"purpose":"Owns modular chat elements. Config: data/config/chat.json -> chat_common.chat_elements and show_activity_groups, merged with desktop/mobile/widget profiles.","span_lines":30,"configuration":{"status":"configured","references":[{"path":"data/config/chat.json","pointers":["/chat_common/chat_elements","/chat_common/show_activity_groups"],"profile_overrides":true}],"opportunities":[]}}
// Activity group owns its compact label and disclosure; mini-transcript owns the body.
/** Connect a persistent activity disclosure to an independently owned transcript. */
export function createActivityGroup({transcript}) {
  const node=document.createElement('details');node.className='v2-activity-group';
  const summary=document.createElement('summary');
  node.append(summary,transcript.element);
  // Native details supplies pointer and keyboard disclosure semantics.
  const toggle=()=>{node.open?transcript.open():transcript.close();};
  node.addEventListener('toggle',toggle);
  // Refresh counts/status without replacing the focused summary or open transcript.
  function update(group,{error='',active=false}={}) {
    node.dataset.activityGroupId=group.id;
    // Summarize nonzero category counts without changing the underlying journal rows.
    const parts=Object.entries(group.counts).filter(([,count])=>count)
      .map(([kind,count])=>count+' '+(count===1?kind.replace(/s$/,''):kind));
    const phase={running:'Working…',queued:'Queued…',waiting_external:'Waiting for build…',finalizing:'Checking response…',
      stopping:'Stopping…',completed:'Completed',failed:'Failed',cancelled:'Stopped',
      finished:'Finished'}[group.state] || group.state;
    const progress=group.kind==='compaction' && ['queued','running','finalizing'].includes(group.state)
      ? (group.state==='queued' ? 'Compaction queued…' : 'Compacting…') : phase;
    // Normal completion needs no status suffix; retain active and exceptional states.
    const status=error || (['completed','finished'].includes(group.state)?'':progress);
    node.firstElementChild.textContent=[...parts,status,
      group.boundary_quality==='legacy_unverified'?'Legacy boundary unverified':''].filter(Boolean).join(' · ');
    node.setAttribute('aria-label','Activity group');
    node.classList.toggle('v2-activity-group-glimmer',active);
    transcript.update(group);
  }
  return {element:node,update,
    // Remove the listener and all transcript-owned resources on session teardown.
    dispose(){node.removeEventListener('toggle',toggle);transcript.dispose();node.remove();}};
}
