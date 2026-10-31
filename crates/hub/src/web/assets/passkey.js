// Passkeys on the hub's own pages: make one from an owner link, or sign in with one.
// The hub checks everything; this only moves bytes between the browser's WebAuthn API and
// the hub, base64url on the wire. No other script runs on these pages.
"use strict";

(function () {
  const b64 = {
    decode(text) {
      const plain = text.replace(/-/g, "+").replace(/_/g, "/");
      const padded = plain + "=".repeat((4 - (plain.length % 4)) % 4);
      return Uint8Array.from(atob(padded), (c) => c.charCodeAt(0)).buffer;
    },
    encode(buffer) {
      const bytes = new Uint8Array(buffer);
      let text = "";
      for (const byte of bytes) text += String.fromCharCode(byte);
      return btoa(text).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
    },
  };

  async function post(path, body) {
    const response = await fetch(path, {
      method: "POST",
      credentials: "same-origin",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(body || {}),
    });
    const answer = await response.json().catch(() => ({}));
    if (!response.ok) {
      throw new Error(answer.message || "The hub did not accept that. Try again.");
    }
    return answer;
  }

  function say(text) {
    const status = document.getElementById("passkey-status");
    if (status) status.textContent = text;
  }

  function supported() {
    if (!window.PublicKeyCredential || !navigator.credentials) {
      say("This browser cannot use passkeys. Open this page in a current browser.");
      return false;
    }
    return true;
  }

  // The owner's link carries its token after "#": read it once, then take it out of the
  // address bar and the history, so it is not left behind on this device.
  // A new link opened in the same tab changes only the hash, so it is taken up then too.
  let link = "";
  function takeLink() {
    if (location.hash.length > 1) {
      link = location.hash.slice(1);
      history.replaceState(null, "", location.pathname);
    }
  }
  if (document.getElementById("passkey-register")) {
    takeLink();
    window.addEventListener("hashchange", takeLink);
  }

  async function register() {
    if (!link) {
      throw new Error("Open the link devagotchi hub owner printed; this page needs it.");
    }
    const options = await post("/passkey/register/options", { link });
    say("Making the passkey for " + options.user.displayName + "…");
    const publicKey = Object.assign({}, options, {
      challenge: b64.decode(options.challenge),
      user: Object.assign({}, options.user, { id: b64.decode(options.user.id) }),
      excludeCredentials: options.excludeCredentials.map((c) =>
        Object.assign({}, c, { id: b64.decode(c.id) })
      ),
    });
    const credential = await navigator.credentials.create({ publicKey });
    const response = credential.response;
    const key = response.getPublicKey && response.getPublicKey();
    if (!key) {
      throw new Error("This browser did not hand over the new passkey's key. Update it and try again.");
    }
    const done = await post("/passkey/register", {
      link,
      id: b64.encode(credential.rawId),
      clientDataJSON: b64.encode(response.clientDataJSON),
      authenticatorData: b64.encode(response.getAuthenticatorData()),
      publicKey: b64.encode(key),
      publicKeyAlgorithm: response.getPublicKeyAlgorithm(),
    });
    location.assign(done.next);
  }

  async function signIn(button) {
    const options = await post("/passkey/sign-in/options");
    const publicKey = Object.assign({}, options, {
      challenge: b64.decode(options.challenge),
    });
    const credential = await navigator.credentials.get({ publicKey });
    const response = credential.response;
    const done = await post("/passkey/sign-in", {
      id: b64.encode(credential.rawId),
      clientDataJSON: b64.encode(response.clientDataJSON),
      authenticatorData: b64.encode(response.authenticatorData),
      signature: b64.encode(response.signature),
      userHandle: response.userHandle ? b64.encode(response.userHandle) : null,
      returnTo: button.dataset.returnTo || "/account",
    });
    location.assign(done.next);
  }

  function wire(id, run) {
    const button = document.getElementById(id);
    if (!button) return;
    button.disabled = false;
    button.addEventListener("click", async () => {
      if (!supported()) return;
      button.disabled = true;
      say("Waiting for your passkey…");
      try {
        await run(button);
      } catch (error) {
        // Cancelling the browser's prompt is not an error worth a red line.
        say(error && error.name === "NotAllowedError"
          ? "Cancelled. Press the button to try again."
          : (error && error.message) || "That did not work. Try again.");
        button.disabled = false;
      }
    });
  }

  wire("passkey-register", register);
  wire("passkey-sign-in", signIn);
})();
