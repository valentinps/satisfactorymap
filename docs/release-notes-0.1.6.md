# v0.1.6

A quality-of-life release: modded off-map builds are now reachable, the whole
map can be selected in one keystroke, a loaded save can be unloaded, and the
top bar got straightened out.

## New

- **Builds outside the map are now reachable.** Some mods let you build past
  the vanilla world border; those saves loaded fine but the map refused to
  pan out to them. The pan limits now grow to fit the save's actual extents
  when it outgrows the map — vanilla saves keep the usual limits, and panning
  still can't wander off into the void.
- **Ctrl+A selects everything on the map** — same rules as a box selection
  over the whole world (visible layers only, altitude filter respected), so
  megabase-wide move/copy/delete is one keystroke. On very large saves
  (500k+ objects) it asks for confirmation first. Ctrl+A inside a text field
  still just selects the text.
- **Unload a save without reloading the page.** A new "Unload" button next to
  the Save File header clears the map and sidebar and frees the parser's
  memory (useful before opening a second large save). If you have unexported
  edits it asks first.

## UI fixes

- The search bar is now exactly centered in the top bar instead of drifting
  with whatever buttons surround it.
- The desktop app's update button moved next to the logo, where it no longer
  pushes the search bar off-center when an update is available.

## Checksums (SHA-256)

```
{CHECKSUMS}
```
