# Homepage: Steel Blue Palette and Flowing Grid — Design

## Intent

The homepage currently sits on near-black (`#0a0a0b`) and reads as too dark.
The palette moves to a steel blue, and a very faint, slowly flowing grid gives
the pages depth without becoming decoration that fights the copy.

Decided with the operator in a visual brainstorming session: steel blue
(`#2a4d6e`), grid across the whole page, slower and weaker than the first
draft.

## Goals

- Warm-neutral near-black replaced by a steel blue ramp on every page.
- A fixed, decorative grid layer behind all content on every page, 48 px tile,
  24 s per pass, opacity `0.18`.
- Text and panel contrast stays at or above WCAG AA on every surface.
- The grid costs no new dependency, no asset, and no runtime request.

## Non-goals

- No layout, copy, navigation, or component changes.
- No light theme, no theme switch.
- No new 3D objects beyond the grid.
- OG image and its dark gradient overlay stay as they are.

## Anti-goals

A grid strong enough to speckle code blocks or the legal pages. Animated
background-position across the full viewport. A layer that swallows clicks or
overlaps focus outlines.

## Constraints

- `website/homepage/README.md` applies: no client-side framework, no new
  dependency, no runtime request, `prefers-reduced-motion` respected.
- Static output only; the layer must work with JavaScript disabled.
- `npm test` in `website/homepage` is the gate.

## Approaches

- **A — animate `background-position` on a body pseudo-element.** Least markup,
  but repaints the full viewport every frame and stutters on phones.
- **B — fixed decoration layer, moved with `transform` (chosen).** Compositor
  friendly at full page height, no asset, easy to freeze.
- **C — pre-rendered WebP tile.** Same cost profile as B, plus an asset and
  visible seams when zoomed.

## Decisions

- `src/styles/tokens.css` gets a steel-blue ramp: `--ink #2a4d6e`,
  `--panel #2f5375`, `--panel-raised #345b80`, `--footer #24425f`,
  `--text #f7f5ef`, `--muted #c9d1dc`, plus new `--text-soft` for the
  previously hard-coded `#c9c6bd`. Low end of each pair: muted on the lightest
  surface 4.62:1, body text 6.52:1.
- The ~15 hard-coded `#c9c6bd` / `#d6d3ca` values in `components.css` become
  `--text-soft` so the ramp has one source.
- `.button-primary` and `.skip-link` keep `--text` on `--ink`; that pairing is
  now blue-on-cream and still passes.
- `--line` and the white overlay alphas move from warm cream to the new text
  hue.
- `theme-color` in `Base.astro` and both colours in `site.webmanifest` become
  `#2a4d6e`; the favicon tile follows.
- New `src/components/GridField.astro`, included once in `Base.astro` before
  the skip link: `aria-hidden`, `pointer-events: none`, `position: fixed`,
  `z-index: -1`, top/bottom mask, two `linear-gradient` layers on an over-tall
  plane with `perspective` and `rotateX`.
- Tile is 20 px with `rotateX(42deg)`: tight, near-square cells. A 48 px tile at
  steeper tilt read as single sparse slashes across a full viewport.
- The plane animates `translateY` over 24 s, linear, infinite.
- `@media (max-width: 699px)` and `prefers-reduced-motion: reduce` set
  `animation: none`; mobile additionally drops opacity to `0.12`.
- Legal pages inherit the layer and keep the same low opacity.

## Tests

- Content test: exactly one `.grid-field`, `aria-hidden="true"`, and the new
  `theme-color`.
- E2E: computed `pointer-events` is `none`; under reduced motion the layer's
  `animation-name` is `none`.
- Existing contrast, overflow, and axe checks must stay green.
