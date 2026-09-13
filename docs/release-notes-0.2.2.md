# v0.2.2

A fix for modded saves that refused to load at all, and a clearer answer when
part of a save can't be read.

## Fixes

- **Saves using Modular Load Balancers load again.** The mod keeps its
  item-routing table in a compact binary form the parser didn't recognise, and
  the whole save was rejected over it:

  ```
  Failed to load save: object re-parse failed during index build:
  object at (3946, 5836): String decode failure at offset 24369724 of length 3670015
  ```

  The deeper problem was not the mod. One object the parser couldn't read was
  enough to abort the entire load — on the save this was reported against, 106
  objects out of 37,958. An object that can't be read is now skipped and
  reported instead, so no single piece of mod data can stop a save from
  opening.

  **Modded buildings still appear on the map.** Where a building sits, which
  way it faces and what it is all come from a part of the save that always
  reads cleanly, so those 106 load balancers draw exactly where they belong,
  under the **Unknown** category. What's missing is what's *inside* them —
  item filters and inventory — and they can't be edited. The load status line
  says how many objects this applied to; the browser console lists the first
  few in full, which is the useful half of a bug report.

  This is deliberately general rather than a fix for one mod: whatever a mod
  stores in a shape the parser doesn't know, the rest of the save still opens.

- **A plain explanation instead of a parser error when editing unreadable
  content.** Deleting or duplicating one of those objects used to report
  `String decode failure at offset 24369724 of length 3670015`. It now names
  the object and says it can be viewed on the map but not edited.

## Under the hood

- Saves are unchanged by all of this: a save with nothing unreadable in it
  produces byte-for-byte the same map data as v0.2.1, verified against four
  large vanilla saves.
- The save editor is no less strict. An object that read correctly *before* an
  edit and stops afterwards is still treated as the editor corrupting the
  save, and the edit is refused — that check is what the load-time strictness
  was really there for.

## Checksums (SHA-256)

```
{CHECKSUMS}
```
