// Drives the save-load UI flow: the user picks a local .sav (file dialog or
// drag-drop anywhere on the page), SaveClient parses it in the WASM worker
// (see save_client.js/worker.js), and the payload feeds the same build chain
// as always. The save never leaves the browser.

(function() {
  "use strict";

  var loadStatus = document.getElementById("loadStatus");
  var progressBar = document.getElementById("loadProgressBar");
  var progressFill = document.getElementById("loadProgressFill");
  var gameSettingsPanel = document.getElementById("gameSettingsPanel");

  var loadInFlight = false;

  function setStatus(text) {
    loadStatus.textContent = text;
  }

  function showProgress(phase, percent) {
    progressBar.style.display = "block";
    progressFill.style.width = Math.max(0, Math.min(100, percent)) + "%";
    setStatus((phase || "Working") + "... " + Math.round(percent) + "%");
  }

  function hideProgress() {
    progressBar.style.display = "none";
    progressFill.style.width = "0%";
  }

  // ---- Busy overlay -------------------------------------------------------
  // A full-screen modal spinner + progress bar for save-EDIT operations
  // (copy/paste/delete/undo/redo/export -- editor.js drives it). The thin
  // top progress bar above was easy to miss on big operations, leaving no
  // clear sign that anything was happening. Shown after a short delay so
  // instant edits don't flash it.
  var busyOverlay = document.getElementById("busyOverlay");
  var busyLabel = document.getElementById("busyLabel");
  var busyFill = document.getElementById("busyFill");
  var busyPhase = document.getElementById("busyPhase");
  var BUSY_SHOW_DELAY_MS = 150;
  var busyTimer = null;

  function showBusy(label) {
    busyLabel.textContent = label || "Working…";
    busyPhase.textContent = "";
    busyFill.style.width = "0%";
    if (busyTimer === null && busyOverlay.style.display === "none") {
      busyTimer = setTimeout(function() {
        busyTimer = null;
        busyOverlay.style.display = "flex";
      }, BUSY_SHOW_DELAY_MS);
    }
  }

  function busyProgress(phase, percent) {
    busyFill.style.width = Math.max(0, Math.min(100, percent)) + "%";
    busyPhase.textContent = (phase || "Working") + "… " + Math.round(percent) + "%";
  }

  function hideBusy() {
    if (busyTimer !== null) {
      clearTimeout(busyTimer);
      busyTimer = null;
    }
    busyOverlay.style.display = "none";
  }

  // Load panel: always-on drop zone + hidden file input (the click target).
  var uploadDropZone = document.getElementById("uploadDropZone");
  var uploadDropText = document.getElementById("uploadDropText");
  var uploadFileInput = document.getElementById("uploadFileInput");
  var UPLOAD_DROP_DEFAULT_TEXT = uploadDropText.textContent;

  function resetUploadZone() {
    uploadDropZone.classList.remove("uploading");
    uploadDropText.textContent = UPLOAD_DROP_DEFAULT_TEXT;
  }

  // Game-mode settings (cost multipliers, Purity Modifier, Node
  // Randomization) chosen at world creation -- see
  // collect_game_settings in the Rust parser. These can silently change
  // what every resource node on the map actually is/yields relative to a
  // vanilla world, so they're shown unconditionally rather than only when
  // non-default, in case the displayed value itself is what someone's
  // trying to confirm. (A multiplier left at the default 1x is absent from
  // the save entirely -- null here -- so its row is dropped.)
  function showGameSettings(gameSettings) {
    gameSettingsPanel.innerHTML = "";
    if (!gameSettings || Object.keys(gameSettings).length === 0) {
      gameSettingsPanel.style.display = "none";
      return;
    }
    function multiplier(value) {
      return value !== undefined && value !== null ? value + "x" : null;
    }
    var rows = [
      ["Resource cost", multiplier(gameSettings.resourceCostMultiplier)],
      ["Power cost", multiplier(gameSettings.powerCostMultiplier)],
      ["Space Elevator cost", multiplier(gameSettings.spaceElevatorCostMultiplier)],
      ["Node purity", gameSettings.nodePuritySettings],
      ["Node randomization", gameSettings.nodeRandomization],
    ];
    var hasAny = false;
    rows.forEach(function(pair) {
      if (pair[1] === null || pair[1] === undefined) {
        return;
      }
      hasAny = true;
      var row = document.createElement("div");
      row.className = "gameSettingRow";
      var label = document.createElement("span");
      label.className = "gameSettingLabel";
      label.textContent = pair[0];
      var value = document.createElement("span");
      value.className = "gameSettingValue";
      value.textContent = pair[1];
      row.appendChild(label);
      row.appendChild(value);
      gameSettingsPanel.appendChild(row);
    });
    gameSettingsPanel.style.display = hasAny ? "block" : "none";
  }

  // Buckets are entirely rebuilt by Filters.build on every load (fresh
  // objects, even for the exact same building), so a pinned tooltip/highlight
  // can't just be left alone -- it has to be captured by stable bucket key +
  // id before the rebuild and re-resolved against the new buckets afterward.
  function restorePinnedSelection(selection) {
    var bucket = MapApp.layer.buckets.filter(function(b) { return b.key === selection.bucketKey; })[0];
    if (!bucket || !bucket.ids) {
      return; // That kind of thing no longer exists in the new payload.
    }
    var idx = bucket.ids.indexOf(selection.id);
    if (idx === -1) {
      return; // That specific instance is gone (dismantled/collected/etc).
    }
    var stride = bucket.pointStride;
    var z = bucket.renderType === "line" ? bucket.lines[idx][stride - 1] : bucket.points[idx * stride + stride - 1];
    var hit = { bucket: bucket, id: selection.id, index: idx, z: z };
    Tooltip.pin(selection.clientX, selection.clientY, hit);
    MapApp.setHighlight(bucket, selection.id);
  }

  // The payload -> UI build chain, shared by save loads and save edits (the
  // editor's applyEdits returns a payload of exactly the same shape).
  function applyPayload(payload) {
    Tooltip.hide();
    MapApp.setHighlight(null, null);
    Filters.build(payload);
    Altitude.build(payload);
    FindItem.build(payload);
    Progression.build(payload);
    SelectionTool.reset();
    showGameSettings(payload.gameSettings);
    // Widen the pan limits if this save builds outside the vanilla map
    // (mods allow it); recomputed per payload so an edit that deletes the
    // out-of-map build snaps the limits back too.
    MapApp.refreshMaxBounds();
    clearSaveBtn.style.display = "flex";
  }

  // "Unload" (the eject button next to the Save File header): drop the
  // loaded save entirely -- map, sidebar, editor state, and the parse
  // session's memory (SaveClient.reset terminates the wasm worker; native
  // side frees its session). Pending edits die with the session, so those
  // get a confirmation first; a plain viewed save just unloads.
  var clearSaveBtn = document.getElementById("clearSaveBtn");

  function clearSave() {
    if (loadInFlight) {
      return; // Mid-parse the worker owns the UI; let it finish first.
    }
    var edits = EditorTool.opCount();
    if (edits > 0 && !window.confirm(
        "Unload this save? Your " + edits + " unexported edit" + (edits === 1 ? "" : "s")
        + " will be lost. (Use \"Download save\" first to keep them.)")) {
      return;
    }
    SaveClient.reset();
    MapApp.currentFile = null;
    currentFile = null;
    currentPath = null;
    Tooltip.unpin();
    Tooltip.hide();
    MapApp.setHighlight(null, null);
    SelectionTool.reset();
    EditorTool.onSaveClosed();
    Filters.clear();
    Altitude.clear();
    FindItem.build({});   // empty catalogs; also resets any search highlight state
    Progression.build({}); // no progression data
    showGameSettings(null);
    MapApp.refreshMaxBounds(); // back to the vanilla pan limits
    clearSaveBtn.style.display = "none";
    hideProgress();
    setStatus("Save unloaded -- drop a .sav anywhere or click above.");
  }

  // Editor.js drives the same progress/status UI during applyEdits, and
  // reloads the original file to recover from a lost worker session.
  window.SaveLoadFlow = {
    applyPayload: applyPayload,
    showProgress: showProgress,
    hideProgress: hideProgress,
    setStatus: setStatus,
    showBusy: showBusy,
    busyProgress: busyProgress,
    hideBusy: hideBusy,
    canReload: function() { return currentFile !== null || currentPath !== null; },
    reloadCurrentFile: function() {
      // Desktop (Tauri) reloads by path; the browser re-reads the File.
      return currentPath !== null ? loadLocalPath(currentPath) : loadLocalFile(currentFile);
    },
  };

  // The last successfully picked File, kept so the editor can recover from
  // a lost worker session (out of memory on huge saves) by re-reading it.
  var currentFile = null;
  // Desktop (Tauri) counterpart: the last loaded save's native path, for the
  // same recovery-by-reload flow (there is no File object there).
  var currentPath = null;

  function loadLocalFile(file) {
    if (!file) {
      return Promise.resolve();
    }
    if (!file.name.endsWith(".sav")) {
      setStatus("Only .sav save files can be loaded.");
      return Promise.resolve();
    }
    if (loadInFlight) {
      return Promise.resolve(); // A parse is already running; don't queue a second one.
    }
    loadInFlight = true;
    uploadDropZone.classList.add("uploading");
    uploadDropText.textContent = "Loading " + file.name + "…";
    var pinnedSelection = Tooltip.getPinnedSelection();
    showProgress("Reading file", 0);

    return file.arrayBuffer()
      .then(function(buffer) {
        return SaveClient.loadSave(buffer, function(phase, current, total) {
          var percent = total > 0 ? (current / total) * 100 : 0;
          showProgress(phase, percent);
        });
      })
      .then(function(payload) {
        hideProgress();
        loadInFlight = false;
        resetUploadZone();
        // Stable per-file key: detail features guard on this, and the
        // pinned-tooltip restore survives re-loading the same file.
        MapApp.currentFile = "local:" + file.name + ":" + file.size + ":" + file.lastModified;
        currentFile = file;
        EditorTool.onSaveLoaded(file.name);
        applyPayload(payload);
        if (pinnedSelection) {
          restorePinnedSelection(pinnedSelection);
        }
        setStatus("Loaded: " + payload.sessionName + " (" + payload.saveDatetime + ")");
      })
      .catch(function(error) {
        hideProgress();
        loadInFlight = false;
        resetUploadZone();
        setStatus("Failed to load save: " + (error && error.message || error));
        throw error;
      });
  }

  // Desktop (Tauri) load: sav_core reads the .sav natively from a path, so a
  // big save never crosses the IPC boundary as a buffer. Mirrors loadLocalFile
  // otherwise -- same progress/status UI and recovery bookkeeping.
  function loadLocalPath(path) {
    if (!path) {
      return Promise.resolve();
    }
    if (!/\.sav$/i.test(path)) {
      setStatus("Only .sav save files can be loaded.");
      return Promise.resolve();
    }
    if (loadInFlight) {
      return Promise.resolve();
    }
    loadInFlight = true;
    var name = path.split(/[\\/]/).pop();
    uploadDropZone.classList.add("uploading");
    uploadDropText.textContent = "Loading " + name + "…";
    var pinnedSelection = Tooltip.getPinnedSelection();
    showProgress("Reading file", 0);

    return SaveClient.loadSavePath(path, function(phase, current, total) {
      var percent = total > 0 ? (current / total) * 100 : 0;
      showProgress(phase, percent);
    })
      .then(function(payload) {
        hideProgress();
        loadInFlight = false;
        resetUploadZone();
        MapApp.currentFile = "tauri:" + path;
        currentPath = path;
        currentFile = null;
        EditorTool.onSaveLoaded(name);
        applyPayload(payload);
        if (pinnedSelection) {
          restorePinnedSelection(pinnedSelection);
        }
        setStatus("Loaded: " + payload.sessionName + " (" + payload.saveDatetime + ")");
      })
      .catch(function(error) {
        hideProgress();
        loadInFlight = false;
        resetUploadZone();
        setStatus("Failed to load save: " + (error && error.message || error));
        throw error;
      });
  }

  // Open the native file picker (Tauri dialog plugin) and load the choice.
  // Prefer the injected global binding (withGlobalTauri); fall back to a raw
  // invoke of the plugin command if it isn't present.
  function pickAndLoadTauri() {
    var options = {
      multiple: false,
      directory: false,
      filters: [{ name: "Satisfactory save", extensions: ["sav"] }],
    };
    var tauri = window.__TAURI__;
    var opened = (tauri.dialog && tauri.dialog.open)
      ? tauri.dialog.open(options)
      : tauri.core.invoke("plugin:dialog|open", { options: options });
    return opened.then(function(selected) {
      if (!selected) {
        return;
      }
      var path = typeof selected === "string" ? selected : (selected && selected.path);
      return loadLocalPath(path);
    });
  }

  // ---- Desktop only: fetch the newest save from a dedicated server ---------
  // Small disclosure form under the drop zone (hidden in the browser build).
  // The native side does the whole exchange against the official dedicated-
  // server HTTPS API (server_fetch_latest command: login -> enumerate ->
  // download into the app-data dir) and hands back a path that loads through
  // the normal loadLocalPath flow. The address persists in localStorage (not
  // a secret); the password, when "Remember password" is ticked, lives in the
  // OS credential store via the server_password_* commands -- never in
  // localStorage. (An older build stored it in localStorage; setup migrates
  // and deletes any leftover.)
  var SERVER_HOST_KEY = "smap.serverFetchHost";
  var LEGACY_SERVER_PASS_KEY = "smap.serverFetchPassword";

  function setupServerFetch() {
    var panel = document.getElementById("serverFetchPanel");
    var toggle = document.getElementById("serverFetchToggle");
    var form = document.getElementById("serverFetchForm");
    var hostInput = document.getElementById("serverFetchHost");
    var passInput = document.getElementById("serverFetchPassword");
    var rememberBox = document.getElementById("serverFetchRememberBox");
    var button = document.getElementById("serverFetchButton");
    var invoke = window.__TAURI__.core.invoke;
    panel.style.display = "block";
    try {
      hostInput.value = window.localStorage.getItem(SERVER_HOST_KEY) || "";
    } catch (e) { /* storage blocked: the field just starts empty */ }

    var storedHost = hostInput.value.trim();
    var legacyPass = null;
    try {
      legacyPass = window.localStorage.getItem(LEGACY_SERVER_PASS_KEY);
    } catch (e) { /* nothing to migrate */ }
    if (legacyPass !== null && storedHost) {
      // One-time migration of the pre-keyring plaintext copy.
      invoke("server_password_store", { host: storedHost, password: legacyPass })
        .then(function() {
          try { window.localStorage.removeItem(LEGACY_SERVER_PASS_KEY); } catch (e) { /* ok */ }
        })
        .catch(function() { /* keyring unavailable: keep the legacy copy */ });
      passInput.value = legacyPass;
      rememberBox.checked = true;
    } else if (storedHost) {
      // A stored password IS the remember choice -- no separate flag.
      invoke("server_password_get", { host: storedHost }).then(function(stored) {
        if (stored !== null && passInput.value === "") {
          passInput.value = stored;
          rememberBox.checked = true;
        }
      }).catch(function() { /* credential store unavailable: start empty */ });
    }

    // Unticking forgets immediately -- don't make the user fetch to be
    // sure the password is gone from the credential store.
    rememberBox.addEventListener("change", function() {
      if (!rememberBox.checked) {
        var h = hostInput.value.trim();
        if (h) {
          invoke("server_password_forget", { host: h }).catch(function() { /* nothing stored */ });
        }
      }
    });

    toggle.addEventListener("click", function() {
      var open = form.style.display === "none";
      form.style.display = open ? "flex" : "none";
      toggle.classList.toggle("open", open);
      toggle.setAttribute("aria-expanded", String(open));
      if (open) {
        (hostInput.value ? button : hostInput).focus();
      }
    });

    function fetchLatest() {
      var host = hostInput.value.trim();
      if (!host) {
        setStatus("Enter the server's address first.");
        hostInput.focus();
        return;
      }
      if (loadInFlight || button.disabled) {
        return;
      }
      try {
        window.localStorage.setItem(SERVER_HOST_KEY, host);
      } catch (e) { /* not persisting is fine */ }
      (rememberBox.checked
        ? invoke("server_password_store", { host: host, password: passInput.value })
        : invoke("server_password_forget", { host: host })
      ).catch(function() { /* credential store unavailable: fetch anyway */ });
      button.disabled = true;
      setStatus("Connecting to " + host + "…");
      var channel = new window.__TAURI__.core.Channel();
      channel.onmessage = function(stage) {
        setStatus(String(stage));
      };
      window.__TAURI__.core.invoke("server_fetch_latest", {
        host: host,
        password: passInput.value,
        onProgress: channel,
      }).then(function(result) {
        button.disabled = false;
        // The .sav is on disk now; from here it's a normal path-based load.
        return loadLocalPath(result.path);
      }, function(error) {
        button.disabled = false;
        var message = String((error && error.message) || error);
        // Pinned-certificate mismatch (see server_api.rs): the server's TLS
        // cert changed since first login. Legitimate after a server
        // reinstall, so offer to trust the new one -- explicitly, never
        // silently.
        if (message.indexOf("TOFU_PIN_MISMATCH") !== -1) {
          var trust = window.confirm(
            "The server's TLS certificate has changed since it was first trusted." +
            "\n\nIf you reinstalled the server or it regenerated its certificate, " +
            "this is expected and you can trust the new certificate." +
            "\n\nIf not, someone may be intercepting the connection -- click Cancel." +
            "\n\nTrust the new certificate and retry?");
          if (trust) {
            invoke("server_forget_pin", { host: host })
              .then(fetchLatest, function(forgetError) {
                setStatus("Could not reset the pinned certificate: " + String(forgetError));
              });
          } else {
            setStatus("Server fetch cancelled: certificate not trusted.");
          }
          return;
        }
        setStatus("Server fetch failed: " + message);
      });
    }

    button.addEventListener("click", fetchLatest);
    [hostInput, passInput].forEach(function(input) {
      input.addEventListener("keydown", function(e) {
        if (e.key === "Enter") {
          fetchLatest();
        }
      });
    });
  }

  document.addEventListener("DOMContentLoaded", function() {
    MapApp.init();
    setStatus("No save loaded -- drop a .sav anywhere or click above.");

    var isTauri = SaveClient.isTauri();

    uploadDropZone.addEventListener("click", function() {
      // Desktop: native "Open" dialog returning a path. Browser: hidden file input.
      if (isTauri) { pickAndLoadTauri(); } else { uploadFileInput.click(); }
    });
    clearSaveBtn.addEventListener("click", clearSave);
    uploadFileInput.addEventListener("change", function() {
      loadLocalFile(uploadFileInput.files[0]);
      uploadFileInput.value = ""; // Re-selecting the same file should fire change again.
    });

    // Desktop file drops: the webview suppresses HTML5 file DnD (dragDropEnabled),
    // so Tauri delivers dropped paths via its own event instead of the DOM drop
    // handlers below.
    if (isTauri) {
      setupServerFetch();
    }
    if (isTauri && window.__TAURI__.event) {
      window.__TAURI__.event.listen("tauri://drag-drop", function(e) {
        var paths = e && e.payload && e.payload.paths;
        if (paths && paths.length) {
          loadLocalPath(paths[0]);
        }
      });
    }
    uploadDropZone.addEventListener("dragover", function(e) {
      e.preventDefault();
      uploadDropZone.classList.add("drag-over");
    });
    uploadDropZone.addEventListener("dragleave", function() {
      uploadDropZone.classList.remove("drag-over");
    });
    uploadDropZone.addEventListener("drop", function(e) {
      e.preventDefault();
      uploadDropZone.classList.remove("drag-over");
      loadLocalFile(e.dataTransfer && e.dataTransfer.files[0]);
    });

    // Dropping a save anywhere on the page works too -- with no landing
    // page, this is the fastest path from "opened the site" to "map loaded".
    document.addEventListener("dragover", function(e) {
      e.preventDefault();
    });
    document.addEventListener("drop", function(e) {
      e.preventDefault();
      loadLocalFile(e.dataTransfer && e.dataTransfer.files[0]);
    });
  });
})();
