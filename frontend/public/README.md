# public/

Static assets served at root.

## Files

- `favicon.svg` — primary vector favicon (chat bubble + orange accent dot)
- `favicon-32.png` — 32×32 PNG favicon fallback for browsers without SVG support
- `apple-touch-icon.png` — 180×180, white background, generated from `favicon.svg`
- `icon-192.png`, `icon-512.png` — PWA icons (`purpose: any`) referenced by `site.webmanifest`
- `icon-maskable.svg` — maskable source: glyph shrunk to the ~60% centre safe zone
- `icon-192-maskable.png`, `icon-512-maskable.png` — PWA maskable icons (`purpose: maskable`)
- `site.webmanifest` — PWA manifest for "Add to Home Screen" on iOS / Android

## Regenerating PNGs

If `favicon.svg` or `icon-maskable.svg` changes, regenerate the rasters:

```sh
brew install librsvg
cd frontend/public
# any-purpose icons + favicon fallback, from favicon.svg
rsvg-convert -w 32  -h 32  -b "#ffffff" favicon.svg -o favicon-32.png
rsvg-convert -w 180 -h 180 -b "#ffffff" favicon.svg -o apple-touch-icon.png
rsvg-convert -w 192 -h 192 -b "#ffffff" favicon.svg -o icon-192.png
rsvg-convert -w 512 -h 512 -b "#ffffff" favicon.svg -o icon-512.png
# maskable icons, from icon-maskable.svg (glyph in the safe zone)
rsvg-convert -w 192 -h 192 -b "#ffffff" icon-maskable.svg -o icon-192-maskable.png
rsvg-convert -w 512 -h 512 -b "#ffffff" icon-maskable.svg -o icon-512-maskable.png
```

The `-b "#ffffff"` flag adds an opaque white background — iOS rejects
transparent home-screen icons.
