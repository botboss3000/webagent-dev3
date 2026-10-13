# python

Run approved python commands in the current session workspace. They may inspect mounted development source read-only and write only this workspace. Source writes require broker task file tools. Sibling workspaces, application state and control sockets are excluded from command access. Live runtime policy determines approval and authorization.

For isolated task verification use build_work check with the current task digest. Workspace command results do not verify an isolated broker draft. Keep checks bounded and preserve cancellation/recovery.
