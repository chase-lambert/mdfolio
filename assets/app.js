(() => {
  "use strict";

  const THEMES = Object.freeze([
    Object.freeze({
      id: "folio",
      label: "Folio",
      modes: Object.freeze(["light", "dark"]),
    }),
    Object.freeze({
      id: "linen",
      label: "Linen",
      modes: Object.freeze(["light", "dark"]),
    }),
    Object.freeze({
      id: "grove",
      label: "Grove",
      modes: Object.freeze(["light", "dark"]),
    }),
    Object.freeze({
      id: "nocturne",
      label: "Nocturne",
      modes: Object.freeze(["dark"]),
    }),
  ]);
  const STORAGE_KEYS = Object.freeze({
    theme: "mdfolio.theme",
    mode: "mdfolio.mode",
  });

  function themeById(value) {
    return THEMES.find((theme) => theme.id === value) || THEMES[0];
  }

  function themeLabel(theme) {
    return theme.modes.length === 1
      ? `${theme.label} (${theme.modes[0]})`
      : theme.label;
  }

  function validMode(value) {
    return value === "light" || value === "dark" ? value : null;
  }

  function readStorage(storage, key) {
    try {
      return storage?.getItem(key) ?? null;
    } catch {
      return null;
    }
  }

  function writeStorage(storage, key, value) {
    if (!storage) return false;
    try {
      storage.setItem(key, value);
      return true;
    } catch {
      return false;
    }
  }

  function resolveAppearance(themeValue, modeValue, systemDark) {
    const theme = themeById(themeValue);
    const preferredMode = validMode(modeValue);
    const forced = theme.modes.length === 1;
    return {
      theme,
      preferredMode,
      forced,
      effectiveMode: forced
        ? theme.modes[0]
        : preferredMode || (systemDark ? "dark" : "light"),
    };
  }

  function rootAttributes(appearance) {
    return {
      theme: appearance.theme.id,
      mode: appearance.forced
        ? appearance.effectiveMode
        : appearance.preferredMode,
    };
  }

  function applyRootState(root, appearance) {
    const attributes = rootAttributes(appearance);
    root.dataset.theme = attributes.theme;
    if (attributes.mode) {
      root.dataset.mode = attributes.mode;
    } else {
      delete root.dataset.mode;
    }
  }

  const exported = {
    THEMES,
    STORAGE_KEYS,
    readStorage,
    writeStorage,
    resolveAppearance,
    rootAttributes,
  };
  if (typeof module !== "undefined" && module.exports) {
    module.exports = exported;
  }
  if (typeof document === "undefined" || typeof window === "undefined") {
    return;
  }

  const media = window.matchMedia("(prefers-color-scheme: dark)");
  let storage = null;
  try {
    storage = window.localStorage;
  } catch {
    // Storage is an optional enhancement.
  }

  let themeValue = readStorage(storage, STORAGE_KEYS.theme);
  let modeValue = readStorage(storage, STORAGE_KEYS.mode);
  let appearance = resolveAppearance(themeValue, modeValue, media.matches);
  themeValue = appearance.theme.id;
  modeValue = appearance.preferredMode;
  applyRootState(document.documentElement, appearance);

  function initializePage() {
    const controls = document.querySelector("[data-appearance]");
    const themePicker = document.querySelector("[data-theme-picker]");
    const themeToggle = document.querySelector("[data-theme-toggle]");
    const themeMenu = document.querySelector("[data-theme-menu]");
    const modeToggle = document.querySelector("[data-mode-toggle]");
    const modeLabel = document.querySelector("[data-mode-label]");

    function closeThemeMenu({ restoreFocus = false } = {}) {
      if (!themeMenu || !themeToggle) return;
      themeMenu.hidden = true;
      themeToggle.setAttribute("aria-expanded", "false");
      if (restoreFocus) themeToggle.focus();
    }

    function openThemeMenu() {
      if (!themeMenu || !themeToggle) return;
      themeMenu.hidden = false;
      themeToggle.setAttribute("aria-expanded", "true");
    }

    function renderAppearance() {
      appearance = resolveAppearance(themeValue, modeValue, media.matches);
      applyRootState(document.documentElement, appearance);
      if (!controls || !themeToggle || !themeMenu || !modeToggle || !modeLabel) {
        return;
      }

      themeToggle.setAttribute(
        "aria-label",
        `Theme: ${themeLabel(appearance.theme)}. Choose theme`
      );
      for (const option of themeMenu.querySelectorAll("[data-theme-option]")) {
        const current = option.dataset.themeOption === appearance.theme.id;
        option.classList.toggle("is-current", current);
        option.setAttribute("aria-pressed", String(current));
      }
      modeLabel.textContent =
        appearance.effectiveMode === "dark" ? "Dark" : "Light";
      modeToggle.disabled = appearance.forced;
      modeToggle.setAttribute(
        "aria-pressed",
        String(appearance.effectiveMode === "dark")
      );
      modeToggle.setAttribute(
        "aria-label",
        appearance.forced
          ? `${appearance.theme.label} is ${appearance.effectiveMode} only`
          : `Switch to ${appearance.effectiveMode === "dark" ? "light" : "dark"} mode`
      );
    }

    if (
      controls &&
      themePicker &&
      themeToggle &&
      themeMenu &&
      modeToggle &&
      modeLabel
    ) {
      for (const theme of THEMES) {
        const option = document.createElement("button");
        option.className = "theme-option";
        option.type = "button";
        option.dataset.themeOption = theme.id;

        const label = document.createElement("span");
        label.textContent = themeLabel(theme);
        option.append(label);

        const check = document.createElement("span");
        check.className = "theme-check";
        check.setAttribute("aria-hidden", "true");
        check.textContent = "✓";
        option.append(check);

        option.addEventListener("click", () => {
          themeValue = theme.id;
          writeStorage(storage, STORAGE_KEYS.theme, themeValue);
          closeThemeMenu({ restoreFocus: true });
          renderAppearance();
        });
        themeMenu.append(option);
      }

      themeToggle.addEventListener("click", () => {
        if (themeMenu.hidden) openThemeMenu();
        else closeThemeMenu();
      });
      themeToggle.addEventListener("keydown", (event) => {
        if (event.key !== "ArrowDown") return;
        event.preventDefault();
        openThemeMenu();
        themeMenu.querySelector(".is-current")?.focus();
      });
      themeMenu.addEventListener("keydown", (event) => {
        if (event.key === "Escape") {
          event.preventDefault();
          closeThemeMenu({ restoreFocus: true });
        }
      });
      document.addEventListener("click", (event) => {
        if (!themePicker.contains(event.target)) closeThemeMenu();
      });

      modeToggle.addEventListener("click", () => {
        if (appearance.forced) return;
        modeValue = appearance.effectiveMode === "dark" ? "light" : "dark";
        writeStorage(storage, STORAGE_KEYS.mode, modeValue);
        renderAppearance();
      });

      controls.hidden = false;
      renderAppearance();
    }

    const updateSystemMode = () => {
      if (!modeValue && !appearance.forced) renderAppearance();
    };
    if (typeof media.addEventListener === "function") {
      media.addEventListener("change", updateSystemMode);
    } else if (typeof media.addListener === "function") {
      media.addListener(updateSystemMode);
    }

    initializeFilter();
    initializeNavigation();
  }

  function initializeFilter() {
    const filter = document.querySelector("[data-filter]");
    const items = Array.from(document.querySelectorAll("[data-filter-item]"));
    if (!filter || !items.length) return;

    filter.addEventListener("input", () => {
      const query = filter.value.trim().toLocaleLowerCase();
      for (const item of items) {
        item.hidden = query !== "" && !item.dataset.filterValue.includes(query);
      }
    });

    document.addEventListener("keydown", (event) => {
      if (event.key === "/" && !event.metaKey && !event.ctrlKey && !event.altKey) {
        const tag = document.activeElement?.tagName;
        if (tag !== "INPUT" && tag !== "TEXTAREA" && tag !== "SELECT") {
          event.preventDefault();
          filter.focus();
        }
      }
    });
  }

  function initializeNavigation() {
    const toggle = document.querySelector("[data-nav-toggle]");
    const navigation = document.querySelector("[data-nav]");
    if (!toggle || !navigation) return;

    toggle.addEventListener("click", () => {
      const open = navigation.classList.toggle("is-open");
      toggle.setAttribute("aria-expanded", String(open));
    });
  }

  if (document.readyState === "loading") {
    document.addEventListener("DOMContentLoaded", initializePage, { once: true });
  } else {
    initializePage();
  }
})();
