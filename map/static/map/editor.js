// EditorTool: save-editing state and UI. The uploaded file is never
// modified; edits happen on the parsed body inside the WASM worker and
// "Download save" exports a fresh .sav (see SaveClient.exportSave).
var EditorTool = (function() {
  "use strict";

  var downloadBtn = null;
  var exportInFlight = false;

  // Set on every successful save load (data.js); used for the download name.
  var currentFileName = null;

  function downloadName() {
    var base = currentFileName || "save.sav";
    return base.replace(/\.sav$/i, "") + "_edited.sav";
  }

  function exportSave() {
    if (exportInFlight || !MapApp.currentFile) {
      return;
    }
    exportInFlight = true;
    downloadBtn.disabled = true;
    downloadBtn.textContent = "Exporting…";
    SaveClient.exportSave()
      .then(function(bytes) {
        var blob = new Blob([bytes], { type: "application/octet-stream" });
        var url = URL.createObjectURL(blob);
        var a = document.createElement("a");
        a.href = url;
        a.download = downloadName();
        document.body.appendChild(a);
        a.click();
        a.remove();
        // Give the click a tick to start the download before revoking.
        setTimeout(function() { URL.revokeObjectURL(url); }, 5000);
      })
      .catch(function(error) {
        alert("Failed to export save: " + (error && error.message || error));
      })
      .then(function() {
        exportInFlight = false;
        downloadBtn.disabled = false;
        downloadBtn.textContent = "Download save";
      });
  }

  document.addEventListener("DOMContentLoaded", function() {
    downloadBtn = document.getElementById("downloadSaveBtn");
    downloadBtn.addEventListener("click", exportSave);
  });

  return {
    // Called by data.js after every successful load.
    onSaveLoaded(fileName) {
      currentFileName = fileName;
      downloadBtn.style.display = "";
    },
  };
})();
