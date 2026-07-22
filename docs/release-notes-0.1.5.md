# v0.1.5

A large maintenance release from a full-codebase review: security hardening,
a much smaller/faster map payload, parser robustness, and a batch of editor
and UI fixes.

## Security
- **Dedicated-server fetch now pins the server's TLS certificate** on the
  first successful login (trust-on-first-use). A changed certificate is
  reported and requires explicit confirmation before trusting it, instead of
  silently accepting any certificate.
- **The remembered admin password moved to the Windows Credential Manager**
  (from plaintext local storage). An existing saved password is migrated
  automatically on first launch.
- The in-app webview now runs under a strict Content-Security-Policy.

## Performance
- **The map payload is ~40% smaller and loads faster**, which also cuts peak
  memory when loading large saves (about 1.8 GB less on a 50 MB save).
- Parser and map-data build are faster and use less memory on big saves.
- Very large saves (over ~2 GB decompressed) now show a clear "use the
  desktop app" message in the browser instead of failing cryptically — and
  the desktop app loads them.

## Robustness
- Corrupt or truncated save files now fail with a clear error instead of
  crashing.
- Resource nodes not in the bundled data tables are still shown on the map
  (under their own type name) instead of silently disappearing.

## Editor & UI fixes
- More reliable save editing: stricter internal validation before writing,
  and a fix for a rare edge case when duplicating signs.
- Isolating a train on the map no longer misplaces its markers.
- Item/building searches no longer show stale results when you search again
  quickly or load a different save mid-search.
- Escape now closes one thing at a time (a modal, a placement, a highlight)
  instead of several at once.
- Altitude filter "Reset" no longer re-applies an old range to the next save.

## Checksums (SHA-256)

```
{CHECKSUMS}
```
