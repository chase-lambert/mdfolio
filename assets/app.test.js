"use strict";

const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const test = require("node:test");

const {
  THEMES,
  STORAGE_KEYS,
  readStorage,
  writeStorage,
  resolveAppearance,
  rootAttributes,
} = require("./app.js");

test("theme catalog has the curated supported-mode sets", () => {
  assert.deepEqual(
    THEMES.map(({ id, modes }) => [id, [...modes]]),
    [
      ["folio", ["light", "dark"]],
      ["linen", ["light", "dark"]],
      ["grove", ["light", "dark"]],
      ["nocturne", ["dark"]],
    ]
  );
});

test("unknown theme and mode values fall back safely", () => {
  const appearance = resolveAppearance("lost-theme", "sepia", true);

  assert.equal(appearance.theme.id, "folio");
  assert.equal(appearance.preferredMode, null);
  assert.equal(appearance.effectiveMode, "dark");
  assert.equal(appearance.forced, false);
});

test("explicit mode is independent of theme selection", () => {
  const linen = resolveAppearance("linen", "light", true);
  const grove = resolveAppearance("grove", "light", true);

  assert.equal(linen.preferredMode, "light");
  assert.equal(linen.effectiveMode, "light");
  assert.equal(grove.preferredMode, "light");
  assert.equal(grove.effectiveMode, "light");
});

test("Nocturne forces dark without replacing the preferred mode", () => {
  const nocturne = resolveAppearance("nocturne", "light", false);
  const folio = resolveAppearance("folio", nocturne.preferredMode, false);

  assert.equal(nocturne.effectiveMode, "dark");
  assert.equal(nocturne.preferredMode, "light");
  assert.equal(nocturne.forced, true);
  assert.equal(folio.effectiveMode, "light");
});

test("missing explicit mode follows the current system mode", () => {
  const systemLight = resolveAppearance("folio", null, false);
  const systemDark = resolveAppearance("folio", null, true);
  const explicitLight = resolveAppearance("folio", "light", true);
  const forcedDark = resolveAppearance("nocturne", "light", false);

  assert.equal(systemLight.effectiveMode, "light");
  assert.equal(systemDark.effectiveMode, "dark");
  assert.deepEqual(rootAttributes(systemLight), { theme: "folio", mode: null });
  assert.deepEqual(rootAttributes(systemDark), { theme: "folio", mode: null });
  assert.deepEqual(rootAttributes(explicitLight), {
    theme: "folio",
    mode: "light",
  });
  assert.deepEqual(rootAttributes(forcedDark), {
    theme: "nocturne",
    mode: "dark",
  });
});

test("storage failures remain non-fatal", () => {
  const storage = {
    getItem() {
      throw new Error("denied");
    },
    setItem() {
      throw new Error("denied");
    },
  };

  assert.equal(readStorage(storage, STORAGE_KEYS.theme), null);
  assert.equal(writeStorage(storage, STORAGE_KEYS.mode, "dark"), false);
  assert.equal(writeStorage(null, STORAGE_KEYS.mode, "dark"), false);
});

function themeBlock(css, selector) {
  const marker = `${selector} {`;
  const start = css.indexOf(marker);
  assert.notEqual(start, -1, `missing CSS block for ${selector}`);
  const end = css.indexOf("\n}", start);
  assert.notEqual(end, -1, `unterminated CSS block for ${selector}`);
  return css.slice(start, end);
}

function palette(css, selector, mode) {
  const tokens = new Map();
  const pattern = new RegExp(`--${mode}-([a-z-]+):\\s*(#[0-9a-f]{6});`, "gi");
  for (const match of themeBlock(css, selector).matchAll(pattern)) {
    tokens.set(match[1], match[2]);
  }
  return tokens;
}

function tokenNames(css, selector, mode) {
  const names = new Set();
  const pattern = new RegExp(`--${mode}-([a-z-]+):`, "gi");
  for (const match of themeBlock(css, selector).matchAll(pattern)) {
    names.add(match[1]);
  }
  return names;
}

