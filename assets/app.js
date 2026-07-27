(() => {
  const filter = document.querySelector("[data-filter]");
  const items = Array.from(document.querySelectorAll("[data-filter-item]"));

  if (filter && items.length) {
    filter.addEventListener("input", () => {
      const query = filter.value.trim().toLocaleLowerCase();
      for (const item of items) {
        item.hidden = query !== "" && !item.dataset.filterValue.includes(query);
      }
    });

    document.addEventListener("keydown", (event) => {
      if (event.key === "/" && !event.metaKey && !event.ctrlKey && !event.altKey) {
        const tag = document.activeElement?.tagName;
        if (tag !== "INPUT" && tag !== "TEXTAREA") {
          event.preventDefault();
          filter.focus();
        }
      }
    });
  }

  const toggle = document.querySelector("[data-nav-toggle]");
  const navigation = document.querySelector("[data-nav]");
  if (toggle && navigation) {
    toggle.addEventListener("click", () => {
      const open = navigation.classList.toggle("is-open");
      toggle.setAttribute("aria-expanded", String(open));
    });
  }

  const currentDocument = document.body.dataset.document;
  const assetPrefix = "/_mdfolio/asset/";
  const currentAssets = new Set(
    Array.from(
      document.querySelectorAll(
        ".markdown-body img[src], .markdown-body a[href]"
      )
    ).flatMap((element) => {
      const value = element.getAttribute("src") || element.getAttribute("href");
      if (!value) return [];
      const url = new URL(value, window.location.href);
      if (url.origin !== window.location.origin || !url.pathname.startsWith(assetPrefix)) {
        return [];
      }
      try {
        return [decodeURIComponent(url.pathname.slice(assetPrefix.length))];
      } catch {
        return [];
      }
    })
  );
  const events = new EventSource("/_mdfolio/events");
  events.addEventListener("reload", (event) => {
    const change = JSON.parse(event.data);
    if (
      change.kind === "catalog" ||
      (change.kind === "asset" &&
        change.paths.some((path) => currentAssets.has(path))) ||
      (change.kind === "documents" && change.paths.includes(currentDocument))
    ) {
      window.location.reload();
    }
  });
})();
