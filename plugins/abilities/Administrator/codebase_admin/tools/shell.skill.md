# shell

Run approved shell commands in the current session workspace. They may read every file in the container, including the live checkout, application state under /state and sibling workspaces, and write only this workspace. Source writes use broker task file tools, or checkout_edit for a direct live-checkout edit. Control sockets stay excluded. Live runtime policy determines approval and authorization.

For isolated task verification use build_work check with the current task digest. Workspace command results do not verify an isolated broker draft. Keep checks bounded and preserve cancellation/recovery.

Search source with file_search, or with rg, which skips the ignored build-jobs/, target/ and data/ trees. grep -r from the checkout root walks hundreds of gigabytes of build artifacts and runs out of time. A command stops after timeout_seconds (default 120, at most 600) and returns as a failed result; narrow it instead of repeating it. Do not read application databases or other runtime state to answer a source or configuration question.
