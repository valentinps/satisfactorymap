# Load the save off the machine that runs the game (desktop app)

The desktop app can load the `.sav` straight off the gaming PC and keep
reloading it as the game autosaves. It does that with what that machine already
has — **its OpenSSH server** — rather than by asking it to host the file:
nothing listens, no port is opened for the app, and the authentication is the
key and `~/.ssh/config` the user already has.

Two transports, in one dialog (**Save File → From another machine**):

| Transport | Needs on the game machine | Gives |
|---|---|---|
| **SSH / SFTP** | OpenSSH server (built into Windows 10/11 and every Linux) | folder browsing, a cheap change check, rsync when it is installed |
| **Mounted share** | SMB/NFS/GVFS share mounted by the OS | the same browsing and change check, no protocol code at all |

## Setting up the game machine (Windows)

Run these in an **elevated PowerShell**. The save folder comes with the server
capability; nothing else is installed.

```powershell
Add-WindowsCapability -Online -Name OpenSSH.Server~~~~0.0.1.0
Set-Service -Name sshd -StartupType Automatic
Start-Service sshd
New-NetFirewallRule -Name sshd -DisplayName 'OpenSSH Server (sshd)' -Enabled True `
  -Direction Inbound -Protocol TCP -Action Allow -LocalPort 22
```

Then, as the account that owns the saves (add your own public key):

```powershell
mkdir -Force $env:USERPROFILE\.ssh | Out-Null
Add-Content $env:USERPROFILE\.ssh\authorized_keys 'ssh-ed25519 AAAA... you@laptop'
icacls $env:USERPROFILE\.ssh\authorized_keys /inheritance:r /grant "$($env:USERNAME):R"
```

The saves are at
`C:\Users\<you>\AppData\Local\FactoryGame\Saved\SaveGames\<steamid64>\`, which
is why the dialog opens on the SFTP form of that path: `/C:/Users/...`. On
Linux the game writes into the Proton prefix — the dialog's suggested path is
the usual one.

An admin password is never asked for and never stored: the dialog sets
`BatchMode=yes`, so authentication is key/agent only by design. A host whose
key is not in `known_hosts` is refused until **Trust this host's key the first
time** is ticked, which adds it once (trust on first use, the same disclosure as
the dedicated-server certificate pin). A *changed* key is still refused.

## What the dialog does

- **Validate connection** answers what the transport can actually do before a
  path is typed: SSH login, whether the SFTP subsystem answers (browsing and
  change detection depend on it), whether the far side has rsync, the platform
  and home directory, and how many `.sav` files are in the folder. A partial
  answer stays partial — a server with no SFTP subsystem is reported as such
  rather than as a failed connection.
- **Save folder** lists the remote directory and walks it (`Up`, folders first,
  `.sav` files marked). Remote names are rendered as text, never as markup.
  With SFTP unavailable the listing is disabled and the path is typed by hand.
- **Always take the newest `.sav`** is the autosave case: the rotation writes a
  new file every few minutes and the map should follow it.
- **Watch it and reload** polls the folder's name/size/timestamp and reloads
  only when the bytes actually changed. While the save is being watched the
  status line says what each tick saw. Reloading is refused while the editor
  holds pending edits (a reload discards them), while a parse is already
  running, and while the window is hidden — polling resumes on its own.

A fetched save lands in the app's data directory under `remote-saves/`, written
to a `.part` file and renamed only when complete, and capped at 8 GiB, so a
wrong path cannot fill the disk and a truncated transfer cannot become the file
the parser reads.

## Running it locally

**The dialog, without building anything** (needs only Python):

```sh
python3 tools/serve_save_source_demo.py --dir /path/to/SaveGames
# then open http://127.0.0.1:8793/ and click Save File -> From another machine
```

That serves `map/static/map/` with a stubbed desktop bridge: the three commands
answer from a real folder, so the listing, its sizes and timestamps, the
"newest .sav" pick and the watch's change check are real (drop a file in the
folder and the next tick notices). Only the SSH transport is canned, and loading
stops at the parser -- `sav_core` is in the desktop build and `dist/pkg` is the
wasm build, neither of which that server has -- so the status line says so
rather than pretending.

**The real thing on Linux** (the app itself, real SSH, real parsing). The Tauri
tree needs the WebKitGTK/GTK development packages, which an immutable distro
does not carry, so build it in a container:

```sh
distrobox create --name satisfactory-map --image registry.fedoraproject.org/fedora:41
distrobox enter satisfactory-map
sudo dnf install -y webkit2gtk4.1-devel gtk3-devel libsoup3-devel librsvg2-devel \
  openssl-devel cairo-devel pango-devel gdk-pixbuf2-devel
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
cd <checkout>/rust_parser && cargo build -p sav_tauri --release
cargo tauri dev            # or: cargo tauri build
```

**The real thing on Windows** is the repo's normal build (`cargo tauri build`)
plus the sshd setup above -- and Windows is where the game machine is, so this
is the path that ends in a working map.

### If SSH is already broken on the machine running the app

The dialog reports the ssh client's own words, and one of them is worth knowing
about: on an ostree/Bluefin-style install, files under `/etc/ssh/ssh_config.d/`
can end up owned by an unmapped uid (`nfsnobody`). OpenSSH treats a non-root
owned included config as fatal, so **every** `ssh` invocation fails with:

```
Bad owner or permissions on /etc/ssh/ssh_config.d/20-systemd-ssh-proxy.conf
```

That is not this app's bug and it breaks `ssh` in a terminal too; fix the
ownership (`sudo chown root:root /etc/ssh/ssh_config.d/*`) and Validate
connection starts answering.

## Implementation notes

- `map/static/map/save_source.js` — the dialog, the browser and the watch loop.
- `rust_parser/tauri/src/ssh_source.rs` — runs `ssh`, `sftp` and `rsync` as
  subprocesses with a deadline, parses their output, and hashes what arrives.
  The app ships no SSH stack: host keys, keys, agents and `ProxyJump` are
  OpenSSH's, so the app never grows a second implementation of any of it.
- Commands: `source_validate`, `source_browse`, `source_fetch` -- the three
  jobs the dialog has. The watch probe is a `source_browse`, because a listing
  is what says both what is in the folder and whether it moved. Every remote
  path crosses into an SFTP batch script, so control characters and quotes are
  rejected at the boundary; a host, user or port that could be read as an
  option is rejected too.
