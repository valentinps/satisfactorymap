// Tools -- the Tools tab of the right tool dock, and the Copy & paste panel.
//
// The app bar's Tools button opens the tab (#toolsPanel), which lists every
// map tool; picking one swaps its panel into the dock (Panels.openTool), and
// each tool panel's back button returns here. The dock itself -- one panel at
// a time, teardown when one is swapped out -- is panels.js's job; this file
// only decides WHICH panel goes in.
//
// To add a tool: give it a body-level panel in index.html (pre-adopted by
// panels.js), an open() entry point, and one row in TOOLS below. The search
// bar's tool entries (finditem.js's TOOL_ENTRIES) open through Tools.open too.

var Tools = {};

(function() {
  "use strict";

  var el = UI.el;

  var toolsButton = document.getElementById("toolsButton");
  var toolsPanel = document.getElementById("toolsPanel");
  var toolsList = document.getElementById("toolsList");

  var cp = {
    panel: document.getElementById("copyPastePanel"),
    noSave: document.getElementById("copyPasteNoSave"),
    selection: document.getElementById("copyPasteSelection"),
    copyBtn: document.getElementById("copyPasteCopyBtn"),
    clipboard: document.getElementById("copyPasteClipboard"),
    pasteBtn: document.getElementById("copyPastePasteBtn"),
  };

  // Icons reuse each panel's own header art (index.html), so a tool looks
  // the same in the list as it does once open.
  function headerIcon(panelId, selector) {
    var source = document.querySelector("#" + panelId + " " + selector);
    return source ? source.cloneNode(true) : null;
  }

  var TOOLS = [
    {
      id: "copyPaste",
      label: "Copy & paste",
      description: "Copy part of your factory and paste it elsewhere — even into another save.",
      needsSave: true,
      icon: function() { return headerIcon("copyPastePanel", ".toolPanelHeaderIcon svg"); },
      open: openCopyPaste,
    },
    {
      id: "network",
      label: "Optimal network finder (EMST)",
      description: "Shortest set of links joining a group of points — for planning belts, rails or power.",
      needsSave: false,
      icon: function() { return headerIcon("networkPanel", "#networkPanelIcon"); },
      open: function() {
        if (window.NetworkTool) {
          NetworkTool.open();
        }
      },
    },
  ];

  function hasSave() {
    return !!(window.MapApp && MapApp.currentFile);
  }

  // ---- The Tools tab --------------------------------------------------------

  function renderToolList() {
    toolsList.innerHTML = "";
    TOOLS.forEach(function(tool) {
      var row = el("button", "toolLauncherRow");
      row.type = "button";
      row.setAttribute("role", "listitem");
      var icon = el("span", "toolLauncherIcon");
      var art = tool.icon();
      if (art) {
        art.removeAttribute("id"); // A clone must not duplicate the panel's ids.
        icon.appendChild(art);
      }
      row.appendChild(icon);
      var text = el("span", "toolLauncherText");
      text.appendChild(el("span", "toolLauncherLabel", tool.label));
      text.appendChild(el("span", "toolLauncherDesc", tool.description));
      if (tool.needsSave && !hasSave()) {
        text.appendChild(el("span", "toolLauncherNote", "Needs a loaded save"));
      }
      row.appendChild(text);
      row.addEventListener("click", function() { tool.open(); });
      toolsList.appendChild(row);
    });
  }

  Tools.openLauncher = function() {
    renderToolList();
    Panels.openTool(toolsPanel);
  };

  // Open one tool by id ("copyPaste", "network") -- the search bar's entry.
  Tools.open = function(id) {
    for (var i = 0; i < TOOLS.length; i++) {
      if (TOOLS[i].id === id) {
        TOOLS[i].open();
        return;
      }
    }
  };

  // The button toggles: nothing open -> the Tools tab; anything open (the tab
  // or a tool) -> close the dock. A tool's own back button is the way from a
  // tool to the list.
  toolsButton.addEventListener("click", function() {
    var current = Panels.currentTool();
    if (current) {
      Panels.closeTool(current);
    } else {
      Tools.openLauncher();
    }
  });

  // Lit while anything the button can close is in the dock.
  Panels.onToolChange(function(current) {
    var active = !!current;
    toolsButton.classList.toggle("open", active);
    toolsButton.setAttribute("aria-pressed", String(active));
  });

  document.getElementById("toolsPanelClose").addEventListener("click", function() {
    Panels.closeTool(toolsPanel);
  });

  // ---- Copy & paste ---------------------------------------------------------

  function openCopyPaste() {
    Panels.openTool(cp.panel);
    refreshCopyPaste();
  }

  function plural(n, word) {
    return n.toLocaleString() + " " + word + (n === 1 ? "" : "s");
  }

  function refreshCopyPaste() {
    var loaded = hasSave();
    cp.noSave.style.display = loaded ? "none" : "";

    var targets = window.SelectionTool ? SelectionTool.currentEditTargets() : null;
    var total = window.SelectionTool ? SelectionTool.selectedCount() : 0;
    var editable = targets ? targets.actorNames.length + targets.lightweight.length : 0;
    if (total === 0) {
      cp.selection.textContent = "Nothing selected";
    } else if (editable === total) {
      cp.selection.textContent = plural(total, "object") + " selected";
    } else {
      // Collectables, nodes, vehicles... can be selected but not copied.
      cp.selection.textContent = plural(total, "object") + " selected · "
        + (editable === 0 ? "none" : editable.toLocaleString()) + " copyable";
    }
    cp.copyBtn.disabled = !loaded || editable === 0;
    cp.copyBtn.textContent = editable > 0 ? "Copy " + plural(editable, "object") : "Copy selection";

    var held = window.EditorTool ? EditorTool.clipboardCount() : 0;
    cp.clipboard.textContent = held > 0
      ? "Clipboard: " + plural(held, "object")
      : "Clipboard empty here — a copy from another tab still pastes";
    cp.pasteBtn.disabled = !loaded;
  }

  cp.copyBtn.addEventListener("click", function() {
    var targets = SelectionTool.currentEditTargets();
    if (targets) {
      EditorTool.copyTargets(targets); // Calls back into Tools.refresh.
    }
  });

  // Hands the dock over to the paste placement panel, like Ctrl+V.
  cp.pasteBtn.addEventListener("click", function() {
    EditorTool.startPaste();
  });

  document.getElementById("copyPastePanelBack").addEventListener("click", Tools.openLauncher);
  document.getElementById("copyPastePanelClose").addEventListener("click", function() {
    Panels.closeTool(cp.panel);
  });

  // Called by editor.js (clipboard / save state) and selection.js (the
  // selection) whenever what these panels show may have changed.
  Tools.refresh = function() {
    var current = Panels.currentTool();
    if (current === cp.panel) {
      refreshCopyPaste();
    } else if (current === toolsPanel) {
      renderToolList(); // The "Needs a loaded save" notes follow the save.
    }
  };

  // Escape closes the Tools tab or the Copy & paste panel (the network finder
  // and the paste panel register their own -- see network.js / editor.js).
  UI.onEscape(UI.LAYER.tool, function() {
    var current = Panels.currentTool();
    if (current !== toolsPanel && current !== cp.panel) {
      return false;
    }
    Panels.closeTool(current);
    return true;
  });
})();
