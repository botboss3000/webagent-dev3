// @code-index {"version":1,"feature":"chat-elements","aliases":["chat elements","activity group","typing ellipsis","mini transcript"],"purpose":"Owns modular chat elements. Config: ui/chat/chat.json -> chat_common.chat_elements and show_activity_groups, merged with desktop/mobile/widget profiles.","span_lines":30,"configuration":{"status":"configured","references":[{"path":"ui/chat/chat.json","pointers":["/chat_common/chat_elements","/chat_common/show_activity_groups"],"profile_overrides":true}],"opportunities":[]}}
// Activity group owns its compact label and disclosure; mini-transcript owns the body.
/** Compact group label: nonzero counts, any extra parts (such as sub-agent notices), then status. */
export function activityLabel(group,{error='',extra=[]}={}) {
    // Summarize nonzero category counts without changing the underlying journal rows.
    const nouns={retries:['retry','retries'],subagents:['sub-agent','sub-agents']};
    // Tools and sub agents lead, whatever order the summary returned them in.
    const leading=['tools','subagents'];
    const entries=[...leading.filter(kind=>group.counts[kind]).map(kind=>[kind,group.counts[kind]]),
      ...Object.entries(group.counts).filter(([kind,count])=>count&&!leading.includes(kind))];
    const parts=entries
      .map(([kind,count])=>{const noun=nouns[kind]||[kind.replace(/s$/,''),kind];
        return count+' '+noun[count===1?0:1];});
    const phase={running:'Working…',queued:'Queued…',waiting_external:'Waiting for build…',finalizing:'Checking response…',
      stopping:'Stopping…',completed:'Completed',failed:'Failed',cancelled:'Stopped',
      finished:'Finished'}[group.state] || group.state;
    // Loaded rows decide; before the group is expanded its server summary says whether it is retrying.
    const last=group.rows?.at(-1);
    const retrying=last?(last.retry?(last.autoResume?'recovering':'retry'):null):group.retrying;
    const progress=group.state==='queued' && retrying ? (retrying==='recovering' ? 'Recovering…' : 'Retrying connection…')
      : group.kind==='compaction' && ['queued','running','finalizing'].includes(group.state)
      ? (group.state==='queued' ? 'Compaction queued…' : 'Compacting…') : phase;
    // Normal completion needs no status suffix; retain active and exceptional states.
    const status=error || (['completed','finished'].includes(group.state)?'':progress);
    return [...parts,...extra,status,
      group.boundary_quality==='legacy_unverified'?'Legacy boundary unverified':''].filter(Boolean).join(' · ');
}
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
    const label=activityLabel(group,{error});
    node.firstElementChild.textContent=label;
    // A finished group with no rows and no status has nothing to disclose, such as a superseded stop.
    node.hidden=!label;
    node.setAttribute('aria-label','Activity group');
    node.classList.toggle('v2-activity-group-glimmer',active);
    transcript.update(group);
  }
  return {element:node,update,
    // Remove the listener and all transcript-owned resources on session teardown.
    dispose(){node.removeEventListener('toggle',toggle);transcript.dispose();node.remove();}};
}
