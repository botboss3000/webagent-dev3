# python

Run approved python commands in the current session workspace. They may read every file in the container, including the live checkout, application state under /state and sibling workspaces, and write only this workspace. Source writes use broker task file tools, or checkout_edit for a direct live-checkout edit. Control sockets stay excluded. Live runtime policy determines approval and authorization.

For isolated task verification use build_work check with the current task digest. Workspace command results do not verify an isolated broker draft. Keep checks bounded and preserve cancellation/recovery.
