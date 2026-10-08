# Vendored `gpui-component` patches

This directory is a copy of the published [`gpui-component`
0.5.1](https://crates.io/crates/gpui-component) crate (Apache-2.0, see
`LICENSE-APACHE`) with local patches. The workspace wires it in via
`[patch.crates-io]` in the root `Cargo.toml`, so every crate in this repo
builds against this copy instead of the registry one.

Nothing else about the crate is changed: `Cargo.toml` is exactly as published
(the crate's own `Cargo.lock` and `.cargo*` metadata files were dropped), so
the dependency graph and feature set still match what the lockfile expects.

The one exception is formatting: the patched file was run through this repo's
`rustfmt` (edition 2024), which reorders `use` items. Diffing a vendored file
against the registry copy therefore shows a few import-ordering lines next to
the actual patch — only `src/text/inline.rs` is affected.

**To re-vendor a newer version**: copy the new crate source over this
directory, re-apply the patches below (each is marked with a `LOCAL PATCH`
comment in the source), and re-run `cargo check --workspace`.

**To drop the patch entirely**: delete this directory and the
`[patch.crates-io]` / `exclude = [...]` blocks in the root `Cargo.toml`.

---

## 1. `src/text/inline.rs` — selection painting is O(chars²) per frame

**Symptom.** With a selection inside a long `TextView` (in CrabPort: selecting
text in a long message in the AI panel, then scrolling), every frame takes tens
to hundreds of milliseconds — the UI visibly stutters. Short messages are fine,
and the same selection in a shorter view (the assistant's reply) is smooth.
Measured from the outside: the panel's own `render` is fast; the time goes into
the `TextView`'s paint.

**Cause.** `Inline::layout_selections` runs from `Inline::paint`, i.e. on every
frame for every painted inline. Whenever the `TextView` has *any* selection it
walked **every character of that inline** and, per character, called
`TextLayout::position_for_index` **twice**. That call is itself O(chars): it
walks the layout's lines/wrap boundaries and then scans glyphs linearly
(`gpui::text_system::line_layout`). A long paragraph is a *single* inline (soft
line breaks do not split markdown paragraphs), so the function cost
O(chars²) per frame regardless of where the selection was or how big it was.

**Patch.** Three changes in `layout_selections`:

1. Reject inlines the selection rectangle doesn't reach
   (`!bounds.intersects(&selection_bounds)`) before doing any per-character
   work. `is_selection` stays `true` so the caller keeps the I-beam cursor and
   the suppressed link clicks it would have while a selection exists.
2. If the selection rectangle contains the inline whole (select-all, or a drag
   past both ends), the inline's entire text is selected — return that without
   testing any character. This mirrors the "band covers the rows" fast path
   upstream added in 0.7.
3. Clamp the remaining per-character scan to the byte window the selection
   rectangle can touch, derived from its two corners with
   `TextLayout::index_for_position` (window coordinates in, byte index out;
   `Err` carries the closest index). A 64-byte margin keeps the
   half-character-width edge test exact, and the window is clamped to UTF-8
   char boundaries before slicing (hence the `floor_char_boundary` /
   `ceil_char_boundary` helpers).

Only the characters inside that window keep the original per-character test, so
selection boundaries — including the partial-character cases the original test
handled — behave exactly as before.

**Cost after the patch**: O(1) for inlines the selection doesn't touch, O(1)
for inlines it covers whole, and O(window) for the rest, instead of O(chars²)
for every painted inline.

**Upstream status.** The 0.7 line (where the text stack moved to the new
`gpui-base` crate) fixes the same problem: `layout_selections` gained exactly
those two fast paths, and selection painting now derives "one box per laid-out
row" from the wrap boundaries instead of walking characters. Its comments name
the symptom we hit ("one long code block alone made every paint of a held
selection cost tens of milliseconds", "the largest single cost of painting a
long chat message while it scrolls"). Nothing above is exposed to users, so
this patch can be dropped once the app moves to gpui 0.3 / gpui-component 0.7.

**Reference implementation.** Zed's markdown has the same problem shape and
solves it by turning a source range into per-row quads instead of walking
characters — `crates/markdown/src/markdown.rs`
(`bounds_for_sorted_source_ranges` / `push_bounds_for_line_source_range`), which
skips lines that don't intersect the selection with a single comparison and
uses the layout-time wrap boundaries for the rows that do. The patch above is
the same idea adapted to `gpui-component`'s screen-space selection model.
