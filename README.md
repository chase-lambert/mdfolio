# mdfolio

`mdfolio` is a quiet local reader for the Markdown already in your
repositories. Point it at one repository to open its README. Point it at a
directory of repositories to browse them as a shelf.

It does not import, reorganize, or generate documentation. The filesystem is
the catalog.

![mdfolio reading a repository README](assets/mdfolio-reader.png)

## Install

Build and install the current checkout with Cargo:

```sh
cargo install --path .
```

The minimum supported Rust version is 1.88.

## Use

Run inside a repository:

```sh
mdfolio
```

Open a repository shelf:

```sh
mdfolio ~/projects
```

By default, `mdfolio` binds to an available loopback port and opens the
browser. Keep the browser closed or choose a stable port when needed:

```sh
mdfolio ~/projects --no-open
mdfolio ~/projects --port 4040
```

On Linux, `mdfolio` opens the browser with `xdg-open`. If `xdg-open` is
unavailable or fails, `mdfolio` still prints the local URL for manual opening.
The server stays in the foreground until `Ctrl-C`.

## Appearance

Use the controls in the page header to choose a theme and switch between light
and dark modes. The built-in themes are:

- **Folio** — warm paper and umber.
- **Linen** — airy neutrals and cool blue.
- **Grove** — mineral and botanical tones.
- **Nocturne (dark)** — a dark-only indigo theme.

The browser stores the theme and the mode independently. A dark-only theme
uses dark mode while it is active. It does not change the preferred mode. A
light-and-dark theme restores that preferred mode. Until you choose a mode,
`mdfolio` follows the operating-system preference.

The browser scopes storage to the complete local address, including its port.
The default available port can change between launches. Use a stable
`--port`, such as `--port 4040`, to keep one remembered appearance across
restarts.

## Reading model

- `mdfolio` discovers `.md` and `.markdown` files recursively and matches them
  case-insensitively.
- Git boundaries group documents into repositories. One collection opens
  directly. Multiple repositories or loose documents open the shelf.
- The selected directory is a strict library root. A repository subdirectory
  does not import its parent `.git` or files. That subtree appears as one
  loose folio.
- A repository opens `README.md`, then a root `index.md`, then its shallowest
  alphabetical Markdown path. Matching is case-insensitive and deterministic.
- `mdfolio` honors repository-local `.gitignore`, `.git/info/exclude`, and
  `.ignore` files. It always excludes common dependency and build caches.
- Hidden authoring directories such as `.agents` stay visible unless ignored.
- Refresh the browser to pick up Markdown edits and current filesystem
  membership. `mdfolio` also rescans when it opens the library, serves the
  shelf or a reader, or revisits a previously missing document. It does not
  watch files or refresh pages in the background.
- The shelf and page list filter by repository name, title, and path. Press
  `/` outside a form field to focus the filter.

## Markdown and links

`mdfolio` renders CommonMark and common GitHub-flavored features: tables, task
lists, strikethrough, footnotes, description lists, heading anchors, and
server-side syntax highlighting. Fenced languages outside Syntect's built-in
syntax set remain readable as plain code.

Relative Markdown links resolve through the catalog. `mdfolio` supports
extensionless links, directory links, scan-root-relative links, and heading
fragments. Relative images can use PNG, JPEG, GIF, SVG, WebP, or AVIF. PDF is
the only other local attachment type, and the browser downloads it as a file.

`mdfolio` allows external HTTP(S) links and images and `mailto:` links. It
blocks `javascript:`, `file:`, `data:`, arbitrary local attachments, and MDX
execution.

## Local security boundary

`mdfolio` is read-only and binds only to `127.0.0.1`. Document routes require
catalog membership. `mdfolio` canonicalizes asset paths beneath the selected
root, sanitizes raw HTML, and sandboxes SVG responses. It does not scan
symlinked directories. An in-root symlinked image or PDF can be served.
`mdfolio` rejects an out-of-root target.

`mdfolio` skips paths with invalid UTF-8 and prints a terminal warning. Such
paths cannot have an unambiguous browser URL.

Markdown pages have a 16 MiB limit. The limit bounds the rendering cost of
concurrent browser requests. `mdfolio` streams allowed images and PDFs in
64 KiB chunks. It does not load whole files into memory.

## Development

```sh
cargo fmt -- --check
cargo test
cargo clippy --all-targets --all-features -- -D warnings
node --check assets/app.js
node --test assets/app.test.js
git diff --check
cargo build --release
```

## Architecture

Each direct runtime dependency has one narrow job:

- `comrak` parses Markdown and integrates Syntect highlighting. `ammonia`
  sanitizes the resulting HTML. `askama` escapes and renders the application
  shell.
- `axum` serves loopback HTTP on Tokio. The build enables only the `http1` and
  `tokio` features of Axum. Tokio enables only the filesystem, I/O, macros,
  networking, current-thread runtime, and signal support that the binary uses.
- `ignore` applies Git-compatible discovery rules on each catalog scan.
- `async-stream` expresses bounded asset streams. `percent-encoding` makes
  browser paths unambiguous. `thiserror` defines the catalog and server error
  boundaries.

The catalog in `src/catalog.rs` is the canonical data core. HTTP and rendering
code address documents by stable root-relative paths rather than scan-order
IDs. Each shelf or reader request owns a fresh catalog scan. The application
state retains only the canonical library root and the Markdown renderer.
