# V3 boot animation

The initial HTML contains a canvas using the existing v2 bullet-grid renderer: identical dot displacement, influence radius, growth and easing. Its registry, palette and renderer load synchronously before boot.js starts the same seven-second circular phantom input. Both real and phantom pointers affect the grid during boot only.

While body.is-booting is set, all app surfaces are visibility-gated (retaining layout for initialization). The curtain, grid and glow stay black/dark in every theme, so the app theme applying mid-boot does not recolour them. Readiness clears the gate, then fades the grid out over black in 0.2s (its blue-grey dots would read blue over a light app) and the curtain and glow out over the app in the next 0.5s before removing them and stopping the grid (immediately under reduced motion). There are no persistent background or card effects.

Authentication failure and the ten-second safety timeout also release the scene. Reduced motion skips the orbit. Shared v2 source is unchanged. Mounted UI changes require refresh, not a rebuild.
