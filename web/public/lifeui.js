// JS glue for the Phase 1 spike. All functions return JSON strings so the Rust side stays tiny.
// Passkeys here are a CLIENT-SIDE feasibility test only: the challenge is generated in the
// browser and nothing is verified. Real registration/login (webauthn-rs) comes later.
(function () {
  const KEY = "lifeui.spike.credentialId";
  const b64 = (buf) => btoa(String.fromCharCode(...new Uint8Array(buf)))
    .replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
  const unb64 = (s) => Uint8Array.from(atob(s.replace(/-/g, "+").replace(/_/g, "/")), (c) => c.charCodeAt(0));
  const fail = (e) => JSON.stringify({ ok: false, error: `${e && e.name}: ${e && e.message}`, rpId: location.hostname });

  async function postJson(path, body) {
    try {
      const response = await fetch(path, {
        method: "POST",
        headers: { "content-type": "application/json", accept: "application/json" },
        body: JSON.stringify(body),
        credentials: "same-origin",
      });
      let data = {};
      try { data = await response.json(); } catch { /* non-JSON body */ }
      return { status: response.status, data };
    } catch (e) {
      return { status: 0, data: { error: "Cannot reach the server: " + e } };
    }
  }
  // "" = success, otherwise a message that is safe to show the user.
  function outcome(res) {
    if (res.status === 200 && res.data && res.data.ok) return "";
    const d = res.data || {};
    const wait = d.retry_after_secs ? ` Try again in ${d.retry_after_secs} s.` : "";
    return (d.error || `Unexpected response (${res.status}).`) + wait;
  }

  window.lifeui = {
    // "setup" | "login" | "app" | "error:<message>"
    async auth_state() {
      try {
        const response = await fetch("/api/auth_status", { credentials: "same-origin", headers: { accept: "application/json" } });
        const d = await response.json();
        if (!response.ok || !d.ok) return "error:" + (d.error || response.status);
        if (d.setup_required) return "setup";
        return d.authenticated ? "app" : "login";
      } catch (e) { return "error:" + e; }
    },
    async auth_register(setupCode, password) { return outcome(await postJson("/api/auth_setup", { setup_code: setupCode, password })); },
    async auth_login(password) { return outcome(await postJson("/api/auth_login", { password })); },
    async auth_logout() { await postJson("/api/auth_logout", {}); return ""; },

    environment() {
      return JSON.stringify({
        host: location.hostname,
        secureContext: window.isSecureContext,
        standalone: window.matchMedia("(display-mode: standalone)").matches || navigator.standalone === true,
        serviceWorker: "serviceWorker" in navigator,
        webauthn: !!window.PublicKeyCredential,
        userAgent: navigator.userAgent,
      }, null, 2);
    },

    // App screens: JSON string {"status": N, "data": <body>}; status 0 = network failure.
    async api_get(path) {
      try {
        const response = await fetch(path, { credentials: "same-origin", headers: { accept: "application/json" } });
        let data = {};
        try { data = await response.json(); } catch { /* non-JSON body */ }
        return JSON.stringify({ status: response.status, data });
      } catch (e) { return JSON.stringify({ status: 0, data: { error: "Cannot reach the server." } }); }
    },
    async api_post(path, bodyJson) {
      const res = await postJson(path, JSON.parse(bodyJson));
      return JSON.stringify(res);
    },
    // Today's local date, "YYYY-MM-DD".
    today() {
      const d = new Date();
      const p = (n) => String(n).padStart(2, "0");
      return `${d.getFullYear()}-${p(d.getMonth() + 1)}-${p(d.getDate())}`;
    },
    // "2026-05-29" + "14:30" (local time) -> RFC 3339 UTC instant, "" if invalid.
    local_to_instant(date, time) {
      const d = new Date(`${date}T${time}`);
      return isNaN(d.getTime()) ? "" : d.toISOString().replace(/\.\d{3}Z$/, "Z");
    },

    // Small per-device memory (ticked to-dos). Returns "" when nothing is stored or storage is blocked.
    store_get(key) {
      try { return localStorage.getItem("lifeui:" + key) || ""; } catch (e) { return ""; }
    },
    store_set(key, value) {
      try { localStorage.setItem("lifeui:" + key, value); } catch (e) { /* private mode: the tick just is not remembered */ }
    },

    // Animated counter. `id` names an empty <span>; NumberFlow (vendored in /number-flow.js, registers the
    // <number-flow> element) animates the digits when the value changes. Until that script has loaded, or if it
    // never does, the span simply shows the number as text.
    flow(id, value) {
      const host = document.getElementById(id);
      if (!host) return;
      host.dataset.value = String(value);
      if (!customElements.get("number-flow")) {
        host.textContent = String(value);
        customElements.whenDefined("number-flow").then(() => flowUpdate(host));
        return;
      }
      flowUpdate(host);
    },

    // Bottom sheet: the native <dialog id="sheet"> (focus trap, Esc, inert background come from the browser).
    // The CSS transition does the animation; Rust calls this with true/false and handles the "cancel" event
    // (Esc, backdrop tap, or a drag-dismiss from bindSheetDrag) by closing through here, so closing always animates.
    sheet(open) {
      const d = document.getElementById("sheet");
      if (!d) return;
      if (open) {
        if (!d.open) d.showModal();
        d.classList.remove("dragging");
        d.style.transform = "";
        d.style.removeProperty("--p");
        void d.offsetHeight; // flush: the transition must start from the closed position
        d.dataset.state = "open";
        bindSheetDrag(d);
      } else if (d.open && d.dataset.state !== "closing") {
        d.style.transform = ""; // a dragged sheet continues from where the finger left it
        d.style.removeProperty("--p");
        d.dataset.state = "closing";
        setTimeout(() => {
          if (d.dataset.state === "closing") { d.close(); d.dataset.state = ""; }
        }, 240);
      }
    },

    async call_api(path) {
      try {
        const started = performance.now();
        const response = await fetch(path, { headers: { accept: "application/json" } });
        const text = await response.text();
        return JSON.stringify({ status: response.status, ms: Math.round(performance.now() - started), body: safeJson(text) }, null, 2);
      } catch (e) { return fail(e); }
    },

    async passkeyCreate() {
      try {
        const rpId = location.hostname;
        const credential = await navigator.credentials.create({
          publicKey: {
            rp: { id: rpId, name: "LifeUI" },
            user: { id: crypto.getRandomValues(new Uint8Array(16)), name: "owner", displayName: "Owner" },
            challenge: crypto.getRandomValues(new Uint8Array(32)),
            pubKeyCredParams: [{ type: "public-key", alg: -7 }, { type: "public-key", alg: -257 }],
            authenticatorSelection: { residentKey: "required", userVerification: "required" },
            timeout: 60000,
          },
        });
        localStorage.setItem(KEY, b64(credential.rawId));
        return JSON.stringify({ ok: true, step: "created", rpId, credentialId: credential.id, type: credential.type }, null, 2);
      } catch (e) { return fail(e); }
    },

    async passkeyLogin() {
      try {
        const rpId = location.hostname;
        const stored = localStorage.getItem(KEY);
        const assertion = await navigator.credentials.get({
          publicKey: {
            rpId,
            challenge: crypto.getRandomValues(new Uint8Array(32)),
            userVerification: "required",
            allowCredentials: stored ? [{ type: "public-key", id: unb64(stored) }] : [],
            timeout: 60000,
          },
        });
        return JSON.stringify({ ok: true, step: "signed-in", rpId, credentialId: assertion.id }, null, 2);
      } catch (e) { return fail(e); }
    },
  };

  function flowUpdate(host) {
    let el = host.querySelector("number-flow");
    if (!el) {
      host.textContent = "";
      el = document.createElement("number-flow");
      host.appendChild(el);
    }
    el.update(Number(host.dataset.value));
  }

  // Drag the sheet down by its grab handle. While the finger is down the sheet follows it 1:1 (no easing), pulling
  // upwards is rubber-banded, and on release it dismisses past 40% of its height OR on a quick flick (velocity),
  // otherwise it springs back. Dismissing dispatches "cancel", the same path as the Esc key.
  const FLICK_PX_PER_MS = 0.4;
  function bindSheetDrag(d) {
    if (d._dragBound) return;
    d._dragBound = true;
    let pointer = null, startY = 0, lastY = 0, lastT = 0, velocity = 0;
    d.addEventListener("pointerdown", (e) => {
      if (!e.target.closest(".sheet-grab") || pointer !== null) return;
      pointer = e.pointerId;
      startY = lastY = e.clientY;
      lastT = performance.now();
      velocity = 0;
      e.target.setPointerCapture(pointer);
      d.classList.add("dragging");
    });
    d.addEventListener("pointermove", (e) => {
      if (e.pointerId !== pointer) return;
      const dy = e.clientY - startY;
      const y = dy >= 0 ? dy : dy * 0.2; // rubber-band
      const now = performance.now();
      velocity = (e.clientY - lastY) / Math.max(1, now - lastT);
      lastY = e.clientY;
      lastT = now;
      d.style.transform = `translateY(${y}px)`;
      d.style.setProperty("--p", String(Math.min(1, Math.max(0, y / d.offsetHeight))));
    });
    const release = (e) => {
      if (e.pointerId !== pointer) return;
      pointer = null;
      d.classList.remove("dragging");
      const dy = e.clientY - startY;
      if (e.type !== "pointercancel" && (dy > d.offsetHeight * 0.4 || velocity > FLICK_PX_PER_MS)) {
        d.dispatchEvent(new Event("cancel", { cancelable: true }));
      } else {
        d.style.transform = ""; // snap back through the CSS transition
        d.style.removeProperty("--p");
      }
    };
    d.addEventListener("pointerup", release);
    d.addEventListener("pointercancel", release);
  }

  function safeJson(text) { try { return JSON.parse(text); } catch { return text.slice(0, 300); } }

  if ("serviceWorker" in navigator && window.isSecureContext) {
    window.addEventListener("load", () => navigator.serviceWorker.register("/sw.js").catch(() => {}));
  }
})();
