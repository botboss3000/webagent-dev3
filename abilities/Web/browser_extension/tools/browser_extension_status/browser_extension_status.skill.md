# browser_extension_status

## Usage

- Whether the chatting user's browser extension is linked and online, which sites this agent may control, and setup steps (install_url) when it is not linked.
- Call before browser_extension_action.
- `refused_sites` lists entries on the agent's list that the extension never uses: this app's own address, loopback, private-network or Tailscale hosts and IP addresses. Tell the user an operator listed them; you cannot open them.
- `status: outdated`: the user's extension is too old to take actions; give them `install_url` to download it again and reload it in chrome://extensions. Each device shows its `version`.
