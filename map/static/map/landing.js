(function() {
  "use strict";

  var errorEl = document.getElementById("error");

  function showError(msg) { errorEl.textContent = msg || ""; }

  function setLoading(btn, label) {
    btn.disabled = true;
    btn.dataset.origText = btn.textContent;
    btn.textContent = label;
  }

  function resetBtn(btn, error) {
    btn.disabled = false;
    btn.textContent = btn.dataset.origText || btn.textContent;
    if (error) showError(error);
  }

  // Pre-fill fields from /api/mode (restore last session or pre-fill SFTP from config file)
  fetch("/api/mode")
    .then(function(r) { return r.json(); })
    .then(function(m) {
      if (m.defaultLocalPath) {
        document.getElementById("localPath").value = m.defaultLocalPath;
      }
      // Pre-fill SFTP fields whether mode is active or from a saved sftp_config.json
      if (m.host)         { document.getElementById("sftpHost").value     = m.host; }
      if (m.port)         { document.getElementById("sftpPort").value     = m.port; }
      if (m.username)     { document.getElementById("sftpUser").value     = m.username; }
      if (m.remotePath)   { document.getElementById("sftpPath").value     = m.remotePath; }
      if (m.syncInterval) { document.getElementById("sftpInterval").value = m.syncInterval; }
      if (m.type === "local" && m.path) {
        document.getElementById("localPath").value = m.path;
      }
    })
    .catch(function() {});

  // ── Upload card ───────────────────────────────────────────────────────────
  var fileInput  = document.getElementById("fileInput");
  var fileDrop   = document.getElementById("fileDrop");
  var fileText   = document.getElementById("fileText");
  var uploadBtn  = document.getElementById("uploadBtn");

  function onFileChosen(file) {
    if (!file) return;
    fileText.textContent = file.name;
    fileDrop.classList.add("has-file");
    uploadBtn.disabled = false;
    showError("");
  }

  fileInput.addEventListener("change", function() {
    onFileChosen(fileInput.files[0]);
  });

  // Drag-and-drop on the drop zone
  fileDrop.addEventListener("dragover", function(e) {
    e.preventDefault();
    fileDrop.classList.add("drag-over");
  });
  fileDrop.addEventListener("dragleave", function() {
    fileDrop.classList.remove("drag-over");
  });
  fileDrop.addEventListener("drop", function(e) {
    e.preventDefault();
    fileDrop.classList.remove("drag-over");
    var file = e.dataTransfer && e.dataTransfer.files[0];
    if (file && file.name.endsWith(".sav")) {
      onFileChosen(file);
      // Swap the DataTransfer into the real input so the form submit works
      try {
        var dt = new DataTransfer();
        dt.items.add(file);
        fileInput.files = dt.files;
      } catch(ex) {}
    } else if (file) {
      showError("Please drop a .sav file.");
    }
  });

  uploadBtn.addEventListener("click", function() {
    var file = fileInput.files[0];
    if (!file) return;
    showError("");
    setLoading(uploadBtn, "Uploading…");
    var fd = new FormData();
    fd.append("file", file);
    fetch("/api/upload-save", { method: "POST", body: fd })
      .then(function(r) { return r.json(); })
      .then(function(res) {
        if (res.error) { resetBtn(uploadBtn, res.error); return; }
        window.location.href = "/map";
      })
      .catch(function(e) { resetBtn(uploadBtn, "Upload failed: " + e); });
  });

  // ── Local folder card ─────────────────────────────────────────────────────
  document.getElementById("localBtn").addEventListener("click", function() {
    var path = document.getElementById("localPath").value.trim();
    if (!path) { showError("Please enter a folder path."); return; }
    showError("");
    var btn = document.getElementById("localBtn");
    setLoading(btn, "Opening…");
    fetch("/api/set-mode", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ mode: "local", path: path }),
    })
      .then(function(r) { return r.json(); })
      .then(function(res) {
        if (res.error) { resetBtn(btn, res.error); return; }
        window.location.href = "/map";
      })
      .catch(function(e) { resetBtn(btn, "Error: " + e); });
  });

  // ── SFTP card ─────────────────────────────────────────────────────────────
  document.getElementById("sftpBtn").addEventListener("click", function() {
    var host        = document.getElementById("sftpHost").value.trim();
    var port        = parseInt(document.getElementById("sftpPort").value) || 22;
    var username    = document.getElementById("sftpUser").value.trim();
    var password    = document.getElementById("sftpPass").value;
    var remotePath  = document.getElementById("sftpPath").value.trim();
    var syncInterval = parseInt(document.getElementById("sftpInterval").value) || 60;
    if (!host || !username || !remotePath) {
      showError("Host, username and remote path are required.");
      return;
    }
    showError("");
    var btn = document.getElementById("sftpBtn");
    setLoading(btn, "Connecting…");
    fetch("/api/set-mode", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ mode: "sftp", host: host, port: port, username: username,
                             password: password, remotePath: remotePath, syncInterval: syncInterval }),
    })
      .then(function(r) { return r.json(); })
      .then(function(res) {
        if (res.error) { resetBtn(btn, res.error); return; }
        window.location.href = "/map";
      })
      .catch(function(e) { resetBtn(btn, "Error: " + e); });
  });
})();
