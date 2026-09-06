# v0.2.1

A fix for saves that mods write their own data into, and a link to the
project's source from the app bar.

## Fixes

- **Saves recorded with FicsItCam load again.** The camera mod parks a
  recording on its own component — a few hundred KB per camera — and the
  loader refused the whole save over it:

  ```
  Found 267135 extra trailing bytes for ComponentHeader /Script/FicsItCam.FICCamera.
  ```

  An object in a save declares its own size, so data written by a mod can be
  stepped over without understanding any of it. Anything a mod puts in its own
  actors and components is now carried through untouched instead of stopping
  the load — including through an edit and re-save, so a camera recording
  survives the save editor intact. Vanilla content is deliberately unchanged:
  if the map ever fails to account for every byte of *that*, it still says so
  rather than quietly skipping it.

  This was never specific to the mod's version or the game's — a save simply
  had to contain a camera recording to hit it. Other mods in the same save
  (DoggoHardHat, SkyUI, UtilityMod, EfficiencyChecker and the rest) were
  already fine and are untouched.

## New

- **A link to the project's GitHub** at the far right of the app bar, set off
  by a hairline from the app's own controls.

## Under the hood

- The hosted site now counts basic usage (page views, saves loaded, tools
  opened) with PostHog. **The desktop app does not** — it is gated off
  explicitly rather than merely blocked, so this build phones home nowhere.
  Nothing about a save is ever sent in either case: no session name, no file
  name, no coordinates, no item names.

## Checksums (SHA-256)

```
{CHECKSUMS}
```