function relativeLuminance(hex) {
  const channels = hex
    .slice(1)
    .match(/.{2}/g)
    .map((value) => Number.parseInt(value, 16) / 255)
    .map((value) =>
      value <= 0.04045
        ? value / 12.92
        : ((value + 0.055) / 1.055) ** 2.4
    );
  return 0.2126 * channels[0] + 0.7152 * channels[1] + 0.0722 * channels[2];
}

function contrast(first, second) {
  const lighter = Math.max(relativeLuminance(first), relativeLuminance(second));
  const darker = Math.min(relativeLuminance(first), relativeLuminance(second));
  return (lighter + 0.05) / (darker + 0.05);
}

test("every supported palette keeps text contrast at or above 4.5 to 1", () => {
  const css = fs.readFileSync(path.join(__dirname, "style.css"), "utf8");
  const supported = [
    ["Folio light", ":root", "light"],
    ["Folio dark", ":root", "dark"],
    ["Linen light", ':root[data-theme="linen"]', "light"],
    ["Linen dark", ':root[data-theme="linen"]', "dark"],
    ["Grove light", ':root[data-theme="grove"]', "light"],
    ["Grove dark", ':root[data-theme="grove"]', "dark"],
    ["Nocturne dark", ':root[data-theme="nocturne"]', "dark"],
  ];

  for (const [name, selector, mode] of supported) {
    const tokens = palette(css, selector, mode);
    for (const foreground of ["ink", "muted", "accent"]) {
      assert.ok(tokens.has(foreground), `${name} lacks ${foreground}`);
      assert.ok(
        contrast(tokens.get(foreground), tokens.get("paper")) >= 4.5,
        `${name} ${foreground} lacks 4.5:1 contrast on paper`
      );
    }
    for (const foreground of [
      "code-ink",
      "syntax-comment",
      "syntax-string",
      "syntax-keyword",
      "syntax-entity",
      "syntax-variable",
    ]) {
      assert.ok(tokens.has(foreground), `${name} lacks ${foreground}`);
      assert.ok(
        contrast(tokens.get(foreground), tokens.get("code")) >= 4.5,
        `${name} ${foreground} lacks 4.5:1 contrast on code`
      );
    }
  }
});

test("every supported palette supplies the complete visual token contract", () => {
  const css = fs.readFileSync(path.join(__dirname, "style.css"), "utf8");
  const supported = [
    [":root", "light"],
    [":root", "dark"],
    [':root[data-theme="linen"]', "light"],
    [':root[data-theme="linen"]', "dark"],
    [':root[data-theme="grove"]', "light"],
    [':root[data-theme="grove"]', "dark"],
    [':root[data-theme="nocturne"]', "dark"],
  ];
  const required = [
    "paper",
    "paper-deep",
    "ink",
    "muted",
    "faint",
    "line",
    "accent",
    "accent-soft",
    "code",
    "code-ink",
    "glow",
    "hover",
    "inline-code",
    "shadow",
    "syntax-comment",
    "syntax-string",
    "syntax-keyword",
    "syntax-entity",
    "syntax-variable",
  ];

  for (const [selector, mode] of supported) {
    const names = tokenNames(css, selector, mode);
    for (const name of required) {
      assert.ok(names.has(name), `${selector} ${mode} lacks ${name}`);
    }
  }
  for (const selector of [
    ":root",
    ':root[data-theme="linen"]',
    ':root[data-theme="grove"]',
    ':root[data-theme="nocturne"]',
  ]) {
    const block = themeBlock(css, selector);
    assert.match(block, /--serif:/);
    assert.match(block, /--sans:/);
  }

  assert.match(css, /:root\[data-mode="dark"\]\s*\{/);
  assert.match(
    css,
    /@media \(prefers-color-scheme: dark\)[\s\S]*:root:not\(\[data-mode\]\)/
  );
});
