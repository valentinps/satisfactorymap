# Licensing notices

## License

This project is licensed under **AGPL-3.0** (see [LICENSE](LICENSE)).

## The Rust parser is a port of GreyHak/sat_sav_parse

The save parser (`rust_parser/core`) is a derivative work: a Rust port of
[GreyHak/sat_sav_parse](https://github.com/GreyHak/sat_sav_parse), which is
licensed **GPL-3.0-only**. Credit to GreyHak and sat_sav_parse must stay in
place in this repository, its documentation, and any crate release of the
port.

On 2026-07-20, GreyHak granted written permission (Discord) for this port to
be distributed under **AGPL-3.0**, with the following scope:

- The permission covers **this Rust port specifically** — the derivative
  work ported from the Python reference and distributed as WASM/crate. It is
  *not* a blanket permission for anyone to relicense sat_sav_parse itself,
  which remains GPL-3.0-only.
- It is tied to the project as described to him: a client-side,
  browser-based tool. If the project's nature changes materially, the
  permission must be re-confirmed rather than assumed to carry over.
- Earning money from the port under AGPL-3.0 (sponsorships, commercial or
  alternative licensing) is permitted.
- The permission is **non-transferable**: it may not be handed to another
  party, and the port itself may not be sold outright.
- Publishing the port as its own crate is permitted under the same terms,
  credited to GreyHak and sat_sav_parse as the originating upstream.

A copy of the exchange is retained by the project author.

## Third-party code bundled in this repository

Vendored under `map/static/map/vendor/` and shipped as-is, each under its own
license:

- **Leaflet** (`vendor/leaflet.js`, `vendor/leaflet.css`) -- BSD-2-Clause,
  (c) Volodymyr Agafonkin / CloudMade. <https://leafletjs.com/>
- **posthog-js** 1.427.2 (`vendor/posthog.js`, the upstream
  `dist/array.no-external.js` build) -- MIT and Apache-2.0, (c) PostHog Inc.
  <https://github.com/PostHog/posthog-js>. Loaded only by the hosted site;
  see `analytics.js` for the gate and for what is sent.

## Not licensed by this repository

- The **Satisfactory Save Map** name, logo, and the `satisfactorymap.net`
  domain (see the README's License & trademark section).
- Game-derived data (icons, map image, item/building/world tables): property
  of Coffee Stain Studios. None of it is in this repository -- everything
  under `game_data/generated/` and `map/static/map/icons/` is produced from
  your own copy of the game by `game_data/extract_all.py`, and is gitignored.
  A prebuilt `game_data.zip` of exactly those outputs is attached to a release
  as a convenience for building and for CI; it is the game's data, not this
  project's, and it is offered on the same terms as any other extraction of
  assets you already own. Satisfactory is a trademark of Coffee Stain Studios;
  this project is not affiliated with or endorsed by them.
