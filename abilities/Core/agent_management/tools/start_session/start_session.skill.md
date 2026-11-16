# start_session

Starts a separate session that appears in the user's session list.

By default it is a fork of this agent. It carries this conversation's history up
to the last completed reply (not the current request) and starts with `text`, so
put the task there. The fork is told to do the work directly and not to fork
again. If the parameters include `agent_id`, its description explains how to
delegate to another agent instead.

`notify` (default false) sends this conversation the session's final reply when
its run ends, like a sub agent notice. With notify true, end your turn and wait
for the notice instead of polling. Use start_spawn for work that should stay a
tab of this conversation.
