# browser_extension_action

## Usage

- Act in the chatting user's own browser, in the agent's own WebAgent window, on allowed sites only. open: new tab at url. navigate: current agent tab to url. read: the page's text and controls, each control with a ref. click/type/press need a ref from your latest read. scroll: dy pixels. tabs: list agent tabs. close: close the current agent tab.
- The user watches and can take over at any time.
- If a page leads off the site list, every action there is refused except navigate (or open) to an allowed address.
