// SaveSource: load a save that lives on the machine running the game, without
// asking that machine to host anything. The transports are the ones the gaming
// PC already speaks -- its OpenSSH server (ssh/sftp, plus rsync when it has it)
// and any share the OS has mounted -- and both are reached through the Tauri
// commands in rust_parser/tauri/src/ssh_source.rs.
//
// Three jobs, one dialog:
//   - validate: say what the connection can actually do (keys, SFTP subsystem,
//     rsync) before the user commits to a path;
//   - browse: list the remote folder and pick the .sav, the way the local file
//     dialog works;
//   - watch: notice when the save changes and reload the map, so a game
//     autosaving on another machine keeps this map current. The change check is
//     a listing (name/size/timestamp), and the downloaded bytes are hashed
//     before reloading, so a coarse timestamp can cost a transfer but never a
//     wrong reload.
//
// Desktop only: every command here is Tauri. The browser build never calls
// init(), and nothing in this file runs without window.__TAURI__.

window.SaveSource = (function() {
  "use strict";

  var KEY = "smap.saveSource";
  // The game's own save location, per platform -- prefilled after a successful
  // validate purely as a convenience; browsing corrects it in one click.
  var SAVE_DIR = {
    windows: "AppData/Local/FactoryGame/Saved/SaveGames",
    linux: ".steam/steam/steamapps/compatdata/526870/pfx/drive_c/users/steamuser/AppData/Local/FactoryGame/Saved/SaveGames",
  };

  var tauriInvoke = null; // window.__TAURI__.core.invoke, set by init
  var options = {};
  var timer = null;
  var el = {}; // cached elements

  var state = {
    transport: "ssh", // "ssh" | "local"
    target: { host: "", user: "", port: 0, identity: "", acceptNewHostKey: false },
    dir: "",
    file: "",
    takeNewest: true,
    watch: false,
    interval: 60,
    capabilities: null, // last validate result
    seen: null, // "name|size|mtime" of the save we last loaded from
    sha: null, // hash of the bytes we last loaded (null = not loaded yet)
    loadedPath: "",
  };

  // ---- Small helpers -------------------------------------------------------

  function remember() {
    try {
      window.localStorage.setItem(KEY, JSON.stringify({
        transport: state.transport,
        target: state.target,
        dir: state.dir,
        file: state.file,
        takeNewest: state.takeNewest,
        watch: state.watch,
        interval: state.interval,
      }));
    } catch (e) { /* storage blocked: the dialog still works, it just forgets */ }
  }

  function recall() {
    try {
      var saved = JSON.parse(window.localStorage.getItem(KEY) || "null");
      if (!saved || typeof saved !== "object") {
        return;
      }
      state.transport = saved.transport === "local" ? "local" : "ssh";
      state.target = Object.assign(state.target, saved.target || {});
      state.dir = saved.dir || "";
      state.file = saved.file || "";
      state.takeNewest = saved.takeNewest !== false;
      state.watch = !!saved.watch;
      state.interval = parseInt(saved.interval, 10) || 60;
    } catch (e) { /* corrupt or unreadable: start from defaults */ }
  }

  function say(text) {
    if (el.status) {
      el.status.textContent = text;
    }
  }

  function bytes(n) {
    if (!(n > 0)) {
      return "";
    }
    var units = ["B", "KB", "MB", "GB"];
    var at = 0;
    while (n >= 1024 && at < units.length - 1) {
      n /= 1024;
      at++;
    }
    return (at === 0 ? n : n.toFixed(1)) + " " + units[at];
  }

  function clock() {
    return new Date().toTimeString().slice(0, 8);
  }

  function when(seconds) {
    if (!seconds) {
      return "";
    }
    var days = Math.round((Date.now() / 1000 - seconds) / 86400);
    if (days === 0) {
      return "today";
    }
    return days === 1 ? "yesterday" : days + " days ago";
  }

  function isSave(name) {
    return /\.sav$/i.test(name);
  }

  function invoke(command, args) {
    return tauriInvoke(command, args || {});
  }

  function message(error) {
    return String((error && error.message) || error);
  }

  // ---- The dialog ----------------------------------------------------------

  function init(bridge) {
    options = (bridge && bridge.options) || {};
    tauriInvoke = (bridge && bridge.invoke) || (window.__TAURI__ && window.__TAURI__.core.invoke);
    if (!tauriInvoke) {
      return;
    }
    el = {
      trigger: document.getElementById("saveSourceBtn"),
      dialogApi: UI.dialog("saveSourceModal"),
      host: document.getElementById("srcHost"),
      user: document.getElementById("srcUser"),
      port: document.getElementById("srcPort"),
      identity: document.getElementById("srcIdentity"),
      acceptNew: document.getElementById("srcAcceptNew"),
      sshFields: document.getElementById("srcSshFields"),
      localFields: document.getElementById("srcLocalFields"),
      localPath: document.getElementById("srcLocalPath"),
      validate: document.getElementById("srcValidate"),
      checks: document.getElementById("srcChecks"),
      path: document.getElementById("srcPath"),
      up: document.getElementById("srcUp"),
      go: document.getElementById("srcGo"),
      list: document.getElementById("srcList"),
      picked: document.getElementById("srcPicked"),
      newest: document.getElementById("srcNewestBox"),
      watch: document.getElementById("srcWatchBox"),
      watchNote: document.getElementById("srcWatchNote"),
      interval: document.getElementById("srcInterval"),
      load: document.getElementById("srcLoad"),
      status: document.getElementById("srcStatus"),
    };
    if (!el.dialogApi || !el.list || !el.sshFields || !el.localFields) {
      return; // markup missing (older dist): stay inert rather than throw
    }
    if (el.dialogApi.el.dataset.saveSourceWired) {
      return; // a second init would double every click handler and every fetch
    }
    recall();
    fillForm();
    wire();
    syncWatchNote();
    el.dialogApi.el.dataset.saveSourceWired = "1";
    if (el.trigger) {
      el.trigger.style.display = "";
      el.trigger.addEventListener("click", open);
    }
    // The source is remembered, so the checkbox survives a restart -- but the
    // poll only starts once a save has been loaded from it in this session
    // (see arm()), or it would download and load a save nobody asked for.
    if (state.watch) {
      say("Watching " + (state.file || state.dir) + " every " + state.interval + "s once a save is loaded.");
    }
  }

  function open() {
    fillForm();
    renderCapabilities(state.capabilities);
    el.dialogApi.open();
    if (state.dir) {
      browse(state.dir);
    } else if (el.path && !el.path.value) {
      el.path.focus();
    }
  }

  function fillForm() {
    var t = state.target;
    if (el.host) {
      el.host.value = t.host || "";
      el.user.value = t.user || "";
      el.port.value = t.port || "";
      el.identity.value = t.identity || "";
      el.acceptNew.checked = !!t.acceptNewHostKey;
    }
    if (el.localPath) {
      el.localPath.value = state.dir || "";
    }
    if (el.path) {
      el.path.value = state.dir || "";
    }
    if (el.newest) {
      el.newest.checked = state.takeNewest;
    }
    if (el.watch) {
      el.watch.checked = state.watch;
    }
    if (el.interval) {
      el.interval.value = String(state.interval);
    }
    syncTransport();
    renderPicked();
  }

  function readForm() {
    state.target = {
      host: el.host.value.trim(),
      user: el.user.value.trim(),
      port: parseInt(el.port.value, 10) || 0,
      identity: el.identity.value.trim(),
      acceptNewHostKey: el.acceptNew.checked,
    };
    state.takeNewest = el.newest.checked;
    state.watch = el.watch.checked;
    state.interval = parseInt(el.interval.value, 10) || 60;
    remember();
  }

  function syncTransport() {
    var local = state.transport === "local";
    el.sshFields.style.display = local ? "none" : "";
    el.localFields.style.display = local ? "" : "none";
    document.querySelectorAll("#srcTransportTabs button").forEach(function(button) {
      var on = button.dataset.transport === state.transport;
      button.classList.toggle("is-on", on);
      button.setAttribute("aria-pressed", String(on));
    });
  }

  // Whether watching is even possible: SFTP is what gives us a cheap listing
  // to compare, so a server without the subsystem gets a manual path and a
  // manual reload instead of a checkbox that can never fire. Before the first
  // validate we do not know, and a remembered source that worked yesterday is
  // allowed to try again.
  function watchPossible() {
    return state.transport === "local" || !state.capabilities || !!state.capabilities.sftp;
  }

  function syncWatchNote() {
    if (!el.watchNote) {
      return;
    }
    if (watchPossible()) {
      el.watch.checked = state.watch;
      el.watch.disabled = false;
      el.watchNote.textContent = "";
      return;
    }
    el.watch.checked = false;
    el.watch.disabled = true;
    el.watchNote.textContent = "This server has no SFTP subsystem, so the folder cannot be listed or watched. Type the save's path and load it by hand.";
  }

  function wire() {
    document.querySelectorAll("#srcTransportTabs button").forEach(function(button) {
      button.addEventListener("click", function() {
        state.transport = button.dataset.transport;
        state.capabilities = null;
        state.dir = state.transport === "local"
          ? (el.localPath.value.trim() || state.dir)
          : state.dir;
        el.path.value = state.dir;
        renderCapabilities(null);
        syncTransport();
        syncWatchNote();
        remember();
      });
    });
    el.validate.addEventListener("click", validate);
    el.go.addEventListener("click", function() { browse(el.path.value.trim()); });
    el.path.addEventListener("keydown", function(e) {
      if (e.key === "Enter") {
        e.preventDefault();
        browse(el.path.value.trim());
      }
    });
    el.up.addEventListener("click", function() {
      if (state.parent) {
        browse(state.parent);
      }
    });
    ["host", "user", "port", "identity"].forEach(function(name) {
      el[name].addEventListener("change", readForm);
    });
    el.acceptNew.addEventListener("change", readForm);
    el.newest.addEventListener("change", function() {
      readForm();
      renderPicked();
    });
    el.watch.addEventListener("change", function() {
      readForm();
      schedule();
      say(state.watch ? "Watching -- every " + state.interval + "s once a save is loaded."
        : "Watching off.");
    });
    el.interval.addEventListener("change", function() {
      readForm();
      schedule();
    });
    el.load.addEventListener("click", function() { load(true); });
    el.dialogApi.onClose(function() {
      if (!state.watch) {
        stop();
      }
    });
  }

  // ---- Validate ------------------------------------------------------------

  function validate() {
    readForm();
    if (state.transport === "ssh" && !state.target.host) {
      say("Enter the host name or IP of the machine running the game.");
      el.host.focus();
      return;
    }
    el.validate.disabled = true;
    renderCapabilities(null);
    say("Validating " + (state.transport === "local" ? state.dir : state.target.host) + "…");
    invoke("source_validate", {
      target: state.target,
      dir: state.transport === "ssh" ? el.path.value.trim() : "",
    }).then(function(result) {
      el.validate.disabled = false;
      state.capabilities = result;
      renderCapabilities(result);
      syncWatchNote();
      if (!result.ok) {
        say(result.detail || "That connection did not work.");
        return;
      }
      say(result.detail);
      // Land the browser somewhere useful: the folder the caller gave, else the
      // game's usual save location under this user's home.
      if (!el.path.value.trim() && state.transport === "ssh") {
        el.path.value = suggestDir(result);
        browse(el.path.value);
      }
    }).catch(function(error) {
      el.validate.disabled = false;
      say("Validation failed: " + message(error));
    });
  }

  function suggestDir(result) {
    var home = (result.home || "").replace(/[\\/]+$/, "");
    var user = state.target.user || "";
    if (result.platform === "Windows") {
      return home + "/" + SAVE_DIR.windows;
    }
    if (home) {
      return home + "/" + SAVE_DIR.linux;
    }
    return "";
  }

  function renderCapabilities(result) {
    el.checks.innerHTML = "";
    if (!result) {
      return;
    }
    // Three states, because "no rsync on the remote" is not a failure: the
    // connection is fine and SFTP carries the transfer. Red is reserved for the
    // one thing browsing and watching actually depend on.
    var rows = [
      [result.ok ? "ok" : "bad", "SSH login", result.detail || "no answer"],
      [result.sftp ? "ok" : "bad", "SFTP subsystem", result.sftp
        ? "browsing and change detection available (" + (result.platform || "unknown")
          + (result.home ? ", " + result.home : "") + ")"
        : "not available -- type the save's path by hand"],
      [result.rsync ? "ok" : "off", "rsync on the remote", result.rsync
        ? "available (delta transfers, less to re-fetch)"
        : "not installed there -- SFTP is used instead"],
    ];
    if (result.saveCount || result.newestSave) {
      rows.push(["ok", "Saves in that folder",
        result.saveCount + " .sav file" + (result.saveCount === 1 ? "" : "s")
        + (result.newestSave ? ", newest " + result.newestSave : "")]);
    }
    rows.forEach(function(row) {
      var line = document.createElement("div");
      line.className = "srcCheck is-" + row[0];
      var mark = document.createElement("span");
      mark.className = "srcCheckMark";
      mark.textContent = row[0] === "ok" ? "✓" : (row[0] === "bad" ? "✕" : "·");
      var name = document.createElement("span");
      name.className = "srcCheckName";
      name.textContent = row[1];
      var detail = document.createElement("span");
      detail.className = "srcCheckDetail";
      detail.textContent = row[2] || "";
      line.appendChild(mark);
      line.appendChild(name);
      line.appendChild(detail);
      el.checks.appendChild(line);
    });
  }

  // ---- Browse --------------------------------------------------------------

  function browse(path, quiet) {
    if (!path) {
      say("Enter a folder to open.");
      return Promise.resolve();
    }
    if (state.transport === "ssh" && !state.target.host) {
      say("Set the host first (and press Validate).");
      return Promise.resolve();
    }
    state.dir = path;
    el.path.value = path;
    if (state.transport === "local") {
      el.localPath.value = path;
    }
    if (!quiet) {
      el.list.innerHTML = "";
      var loading = document.createElement("div");
      loading.className = "srcEmpty";
      loading.textContent = "Listing " + path + "…";
      el.list.appendChild(loading);
      say("Listing " + path + "…");
    }
    return invoke("source_browse", {
      target: state.target,
      path: path,
      local: state.transport === "local",
    }).then(function(listing) {
      renderListing(listing, quiet);
      remember();
    }).catch(function(error) {
      if (!quiet) {
        el.list.innerHTML = "";
        var failed = document.createElement("div");
        failed.className = "srcEmpty";
        failed.textContent = message(error);
        el.list.appendChild(failed);
      }
      throw error;
    });
  }

  function renderListing(listing, quiet) {
    state.parent = listing.parent;
    state.entries = listing.entries || [];
    el.list.innerHTML = "";
    if (state.entries.length === 0) {
      var empty = document.createElement("div");
      empty.className = "srcEmpty";
      empty.textContent = "Nothing in " + listing.path + ".";
      el.list.appendChild(empty);
    }
    state.entries.forEach(function(entry) {
      // A button, so the whole list is reachable by keyboard and Enter works
      // the way a file dialog's does.
      var row = document.createElement("button");
      row.type = "button";
      row.className = "row row-clickable srcRow" + (entry.isDir ? " is-dir" : "")
        + (isSave(entry.name) ? " is-sav" : "");
      var icon = document.createElement("span");
      icon.className = "srcRowIcon";
      icon.textContent = entry.isDir ? "▸" : (isSave(entry.name) ? "●" : "·");
      var label = document.createElement("span");
      label.className = "row-label";
      // Remote names are untrusted data: textContent, never innerHTML.
      label.textContent = entry.name;
      var meta = document.createElement("span");
      meta.className = "row-meta";
      meta.textContent = entry.isDir ? "folder" : bytes(entry.size) + " · " + when(entry.mtime);
      row.appendChild(icon);
      row.appendChild(label);
      row.appendChild(meta);
      if (state.file === entry.name && !entry.isDir) {
        row.classList.add("row-active");
      }
      row.addEventListener("click", function() {
        if (entry.isDir) {
          browse(entry.path);
          return;
        }
        if (!isSave(entry.name)) {
          say(entry.name + " is not a .sav file.");
          return;
        }
        state.file = entry.name;
        state.filePath = entry.path;
        // Picking one file means watching that file, not the newest in the
        // folder -- otherwise the pick would be overwritten by the next tick.
        el.newest.checked = false;
        readForm();
        remember();
        renderListing(listing);
        renderPicked();
      });
      row.addEventListener("keydown", function(e) {
        if (e.key === "Enter" || e.key === " ") {
          e.preventDefault();
          row.click();
        }
      });
      el.list.appendChild(row);
    });
    if (!quiet) {
      var count = state.entries.filter(function(entry) {
        return !entry.isDir && isSave(entry.name);
      }).length;
      say(count + " .sav file" + (count === 1 ? "" : "s") + " in " + listing.path + ".");
    }
    if (state.file && !el.newest.checked) {
      // The picked file may be gone; the folder listing is the truth.
      var stillThere = state.entries.some(function(entry) {
        return entry.name === state.file && !entry.isDir;
      });
      if (!stillThere) {
        state.file = "";
        state.filePath = "";
      }
    }
    renderPicked();
  }

  function renderPicked() {
    if (!el.picked) {
      return;
    }
    var chosen = state.takeNewest
      ? "newest .sav in the folder"
      : (state.file || "nothing picked yet -- click a .sav above");
    el.picked.textContent = "Will load: " + chosen;
    el.load.disabled = !(state.takeNewest ? state.dir : state.filePath);
  }

  // ---- Load and watch ------------------------------------------------------

  // The one place a save is actually pulled and handed to the map.
  //
  // `manual` means the user pressed the button: always load, and leave the
  // watch baseline unset so the next tick re-establishes it (that tick may
  // fetch once more, but the hash comparison then keeps the map as it is).
  // A watch tick passes the stat key it saw, so a folder that has not changed
  // again stays quiet.
  function load(manual, seenKey) {
    if (state.transport === "ssh" && !state.target.host) {
      say("Set the host and validate the connection first.");
      return Promise.resolve();
    }
    // Re-read the folder first: "newest" only exists in a listing, and the one
    // on screen may be minutes old -- a tick that just noticed a new autosave
    // has to see it, or it would load the file it already had.
    return browse(state.dir, true).then(function() {
      var entry = currentTarget();
      if (!entry) {
        say(state.takeNewest
          ? "No .sav file in " + state.dir + "."
          : "Pick a .sav file to load" + (state.file ? " (" + state.file + " is gone)" : "") + ".");
        return null;
      }
      state.file = entry.name;
      state.filePath = entry.path;
      remember();
      if (state.transport === "local") {
        // A mounted file is read in place, and it carries its own timestamp,
        // so the baseline is exact and the next tick has nothing to confirm.
        state.sha = null;
        state.seen = seenKey || statKey(entry);
        return options.loadPath(entry.path).then(function() {
          say("Loaded " + entry.name + " (" + bytes(entry.size) + ") at " + clock() + ".");
          arm();
        }).catch(function(error) {
          say("Load failed: " + message(error));
        });
      }
      // rsync only when the far side actually has it (validate says so); SFTP is
      // the transport that works against a stock Windows OpenSSH server.
      var transport = state.capabilities && state.capabilities.rsync ? "rsync" : "sftp";
      say("Fetching " + entry.name + " over " + transport + "\u2026");
      return invoke("source_fetch", {
        target: state.target,
        remotePath: entry.path,
        transport: transport,
      }).then(function(fetched) {
        if (!manual && fetched.sha256 === state.sha) {
          state.seen = seenKey || null;
          say("Auto-refresh " + clock() + ": " + fetched.saveName + " re-fetched, bytes unchanged.");
          return null;
        }
        state.sha = fetched.sha256;
        state.seen = manual ? null : (seenKey || null);
        state.loadedPath = fetched.path;
        return options.loadPath(fetched.path).then(function() {
          say("Loaded " + fetched.saveName + " (" + bytes(fetched.bytes) + ") over " + transport + " at " + clock() + ".");
          arm();
        });
      }).catch(function(error) {
        say((manual ? "Fetch failed: " : "Auto-refresh " + clock() + ": ") + message(error));
      });
    });
  }

  // The picked file, as the last listing described it.
  function localEntry(name) {
    return (state.entries || []).filter(function(entry) {
      return !entry.isDir && entry.name === name;
    })[0] || null;
  }

  // What "load" means right now: the newest .sav in the folder, or the one the
  // user picked. The listing is the only source for both.
  function currentTarget() {
    if (!state.takeNewest) {
      return localEntry(state.file);
    }
    var saves = (state.entries || []).filter(function(entry) {
      return !entry.isDir && isSave(entry.name);
    });
    saves.sort(function(a, b) {
      return (b.mtime || 0) - (a.mtime || 0);
    });
    return saves[0] || null;
  }

  // The identity of "the file we would load": the three things a listing gives
  // us, and the only things a change can move.
  function statKey(stat) {
    if (!stat || !stat.name) {
      return null;
    }
    return stat.name + "|" + stat.size + "|" + stat.mtime;
  }

  // One watch tick: re-read the folder and go further only when the file we
  // would load has changed. Paused while the tab is hidden, while a parse is
  // running, and while the editor holds pending edits -- a reload discards
  // those, so the user finishes first and polling resumes on its own.
  function tick() {
    if (!state.watch || document.hidden || options.isBusy()) {
      return;
    }
    var pending = options.pendingEdits();
    if (pending > 0) {
      say("Auto-refresh paused: " + pending + " edit(s) pending -- undo or export them and it resumes.");
      return;
    }
    if (!state.dir) {
      return;
    }
    browse(state.dir, true).then(function() {
      var entry = currentTarget();
      if (!entry) {
        say("Auto-refresh " + clock() + ": " + (state.takeNewest
          ? "no .sav in " + state.dir + " yet"
          : state.file + " is no longer in " + state.dir
            + " -- tick \"always take the newest\" to follow the autosaves"));
        return null;
      }
      var key = statKey(entry);
      if (key === state.seen) {
        say("Auto-refresh " + clock() + ": " + entry.name + " unchanged.");
        return null;
      }
      // Changed (or first look): fetch, hash, and let load() decide whether the
      // map actually needs rebuilding.
      say("Auto-refresh " + clock() + ": " + entry.name + " changed, fetching\u2026");
      return load(false, key);
    }).catch(function(error) {
      // Machine asleep, sshd stopped, share unmounted: say so and keep trying.
      say("Auto-refresh " + clock() + ": " + message(error) + " (will retry).");
    });
  }

  // The poll starts on the first load from this source, and every load restarts
  // the clock -- so a save loaded by hand is not re-fetched a moment later.
  function arm() {
    if (state.watch) {
      schedule();
    }
  }

  function schedule() {
    stop();
    if (!state.watch || !watchPossible()) {
      return;
    }
    timer = window.setInterval(tick, Math.max(5, state.interval) * 1000);
  }

  function stop() {
    if (timer !== null) {
      window.clearInterval(timer);
      timer = null;
    }
  }

  return {
    init: function(bridge) {
      try {
        init(bridge);
      } catch (error) {
        // A missing dialog or a broken bridge must not take the load flow down.
        console.warn("SaveSource: not available:", error);
      }
    },
    open: open,
    state: state,
  };
})();
