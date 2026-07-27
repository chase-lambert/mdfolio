use std::{
    convert::Infallible,
    io,
    path::{Path as FsPath, PathBuf},
    sync::Arc,
    time::Duration,
};

use askama::Template;
use axum::{
    Router,
    body::{Body, Bytes},
    extract::{Path, Request, State},
    http::{
        HeaderValue, StatusCode,
        header::{
            CACHE_CONTROL, CONTENT_DISPOSITION, CONTENT_SECURITY_POLICY, CONTENT_TYPE,
            X_CONTENT_TYPE_OPTIONS,
        },
    },
    middleware::{self, Next},
    response::{
        Html, IntoResponse, Redirect, Response,
        sse::{Event, KeepAlive, Sse},
    },
    routing::get,
};
use serde::Serialize;
use thiserror::Error;
use tokio::{
    io::AsyncReadExt,
    sync::{RwLock, broadcast, watch},
};

use crate::{
    catalog::{Catalog, DocumentId, Landing},
    markdown::{AssetKind, MarkdownRenderer, asset_kind},
    pathing::{document_url, normalize_library_path},
};

const STYLE: &str = include_str!("../assets/style.css");
const SCRIPT: &str = include_str!("../assets/app.js");
const FAVICON: &str = include_str!("../assets/favicon.svg");
const MAX_MARKDOWN_BYTES: u64 = 16 * 1024 * 1024;
const ASSET_CHUNK_BYTES: usize = 64 * 1024;

#[derive(Clone)]
pub struct AppState {
    catalog: Arc<RwLock<Arc<Catalog>>>,
    renderer: Arc<MarkdownRenderer>,
    reloads: broadcast::Sender<ReloadEvent>,
    shutdown: watch::Sender<bool>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ReloadEvent {
    kind: &'static str,
    paths: Vec<String>,
}

#[derive(Template)]
#[template(path = "shelf.html")]
struct ShelfTemplate {
    page_title: String,
    body_class: &'static str,
    current_document: &'static str,
    root_path: String,
    collections: Vec<CollectionView>,
    show_filter: bool,
    empty: bool,
    has_diagnostics: bool,
}

#[derive(Debug)]
struct CollectionView {
    name: String,
    qualifier: String,
    has_qualifier: bool,
    href: String,
    filter_value: String,
}

#[derive(Template)]
#[template(path = "reader.html")]
struct ReaderTemplate {
    page_title: String,
    body_class: &'static str,
    current_document: String,
    library_name: String,
    show_shelf_link: bool,
    navigation: Vec<NavigationView>,
    document_path: String,
    article_html: String,
}

#[derive(Debug)]
struct NavigationView {
    title: String,
    path: String,
    href: String,
    filter_value: String,
    current: bool,
}

#[derive(Template)]
#[template(path = "error.html")]
struct ErrorTemplate {
    page_title: String,
    body_class: &'static str,
    current_document: &'static str,
    status: String,
    heading: String,
    message: String,
}

#[derive(Debug, Error)]
enum MarkdownReadError {
    #[error("{0}")]
    Io(#[from] std::io::Error),
    #[error("this Markdown page exceeds the 16 MiB reading limit")]
    TooLarge,
    #[error("this Markdown page is not valid UTF-8")]
    InvalidUtf8,
}

impl AppState {
    #[must_use]
    pub fn new(catalog: Catalog) -> Self {
        let (reloads, _) = broadcast::channel(64);
        let (shutdown, _) = watch::channel(false);
        Self {
            catalog: Arc::new(RwLock::new(Arc::new(catalog))),
            renderer: Arc::new(MarkdownRenderer::new()),
            reloads,
            shutdown,
        }
    }

    pub async fn catalog(&self) -> Arc<Catalog> {
        let catalog = self.catalog.read().await;
        Arc::clone(&*catalog)
    }

