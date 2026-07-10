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

  function loadLocalFile(file) {
    if (!file) {
      return;
    }
    if (!file.name.endsWith(".sav")) {
      setStatus("Only .sav save files can be loaded.");
      return;
    }
    if (loadInFlight) {
      return; // A parse is already running; don't queue a second one.
    }
    loadInFlight = true;
    uploadDropZone.classList.add("uploading");
    uploadDropText.textContent = "Loading " + file.name + "…";
    var pinnedSelection = Tooltip.getPinnedSelection();
    showProgress("Reading file", 0);

    file.arrayBuffer()
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
        Tooltip.hide();
        MapApp.setHighlight(null, null);
        // Stable per-file key: detail features guard on this, and the
        // pinned-tooltip restore survives re-loading the same file.
        MapApp.currentFile = "local:" + file.name + ":" + file.size + ":" + file.lastModified;
        EditorTool.onSaveLoaded(file.name);
        Filters.build(payload);
        Altitude.build(payload);
        FindItem.build(payload);
        Progression.build(payload);
        SelectionTool.reset();
        if (pinnedSelection) {
          restorePinnedSelection(pinnedSelection);
        }
        showGameSettings(payload.gameSettings);
        setStatus("Loaded: " + payload.sessionName + " (" + payload.saveDatetime + ")");
      })
      .catch(function(error) {
        hideProgress();
        loadInFlight = false;
        resetUploadZone();
        setStatus("Failed to load save: " + (error && error.message || error));
      });
  }

  document.addEventListener("DOMContentLoaded", function() {
    MapApp.init();
    setStatus("No save loaded -- drop a .sav anywhere or click above.");

    uploadDropZone.addEventListener("click", function() { uploadFileInput.click(); });
    uploadFileInput.addEventListener("change", function() {
      loadLocalFile(uploadFileInput.files[0]);
      uploadFileInput.value = ""; // Re-selecting the same file should fire change again.
    });
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
