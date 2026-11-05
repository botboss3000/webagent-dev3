# start_session

Starts a separate session that appears in the user's session list.

- Without `agent_id`: a fork of this agent. It carries this conversation's
  history up to the last completed reply (not the current request) and starts
  with `text`, so put the task there. The fork is told to do the work directly
  and not to fork again.
- With `agent_id`: delegates to that agent in a fresh session. Only works when
  this agent's Agent Management settings allow delegation. Use
  list_delegatable_agents to find IDs.

`notify` (default false) sends this conversation the session's final reply when
its run ends, like a sub agent notice. With notify true, end your turn and wait
for the notice instead of polling. Use start_spawn for work that should stay a
tab of this conversation.
