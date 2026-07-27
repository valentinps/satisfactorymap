# Modeler export — test save specifications

Build checklist for the purpose-built `.sav` fixtures behind the Modeler
export tests. PhantomTorture proves the export *scales*; it cannot prove it is
*right*, because nobody knows its true graph. These saves are small enough
that the correct answer is known in advance, which turns "looks plausible"
into a committed assertion.

## Ground rules (read once, applies to all six)

- **One world, six saves.** Use an existing world with everything unlocked —
  a fresh world can't build a Manufacturer. Build each scenario in its own
  area, a few hundred metres from the others, and save a new file after each
  one. Later saves containing earlier scenarios is fine and even useful: the
  tests locate nodes by recipe, they don't assert on the whole file.
- **A shared power grid is fine.** Power is not part of the item graph —
  power lines and poles are ignored entirely by the export.
- **Nothing needs to run.** The export reads recipes and machine counts, not
  throughput, so belts don't need items on them and machines don't need to
  have produced anything. **The one exception is save 6**, which needs stock
  sitting in the station inventory.
- **Every machine must have its recipe set.** A machine with no recipe is
  invisible to the export by design.
- **Chain splitters and mergers freely.** A splitter has only 3 outputs, so
  anything wider needs a tree — that is fine and expected. Contracting such
  trees is the entire job of the export's first stage: 3 chained splitters
  and 1 splitter are the same single transport network to it. What matters is
  only that the tree stays *one connected run of belts*, never that it is one
  building. The realistic case is the one worth testing.
- **Don't let an output belt loop back into its own input belt.** That fuses
  the two sides into a single network and changes the expected result. Where a
  scenario says "one splitter tree in, one merger tree out", those two runs
  must not touch each other anywhere except through the machines.
- **Belts may dangle at both ends.** A belt run with nothing feeding it is
  still a transport network, so the machines on it still key correctly and
  their `Max` / `ClockSpeed` / `ProductionShards` still assert. The node just
  comes out with no input edge, which Modeler reads as "supplied externally" —
  the normal way you would hand-author it.
- **A storage container is *not* a source or a sink.** Containers are
  logistics: they get contracted **into** the belt network exactly like a
  splitter. Capping a dangling belt with an empty container therefore changes
  nothing at all. If you want real edges at the ends, use a **Smelter or
  miner** on the input (producers are what put items on a network) and an
  **AWESOME Sink** on the output (that one *is* a boundary consumer).
- **Be consistent within a group of machines meant to merge.** Either all of
  them have an output belt or none do. If some do and some don't, their
  output-network sets differ and they split into separate nodes — failing the
  test for a reason unrelated to what it is testing.
- **Drop the files in `map/uploads/`** (gitignored, same place
  `tools/fetch_test_saves.py` puts the existing corpus). Name them exactly as
  given below so the tests can find them.

Priority if you only want to build some: **3, then 1, then 2.** Those three
cover the assertions I cannot make any other way.

---

## 1. `modeler_sushi.sav` — mixed bus, per-item routing

**The single hardest thing in the design.** Several item types share one belt
network, and each consumer must be connected to *only* the items its recipe
actually uses.

Build, all on one belt network:

| # | Building | Recipe | Notes |
|---|---|---|---|
| 1 | Miner (any Mk) | — | on an iron node |
| 1 | Smelter | Iron Ingot | fed by the miner |
| 1 | Constructor **A** | Iron Plate | fed from a splitter off the ingot belt |
| 1 | Constructor **B** | Iron Rod | fed from the same splitter |
| 1 | Constructor **C** | Screw | fed **from the bus** |
| 1 | Assembler **D** | Reinforced Iron Plate | fed **from the bus** |

Topology — this part matters more than the buildings:

- Outputs of **A**, **B** and **C** all merge onto **one shared belt** (the
  "bus"), via mergers.
- **C** and **D** both take their input **from that same bus**, via splitters.
- Put a **Smart Splitter** somewhere on the bus, filtered to Iron Rod on one
  output. (Phase 1 ignores filters — the recipes alone already disambiguate
  here — but it pins the topology for the Phase 2 filter work.)

So the bus carries **Iron Plate + Iron Rod + Screw**, and both consumers draw
from it.

**What the test asserts.** Edges `Iron Rod → Screw`, `Iron Plate → Reinforced
Iron Plate`, `Screw → Reinforced Iron Plate` — and, critically, that there is
**no** `Iron Rod → Reinforced Iron Plate` edge and **no** `Iron Plate → Screw`
edge. That negative is the whole point: it separates correct per-item routing
from naively wiring everything on a bus to everything else.

