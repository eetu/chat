# public/

Static assets served at root.

## Files

- `favicon.svg` — primary vector favicon (chat bubble + orange accent dot)
- `apple-touch-icon.png` — 180×180, white background, generated from `favicon.svg`
- `icon-192.png`, `icon-512.png` — PWA icons referenced by `site.webmanifest`
- `site.webmanifest` — PWA manifest for "Add to Home Screen" on iOS / Android

## Regenerating PNGs

If `favicon.svg` changes, regenerate the rasters:

```sh
brew install librsvg
cd frontend/public
rsvg-convert -w 180 -h 180 -b "#ffffff" favicon.svg -o apple-touch-icon.png
rsvg-convert -w 192 -h 192 -b "#ffffff" favicon.svg -o icon-192.png
rsvg-convert -w 512 -h 512 -b "#ffffff" favicon.svg -o icon-512.png
```

The `-b "#ffffff"` flag adds an opaque white background — iOS rejects
transparent home-screen icons.
