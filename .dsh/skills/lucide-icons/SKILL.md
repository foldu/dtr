---
name: lucide-icons
description: Use Lucide icons (lucide.dev) as real icons in this repository instead of hand-drawn shapes, by fetching the SVG for the named icon from lucide.dev's API. Use when a UI element needs a close/plus/chevron/gear glyph, or when a glyph has been drawn by hand or taken from a font.
---

# Lucide icons

Lucide (<https://lucide.dev>) is a set of ~1600 line icons, ISC licensed. Each icon
has a stable name and a page at `https://lucide.dev/icons/<name>`; the raw SVG for
that icon is served by the site's own API:

```
https://lucide.dev/api/icons/<name>
```

That endpoint returns the real file — a 24×24 `viewBox` with `stroke="currentColor"`
and `stroke-width="2"` — not a page. `https://lucide.dev/icons/<name>.svg` is a
404, and `https://lucide.dev/icons/<name>` is the HTML page; only the `/api/` URL
is an image.

## Fetching

`scripts/fetch.sh NAME...` writes `crates/dtr/assets/icons/<name>.svg`, refusing to
overwrite an icon that is already there (a fetched icon is a reviewed asset; a
silent replacement changes what the UI draws):

```sh
.dsh/skills/lucide-icons/scripts/fetch.sh x plus chevron-right
```

It prints the SHA-256 of each file. Commit the SVGs: the build must not need the
network.

## Using one in egui

dtr loads them through `egui_extras`' SVG loader, installed once at startup
(`egui_extras::install_image_loaders(ctx)`), and drawn as an image:

```rust
let icon = egui_extras::include_image!("../assets/icons/x.svg");
ui.add(
    egui::Image::new(icon)
        .fit_to_exact_size(egui::vec2(12.0, 12.0))
        .tint(colour),
);
```

`include_image!` embeds the bytes, so the icon ships inside the binary — which is
what makes the one-file Windows build still one file.

## Rules that save time

- **Tint, do not recolour the file.** Lucide SVGs use `currentColor`; the tint goes
  on the `Image`, and the file stays byte-identical to upstream.
- **Give the icon a name.** A bare image has no text, so attach
  `Response::widget_info(|| WidgetInfo::labeled(Role::Button, true, "close tab 2"))`
  or a tooltip; otherwise a screen reader and the test harness see nothing.
- **Size to the stroke, not the box.** The grid is 24 px with a 2 px stroke, so
  12–16 px renders cleanly; below ~10 px the stroke turns to mush.
- **Check the name first.** Names are kebab-case and there is no `close` — the
  cross is `x`. A wrong name is a 404 and an empty asset.

## Reference

- All icons and names: <https://lucide.dev/icons>
- Licence: ISC, no attribution required in the UI.