    pub async fn replace_catalog(&self, catalog: Catalog) -> bool {
        let catalog = Arc::new(catalog);
        let changed = {
            let current = self.catalog.read().await;
            current.fingerprint() != catalog.fingerprint()
        };
        *self.catalog.write().await = catalog;
        changed
    }

    pub fn notify(&self, event: ReloadEvent) {
        let _ = self.reloads.send(event);
    }

    #[must_use]
    pub fn subscribe(&self) -> broadcast::Receiver<ReloadEvent> {
        self.reloads.subscribe()
    }

    pub fn begin_shutdown(&self) {
        let _ = self.shutdown.send(true);
    }

    #[must_use]
    pub fn subscribe_shutdown(&self) -> watch::Receiver<bool> {
        self.shutdown.subscribe()
    }
}

impl ReloadEvent {
    #[must_use]
    pub fn catalog() -> Self {
        Self {
            kind: "catalog",
            paths: Vec::new(),
        }
    }

    #[must_use]
    pub fn documents(paths: Vec<String>) -> Self {
        Self {
            kind: "documents",
            paths,
        }
    }

    #[must_use]
    pub fn asset(paths: Vec<String>) -> Self {
        Self {
            kind: "asset",
            paths,
        }
    }

    #[must_use]
    pub fn kind(&self) -> &str {
        self.kind
    }

    #[must_use]
    pub fn paths(&self) -> &[String] {
        &self.paths
    }
}

pub fn app(state: AppState) -> Router {
    Router::new()
        .route("/", get(root_redirect))
        .route("/_mdfolio", get(library_landing))
        .route("/_mdfolio/", get(library_landing))
        .route("/_mdfolio/shelf", get(shelf))
        .route("/_mdfolio/doc/{*path}", get(reader))
        .route("/_mdfolio/asset/{*path}", get(asset))
        .route("/_mdfolio/missing/{*path}", get(missing))
        .route("/_mdfolio/events", get(events))
        .route("/_mdfolio/static/style.css", get(style))
        .route("/_mdfolio/static/app.js", get(script))
        .route("/_mdfolio/static/favicon.svg", get(favicon))
        .fallback(not_found)
        .layer(middleware::from_fn(security_headers))
        .with_state(state)
}

async fn root_redirect() -> Redirect {
    Redirect::temporary("/_mdfolio/")
}

async fn library_landing(State(state): State<AppState>) -> Response {
    let catalog = state.catalog().await;
    match catalog.landing() {
        Landing::Shelf => render_shelf(&catalog),
        Landing::Document(id) => catalog.document(id).map_or_else(
            || {
                render_error(
                    StatusCode::NOT_FOUND,
                    "Page not found",
                    "The opening page disappeared.",
                )
            },
            |document| Redirect::temporary(&document_url(&document.relative_path)).into_response(),
        ),
    }
}

async fn shelf(State(state): State<AppState>) -> Response {
    let catalog = state.catalog().await;
    render_shelf(&catalog)
}

fn render_shelf(catalog: &Catalog) -> Response {
    let mut collections = Vec::new();
    for repository in catalog.repositories() {
        if let Some(document) = repository
            .default_document
            .and_then(|id| catalog.document(id))
        {
            let qualifier = repository.qualifier.clone().unwrap_or_default();
            collections.push(CollectionView {
                name: repository.name.clone(),
                has_qualifier: !qualifier.is_empty(),
                filter_value: format!(
                    "{} {}",
                    repository.name.to_ascii_lowercase(),
                    repository
                        .root_relative
                        .to_string_lossy()
                        .to_ascii_lowercase()
                ),
                qualifier,
                href: document_url(&document.relative_path),
            });
        }
    }
    if let Some(document) = catalog.loose_default_document() {
        collections.push(CollectionView {
            name: "Loose folios".to_owned(),
            qualifier: String::new(),
            has_qualifier: false,
            href: document_url(&document.relative_path),
            filter_value: "loose folios".to_owned(),
        });
    }

    render_template(
        StatusCode::OK,
        ShelfTemplate {
            page_title: "mdfolio — repository shelf".to_owned(),
            body_class: "shelf-page",
            current_document: "",
            root_path: catalog.root().to_string_lossy().into_owned(),
            show_filter: collections.len() > 4,
            empty: collections.is_empty(),
            collections,
            has_diagnostics: !catalog.diagnostics().is_empty(),
        },
    )
}

async fn reader(State(state): State<AppState>, Path(path): Path<String>) -> Response {
    let catalog = state.catalog().await;
    let Some(relative_path) = normalize_library_path(FsPath::new(""), &path) else {
        return render_error(
            StatusCode::NOT_FOUND,
            "Page not found",
            "That path leaves the selected library.",
        );
    };
    let Some(document) = catalog.document_by_path(&relative_path).cloned() else {
        return render_error(
            StatusCode::NOT_FOUND,
            "Page not found",
            "That Markdown file is not in this folio.",
        );
    };
    let Some(canonical_path) = confined_path(catalog.root(), &relative_path).await else {
        return render_error(
            StatusCode::NOT_FOUND,
            "Page unavailable",
            "The file moved or is no longer readable beneath this library.",
        );
    };
    let markdown = match read_markdown(&canonical_path).await {
        Ok(markdown) => markdown,
        Err(error) => {
            return render_error(
                error.status(),
                "Page unavailable",
                &format!("The Markdown file could not be read: {error}"),
            );
        }
    };

    let navigation = navigation_for(&catalog, document.id);
    let library_name = document
        .repository
        .and_then(|id| catalog.repositories().get(id))
        .map_or_else(
            || "Loose folios".to_owned(),
            |repository| repository.name.clone(),
        );
    let show_shelf_link = collection_count(&catalog) > 1;
    let renderer = Arc::clone(&state.renderer);
    let render_catalog = Arc::clone(&catalog);
    let render_document = document.clone();
    let article_html = match tokio::task::spawn_blocking(move || {
        renderer.render(&markdown, &render_document, render_catalog)
    })
    .await
    {
        Ok(html) => html,
        Err(error) => {
            return render_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Rendering failed",
                &format!("The page renderer stopped unexpectedly: {error}"),
            );
        }
    };

