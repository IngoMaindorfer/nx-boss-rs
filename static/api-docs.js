(() => {
  'use strict';
  const root = document.getElementById('api-docs-operations');
  if (!root) return;
  const de = root.dataset.lang === 'de';
  const tr = (german, english) => de ? german : english;
  const text = (tag, value) => {
    const node = document.createElement(tag);
    node.textContent = value;
    return node;
  };
  let spec;
  const expand = (value, seen = new Set()) => {
    if (Array.isArray(value)) return value.map((item) => expand(item, seen));
    if (!value || typeof value !== 'object') return value;
    if (typeof value.$ref === 'string' && value.$ref.startsWith('#/') && !seen.has(value.$ref)) {
      const resolved = value.$ref.slice(2).split('/').map((part) => part.replace(/~1/g, '/').replace(/~0/g, '~'))
        .reduce((current, part) => current?.[part], spec);
      if (resolved) return expand(resolved, new Set([...seen, value.$ref]));
    }
    return Object.fromEntries(Object.entries(value).map(([key, item]) => [key, expand(item, seen)]));
  };
  const section = (parent, label, value) => {
    if (value === undefined) return;
    const box = document.createElement('div');
    box.style.marginTop = '.75rem';
    box.append(text('strong', label));
    const code = text('pre', JSON.stringify(expand(value), null, 2));
    code.style.cssText = 'overflow:auto;padding:.75rem;background:var(--page);border-radius:6px;font-size:.8rem';
    box.append(code);
    parent.append(box);
  };

  fetch('/api/openapi.json', { credentials: 'same-origin', cache: 'no-store' })
    .then((response) => {
      if (!response.ok || !response.headers.get('content-type')?.includes('application/json')) {
        throw new Error('OpenAPI unavailable');
      }
      return response.json();
    })
    .then((apiSpec) => {
      spec = apiSpec;
      const server = apiSpec.servers?.[0];
      if (server) {
        const serverCard = document.createElement('div');
        serverCard.className = 'card';
        serverCard.append(text('h3', tr('Scanner-Endpunkt', 'Scanner endpoint')));
        serverCard.append(text('code', server.url));
        serverCard.append(text('p', server.description || ''));
        root.append(serverCard);
      }
      for (const tag of spec.tags || []) {
        const group = document.createElement('section');
        group.className = 'card';
        group.append(text('h3', tag.name));
        group.append(text('p', tag.description || ''));
        for (const [path, methods] of Object.entries(spec.paths || {})) {
          for (const [method, operation] of Object.entries(methods)) {
            if (!(operation.tags || []).includes(tag.name)) continue;
            const detail = document.createElement('details');
            detail.style.cssText = 'border-top:1px solid var(--border);padding:.75rem 0';
            const summary = document.createElement('summary');
            summary.style.cursor = 'pointer';
            const verb = text('strong', method.toUpperCase());
            verb.style.cssText = 'display:inline-block;min-width:5rem;color:var(--heading)';
            summary.append(verb, text('code', path), text('span', ` — ${operation.summary || ''}`));
            detail.append(summary);
            if (operation.description) detail.append(text('p', operation.description));
            detail.append(text('p', operation.security?.length ? tr('Sitzung: Bearer bei userpass', 'Session: bearer in userpass mode') : tr('Keine Sitzung nötig', 'No session needed')));
            section(detail, tr('Parameter', 'Parameters'), operation.parameters);
            section(detail, tr('Anfrage', 'Request'), operation.requestBody);
            section(detail, tr('Antworten', 'Responses'), operation.responses);
            group.append(detail);
          }
        }
        root.append(group);
      }
    })
    .catch(() => root.append(text('p', tr('OpenAPI konnte nicht geladen werden.', 'Could not load OpenAPI.'))));
})();
