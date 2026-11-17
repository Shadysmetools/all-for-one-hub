# All for One - brand

Tagline: **One shared brain for the whole support team.**
Pitch: WhatsApp -> helpdesk, with AI that drafts and humans who approve.

## The mark
A shield crest. Four knights (great helm, bascinet, bascinet, great helm) stand side by side on a ground line, each raising one arm with a gauntleted fist on a sword. The four swords fan apart and meet at one teal point at the top of the shield: "All" (four knights), "for One" (one point). The teal point is a solid dot with a soft halo (the shared brain). The tabards are plain: no crosses and no religious symbols anywhere. The mark is original geometry; it has no text, so the SVG has no font dependency.

Flat fills only, no gradient on the mark. Teal is used only for the meeting point (it means "approved / done" and "the shared brain").

## Colour
| Role | Dark UI | Light UI | Use |
|---|---|---|---|
| Signal (primary) | `#4DA3FF` | `#1F6FD6` | mark, links, data/flow |
| Pulse | `#3FE0C5` | `#0E9F8A` | done, live, judge, approved |
| Flare | `#FFB547` | `#B7791F` | owner approval, warning |
| Alarm | `#FF5C7A` | `#D6334F` | blocked, error |
| Abyss (background) | `#050A18` | `#F5F8FF` | page background |
| Hull (panel) | `#0B1530` | `#FFFFFF` | panels |
| Rule | `#1D2D57` | `#C9D6F2` | hairlines, grid |
| Ink | `#DDE7FF` | `#0B1530` | body text |
| Ink dim | `#8C9CC4` | `#4A5A85` | secondary text |

`#4DA3FF` is about 2.6:1 on white, fine for a graphic mark but too weak for text. Use `#1F6FD6` on light. Always use the `-light` files on light backgrounds.

## Type
- **Wordmark: Playfair Display** (SIL Open Font License 1.1). 800 for "All" and "One", 400 italic for "for". Fallbacks: DM Serif Display (OFL), Georgia, Times New Roman.
- **Labels: JetBrains Mono** (OFL).
- The wordmark in `logo-lockup*.svg` and `hero-*.svg` is live text, which GitHub will not load a web font for. Use the rendered `*.png` files on GitHub and in the README; the SVGs are the editable source (a viewer without Playfair sees Georgia).

## Clear space and size
- Clear space = the width of one knight on every side.
- Full mark: minimum 32 px. At 16-32 px use `favicon.svg` (four helms, thicker blades and the teal point, on a dark tile so it works on light browser tabs; at 16 px it reads as a shield with a bright fan).
- Lockup: minimum 96 px wide.
- On photos or busy backgrounds, put the mark on a solid Abyss or white tile.

## Do
- Use `logo.svg` / `logo-lockup.svg` on dark, the `-light` files on light.
- Keep the four knights, the plain tabards and the four swords meeting at the one teal point.
- Use one accent at a time; teal only for "approved / done".

## Don't
- Don't recolour outside the palette, add gradients, outlines or shadows to the mark.
- Don't change the sword angles or knight spacing, add crosses or other symbols, or move the meeting point.
- Don't stretch or rotate it, or place `#4DA3FF` text on white.
- Don't set the wordmark in another typeface.

## Files
| File | What |
|---|---|
| `logo.svg`, `logo-light.svg` | The mark, for dark and light backgrounds |
| `logo-lockup.svg`, `logo-lockup-light.svg` | Mark + wordmark (live text) |
| `logo-lockup.png`, `logo-lockup-light.png` | Lockup rendered with Playfair Display, 1200 px wide, transparent |
| `logo-512.png`, `logo-256.png`, `logo-64.png` | The mark as PNG |
| `app-icon.svg`, `app-icon-1024.png` | Square 1024 px app icon (mark on Abyss, safe padding, opaque) for the Meta app |
| `favicon.svg` | Small-size mark |
| `hero-dark.svg/.png`, `hero-light.svg/.png` | README banner, PNG at 2400x720 |
| `social-preview.html/.png` | GitHub social preview, 1280x640 |
| `render-brand.mjs` | Renders the PNGs |

## README hero (GitHub dark and light)
```html
<picture>
  <source media="(prefers-color-scheme: dark)" srcset="assets/brand/hero-dark.png">
  <img alt="All for One: one shared brain for the whole support team" src="assets/brand/hero-light.png">
</picture>
```

## Rendering the PNGs
```
cd assets/brand
npm i playwright            # or PW_DIR=<dir with node_modules/playwright>
node render-brand.mjs       # uses the system Edge; loads Playfair Display from Google Fonts at render time
```
Upload the social preview in GitHub: Settings > General > Social preview.