    render_template(
        StatusCode::OK,
        ReaderTemplate {
            page_title: format!("{} — mdfolio", document.title),
            body_class: "reader-page",
            current_document: document.relative_path.to_string_lossy().into_owned(),
            library_name,
            show_shelf_link,
            navigation,
            document_path: document.relative_path.to_string_lossy().into_owned(),
            article_html,
        },
    )
}

async fn asset(State(state): State<AppState>, Path(path): Path<String>) -> Response {
    let Some(relative_path) = normalize_library_path(FsPath::new(""), &path) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let Some(kind) = asset_kind(&relative_path) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let catalog = state.catalog().await;
    let Some(canonical_path) = confined_path(catalog.root(), &relative_path).await else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let mut file = match tokio::fs::File::open(&canonical_path).await {
        Ok(file) => file,
        Err(_) => return StatusCode::NOT_FOUND.into_response(),
    };

    let mime = mime_guess::from_path(&relative_path).first_or_octet_stream();
    let stream = async_stream::stream! {
        let mut buffer = vec![0; ASSET_CHUNK_BYTES];
        loop {
            match file.read(&mut buffer).await {
                Ok(0) => break,
                Ok(read) => yield Ok::<Bytes, io::Error>(Bytes::copy_from_slice(&buffer[..read])),
                Err(error) => {
                    yield Err(error);
                    break;
                }
            }
        }
    };
    let mut response = Response::new(Body::from_stream(stream));
    response
        .headers_mut()
        .insert(CONTENT_TYPE, HeaderValue::from_str(mime.as_ref()).unwrap());
    response
        .headers_mut()
        .insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    if kind == AssetKind::Pdf {
        let filename = relative_path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("attachment.pdf")
            .replace(['"', '\\'], "_");
        if let Ok(value) = HeaderValue::from_str(&format!("attachment; filename=\"{filename}\"")) {
            response.headers_mut().insert(CONTENT_DISPOSITION, value);
        }
    } else if relative_path
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("svg"))
    {
        response.headers_mut().insert(
            CONTENT_SECURITY_POLICY,
            HeaderValue::from_static("sandbox; default-src 'none'; style-src 'unsafe-inline'"),
        );
    }
    response
}