---

## 2. `modeler_independent.sav` — same recipe, two separate lines

Proves two unconnected lines producing the same thing stay two nodes.

- **Line A:** its own miner → its own smelter → **2** Constructors on Iron
  Plate → output into an AWESOME Sink (or a container).
- **Line B:** its own miner → its own smelter → **6** Constructors on Iron
  Plate → its own sink.

**No belt or pipe may connect A to B anywhere.** Build them well apart.
Different counts (2 vs 6) on purpose, so the two nodes are distinguishable.

**What the test asserts.** Exactly **two** Iron Plate nodes, one `Max "2"` and
one `Max "6"` — not a single `Max "8"`.

---

## 3. `modeler_clocks.sav` — clock and somersloop splitting

Highest-value assertion, and the most mechanical to build. **One** input
splitter tree and **one** output merger tree, with every machine between them,
so all of them are genuinely interchangeable and can only be split apart by
clock/sloop. Six machines needs several chained splitters and mergers — that
is fine (see the ground rules); the requirement is that the input side is one
connected belt run and the output side is another, not that either is one
building.

Feed 6 Constructors all on **Iron Plate** from one ingot supply, all merging
to one output belt:

| Count | Clock | Somersloops |
|---|---|---|
| 3 | 100 % | 0 |
| 2 | any value ≠ 100 %, e.g. 250 % | 0 |
| 1 | 100 % | **1** (a Constructor has exactly 1 slot) |

Then, separately in the same area, **1 Assembler on Reinforced Iron Plate with
exactly ONE somersloop installed** (an Assembler has 2 slots, so this is a
half-boost). This one matters: it distinguishes reading the physical sloop
*count* from misreading the boost *multiplier*.

**What the test asserts.** Exactly three Iron Plate nodes — `Max "3"`,
`Max "2"` + the clock you set, and `Max "1"` + `ProductionShards 1` — plus the
Assembler node carrying `ProductionShards 1`, not 2.

---

## 4. `modeler_loop.sav` — a byproduct loop

Tests cycle-breaking in the layout and that a loop still balances.

Cheapest possible loop in the game:

- 1 Water Extractor → pipe → **Packager** set to *Packaged Water*
- that Packager's output → belt → **Packager** set to *Unpackage Water*
- the unpackager's water output → pipe → **back into the first Packager**
- the empty canisters likewise loop back

If you already have a Recycled Plastic / Recycled Rubber or uranium-waste loop
running somewhere, that works just as well — say which and I'll assert on that
instead.

**What the test asserts.** The layout terminates (no infinite loop), both
nodes exist, and edges run in both directions between them.

---

## 5. `modeler_fluids.sav` — pipe networks and m³/min

Fluids are a separate code path from belts: pipe networks contract
differently and rates are stored ×1000 in the game data. This is also the
exact shape that just exposed a bug — 4 870 water pumps were being orphaned
because their only port is named `.FGPipeConnectionFactory` with no
Input/Output in the name.

- **2** Water Extractors → pipes joined through at least **one Pipeline
  Junction** → **2** Packagers on *Packaged Water*.
- Include a pipeline pump somewhere in the run if convenient (it should be
  contracted away like any other pipe part).

**What the test asserts.** One Water resource node whose `Max` is the two
extractors' combined m³/min, the whole pipe run contracted to a single
network, and the two Packagers merged into one node with `Max "2"`.

---

## 6. `modeler_vehicle.sav` — a factory fed only by vehicle

Trains, drones and trucks are deliberately **not** traversed, so a factory fed
only that way would otherwise have no producer and Modeler would solve the
whole chain to zero. This checks the storage stub that prevents it.

- A **Truck Station** (or freight platform) set to *unload*, with **iron ore
  left sitting in its inventory** — this is the one save where stock matters,
  because the station's contents are the only clue to which item it handles.
- Its output belt → Smelter (Iron Ingot) → Constructor (Iron Plate) → sink.
- **No belt or pipe may connect this factory to any producer.**

**What the test asserts.** A `Storage Container` stub node with
`Capacity "Full"` supplying Iron Ore, and the smelter/constructor chain
solving to non-zero rather than going all-zeros.

---

## When they're built

Tell me they're in `map/uploads/` and I'll write the assertions. For each one
I'll also generate the `.sfmd` and hand it back — open it in Modeler and
confirm it matches what you built. Once you've confirmed it, that file gets
committed as the golden output and any future change that alters it has to
justify itself.
