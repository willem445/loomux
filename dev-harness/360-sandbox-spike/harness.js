// Phase-0 spike for #360 (pane plugins) — proves or refutes the plan's central
// isolation assumption: can a sandboxed opaque-origin iframe (sandbox="allow-scripts",
// NO allow-same-origin) reach Tauri IPC from inside loomux's real webview?
//
// This is throwaway dev-harness code. It does NOT ship. Run it by:
//   1. npm run tauri dev
//   2. open devtools on the main window (right-click -> Inspect, or F12)
//   3. paste this whole file into the console and hit enter
//   4. read the `console.table` output it prints after ~3s
//
// Each route below is a distinct escape attempt from #360's spec. "LEAK-*" outcomes
// mean the sandbox failed to contain that route; anything else means it held.
(function sandboxSpike() {
  const results = [];
  function log(route, outcome, detail) {
    results.push({ route, outcome, detail: detail === undefined ? "" : String(detail) });
    console.log(`[spike] ${route}: ${outcome}`, detail ?? "");
  }

  // Baseline: confirm the PARENT (trusted main frame) actually has TAURI_INTERNALS,
  // otherwise every "absent" reading downstream is meaningless (false negative not
  // proof of a hardened sandbox, proof the harness didn't load correctly).
  log(
    "parent-baseline-has-internals",
    typeof window.__TAURI_INTERNALS__ !== "undefined" ? "present" : "MISSING-ABORT",
    typeof window.__TAURI_INTERNALS__
  );

  // The adversarial payload — everything past this point executes INSIDE the
  // sandboxed iframe's own opaque-origin realm, not the trusted parent.
  const payload = `
    (function () {
      function report(route, outcome, detail) {
        try {
          window.parent.postMessage({ __spike__: true, route, outcome, detail: String(detail == null ? '' : detail) }, '*');
        } catch (e) {
          // even postMessage itself throwing is a data point
          console.error('spike report failed', route, e);
        }
      }

      // Route 1: direct global access — does THIS frame's own window get
      // __TAURI_INTERNALS__ injected despite being a sandboxed opaque-origin frame?
      try {
        if (window.__TAURI_INTERNALS__) {
          report('own-window.__TAURI_INTERNALS__', 'LEAK-PRESENT', Object.keys(window.__TAURI_INTERNALS__).join(','));
        } else {
          report('own-window.__TAURI_INTERNALS__', 'absent');
        }
      } catch (e) { report('own-window.__TAURI_INTERNALS__', 'threw', e.message); }

      // Route 1b: wry's raw window.ipc bridge (installed the same way as __TAURI_INTERNALS__)
      try {
        if (window.ipc && typeof window.ipc.postMessage === 'function') {
          report('own-window.ipc', 'LEAK-PRESENT', 'postMessage fn present');
        } else {
          report('own-window.ipc', 'absent');
        }
      } catch (e) { report('own-window.ipc', 'threw', e.message); }

      // Route 1c: the underlying WebView2 native bridge itself
      try {
        if (window.chrome && window.chrome.webview) {
          report('own-window.chrome.webview', 'LEAK-PRESENT', typeof window.chrome.webview.postMessage);
        } else {
          report('own-window.chrome.webview', 'absent');
        }
      } catch (e) { report('own-window.chrome.webview', 'threw', e.message); }

      // Route 2: prototype chain — walk up from a plain object to see if anything
      // was polluted onto shared/global prototypes reachable without an explicit ref.
      try {
        const leak = ({}).__TAURI_INTERNALS__ || Object.prototype.__TAURI_INTERNALS__;
        report('prototype-chain-pollution', leak ? 'LEAK-PRESENT' : 'absent', leak);
      } catch (e) { report('prototype-chain-pollution', 'threw', e.message); }

      // Route 3: window.top / window.parent — should be SOP-opaque and throw on
      // property access for a sandboxed frame without allow-same-origin.
      try {
        const t = window.top.__TAURI_INTERNALS__;
        report('window.top.__TAURI_INTERNALS__', t ? 'LEAK-PRESENT' : 'reached-no-throw', t);
      } catch (e) { report('window.top.__TAURI_INTERNALS__', 'blocked-by-SOP', e.message); }

      try {
        const d = window.parent.document;
        report('window.parent.document', 'LEAK-PRESENT-DOM-ACCESS', d ? d.title : d);
      } catch (e) { report('window.parent.document', 'blocked-by-SOP', e.message); }

      // Route 4: window.opener — N/A for an <iframe>, only applies to window.open()
      // popups; logged for completeness per the spec's route list.
      report('window.opener', window.opener ? 'unexpected-present' : 'not-applicable-not-a-popup');

      // Route 5: actually invoke a real backend command through __TAURI_INTERNALS__,
      // if route 1 found it. Uses pty_backend_info — read-only, no side effects.
      if (window.__TAURI_INTERNALS__ && typeof window.__TAURI_INTERNALS__.invoke === 'function') {
        window.__TAURI_INTERNALS__.invoke('pty_backend_info', {})
          .then((r) => report('invoke("pty_backend_info")', 'LEAK-INVOKE-SUCCEEDED', JSON.stringify(r)))
          .catch((e) => report('invoke("pty_backend_info")', 'invoke-rejected', e && e.message ? e.message : String(e)));
      } else {
        report('invoke("pty_backend_info")', 'no-invoke-fn-reachable');
      }

      // Route 6: raw fetch straight to Tauri's own registered custom-protocol IPC
      // endpoint (ipc.localhost), bypassing __TAURI_INTERNALS__ entirely. wry's
      // WebResourceRequestedFilterWithRequestSourceKinds is documented ALL (not
      // main-frame-only) specifically "to allow ... iframes to work with custom
      // protocols" — so this tests whether the scheme handler itself is reachable
      // from an opaque-origin child frame regardless of any JS-level bridge.
      fetch('http://ipc.localhost/pty_backend_info', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json', 'Tauri-Callback': '0', 'Tauri-Error': '0' },
        body: '{}'
      }).then((r) => r.text().then((body) => report('raw-fetch(ipc.localhost)', 'reached-network-layer status=' + r.status, body.slice(0, 200))))
        .catch((e) => report('raw-fetch(ipc.localhost)', 'fetch-failed', e.message));

      // Route 7: dynamic import of @tauri-apps/api from inside the sandboxed frame —
      // tests whether module resolution/CSP lets the frame pull in the invoke wrapper
      // even if it never got __TAURI_INTERNALS__ handed to it directly.
      import('/@id/@tauri-apps/api/core').then((m) => {
        report('dynamic-import(@tauri-apps/api/core)', 'LEAK-IMPORT-SUCCEEDED', Object.keys(m).join(','));
      }).catch((e) => report('dynamic-import(@tauri-apps/api/core)', 'import-blocked', e.message));

      // Route 8: BroadcastChannel / shared-worker-style rendezvous. Opaque origins
      // are unguessable per-instantiation, so this should never rendezvous with
      // anything the trusted parent is listening on — but confirm empirically.
      try {
        const bc = new BroadcastChannel('tauri:ipc-probe');
        bc.onmessage = (e) => report('broadcastchannel-response', 'LEAK-GOT-RESPONSE', JSON.stringify(e.data));
        bc.postMessage({ cmd: 'pty_backend_info' });
        report('broadcastchannel-probe', 'sent-listening-1500ms');
      } catch (e) { report('broadcastchannel-probe', 'threw', e.message); }

      // Route 9: forge an isolation-pattern-shaped postMessage to the parent, in case
      // the app used the "isolation" Tauri pattern (ipc.js forwards matching-shaped
      // messages from ANY source without checking event.source).
      try {
        window.parent.postMessage({
          payload: { contentType: 'application/json', nonce: 0,
            payload: { cmd: 'pty_backend_info', callback: 1, error: 2 } }
        }, '*');
        report('postmessage-isolation-forge', 'sent-no-response-channel-to-observe');
      } catch (e) { report('postmessage-isolation-forge', 'threw', e.message); }

      report('done', 'all-routes-attempted');
    })();
  `;

  const iframe = document.createElement("iframe");
  iframe.sandbox = "allow-scripts"; // deliberately NOT allow-same-origin
  iframe.style.cssText = "width:1px;height:1px;position:fixed;top:-9999px;left:-9999px;";
  iframe.srcdoc = `<!doctype html><html><body><script>${payload}<\/script></body></html>`;

  window.addEventListener("message", (event) => {
    if (event.data && event.data.__spike__) {
      log(`iframe:${event.data.route}`, event.data.outcome, event.data.detail);
    }
  });

  document.body.appendChild(iframe);

  setTimeout(() => {
    console.log("=== #360 SANDBOX SPIKE RESULTS ===");
    console.table(results);
    window.__spikeResults__ = results;
    console.log("Copy window.__spikeResults__ for the findings comment.");
  }, 3000);
})();
