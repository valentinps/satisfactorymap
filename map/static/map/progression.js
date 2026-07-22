// Drives the top bar's progression buttons -- MAM research, alternate
// recipes, AWESOME Shop purchases, HUB milestones, and Space Elevator
// status -- and the one shared modal behind them (#progressionModal).
// All data arrives pre-grouped from sav_map_data.collectProgression (the
// payload's "progression" key); this file only renders it. Every view lists
// ALL known entries with the not-yet-unlocked ones dimmed, since seeing
// what's still missing is half the point of a progression view.
(function() {
  "use strict";

  window.MapApp = window.MapApp || {};
  var Progression = {};
  window.Progression = Progression;

  var overlay = document.getElementById("progressionModalOverlay");
  var modalIcon = document.getElementById("progressionModalIcon");
  var modalTitle = document.getElementById("progressionModalTitle");
  var modalClose = document.getElementById("progressionModalClose");
  var modalSummary = document.getElementById("progressionModalSummary");
  var modalBody = document.getElementById("progressionModalBody");

  var ITEM_ICON_BASE = "icons/items/";
  var BUILDING_ICON_BASE = "icons/buildings/";

  var data = null; // payload.progression for the currently loaded save (null before any load).

  function el(tag, className, text) {
    var e = document.createElement(tag);
    if (className) e.className = className;
    if (text !== undefined) e.textContent = text;
    return e;
  }

  // Item icon with a building fallback: progression rows reference items
  // (research costs, unlocked recipes' products), but a "product" is often a
  // buildable's descriptor -- e.g. a shop unlock's catwalk -- which has no
  // icons/items/ PNG. The matching icons/buildings/Build_*.png usually exists
  // under the game's own Desc_ -> Build_ naming convention, so that's tried
  // second; if neither exists the img keeps its space (visibility, not
  // display) so rows in the same list stay aligned.
  function attachItemIcon(img, itemClassName) {
    if (!itemClassName) {
      img.style.visibility = "hidden";
      return;
    }
    img.onerror = function() {
      img.onerror = function() {
        img.style.visibility = "hidden";
      };
      var match = /^Desc_(.+)_C$/.exec(itemClassName);
      if (match) {
        img.src = BUILDING_ICON_BASE + "Build_" + match[1] + "_C.png";
      } else {
        img.onerror();
      }
    };
    img.src = ITEM_ICON_BASE + encodeURIComponent(itemClassName) + ".png";
  }

  function openModal(title, iconUrl) {
    modalTitle.textContent = title;
    modalIcon.src = iconUrl;
    modalSummary.textContent = "";
    modalBody.innerHTML = "";
    overlay.style.display = "flex";
  }

  function closeModal() {
    overlay.style.display = "none";
  }

  modalClose.addEventListener("click", closeModal);
  overlay.addEventListener("click", function(e) {
    if (e.target === overlay) {
      closeModal(); // Click on the backdrop, not the dialog itself.
    }
  });
  document.addEventListener("keydown", function(e) {
    if (e.key === "Escape" && !e.defaultPrevented && overlay.style.display !== "none") {
      closeModal();
      e.preventDefault(); // One layer per press -- see finditem.js.
    }
  });

  // One grouped block: header ("Tier 5" / "Quartz" / "Walls"), an n/total
  // tally, a thin completion bar, and the row list. `tag` is an optional
  // warning chip on the header (e.g. a MAM tree the save never discovered).
  function section(title, doneCount, total, tag) {
    var root = el("div", "progressionSection");
    var header = el("div", "progressionSectionHeader");
    header.appendChild(el("span", "progressionSectionTitle", title));
    if (tag) {
      header.appendChild(el("span", "progressionSectionTag", tag));
    }
    var count = el("span", "progressionSectionCount", doneCount + " / " + total);
    if (doneCount >= total && total > 0) {
      count.classList.add("complete");
    }
    header.appendChild(count);
    root.appendChild(header);
    var track = el("div", "progressionBarTrack");
    var fill = el("div", "progressionBarFill");
    fill.style.width = (total > 0 ? (100 * doneCount / total) : 0) + "%";
    if (doneCount >= total && total > 0) {
      fill.classList.add("complete");
    }
    track.appendChild(fill);
    root.appendChild(track);
    var list = el("div", "progressionList");
    root.appendChild(list);
    modalBody.appendChild(root);
    return list;
  }

  function rowIcon(itemClassName, className) {
    var img = document.createElement("img");
    img.className = className || "progressionRowIcon";
    img.alt = "";
    // These lists run to hundreds of rows per view -- don't fetch/decode
    // offscreen icons up front, it makes the whole modal scroll janky.
    img.loading = "lazy";
    img.decoding = "async";
    attachItemIcon(img, itemClassName);
    return img;
  }

  // A plain progression row: optional item icon, label, and a right-hand
  // status -- a green check when done, otherwise `pending`: either a short
  // note string (e.g. "5 coupons") or a cost array [{item, label, amount}].
  // A single-item cost stays inline; a multi-item one gets a "cost" expander
  // that unfolds a vertical per-item list under the row (the old horizontal
  // "25× Rotor, 200× Iron Rod, ..." strings overflowed the modal). The list
  // is only built on first expand, so collapsed rows cost nothing.
  function row(list, entry, iconItem, pending) {
    var r = el("div", "progressionRow" + (entry.done ? "" : " locked"));
    var main = el("div", "progressionRowMain");
    if (iconItem !== undefined) {
      main.appendChild(rowIcon(iconItem));
    }
    main.appendChild(el("span", "progressionRowLabel", entry.label));
    if (entry.done) {
      main.appendChild(el("span", "progressionRowStatus done", "✓"));
    } else if (typeof pending === "string" && pending) {
      main.appendChild(el("span", "progressionRowStatus", pending));
    } else if (Array.isArray(pending) && pending.length === 1) {
      main.appendChild(el("span", "progressionRowStatus", costText(pending)));
    } else if (Array.isArray(pending) && pending.length > 1) {
      var toggle = el("button", "progressionCostToggle");
      toggle.appendChild(el("span", "chev", "▸"));
      toggle.appendChild(document.createTextNode(" cost"));
      var costList = null;
      toggle.addEventListener("click", function() {
        if (costList === null) {
          costList = el("div", "progressionCostList");
          pending.forEach(function(cost) {
            var line = el("div", "progressionCostLine");
            line.appendChild(rowIcon(cost.item, "progressionCostIcon"));
            line.appendChild(el("span", "progressionCostAmount", cost.amount.toLocaleString() + "×"));
            line.appendChild(el("span", null, cost.label));
            costList.appendChild(line);
          });
          r.appendChild(costList);
        } else {
          costList.style.display = costList.style.display === "none" ? "" : "none";
        }
        toggle.classList.toggle("open", costList.style.display !== "none");
      });
      main.appendChild(toggle);
    }
    r.appendChild(main);
    list.appendChild(r);
  }

  function costText(cost) {
    return (cost || []).map(function(c) {
      return c.amount.toLocaleString() + "× " + c.label;
    }).join(", ");
  }

  // ---- The five views -------------------------------------------------------

  function openMam() {
    openModal("MAM Research", BUILDING_ICON_BASE + "Build_Mam_C.png");
    if (!data) {
      modalSummary.textContent = "No save loaded yet.";
      return;
    }
    var trees = data.mamTrees || [];
    var done = 0, total = 0;
    trees.forEach(function(tree) {
      done += tree.doneCount;
      total += tree.nodes.length;
    });
    modalSummary.textContent = done.toLocaleString() + " / " + total.toLocaleString() +
      " research nodes completed across " + trees.length + " trees.";
    trees.forEach(function(tree) {
      var list = section(tree.label, tree.doneCount, tree.nodes.length,
        tree.treeUnlocked ? null : "not discovered");
      tree.nodes.forEach(function(node) {
        var iconItem = node.cost.length ? node.cost[0].item : null;
        row(list, node, iconItem, node.cost);
      });
    });
  }

  function openAlternates() {
    openModal("Alternate Recipes", ITEM_ICON_BASE + "HardDrive.png");
    if (!data) {
      modalSummary.textContent = "No save loaded yet.";
      return;
    }
    var recipes = data.alternateRecipes || [];
    var unlocked = recipes.filter(function(r) { return r.done; });
    var locked = recipes.filter(function(r) { return !r.done; });
    modalSummary.textContent = unlocked.length + " / " + recipes.length + " alternate recipes unlocked.";
    if (unlocked.length) {
      var unlockedList = section("Unlocked", unlocked.length, unlocked.length);
      unlocked.forEach(function(recipe) {
        row(unlockedList, recipe, recipe.productItem);
      });
    }
    if (locked.length) {
      var lockedList = section("Still on hard drives out there", 0, locked.length);
      locked.forEach(function(recipe) {
        row(lockedList, recipe, recipe.productItem);
      });
    }
  }

  function openShop() {
    openModal("AWESOME Shop", ITEM_ICON_BASE + "AwesomeShop.png");
    if (!data) {
      modalSummary.textContent = "No save loaded yet.";
      return;
    }
    var categories = data.shopCategories || [];
    var done = 0, total = 0;
    categories.forEach(function(category) {
      done += category.doneCount;
      total += category.entries.length;
    });
    modalSummary.textContent = done.toLocaleString() + " / " + total.toLocaleString() +
      " unlocks purchased · " + (data.couponsSpent || 0).toLocaleString() + " coupons spent.";
    categories.forEach(function(category) {
      var list = section(category.label, category.doneCount, category.entries.length);
      category.entries.forEach(function(entry) {
        var note = entry.couponCost ? entry.couponCost.toLocaleString() +
          " coupon" + (entry.couponCost === 1 ? "" : "s") : "";
        row(list, entry, entry.productItem, note);
      });
    });
  }

  function openHub() {
    openModal("HUB Milestones", ITEM_ICON_BASE + "Hub.png");
    if (!data) {
      modalSummary.textContent = "No save loaded yet.";
      return;
    }
    var tiers = data.hubTiers || [];
    var done = 0, total = 0;
    var completedTiers = 0;
    tiers.forEach(function(tier) {
      done += tier.doneCount;
      total += tier.milestones.length;
      if (tier.doneCount >= tier.milestones.length) completedTiers++;
    });
    modalSummary.textContent = done.toLocaleString() + " / " + total.toLocaleString() +
      " milestones completed · " + completedTiers + " / " + tiers.length + " tiers fully unlocked.";
    tiers.forEach(function(tier) {
      var list = section(tier.label, tier.doneCount, tier.milestones.length);
      tier.milestones.forEach(function(milestone) {
        // No icon column: milestones have no single representative item
        // (their costs run 3-4 items long), so the cost expander carries
        // the detail.
        row(list, milestone, undefined, milestone.cost);
      });
    });
  }

  function openSpaceElevator() {
    openModal("Space Elevator", BUILDING_ICON_BASE + "Build_SpaceElevator_C.png");
    if (!data || !data.spaceElevator) {
      modalSummary.textContent = "No save loaded yet.";
      return;
    }
    var se = data.spaceElevator;

    var banner = el("div", "progressionPhaseBanner");
    if (se.gameCompleted) {
      banner.classList.add("complete");
      banner.appendChild(el("div", "phaseTitle", "Project Assembly complete"));
      banner.appendChild(el("div", "phaseNote", "Every phase has been delivered. FICSIT thanks you for your service."));
      modalBody.appendChild(banner);
      if (se.costMultiplier !== 1) {
        modalBody.appendChild(el("div", "phaseNote",
          "Space Elevator cost multiplier (game mode setting): ×" + se.costMultiplier));
      }
      return;
    }

    var target = se.targetPhase;
    var phaseTitle = "No phase in progress";
    if (target) {
      phaseTitle = "Phase " + (target.phaseNumber !== null ? target.phaseNumber : "?") +
        (target.name ? ": " + target.name : "") + " — in progress";
    }
    banner.appendChild(el("div", "phaseTitle", phaseTitle));
    if (!se.built) {
      banner.appendChild(el("div", "phaseNote", "No Space Elevator has been built yet."));
    }
    if (se.currentPhase && se.currentPhase.phaseNumber) {
      banner.appendChild(el("div", "phaseNote",
        "Phases completed so far: " + se.currentPhase.phaseNumber));
    }
    if (se.costMultiplier !== 1) {
      banner.appendChild(el("div", "phaseNote",
        "Space Elevator cost multiplier (game mode setting): ×" + se.costMultiplier +
        " — the required amounts below include it."));
    }
    modalBody.appendChild(banner);

    var parts = se.targetCost || [];
    if (!parts.length) {
      modalSummary.textContent = "Nothing to deliver right now.";
      return;
    }
    var fullyDelivered = parts.filter(function(part) {
      return part.required !== null && part.imported >= part.required;
    }).length;
    modalSummary.textContent = fullyDelivered + " / " + parts.length + " parts fully delivered.";

    var list = el("div", "progressionList");
    parts.forEach(function(part) {
      var partDone = part.required !== null && part.imported >= part.required;
      var r = el("div", "progressionRow sePartRow" + (partDone ? "" : " locked"));
      var main = el("div", "progressionRowMain");
      main.appendChild(rowIcon(part.item));
      main.appendChild(el("span", "progressionRowLabel", part.label));
      var statusText = part.required !== null
        ? part.imported.toLocaleString() + " / " + part.required.toLocaleString()
        : part.imported.toLocaleString() + " imported";
      var status = el("span", "progressionRowStatus" + (partDone ? " done" : " countValue"), statusText);
      main.appendChild(status);
      r.appendChild(main);
      if (part.required) {
        var track = el("div", "sePartBarTrack");
        var fill = el("div", "sePartBarFill" + (partDone ? " complete" : ""));
        fill.style.width = Math.min(100, 100 * part.imported / part.required) + "%";
        track.appendChild(fill);
        r.appendChild(track);
      }
      list.appendChild(r);
    });
    modalBody.appendChild(list);
  }

  document.getElementById("mamIconButton").addEventListener("click", openMam);
  document.getElementById("altRecipesIconButton").addEventListener("click", openAlternates);
  document.getElementById("shopIconButton").addEventListener("click", openShop);
  document.getElementById("hubIconButton").addEventListener("click", openHub);
  document.getElementById("spaceElevatorIconButton").addEventListener("click", openSpaceElevator);

  // Called on every save load (see data.js) -- swap in the fresh data and
  // close any open view, since it would be showing the previous save's state.
  Progression.build = function(payload) {
    data = payload.progression || null;
    closeModal();
  };
})();
