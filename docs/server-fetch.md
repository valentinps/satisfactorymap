# Fetch the newest save from a dedicated server (desktop app)

The desktop app can log into a Satisfactory dedicated server and load its
most recent save in one click — no SFTP, no manual copying out of the
server's save folder. It uses the **official dedicated-server HTTPS API**,
the same one the in-game Server Manager talks to, documented by Coffee
Stain in `CommunityResources/DedicatedServerAPIDocs.md` inside every
server install. Nothing about the game was reverse-engineered.

## Using it

In the Save File panel, expand **From dedicated server** and enter:

- **Server address** — a bare host or IP means the default port 7777;
  `host:port` overrides it (bracketed IPv6 like `[::1]:7777` works too;
  any `https://` prefix a user pastes is tolerated and stripped).
- **Admin password** — the server's *administrator* password (the one the
  Server Manager asks for), not the optional join password. Every API call
  the fetch needs requires Administrator privilege.
- **Remember password** — optional. The address always persists across
  sessions; the password persists only while this box is ticked, stored
  unencrypted in the app's local storage (same trust level as a config
  file next to the app). Unticking it forgets the stored copy immediately.

**Fetch latest save** then logs in, lists every session's saves, picks the
newest by save timestamp, downloads it, and loads it through the normal
load path. The `.sav` also stays on disk in
`%LOCALAPPDATA%\com.satisfactorymap.desktop\server-saves\`, so it can be
reopened later or kept as a backup.

## The firewall gotcha: TCP, not just UDP

Gameplay traffic uses **UDP** 7777, but the HTTPS API listens on **TCP**
7777 (same port number, different protocol — the server multiplexes
them). A server that works fine for playing can still refuse the fetch if
the host's firewall only forwards UDP. If the status line says the server
can't be reached while people are happily playing on it, open TCP 7777
(or the custom port) as well — e.g. a cloud-provider firewall rule plus
`ufw allow 7777/tcp` on the box itself.

This is also why the feature is desktop-only: a browser page can't call
the API at all (self-signed certificate + no CORS headers), while the
desktop app does the whole exchange natively in Rust.

## How it works

Everything lives in `rust_parser/tauri/src/server_api.rs` (tauri-free,
unit-tested part of `sav_tauri_lib`) plus the `server_fetch_latest`
command in `main.rs` and the form wiring in `map/static/map/data.js`.

The API is JSON-over-HTTPS: `POST https://host:port/api/v1` with
`{"function": ..., "data": {...}}`. The fetch is three calls:

1. `PasswordLogin` (`MinimumPrivilegeLevel: Administrator`) → bearer token;
2. `EnumerateSessions` → every session with its save headers; the newest
   save is picked by `SaveDateTime` across all sessions (UE's zero-padded
   `YYYY.MM.DD-HH.MM.SS`, so lexicographic order is chronological);
3. `DownloadSaveGame` → the raw `.sav` bytes (an error instead arrives as
   a JSON envelope, detected by content type).

The command writes the bytes into `server-saves/` under the app-data dir
and returns the path; the frontend then reuses the existing path-based
`load`, so nothing downstream of "a .sav exists at this path" changed.

Two protocol quirks worth knowing before extending this:

- **Self-signed TLS by design.** The server generates its own certificate
  on first boot, so the client disables certificate verification
  (`danger_accept_invalid_certs`); the admin password is the actual
  secret gating everything.
- **Inconsistent response-key casing.** The shipped docs say PascalCase
  (`AuthenticationToken`, `Sessions`) but live servers send camelCase for
  at least some fields. Every deserialized field carries serde aliases
  for both — keep doing that for any new field.

## Ideas not implemented (yet)

- Trigger a fresh save first (`SaveGame`, then poll `EnumerateSessions`
  until it appears) instead of taking the latest autosave.
- A session/save picker instead of always-newest.
- Auto-refresh: re-fetch on an interval and reload the map when the
  server has a newer save.
