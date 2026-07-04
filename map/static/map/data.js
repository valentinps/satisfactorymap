// Fetches the save list and map payload from the Flask backend, and drives
// the load-button UI flow, including polling /api/load-progress while a
// parse is in flight (see sav_map_server.py's _ProgressBarHook).

(function() {
  "use strict";

  var PROGRESS_POLL_MS = 400;

  // How often to re-glob the save directory for a newer file while in
  // --auto mode (see sav_map_server.py's AUTO_LOAD_LATEST). Cheap on the
  // server side (just glob + stat), so a short interval is fine.
  var AUTO_WATCH_POLL_MS = 10000;

  var saveSelect = document.getElementById("saveSelect");
  var loadButton = document.getElementById("loadButton");
  var loadStatus = document.getElementById("loadStatus");
  var progressBar = document.getElementById("loadProgressBar");
  var progressFill = document.getElementById("loadProgressFill");
  var gameSettingsPanel = document.getElementById("gameSettingsPanel");
  var sftpPanel = document.getElementById("sftpPanel");

  // Populated by loadSaveList() -- reused by the auto-watch loop to find the
  // mtime of whatever's currently loaded without a second round-trip, and to
  // detect when a fresher save than that has appeared.
  var lastSaves = [];
  var autoLoadEnabled = false;
  var currentLoadedMtime = null;

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

  // SFTP sync panel -- only rendered when the server was started with an
  // sftp_config.json present. Stays in sync with /api/sftp-status so the
  // "last synced" age is always reasonably fresh.
  var sftpEnabled = false;
  var sftpStatusTimer = null;

  function relativeTime(ts) {
    if (ts === null || ts === undefined) {
      return "never";
    }
    var secs = Math.round(Date.now() / 1000 - ts);
    if (secs < 5)  { return "just now"; }
    if (secs < 60) { return secs + "s ago"; }
    var mins = Math.round(secs / 60);
    if (mins < 60) { return mins + "m ago"; }
    return Math.round(mins / 60) + "h ago";
  }

  function renderSftpPanel(status) {
    sftpPanel.innerHTML = "";
    var header = document.createElement("div");
    header.className = "sftpHeader";
    var title = document.createElement("span");
    title.className = "sftpTitle";
    title.textContent = "SFTP";
    var syncBtn = document.createElement("button");
    syncBtn.id = "sftpSyncButton";
    syncBtn.className = "sftpSyncBtn";
    syncBtn.textContent = status.syncing ? "Syncing..." : "Sync now";
    syncBtn.disabled = status.syncing;
    syncBtn.addEventListener("click", triggerSftpSync);
    header.appendChild(title);
    header.appendChild(syncBtn);
    sftpPanel.appendChild(header);

    var statusLine = document.createElement("div");
    statusLine.className = "sftpStatus";
    if (status.lastError) {
      statusLine.textContent = "Error: " + status.lastError;
      statusLine.classList.add("sftpStatusError");
    } else {
      statusLine.textContent = "Last sync: " + relativeTime(status.lastSync);
    }
    sftpPanel.appendChild(statusLine);
    sftpPanel.style.display = "block";
  }

  function refreshSftpStatus() {
    fetch("/api/sftp-status")
      .then(function(r) { return r.json(); })
      .then(function(status) {
        if (!status.enabled) { return; }
        renderSftpPanel(status);
        // If a sync is in flight, poll more frequently until it finishes.
        if (sftpStatusTimer) { clearTimeout(sftpStatusTimer); }
        sftpStatusTimer = setTimeout(refreshSftpStatus, status.syncing ? 1000 : 15000);
      })
      .catch(function() {});
  }

  function triggerSftpSync() {
    var btn = document.getElementById("sftpSyncButton");
    if (btn) { btn.disabled = true; btn.textContent = "Syncing..."; }
    fetch("/api/sftp-sync", { method: "POST" })
      .then(function() { refreshSftpStatus(); })
      .catch(function() { if (btn) { btn.disabled = false; btn.textContent = "Sync now"; } });
  }

  // Game-mode settings (Power Cost Multiplier, Purity Modifier, Node
  // Randomization) chosen at world creation -- see
  // sav_map_data.collectGameSettings(). These can silently change what
  // every resource node on the map actually is/yields relative to a vanilla
  // world, so they're shown unconditionally rather than only when
  // non-default, in case the displayed value itself is what someone's
  // trying to confirm.
  function showGameSettings(gameSettings) {
    gameSettingsPanel.innerHTML = "";
    if (!gameSettings || Object.keys(gameSettings).length === 0) {
      gameSettingsPanel.style.display = "none";
      return;
    }
    var rows = [
      ["Power cost", gameSettings.powerCostMultiplier !== undefined && gameSettings.powerCostMultiplier !== null ? gameSettings.powerCostMultiplier + "x" : null],
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

  // listSaveFiles() (server side) always returns newest-first, so saves[0]
  // is "the latest save" wherever that's needed below.
  function loadSaveList(onDone) {
    fetch("/api/saves")
      .then(function(response) { return response.json(); })
      .then(function(saves) {
        if (saves.error) {
          setStatus(saves.error);
          return;
        }
        lastSaves = saves;
        // Preserve the user's current dropdown selection across a refresh
        // (e.g. the periodic auto-watch poll) instead of always snapping
        // back to the first option.
        var previousValue = saveSelect.value;
        saveSelect.innerHTML = "";
        saves.forEach(function(save) {
          var option = document.createElement("option");
          option.value = save.filename;
          option.textContent = save.displayName;
          saveSelect.appendChild(option);
        });
        if (saves.some(function(save) { return save.filename === previousValue; })) {
          saveSelect.value = previousValue;
        }
        if (saves.length === 0) {
          setStatus("No save files found.");
        }
        if (onDone) {
          onDone(saves);
        }
      })
      .catch(function(error) { setStatus("Failed to list saves: " + error); });
  }

  function pollProgress() {
    var pollTimer = setInterval(function() {
      fetch("/api/load-progress")
        .then(function(response) { return response.json(); })
        .then(function(progress) {
          if (progress.phase) {
            var percent = progress.total > 0 ? (progress.current / progress.total) * 100 : 0;
            showProgress(progress.phase, percent);
          }
        })
        .catch(function() {}); // Transient poll failures are not worth surfacing.
    }, PROGRESS_POLL_MS);
    return pollTimer;
  }

  // Buckets are entirely rebuilt by Filters.build on every load (fresh
  // objects, even for the exact same building), so a pinned tooltip/highlight
  // can't just be left alone -- it has to be captured by stable bucket key +
  // id before the rebuild and re-resolved against the new buckets afterward.
  // Without this, --auto mode picking up a newer save (see checkForNewerSave)
  // would silently drop whatever the user was inspecting every ~10s.
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

  function loadSelectedSave() {
    var filename = saveSelect.value;
    if (!filename) {
      return;
    }
    var pinnedSelection = Tooltip.getPinnedSelection();
    showProgress("Starting", 0);
    loadButton.disabled = true;
    var pollTimer = pollProgress();

    fetch("/api/map-data?file=" + encodeURIComponent(filename))
      .then(function(response) { return response.json(); })
      .then(function(payload) {
        clearInterval(pollTimer);
        hideProgress();
        loadButton.disabled = false;
        if (payload.error) {
          setStatus(payload.error);
          return;
        }
        Tooltip.hide();
        MapApp.setHighlight(null, null);
        MapApp.currentFile = filename;
        Filters.build(payload);
        Altitude.build(payload);
        FindItem.build(payload);
        if (pinnedSelection) {
          restorePinnedSelection(pinnedSelection);
        }
        showGameSettings(payload.gameSettings);
        var loadedSave = lastSaves.filter(function(save) { return save.filename === filename; })[0];
        currentLoadedMtime = loadedSave ? loadedSave.mtime : Date.now() / 1000;
        var statusText = "Loaded: " + payload.sessionName + " (" + payload.saveDatetime + ")";
        setStatus(autoLoadEnabled ? statusText + " -- watching for newer saves" : statusText);
      })
      .catch(function(error) {
        clearInterval(pollTimer);
        hideProgress();
        loadButton.disabled = false;
        setStatus("Failed to load map data: " + error);
      });
  }

  // Re-globs the save directory and, if a save newer than whatever's
  // currently loaded has appeared, switches the dropdown to it and loads it
  // automatically -- this is what makes --auto mode pick up a fresh
  // autosave without the user touching anything. A load already in flight
  // (loadButton disabled) is left alone rather than interrupted.
  function checkForNewerSave() {
    if (sftpEnabled) {
      // Refresh the last-sync display alongside the save-list poll so the age
      // stays current without a separate dedicated timer when --auto is on.
      refreshSftpStatus();
    }
    if (loadButton.disabled) {
      return;
    }
    loadSaveList(function(saves) {
      if (saves.length === 0) {
        return;
      }
      var latest = saves[0];
      if (currentLoadedMtime === null || latest.mtime > currentLoadedMtime) {
        saveSelect.value = latest.filename;
        loadSelectedSave();
      }
    });
  }

  document.addEventListener("DOMContentLoaded", function() {
    MapApp.init();
    loadButton.addEventListener("click", loadSelectedSave);

    // "← Change mode" resets server-side mode and returns to the landing page.
    var changeModeBtn = document.getElementById("changeModeBtn");
    if (changeModeBtn) {
      changeModeBtn.addEventListener("click", function() {
        fetch("/api/reset-mode", { method: "POST" })
          .then(function() { window.location.href = "/"; })
          .catch(function() { window.location.href = "/"; });
      });
    }

    fetch("/api/config")
      .then(function(response) { return response.json(); })
      .then(function(config) {
        autoLoadEnabled = !!config.autoLoadLatest;
        sftpEnabled = !!config.sftpEnabled;
        if (sftpEnabled) {
          refreshSftpStatus();
        }
        loadSaveList(function(saves) {
          if (autoLoadEnabled && saves.length > 0) {
            saveSelect.value = saves[0].filename;
            loadSelectedSave();
          }
        });
        if (autoLoadEnabled) {
          setInterval(checkForNewerSave, AUTO_WATCH_POLL_MS);
        }
      })
      .catch(function() { loadSaveList(); }); // --auto is opt-in; a config fetch failure just falls back to manual loading.
  });
})();
