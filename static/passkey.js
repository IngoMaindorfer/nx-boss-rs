(() => {
  'use strict';

  const decode = (value) => {
    const base64 = value.replace(/-/g, '+').replace(/_/g, '/');
    const binary = atob(base64.padEnd(Math.ceil(base64.length / 4) * 4, '='));
    return Uint8Array.from(binary, (character) => character.charCodeAt(0));
  };
  const encode = (value) => {
    const bytes = new Uint8Array(value);
    let binary = '';
    for (let offset = 0; offset < bytes.length; offset += 8192) {
      binary += String.fromCharCode(...bytes.subarray(offset, offset + 8192));
    }
    return btoa(binary).replace(/\+/g, '-').replace(/\//g, '_').replace(/=+$/, '');
  };

  const creationOptions = (challenge) => {
    const json = challenge.publicKey || challenge;
    if (PublicKeyCredential.parseCreationOptionsFromJSON) {
      return PublicKeyCredential.parseCreationOptionsFromJSON(json);
    }
    return {
      ...json,
      challenge: decode(json.challenge),
      user: { ...json.user, id: decode(json.user.id) },
      excludeCredentials: (json.excludeCredentials || []).map((item) => ({ ...item, id: decode(item.id) })),
    };
  };
  const requestOptions = (challenge) => {
    const json = challenge.publicKey || challenge;
    if (PublicKeyCredential.parseRequestOptionsFromJSON) {
      return PublicKeyCredential.parseRequestOptionsFromJSON(json);
    }
    return {
      ...json,
      challenge: decode(json.challenge),
      allowCredentials: (json.allowCredentials || []).map((item) => ({ ...item, id: decode(item.id) })),
    };
  };
  const credentialJSON = (credential, registration) => {
    if (credential.toJSON) return credential.toJSON();
    const response = registration ? {
      attestationObject: encode(credential.response.attestationObject),
      clientDataJSON: encode(credential.response.clientDataJSON),
      transports: credential.response.getTransports?.() || [],
    } : {
      authenticatorData: encode(credential.response.authenticatorData),
      clientDataJSON: encode(credential.response.clientDataJSON),
      signature: encode(credential.response.signature),
      userHandle: credential.response.userHandle ? encode(credential.response.userHandle) : null,
    };
    return { id: credential.id, rawId: encode(credential.rawId), type: credential.type, response };
  };
  const post = async (path, body = {}) => {
    const response = await fetch(path, {
      method: 'POST', credentials: 'same-origin', cache: 'no-store',
      headers: { 'Content-Type': 'application/json' }, body: JSON.stringify(body),
    });
    if (!response.ok) {
      const error = new Error('Request failed');
      error.status = response.status;
      throw error;
    }
    return response.json();
  };
  const authenticate = async (start, finish) => {
    const { flow, challenge } = await post(start);
    const credential = await navigator.credentials.get({ publicKey: requestOptions(challenge) });
    if (!credential) throw new Error('No passkey selected');
    return post(finish, { flow, credential: credentialJSON(credential, false) });
  };
  const register = async (start, finish, body = {}) => {
    const { flow, challenge } = await post(start, body);
    const credential = await navigator.credentials.create({ publicKey: creationOptions(challenge) });
    if (!credential) throw new Error('No passkey created');
    return post(finish, { flow, ...body, credential: credentialJSON(credential, true) });
  };
  const report = (error) => {
    const target = document.querySelector('[data-passkey-error]');
    if (target) {
      target.hidden = false;
      const detail = error?.name === 'NotAllowedError' ? target.dataset.errorCancel
        : error?.status === 403 || error?.name === 'SecurityError' ? target.dataset.errorOrigin
          : error?.status === 401 ? target.dataset.errorCode : null;
      target.textContent = detail || (document.documentElement.lang === 'en'
        ? 'Passkey action failed. Please try again.' : 'Passkey-Aktion fehlgeschlagen. Bitte erneut versuchen.');
    }
    // Authentication responses and credentials must never be sent to the console.
    void error;
  };

  const setupForm = document.querySelector('[data-passkey-setup]');
  const setupField = setupForm?.elements.token;
  const toggle = setupForm?.querySelector('[data-secret-toggle]');
  const hideSetupCode = () => {
    if (setupField) setupField.type = 'password';
    if (toggle) { toggle.textContent = toggle.dataset.show; toggle.setAttribute('aria-pressed', 'false'); }
  };
  if (toggle && setupField) {
    toggle.hidden = false;
    toggle.addEventListener('click', () => {
      const reveal = setupField.type === 'password';
      setupField.type = reveal ? 'text' : 'password';
      toggle.textContent = reveal ? toggle.dataset.hide : toggle.dataset.show;
      toggle.setAttribute('aria-pressed', String(reveal));
    });
  }

  setupForm?.addEventListener('submit', async (event) => {
    event.preventDefault();
    const form = event.currentTarget;
    const button = form.querySelector('button[type=submit]');
    hideSetupCode();
    button.disabled = true;
    try {
      await register('/passkey/setup/start', '/passkey/setup/finish', {
        token: form.elements.token.value.trim(), username: form.elements.username.value,
      });
      window.location.assign('/');
    } catch (error) { report(error); button.disabled = false; }
  });

  document.querySelector('[data-passkey-login]')?.addEventListener('click', async (event) => {
    const button = event.currentTarget;
    button.disabled = true;
    try {
      await authenticate('/passkey/login/start', '/passkey/login/finish');
      window.location.assign('/');
    } catch (error) { report(error); button.disabled = false; }
  });

  document.querySelector('[data-passkey-add]')?.addEventListener('click', async (event) => {
    const button = event.currentTarget;
    button.disabled = true;
    try {
      await authenticate('/passkey/stepup/start', '/passkey/stepup/finish');
      await register('/passkey/account/add/start', '/passkey/account/add/finish');
      window.location.reload();
    } catch (error) { report(error); button.disabled = false; }
  });

  document.querySelectorAll('form[data-passkey-stepup]').forEach((form) => {
    form.addEventListener('submit', async (event) => {
      if (form.dataset.passkeyConfirmed === 'yes') return;
      event.preventDefault();
      const button = form.querySelector('button[type=submit]');
      if (button) button.disabled = true;
      try {
        await authenticate('/passkey/stepup/start', '/passkey/stepup/finish');
        form.dataset.passkeyConfirmed = 'yes';
        form.requestSubmit();
      } catch (error) { report(error); if (button) button.disabled = false; }
    });
  });
})();
