# browser_read

Every browser action except input. The ability guidance lists each action, its arguments and its results. For a known sequence of reads, use one `script` call; add `"screenshot": true` only to steps whose image you need. Scripts with clicks use browser_input.

- `keyboard` `height`: emulates a phone's on-screen keyboard on a touch viewport (`viewport` `phone` or `tablet` first). The visible viewport shrinks by `height` px from the bottom (a phone keyboard is about 250-350), firing the resize events an app's keyboard handling reads; `0` closes it. Changing the height while open simulates the viewport jitter some phones show during scrolling. The result reports `focused`. It stays open across navigation until closed or a new `viewport`. To check a keyboard bug: focus the text box, `keyboard` `height` 300, read, then act (for example `browser_input` `touch` drag) and confirm `focused` is still the text box; there is no real keyboard, so focus and the page's own keyboard state are the evidence.
- `measure` `styles`: computed CSS per element now includes `touchAction`, so you can check which element blocks or allows a native pan.