async fn missing(Path(path): Path<String>) -> Response {
    render_error(
        StatusCode::NOT_FOUND,
        "Page not found",
        &format!("No readable Markdown or allowed attachment matched “{path}”."),
    )
}

async fn events(State(state): State<AppState>) -> Response {
    let mut receiver = state.reloads.subscribe();
    let mut shutdown = state.subscribe_shutdown();
    let stream = async_stream::stream! {
        if *shutdown.borrow() {
            return;
        }
        loop {
            tokio::select! {
                result = receiver.recv() => {
                    match result {
                        Ok(change) => {
                            let event = Event::default()
                                .event("reload")
                                .json_data(change)
                                .expect("reload events serialize");
                            yield Ok::<Event, Infallible>(event);
                        }
                        Err(broadcast::error::RecvError::Lagged(_)) => {
                            let event = Event::default()
                                .event("reload")
                                .json_data(ReloadEvent::catalog())
                                .expect("reload events serialize");
                            yield Ok::<Event, Infallible>(event);
                        }
                        Err(broadcast::error::RecvError::Closed) => break,
                    }
                }
                result = shutdown.changed() => {
                    if result.is_err() || *shutdown.borrow() {
                        break;
                    }
                }
            }
        }
    };
    Sse::new(stream)
        .keep_alive(
            KeepAlive::new()
                .interval(Duration::from_secs(20))
                .text("still here"),
        )
        .into_response()
}

async fn style() -> Response {
    static_asset(STYLE, "text/css; charset=utf-8")
}

async fn script() -> Response {
    static_asset(SCRIPT, "text/javascript; charset=utf-8")
}

async fn favicon() -> Response {
    static_asset(FAVICON, "image/svg+xml")
}

async fn not_found() -> Response {
    render_error(
        StatusCode::NOT_FOUND,
        "Page not found",
        "There is nothing at this address in the current folio.",
    )
}

