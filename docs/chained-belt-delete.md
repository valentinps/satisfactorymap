# Chained-belt delete — the per-belt item write-back trick

## What the feature does

Deleting a belt/lift that belongs to a conveyor chain (every belt in a 1.0+
save does) removes the belt AND its `FGConveyorChainActor`, and hands every
**surviving** belt of that line its own items back. Net in-game effect after
the next load: the line is cut at the deleted segment, chains are rebuilt on
both sides, and only the deleted segment's items are gone — the same outcome
as dismantling that belt in game (minus the refund).

Implementation: `plan_delete_actors` in `rust_parser/core/src/editor/apply.rs`
(chain cascade + write-back), `belt_item_record` (record serializer),
`apply_plan` (mixed remove+insert offset remapping). Test:
`delete_chained_belt_removes_chain_actor` in
`rust_parser/core/tests/editor_delete.rs`.

## Why a "trick" is needed at all

Since the 1.0 conveyor rework, belts do not own their items. One hidden
`FGConveyorChainActor` per contiguous run of connected belts/lifts holds:

- a packed array of belt entries (belt ref, world-space spline elements,
  three `u32`s of **unverified semantics**, per-belt `lead`/`tail` item
  indices, belt index),
- a ring buffer of item slots (`maximum_items` capacity, chain-level
  `lead`/`tail` indices; empty-path slots = gaps between items),
- scalars we have not reverse-engineered (`cu32`, exact meaning of
  `maximum_items` vs. chain length).

Deleting one belt honestly would mean **splitting** the chain actor into two
new chain actors — synthesizing actors whose fields include those
unverified `u32`s. Getting them wrong corrupts a save *today*. We refuse
that risk.

## The trick

The pre-1.0 save format stored items directly on each belt, and that field
still exists in the current format: every belt object's actor-specific
trailing data starts with a `u32` item count (always 0 in 1.0+ saves) —
see the `conveyor_belts` branch in `rust_parser/core/src/object.rs`.

When the game loads a save containing belts **without a chain actor**, the
chain subsystem builds fresh chain actors for them and seeds the new chains
from each belt's own item list. This is the migration path that carried
every pre-1.0 save through the 1.0 update without emptying players' belts.

So the delete plan:

1. deletes the selected belt(s) + the whole chain actor(s) they belonged to
   (removes only — never touches unverified chain internals);
2. for each surviving belt of a deleted chain, computes the belt's slice of
   the chain's item ring (`lead/tail` per belt, `rem_euclid` ring
   arithmetic — the same math as the tooltip's per-segment item list,
   `conveyor_chain_segment_item_paths` in `mapdata/queries.rs`);
3. writes the non-empty slots into the belt's own item list, spacing
   positions along the belt via the chain's spline chord length (slot
   index / slot count × belt length);
4. leaves each surviving belt's `mConveyorChainActor` property ref dangling
   (the game reads a missing target as null — that is what marks the belt
   "chainless" for rebuild).

The written record is the standard v44+ `InventoryItem` + offset layout:

```
u32 0        InventoryItem leading padding (always 0 in game-written data)
string       item class path (e.g. .../Desc_IronPlate.Desc_IronPlate_C)
u32 0        "has item state" flag (we never write state)
f32          position along the belt, cm from belt start
```

(Pre-v44 encoding — two empty strings instead of the flag — is never
written: chain actors only exist in saves past the v44 gate.)

## Why this is version-safe (and what is NOT guaranteed)

**Format-safe, guaranteed:** the edited file's save version stamp is
unchanged (46+). The per-belt item list is part of what a v44+ belt record
*is* — a future game version cannot drop parsing it for saves of this
version without breaking every 1.0-era save in existence. The file is
byte-valid current-format; nothing about it is "old".

**Transient by design:** the odd state (items on belts, no chain actor)
exists only in the exported file. First game load rebuilds the chains; the
game's next save is canonical current-form (chains own items, belt lists
back to 0). Nothing long-lived depends on the trick.

**Behavior-dependent, NOT guaranteed:** the game *seeding* rebuilt chains
from per-belt items is engine behavior, not format. If a future update
stopped doing it, cut lines would load with empty belts — a soft
degradation back to "items on the deleted chain vanish", never corruption
or a failed load.

**Empirically unverified corner (as of 2026-07):** no save in
`map/uploads` predates chains, so the write-back record was never diffed
against game-written per-belt bytes — the layout comes from the community
InventoryItem consensus (leading u32 = 0) plus our own parser round-trip.
First in-game load of a cut line with items confirms it end-to-end.

## If the compatibility ever drops

Symptom to watch for: after a game update, exporting a save with a
chained-belt delete and loading it in game shows the cut line's surviving
belts **empty** (or, worst case, a load failure — which would indicate a
format regression on the game side, not just dropped seeding).

Recovery plan:

1. Get saves written by the new game version containing belts with items;
   re-derive the chain actor's unverified fields against them
   (`cargo run --release -p sav_core --example dump_belt_items -- <saves>`
   prints per-belt records; `debug_object` hexdumps object spans).
2. Replace the write-back with a true chain **split**: synthesize two chain
   actors from slices of the original (belt entries and item slots are
   contiguous spans, so most bytes can be spliced verbatim; the belt-index
   `confirm_u32` fields need renumbering, `lead/tail` indices rebasing, and
   each surviving belt's `mConveyorChainActor` ref a same-length rename to
   its new chain — the machinery in `editor/rename.rs` covers that).
3. The delete-plan structure already supports it: plans may mix removes,
   inserts, and patches (`apply_plan` remaps insert offsets across removed
   spans), so a split is expressible in one op.
