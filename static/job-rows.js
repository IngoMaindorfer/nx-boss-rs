document.addEventListener("DOMContentLoaded", () => {
  const table = document.getElementById("jobs-table");
  if (!table) return;

  const tbody = table.tBodies[0];
  const status = document.getElementById("job-order-status");
  const rows = () => Array.from(tbody.querySelectorAll("tr.job-row"));
  const order = () => rows().map((row) => Number(row.dataset.jobId));
  let savedOrder = order();
  let dragging = null;
  let saving = false;
  let suppressClick = false;

  const clearDragStyles = () => {
    rows().forEach((row) => row.classList.remove("is-dragging", "drag-before", "drag-after"));
  };

  const restoreOrder = () => {
    const byId = new Map(rows().map((row) => [Number(row.dataset.jobId), row]));
    savedOrder.forEach((id) => tbody.append(byId.get(id)));
  };

  const saveOrder = async () => {
    const nextOrder = order();
    if (nextOrder.every((id, index) => id === savedOrder[index])) return;
    saving = true;
    table.setAttribute("aria-busy", "true");
    try {
      const response = await fetch("/jobs/reorder", {
        method: "POST",
        credentials: "same-origin",
        headers: { "Content-Type": "application/json", "HX-Request": "true" },
        body: JSON.stringify({ expected_order: savedOrder, order: nextOrder }),
      });
      if (response.status === 409) {
        status.textContent = table.dataset.reorderConflict;
        window.location.reload();
        return;
      }
      if (!response.ok) throw new Error(`HTTP ${response.status}`);
      savedOrder = nextOrder;
      status.textContent = table.dataset.reorderSaved;
    } catch (_) {
      restoreOrder();
      status.textContent = table.dataset.reorderFailed;
    } finally {
      saving = false;
      table.removeAttribute("aria-busy");
    }
  };

  tbody.addEventListener("dragstart", (event) => {
    if (saving || !(event.target instanceof Element) ||
        !event.target.closest(".drag-handle")) {
      event.preventDefault();
      return;
    }
    dragging = event.target.closest("tr.job-row");
    dragging.classList.add("is-dragging");
    event.dataTransfer.effectAllowed = "move";
    event.dataTransfer.setData("text/plain", dragging.dataset.jobId);
  });

  tbody.addEventListener("dragover", (event) => {
    if (!dragging || !(event.target instanceof Element)) return;
    const target = event.target.closest("tr.job-row");
    if (!target || target === dragging) return;
    event.preventDefault();
    event.dataTransfer.dropEffect = "move";
    clearDragStyles();
    dragging.classList.add("is-dragging");
    const currentRows = rows();
    target.classList.add(currentRows.indexOf(dragging) < currentRows.indexOf(target)
      ? "drag-after" : "drag-before");
  });

  tbody.addEventListener("drop", (event) => {
    if (!dragging || saving || !(event.target instanceof Element)) return;
    const target = event.target.closest("tr.job-row");
    if (!target || target === dragging) return;
    event.preventDefault();
    const currentRows = rows();
    if (currentRows.indexOf(dragging) < currentRows.indexOf(target)) {
      target.after(dragging);
    } else {
      target.before(dragging);
    }
    dragging = null;
    clearDragStyles();
    suppressClick = true;
    setTimeout(() => { suppressClick = false; }, 0);
    void saveOrder();
  });

  tbody.addEventListener("dragend", () => {
    dragging = null;
    clearDragStyles();
  });

  tbody.addEventListener("keydown", (event) => {
    if (!(event.target instanceof Element)) return;
    if (event.target.matches(".drag-handle") &&
        (event.key === "ArrowUp" || event.key === "ArrowDown")) {
      event.preventDefault();
      if (saving) return;
      const row = event.target.closest("tr.job-row");
      const neighbor = event.key === "ArrowUp" ? row.previousElementSibling : row.nextElementSibling;
      if (!neighbor) return;
      if (event.key === "ArrowUp") neighbor.before(row);
      else neighbor.after(row);
      void saveOrder();
    } else if (event.key === "Enter" && event.target.matches("tr.job-row")) {
      event.preventDefault();
      if (!saving) window.location.assign(event.target.dataset.jobHref);
    }
  });

  tbody.addEventListener("click", (event) => {
    if (!(event.target instanceof Element) || event.defaultPrevented || event.button !== 0 ||
        event.metaKey || event.ctrlKey || event.shiftKey || event.altKey || saving || suppressClick) {
      return;
    }
    const row = event.target.closest("tr.job-row");
    if (!row || event.target.closest("a, button, input, select, textarea")) return;
    if (window.getSelection()?.toString()) return;
    window.location.assign(row.dataset.jobHref);
  });
});
