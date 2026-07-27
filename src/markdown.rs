use std::{
    borrow::Cow,
    collections::HashSet,
    path::{Path, PathBuf},
    sync::Arc,
};

use ammonia::Builder;
use comrak::{
    Options, markdown_to_html_with_plugins, options::Plugins, plugins::syntect::SyntectAdapter,
};

use crate::{
    catalog::{Catalog, Document},
    pathing::{APP_PREFIX, asset_url, document_url, missing_url, normalize_library_path},
};

const IMAGE_EXTENSIONS: [&str; 7] = ["png", "jpg", "jpeg", "gif", "svg", "webp", "avif"];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AssetKind {
    Image,
    Pdf,
}

#[derive(Debug)]
pub struct MarkdownRenderer {
    highlighter: SyntectAdapter,
}

#[derive(Clone, Debug)]
struct LinkResolver {
    catalog: Arc<Catalog>,
    current_directory: PathBuf,
}

impl MarkdownRenderer {
    #[must_use]
    pub fn new() -> Self {
        Self {
            highlighter: SyntectAdapter::new(None),
        }
    }

    #[must_use]
    pub fn render(&self, markdown: &str, document: &Document, catalog: Arc<Catalog>) -> String {
        let resolver = Arc::new(LinkResolver {
            catalog,
            current_directory: document
                .relative_path
                .parent()
                .unwrap_or_else(|| Path::new(""))
                .to_path_buf(),
        });

        let mut options = Options::default();
        options.extension.strikethrough = true;
        options.extension.tagfilter = true;
        options.extension.table = true;
        options.extension.autolink = true;
        options.extension.tasklist = true;
        options.extension.footnotes = true;
        options.extension.description_lists = true;
        options.extension.header_id_prefix = Some(String::new());
        options.render.r#unsafe = true;
        options.parse.smart = true;

        let link_resolver = Arc::clone(&resolver);
        options.extension.link_url_rewriter =
            Some(Arc::new(move |url: &str| link_resolver.rewrite_link(url)));
        let image_resolver = Arc::clone(&resolver);
        options.extension.image_url_rewriter =
            Some(Arc::new(move |url: &str| image_resolver.rewrite_image(url)));

        let mut plugins = Plugins::default();
        plugins.render.codefence_syntax_highlighter = Some(&self.highlighter);
        let rendered = markdown_to_html_with_plugins(markdown, &options, &plugins);

        sanitize_html(&rendered, resolver)
    }
}

impl Default for MarkdownRenderer {
    fn default() -> Self {
        Self::new()
    }
}

impl LinkResolver {
    fn rewrite_link(&self, url: &str) -> String {
        if url.starts_with(APP_PREFIX) || url.starts_with('#') {
            return url.to_owned();
        }
        if let Some(scheme) = external_scheme(url) {
            if ["http", "https", "mailto"]
                .iter()
                .any(|allowed| scheme.eq_ignore_ascii_case(allowed))
            {
                return url.to_owned();
            }
            return String::new();
        }
        if url.starts_with("//") {
            return String::new();
        }

        let (path_and_query, fragment) = split_fragment(url);
        let path = path_and_query.split('?').next().unwrap_or_default();
        if path.is_empty() {
            return fragment
                .map(|fragment| format!("#{fragment}"))
                .unwrap_or_default();
        }
        let Some(target) = normalize_library_path(&self.current_directory, path) else {
            return String::new();
        };

        let mut rewritten = if let Some(document) = self.catalog.resolve_document_target(&target) {
            document_url(&document.relative_path)
        } else if is_pdf(&target) || is_image(&target) {
            asset_url(&target)
        } else {
            missing_url(&target)
        };
        if let Some(fragment) = fragment {
            rewritten.push('#');
            rewritten.push_str(fragment);
        }
        rewritten
    }

    fn rewrite_image(&self, url: &str) -> String {
        if url.starts_with(APP_PREFIX) {
            return url.to_owned();
        }
        if let Some(scheme) = external_scheme(url) {
            if ["http", "https"]
                .iter()
                .any(|allowed| scheme.eq_ignore_ascii_case(allowed))
            {
                return url.to_owned();
            }
            return String::new();
        }
        if url.starts_with("//") {
            return String::new();
        }

        let path = split_fragment(url).0.split('?').next().unwrap_or_default();
        let Some(target) = normalize_library_path(&self.current_directory, path) else {
            return String::new();
        };
        if is_image(&target) {
            asset_url(&target)
        } else {
            String::new()
        }
    }
}

fn sanitize_html(rendered: &str, resolver: Arc<LinkResolver>) -> String {
    let mut schemes = HashSet::new();
    schemes.extend(["http", "https", "mailto"]);

    let mut builder = Builder::default();
    builder
        .url_schemes(schemes)
        .add_tags(&["details", "summary", "kbd", "input"])
        .add_generic_attributes(&["class", "id"])
        .add_tag_attributes("input", &["type", "checked", "disabled"])
        .add_tag_attributes("img", &["width", "height", "align", "loading"])
        .attribute_filter(move |element, attribute, value| {
            let rewritten = match (element, attribute) {
                ("a", "href") => resolver.rewrite_link(value),
                ("img", "src") => resolver.rewrite_image(value),
                _ => return Some(Cow::Borrowed(value)),
            };
            (!rewritten.is_empty()).then_some(Cow::Owned(rewritten))
        });
    builder.clean(rendered).to_string()
}