fn static_asset(contents: &'static str, content_type: &'static str) -> Response {
    let mut response = Response::new(Body::from(contents));
    response
        .headers_mut()
        .insert(CONTENT_TYPE, HeaderValue::from_static(content_type));
    response
        .headers_mut()
        .insert(CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    response
}

fn navigation_for(catalog: &Catalog, current: DocumentId) -> Vec<NavigationView> {
    let Some(current_document) = catalog.document(current) else {
        return Vec::new();
    };
    let ids = current_document.repository.map_or_else(
        || catalog.loose_documents(),
        |repository| &catalog.repositories()[repository].documents,
    );
    ids.iter()
        .filter_map(|id| catalog.document(*id))
        .map(|document| {
            let path = document.relative_path.to_string_lossy().into_owned();
            NavigationView {
                title: document.title.clone(),
                href: document_url(&document.relative_path),
                filter_value: format!(
                    "{} {}",
                    document.title.to_ascii_lowercase(),
                    path.to_ascii_lowercase()
                ),
                current: document.id == current,
                path,
            }
        })
        .collect()
}

fn collection_count(catalog: &Catalog) -> usize {
    catalog.repositories().len() + usize::from(!catalog.loose_documents().is_empty())
}

async fn confined_path(root: &FsPath, relative_path: &FsPath) -> Option<PathBuf> {
    let canonical = tokio::fs::canonicalize(root.join(relative_path))
        .await
        .ok()?;
    let metadata = tokio::fs::metadata(&canonical).await.ok()?;
    (canonical.starts_with(root) && metadata.is_file()).then_some(canonical)
}

fn status_for_io_error(error: &std::io::Error) -> StatusCode {
    match error.kind() {
        std::io::ErrorKind::NotFound | std::io::ErrorKind::PermissionDenied => {
            StatusCode::NOT_FOUND
        }
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

impl MarkdownReadError {
    fn status(&self) -> StatusCode {
        match self {
            Self::Io(error) => status_for_io_error(error),
            Self::TooLarge => StatusCode::PAYLOAD_TOO_LARGE,
            Self::InvalidUtf8 => StatusCode::UNPROCESSABLE_ENTITY,
        }
    }
}

async fn read_markdown(path: &FsPath) -> Result<String, MarkdownReadError> {
    let file = tokio::fs::File::open(path).await?;
    if file.metadata().await?.len() > MAX_MARKDOWN_BYTES {
        return Err(MarkdownReadError::TooLarge);
    }

    let mut bytes = Vec::new();
    file.take(MAX_MARKDOWN_BYTES + 1)
        .read_to_end(&mut bytes)
        .await?;
    if bytes.len() as u64 > MAX_MARKDOWN_BYTES {
        return Err(MarkdownReadError::TooLarge);
    }
    String::from_utf8(bytes).map_err(|_| MarkdownReadError::InvalidUtf8)
}

fn render_template(template_status: StatusCode, template: impl Template) -> Response {
    match template.render() {
        Ok(html) => (template_status, Html(html)).into_response(),
        Err(error) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("mdfolio template error: {error}"),
        )
            .into_response(),
    }
}

fn render_error(status: StatusCode, heading: &str, message: &str) -> Response {
    render_template(
        status,
        ErrorTemplate {
            page_title: format!("{heading} — mdfolio"),
            body_class: "error-page",
            current_document: "",
            status: status.as_str().to_owned(),
            heading: heading.to_owned(),
            message: message.to_owned(),
        },
    )
}

async fn security_headers(request: Request, next: Next) -> Response {
    let mut response = next.run(request).await;
    let headers = response.headers_mut();
    headers
        .entry(X_CONTENT_TYPE_OPTIONS)
        .or_insert(HeaderValue::from_static("nosniff"));
    headers.entry(CONTENT_SECURITY_POLICY).or_insert(HeaderValue::from_static(
        "default-src 'self'; img-src 'self' http: https:; style-src 'self'; script-src 'self'; connect-src 'self'; object-src 'none'; base-uri 'none'; frame-ancestors 'none'",
    ));
    headers
        .entry("referrer-policy")
        .or_insert(HeaderValue::from_static("no-referrer"));
    headers
        .entry("x-frame-options")
        .or_insert(HeaderValue::from_static("DENY"));
    response
}

#[cfg(test)]
mod tests {
    use std::{fs, time::Duration};

    use axum::{
        body::Body,
        http::{
            Request, StatusCode,
            header::{CONTENT_DISPOSITION, CONTENT_SECURITY_POLICY, CONTENT_TYPE, LOCATION},
        },
        response::Response,
    };
    use http_body_util::BodyExt;
    use tempfile::TempDir;
    use tower::ServiceExt;

    use super::{AppState, MAX_MARKDOWN_BYTES, ReloadEvent, app, read_markdown};
    use crate::catalog::Catalog;

    fn fixture() -> (TempDir, AppState) {
        let temp = TempDir::new().unwrap();
        fs::create_dir(temp.path().join(".git")).unwrap();
        fs::write(temp.path().join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
        fs::create_dir(temp.path().join("images")).unwrap();
        fs::create_dir(temp.path().join("_mdfolio")).unwrap();
        fs::write(
            temp.path().join("README.md"),
            "# A Quiet Book\n\n![cover](images/cover.svg)",
        )
        .unwrap();
        fs::write(
            temp.path().join("images/cover.svg"),
            "<svg xmlns=\"http://www.w3.org/2000/svg\"></svg>",
        )
        .unwrap();
        fs::write(temp.path().join("manual.pdf"), b"%PDF-fixture").unwrap();
        fs::write(temp.path().join("_mdfolio/events.md"), "# Not an endpoint").unwrap();
        let state = AppState::new(Catalog::scan(temp.path()).unwrap());
        (temp, state)
    }

    async fn body_text(response: Response) -> String {
        String::from_utf8(
            response
                .into_body()
                .collect()
                .await
                .unwrap()
                .to_bytes()
                .to_vec(),
        )
        .unwrap()
    }

    #[tokio::test]
    async fn landing_redirects_to_the_root_readme() {
        let (_temp, state) = fixture();
        let response = app(state)
            .oneshot(
                Request::builder()
                    .uri("/_mdfolio/")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::TEMPORARY_REDIRECT);
        assert_eq!(
            response.headers().get(LOCATION).unwrap(),
            "/_mdfolio/doc/README.md"
        );
    }

    #[tokio::test]
    async fn reader_renders_markdown_inside_the_application_shell() {
        let (_temp, state) = fixture();
        let response = app(state)
            .oneshot(
                Request::builder()
                    .uri("/_mdfolio/doc/README.md")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = body_text(response).await;
        assert!(body.contains("<h1 id=\"a-quiet-book\">A Quiet Book"));
        assert!(body.contains("/_mdfolio/asset/images/cover.svg"));
        assert!(body.contains("class=\"folio-nav\""));
    }

    #[tokio::test]
    async fn appearance_controls_render_before_css_without_weakening_csp() {
        let (_temp, state) = fixture();
        for uri in [
            "/_mdfolio/shelf",
            "/_mdfolio/doc/README.md",
            "/_mdfolio/missing/not-here.md",
        ] {
            let response = app(state.clone())
                .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
                .await
                .unwrap();

            let csp = response
                .headers()
                .get(CONTENT_SECURITY_POLICY)
                .unwrap()
                .to_str()
                .unwrap()
                .to_owned();
            assert!(csp.contains("script-src 'self'"));
            assert!(!csp.contains("'unsafe-inline'"));

            let body = body_text(response).await;
            assert!(body.contains("data-theme-toggle"));
            assert!(body.contains("data-theme-menu"));
            assert!(body.contains("data-mode-toggle"));
            let script = body.find("/_mdfolio/static/app.js").unwrap();
            let style = body.find("/_mdfolio/static/style.css").unwrap();
            assert!(script < style);
        }
    }

    #[tokio::test]
    async fn svg_assets_are_confined_and_sandboxed() {
        let (_temp, state) = fixture();
        let response = app(state)
            .oneshot(
                Request::builder()
                    .uri("/_mdfolio/asset/images/cover.svg")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        assert!(
            response
                .headers()
                .get(CONTENT_SECURITY_POLICY)
                .unwrap()
                .to_str()
                .unwrap()
                .starts_with("sandbox")
        );
    }

    #[tokio::test]
    async fn pdf_assets_are_downloads() {
        let (_temp, state) = fixture();
        let response = app(state)
            .oneshot(
                Request::builder()
                    .uri("/_mdfolio/asset/manual.pdf")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers().get(CONTENT_TYPE).unwrap(),
            "application/pdf"
        );
        assert!(
            response
                .headers()
                .get(CONTENT_DISPOSITION)
                .unwrap()
                .to_str()
                .unwrap()
                .starts_with("attachment;")
        );
    }

    #[tokio::test]
    async fn document_route_requires_catalog_membership() {
        let (temp, state) = fixture();
        fs::write(temp.path().join("secret.md"), "# Secret").unwrap();
        let response = app(state)
            .oneshot(
                Request::builder()
                    .uri("/_mdfolio/doc/secret.md")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        assert!(body_text(response).await.contains("not in this folio"));
    }

    #[tokio::test]
    async fn denied_asset_extension_is_not_served() {
        let (temp, state) = fixture();
        fs::write(temp.path().join("page.html"), "<h1>Not an asset</h1>").unwrap();
        let response = app(state)
            .oneshot(
                Request::builder()
                    .uri("/_mdfolio/asset/page.html")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn reserved_namespace_does_not_collide_with_document_paths() {
        let (_temp, state) = fixture();
        let response = app(state)
            .oneshot(
                Request::builder()
                    .uri("/_mdfolio/doc/_mdfolio/events.md")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        assert!(body_text(response).await.contains("Not an endpoint"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn asset_symlink_outside_the_root_is_denied() {
        use std::os::unix::fs::symlink;

        let (temp, state) = fixture();
        let outside = TempDir::new().unwrap();
        fs::write(outside.path().join("outside.png"), b"not really an image").unwrap();
        symlink(
            outside.path().join("outside.png"),
            temp.path().join("images/outside.png"),
        )
        .unwrap();

        let response = app(state)
            .oneshot(
                Request::builder()
                    .uri("/_mdfolio/asset/images/outside.png")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn asset_symlink_inside_the_root_is_allowed() {
        use std::os::unix::fs::symlink;

        let (temp, state) = fixture();
        fs::write(temp.path().join("images/original.png"), b"image bytes").unwrap();
        symlink(
            temp.path().join("images/original.png"),
            temp.path().join("images/linked.png"),
        )
        .unwrap();

        let response = app(state)
            .oneshot(
                Request::builder()
                    .uri("/_mdfolio/asset/images/linked.png")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn encoded_asset_traversal_is_denied() {
        let (_temp, state) = fixture();
        let response = app(state)
            .oneshot(
                Request::builder()
                    .uri("/_mdfolio/asset/%2E%2E/secret.pdf")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn empty_catalog_renders_a_valid_shelf() {
        let temp = TempDir::new().unwrap();
        let state = AppState::new(Catalog::scan(temp.path()).unwrap());
        let response = app(state)
            .oneshot(
                Request::builder()
                    .uri("/_mdfolio/shelf")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        assert!(body_text(response).await.contains("No Markdown found."));
    }

    #[tokio::test]
    async fn event_stream_delivers_catalog_reload_after_a_swap() {
        let (_temp, state) = fixture();
        let response = app(state.clone())
            .oneshot(
                Request::builder()
                    .uri("/_mdfolio/events")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        state.notify(ReloadEvent::catalog());

        let mut body = response.into_body();
        let frame = tokio::time::timeout(Duration::from_secs(1), body.frame())
            .await
            .expect("event stream timed out")
            .expect("event stream ended")
            .expect("event stream failed");
        let data = frame.into_data().expect("first SSE frame is data");
        let data = String::from_utf8(data.to_vec()).unwrap();
        assert!(data.contains("event: reload"));
        assert!(data.contains("\"kind\":\"catalog\""));
    }

    #[tokio::test]
    async fn event_stream_ends_when_shutdown_begins() {
        let (_temp, state) = fixture();
        let response = app(state.clone())
            .oneshot(
                Request::builder()
                    .uri("/_mdfolio/events")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let mut body = response.into_body();

        state.begin_shutdown();

        let frame = tokio::time::timeout(Duration::from_secs(1), body.frame())
            .await
            .expect("event stream did not observe shutdown");
        assert!(frame.is_none(), "event stream emitted after shutdown");
    }

    #[tokio::test]
    async fn markdown_reads_are_bounded() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join("large.md");
        fs::write(&path, vec![b'x'; MAX_MARKDOWN_BYTES as usize + 1]).unwrap();

        let error = read_markdown(&path).await.unwrap_err();

        assert_eq!(error.status(), StatusCode::PAYLOAD_TOO_LARGE);
    }
}
