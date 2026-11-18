# sandbox_elements

## Usage

- List this agent's saved UI elements for this user (`mine`), the ones shared with every user of the agent (`shared`), each with name, title, description and revision.

Takes no arguments. Element files are not in the result: read them in the sandbox, where they are read-only at `saved/mine/<name>/index.html` and `saved/shared/<name>/index.html` (`sandbox_file` with that path, or `../saved/...` from `sandbox_shell` and `sandbox_python`). To change one, copy it into your own folder, edit it there and save it again with `sandbox_save`.