fn external_scheme(url: &str) -> Option<&str> {
    let colon = url.find(':')?;
    let slash = url.find('/').unwrap_or(usize::MAX);
    let hash = url.find('#').unwrap_or(usize::MAX);
    let question = url.find('?').unwrap_or(usize::MAX);
    if colon > slash.min(hash).min(question) {
        return None;
    }
    let scheme = &url[..colon];
    (!scheme.is_empty()
        && scheme.bytes().enumerate().all(|(index, byte)| {
            byte.is_ascii_alphabetic()
                || (index > 0 && (byte.is_ascii_digit() || matches!(byte, b'+' | b'-' | b'.')))
        }))
    .then_some(scheme)
}

fn split_fragment(url: &str) -> (&str, Option<&str>) {
    url.split_once('#')
        .map_or((url, None), |(path, fragment)| (path, Some(fragment)))
}

fn is_image(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            IMAGE_EXTENSIONS
                .iter()
                .any(|allowed| extension.eq_ignore_ascii_case(allowed))
        })
}

fn is_pdf(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("pdf"))
}

#[must_use]
pub fn asset_kind(path: &Path) -> Option<AssetKind> {
    if is_image(path) {
        Some(AssetKind::Image)
    } else if is_pdf(path) {
        Some(AssetKind::Pdf)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use std::{fs, path::Path, sync::Arc};

    use tempfile::TempDir;

    use super::MarkdownRenderer;
    use crate::catalog::Catalog;

    fn fixture() -> (TempDir, Arc<Catalog>) {
        let temp = TempDir::new().unwrap();
        fs::create_dir(temp.path().join(".git")).unwrap();
        fs::write(temp.path().join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
        fs::create_dir(temp.path().join("docs")).unwrap();
        fs::write(temp.path().join("README.md"), "# Home").unwrap();
        fs::write(temp.path().join("guide.md"), "# Guide").unwrap();
        fs::write(temp.path().join("Caps.MD"), "# Capitals").unwrap();
        fs::write(temp.path().join("docs/README.md"), "# Docs").unwrap();
        let catalog = Arc::new(Catalog::scan(temp.path()).unwrap());
        (temp, catalog)
    }

    #[test]
    fn renders_gfm_highlighting_and_sanitizes_raw_html() {
        let (_temp, catalog) = fixture();
        let document = catalog.document_by_path(Path::new("README.md")).unwrap();
        let html = MarkdownRenderer::new().render(
            "- [x] done\n\n|a|b|\n|-|-|\n|1|2|\n\n```rust\nfn main() {}\n```\n\n<script>alert(1)</script>",
            document,
            Arc::clone(&catalog),
        );

        assert!(html.contains("type=\"checkbox\""));
        assert!(html.contains("<table>"));
        assert!(html.contains("syntax-highlighting"));
        assert!(!html.contains("<script"));
    }

    #[test]
    fn rewrites_extensionless_directory_and_root_relative_links() {
        let (_temp, catalog) = fixture();
        let document = catalog.document_by_path(Path::new("README.md")).unwrap();
        let html = MarkdownRenderer::new().render(
            "[guide](guide) [caps](Caps) [docs](docs/) [home](/README.md)",
            document,
            Arc::clone(&catalog),
        );

        assert!(html.contains("href=\"/_mdfolio/doc/guide.md\""));
        assert!(html.contains("href=\"/_mdfolio/doc/Caps.MD\""));
        assert!(html.contains("href=\"/_mdfolio/doc/docs/README.md\""));
        assert!(html.contains("href=\"/_mdfolio/doc/README.md\""));
    }

    #[test]
    fn allows_web_images_and_blocks_dangerous_schemes() {
        let (_temp, catalog) = fixture();
        let document = catalog.document_by_path(Path::new("README.md")).unwrap();
        let html = MarkdownRenderer::new().render(
            "![badge](HTTPS://img.shields.io/test.svg)\n\n[bad](javascript:alert(1))\n\n![local](images/cover.png)",
            document,
            Arc::clone(&catalog),
        );

        assert!(html.contains("src=\"HTTPS://img.shields.io/test.svg\""));
        assert!(html.contains("src=\"/_mdfolio/asset/images/cover.png\""));
        assert!(!html.contains("javascript:"));
    }

    #[test]
    fn traversal_and_denied_attachments_do_not_become_asset_routes() {
        let (_temp, catalog) = fixture();
        let document = catalog.document_by_path(Path::new("README.md")).unwrap();
        let html = MarkdownRenderer::new().render(
            "[escape](../secret.pdf) [html](page.html)",
            document,
            Arc::clone(&catalog),
        );

        assert!(!html.contains("secret.pdf"));
        assert!(html.contains("/_mdfolio/missing/page.html"));
    }
}
