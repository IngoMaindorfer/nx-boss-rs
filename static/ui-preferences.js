(() => {
  const key = "nx-boss-theme";
  let theme = "auto";
  try {
    const saved = localStorage.getItem(key);
    if (saved === "light" || saved === "dark") theme = saved;
  } catch (_) {
    // The browser may deny storage; system appearance still works.
  }

  const apply = (value) => {
    if (value === "light" || value === "dark") {
      document.documentElement.dataset.theme = value;
    } else {
      delete document.documentElement.dataset.theme;
    }
  };
  apply(theme);

  document.addEventListener("DOMContentLoaded", () => {
    const locale = document.documentElement.lang === "de" ? "de-DE" : "en-GB";
    const dateFormatter = new Intl.DateTimeFormat(locale, { dateStyle: "short", timeStyle: "short" });
    document.querySelectorAll("time[datetime]").forEach((element) => {
      const date = new Date(element.dateTime);
      if (!Number.isNaN(date.getTime())) element.textContent = dateFormatter.format(date);
    });

    const themeSelect = document.getElementById("ui-theme");
    if (themeSelect) {
      themeSelect.value = theme;
      themeSelect.addEventListener("change", () => {
        theme = themeSelect.value;
        apply(theme);
        try {
          if (theme === "auto") localStorage.removeItem(key);
          else localStorage.setItem(key, theme);
        } catch (_) {
          // Keep the selection for this page when storage is unavailable.
        }
      });
    }

    const languageSelect = document.getElementById("ui-language");
    languageSelect?.addEventListener("change", () => {
      const lang = languageSelect.value;
      if (lang !== "de" && lang !== "en") return;
      const next = window.location.pathname + window.location.search;
      window.location.assign(`/language/${lang}?next=${encodeURIComponent(next)}`);
    });
  });
})();
