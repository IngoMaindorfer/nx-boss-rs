document.addEventListener("click", (event) => {
  if (!(event.target instanceof Element) || event.defaultPrevented || event.button !== 0 ||
      event.metaKey || event.ctrlKey || event.shiftKey || event.altKey) {
    return;
  }
  const row = event.target.closest("tr[data-scan-href]");
  if (!row || event.target.closest("a, button, input, select, textarea")) {
    return;
  }
  if (window.getSelection()?.toString()) {
    return;
  }
  window.location.assign(row.dataset.scanHref);
});

document.addEventListener("keydown", (event) => {
  if (event.key !== "Enter" || !(event.target instanceof Element)) {
    return;
  }
  if (event.target.matches("tr[data-scan-href]")) {
    event.preventDefault();
    window.location.assign(event.target.dataset.scanHref);
  }
});
