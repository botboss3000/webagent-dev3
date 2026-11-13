# Interactive terminals in V3

Terminal Control starts real interactive processes on a pseudo-terminal in the current
session workspace. Each terminal runs under the same sandbox as Codebase Admin `shell`:
it writes only the session workspace, reads system folders and the checkout's source
files under /code, and connects over TCP only to web ports, its own four preview ports
and the ephemeral range. Coding CLI binaries and their credentials are not implicitly
installed or supplied. Use Codebase Admin `shell` for one-shot commands.

Use `terminal_open` → `terminal_read` → `terminal_send` → `terminal_wait` → `terminal_read`, then `terminal_close`. Do not replay input after an uncertain result.

Terminal IDs belong to the current workspace. Output is a bounded byte ring, preserving raw terminal escape sequences; this is not a rendered screen. `cursor` advances in bytes, and `truncated: true` means older output was discarded. Terminal processes are capped at four per workspace, expire after fifteen minutes without tool activity, and have a one-hour absolute lifetime. A worker restart closes them and never replays keystrokes.  A server listening on one of the workspace's preview ports can be shared with Remote Tunnel.

Opening, sending and closing require the live Rust execution policy and any required approval. Read, wait and list are observations; observing never grants permission to execute. Respect the configured caution guidance. If the user asks to stop, close the terminal through its authorized control path.
