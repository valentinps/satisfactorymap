/* Product analytics for the hosted site (PostHog).
 *
 * Deliberately small: is the site used, do saves parse, and which features do
 * people actually open. Nothing about the save itself is sent -- no session
 * name, no file name, no coordinates, no item names -- only shape and timing
 * numbers, because the whole promise of this app is that your save never
 * leaves your machine and analytics must not quietly walk that back.
 */
var Analytics = (function() {
  "use strict";

  // Public (write-only) project key. It is meant to be readable in client
  // code -- it can send events and nothing else. Empty disables analytics
  // entirely, which is the state a fork or a self-hosted copy inherits.
  var PROJECT_KEY = "phc_yiJoNLSnBrq7BApC5VfDmzeB7H6oXui49QsQUEHgBM8W";
  var API_HOST = "https://eu.i.posthog.com";

  // Build-version query of this script's own URL, so the vendored library is
  // cache-busted by a rebuild exactly like every tag build_site.py stamps.
  // (Same idiom as save_client.js; the injected tag is not in index.html, so
  // stampAssetVersion never sees it.) Empty when serving unstamped sources.
  var ASSET_QUERY = (function() {
    try {
      var src = document.currentScript && document.currentScript.src;
      return src ? new URL(src).search : "";
    } catch (e) {
      return "";
    }
  })();

  var loaded = false;
  // Events fired before the library finishes loading. Bounded because a
  // failed load must not grow this without limit for the whole session.
  var pending = [];
  var PENDING_MAX = 32;

  // The desktop app bundles this very dist/ (tauri.conf.json frontendDist),
  // so "hosted site only" is a runtime question, not a build one: there is no
  // separate web build to put the snippet in. The desktop CSP would block the
  // request anyway -- gating here is what keeps the app genuinely
  // phone-home-free rather than merely failing to phone home.
  function enabled() {
    if (!PROJECT_KEY) {
      return false;
    }
    if (window.__TAURI__) {
      return false;
    }
    // Local dev and file:// runs would otherwise land in the same project as
    // real traffic and skew every number in it.
    var host = location.hostname;
    return !!host && host !== "localhost" && host !== "127.0.0.1" && host !== "[::1]";
  }

  var ENABLED = enabled();

  function flush() {
    for (var i = 0; i < pending.length; i++) {
      try {
        window.posthog.capture(pending[i][0], pending[i][1]);
      } catch (e) { /* analytics must never break the app */ }
    }
    pending = [];
  }

  function start() {
    if (!ENABLED) {
      return;
    }
    var script = document.createElement("script");
    // Vendored (see vendor/posthog.js): the site ships COEP require-corp for
    // wasm, under which a plain cross-origin <script> from PostHog's CDN is a
    // no-cors request and gets blocked outright -- and the CDN sends no
    // Cross-Origin-Resource-Policy. Serving it same-origin sidesteps that,
    // and the "no-external" build never injects further script tags, so the
    // only cross-origin traffic left is the CORS-mode ingest request.
    script.src = "vendor/posthog.js" + ASSET_QUERY;
    script.async = true;
    script.onload = function() {
      if (!window.posthog || !window.posthog.init) {
        return;
      }
      try {
        window.posthog.init(PROJECT_KEY, {
          api_host: API_HOST,
          // Cookieless: no cookie and no localStorage entry, so the site
          // needs no consent banner. The cost is that every reload counts as
          // a new anonymous user -- read the totals as visits, not people.
          persistence: "memory",
          person_profiles: "identified_only",
          respect_dnt: true,
          // Every event this app sends is written by hand below. Autocapture
          // on a canvas UI would mostly record "clicked the map".
          autocapture: false,
          capture_pageview: true,
          capture_pageleave: false,
          disable_session_recording: true,
          disable_surveys: true,
          disable_external_dependency_loading: true,
          advanced_disable_feature_flags: true,
          sanitize_properties: stripQueryStrings
        });
        loaded = true;
        flush();
      } catch (e) { /* analytics must never break the app */ }
    };
    document.head.appendChild(script);
  }

  // PostHog attaches the page URL to every event, and this app takes a save
  // to load as ?url=<remote .sav> (data.js) -- which would quietly ship a
  // user's save location to analytics. Drop the query and fragment from every
  // URL-ish property instead of trusting the default set to stay harmless.
  function stripQueryStrings(props) {
    for (var key in props) {
      if (!Object.prototype.hasOwnProperty.call(props, key)) {
        continue;
      }
      var isUrlish = key.indexOf("url") !== -1 || key.indexOf("referrer") !== -1;
      if (isUrlish && typeof props[key] === "string") {
        props[key] = props[key].split("?")[0].split("#")[0];
      }
    }
    return props;
  }

  function capture(name, props) {
    if (!ENABLED) {
      return;
    }
    if (!loaded) {
      if (pending.length < PENDING_MAX) {
        pending.push([name, props || {}]);
      }
      return;
    }
    try {
      window.posthog.capture(name, props || {});
    } catch (e) { /* analytics must never break the app */ }
  }

  // Top-bar and toolbar entry points worth knowing the usage of. An explicit
  // list, not a blanket click handler: anything not named here is not sent,
  // which is a property that survives future markup changes.
  var FEATURES = {
    depotIconButton: "depot",
    mamIconButton: "mam",
    altRecipesIconButton: "alt_recipes",
    shopIconButton: "shop",
    hubIconButton: "hub",
    spaceElevatorIconButton: "space_elevator",
    githubLink: "github",
    downloadSaveBtn: "save_download",
    networkComputeBtn: "network_compute",
    selectionCopyBtn: "selection_copy",
    selectionMoveBtn: "selection_move",
    selectionOffsetBtn: "selection_offset",
    selectionDeleteBtn: "selection_delete"
  };

  function onClick(e) {
    var el = e.target && e.target.closest && e.target.closest("[id]");
    while (el) {
      if (FEATURES[el.id]) {
        capture("feature_used", { feature: FEATURES[el.id] });
        return;
      }
      el = el.parentElement && el.parentElement.closest("[id]");
    }
  }

  // ---- Public API -----------------------------------------------------------

  // Fired once per successful parse. `objects` is counted off the built
  // buckets rather than the payload so it means the same thing on every load
  // path, and the timing is the whole user-visible wait, not just the parser.
  function saveLoaded(source, bytes, ms) {
    if (!ENABLED) {
      return; // Don't walk the buckets for an event nobody will send.
    }
    var objects = 0;
    try {
      var buckets = MapApp.layer && MapApp.layer.buckets;
      for (var i = 0; buckets && i < buckets.length; i++) {
        objects += (buckets[i].points.length / 2) | 0;
      }
    } catch (e) { /* count is a nice-to-have, the event is not */ }
    capture("save_loaded", {
      source: source,
      size_mb: bytes ? Math.round(bytes / 1e5) / 10 : null,
      objects: objects,
      ms: ms ? Math.round(ms) : null
    });
  }

  function toolOpened(id) {
    capture("tool_opened", { tool: id || "unknown" });
  }

  if (ENABLED) {
    document.addEventListener("click", onClick, true);
    start();
  }

  return {
    capture: capture,
    saveLoaded: saveLoaded,
    toolOpened: toolOpened,
    isEnabled: function() { return ENABLED; }
  };
})();
