use std::{
    collections::{BTreeSet, HashMap},
    ffi::OsStr,
    fs::{self, File},
    io::{self, Read},
    path::{Path, PathBuf},
};

use comrak::{Arena, Options, nodes::NodeValue, parse_document};
use ignore::WalkBuilder;
use thiserror::Error;

pub type DocumentId = usize;
pub type RepositoryId = usize;

const TITLE_PREFIX_BYTES: u64 = 64 * 1024;
const MAX_GITFILE_BYTES: u64 = 4 * 1024;
const HARD_EXCLUDED_DIRECTORIES: [&str; 6] = [
    ".git",
    "target",
    "node_modules",
    ".venv",
    ".direnv",
    "vendor",
];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CatalogMode {
    SingleLibrary,
    Shelf,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Landing {
    Shelf,
    Document(DocumentId),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Document {
    pub id: DocumentId,
    pub relative_path: PathBuf,
    pub title: String,
    pub repository: Option<RepositoryId>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Repository {
    pub id: RepositoryId,
    pub root_relative: PathBuf,
    pub name: String,
    pub qualifier: Option<String>,
    pub documents: Vec<DocumentId>,
    pub default_document: Option<DocumentId>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CatalogDiagnostic {
    pub path: Option<PathBuf>,
    pub message: String,
}

#[derive(Clone, Debug)]
pub struct Catalog {
    root: PathBuf,
    mode: CatalogMode,
    repositories: Vec<Repository>,
    documents: Vec<Document>,
    loose_documents: Vec<DocumentId>,
    by_path: HashMap<PathBuf, DocumentId>,
    diagnostics: Vec<CatalogDiagnostic>,
}

#[derive(Debug, Error)]
pub enum ScanError {
    #[error("cannot inspect {path}: {source}")]
    Inspect {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("scan root is not a directory: {0}")]
    NotDirectory(PathBuf),
}

#[derive(Debug)]
struct PendingDocument {
    relative_path: PathBuf,
    title: String,
    repository_root: Option<PathBuf>,
}

impl Catalog {
    pub fn scan(root: impl AsRef<Path>) -> Result<Self, ScanError> {
        let requested_root = root.as_ref();
        let root = fs::canonicalize(requested_root).map_err(|source| ScanError::Inspect {
            path: requested_root.to_path_buf(),
            source,
        })?;
        if !root.is_dir() {
            return Err(ScanError::NotDirectory(root));
        }

        let mut diagnostics = Vec::new();
        let mut pending = Vec::new();
        let mut boundaries: HashMap<PathBuf, bool> = HashMap::new();
        let mut title_buf = Vec::new();

        let mut builder = WalkBuilder::new(&root);
        builder
            .hidden(false)
            .follow_links(false)
            .ignore(true)
            .git_ignore(true)
            .git_exclude(true)
            .parents(false)
            .filter_entry(|entry| {
                entry.depth() == 0
                    || !entry.file_type().is_some_and(|kind| kind.is_dir())
                    || !is_hard_excluded(entry.file_name())
            });

        for result in builder.build() {
            let entry = match result {
                Ok(entry) => entry,
                Err(error) => {
                    diagnostics.push(CatalogDiagnostic {
                        path: ignore_error_path(&error),
                        message: error.to_string(),
                    });
                    continue;
                }
            };
            if entry.file_type().is_some_and(|kind| kind.is_dir()) {
                continue;
            }
            if !entry.file_type().is_some_and(|kind| kind.is_file())
                || !is_markdown_path(entry.path())
            {
                continue;
            }

            let Ok(relative_path) = entry.path().strip_prefix(&root) else {
                continue;
            };
            if relative_path.to_str().is_none() {
                diagnostics.push(CatalogDiagnostic {
                    path: Some(relative_path.to_path_buf()),
                    message: "Markdown path is not valid UTF-8".to_owned(),
                });
                continue;
            }

            let title = match read_title(entry.path(), &mut title_buf) {
                Ok(title) => title.unwrap_or_else(|| file_stem_title(relative_path)),
                Err(error) => {
                    diagnostics.push(CatalogDiagnostic {
                        path: Some(relative_path.to_path_buf()),
                        message: format!("reading the title failed: {error}"),
                    });
                    file_stem_title(relative_path)
                }
            };
            let repository_root = nearest_repository_root(&root, entry.path(), &mut boundaries);
            pending.push(PendingDocument {
                relative_path: relative_path.to_path_buf(),
                title,
                repository_root,
            });
        }

        pending.sort_by_cached_key(|document| path_sort_key(&document.relative_path));

        let repository_roots: Vec<PathBuf> = pending
            .iter()
            .filter_map(|document| document.repository_root.as_deref())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .map(Path::to_path_buf)
            .collect();
        let repository_ids: HashMap<PathBuf, RepositoryId> = repository_roots
            .iter()
            .cloned()
            .enumerate()
            .map(|(id, path)| (path, id))
            .collect();

        let mut documents = Vec::with_capacity(pending.len());
        let mut by_path = HashMap::with_capacity(pending.len());
        let mut repository_documents = vec![Vec::new(); repository_roots.len()];
        let mut loose_documents = Vec::new();

        for document in pending {
            let id = documents.len();
            let repository = document
                .repository_root
                .as_ref()
                .and_then(|root| repository_ids.get(root).copied());
            let relative_path = document.relative_path;
            by_path.insert(relative_path.clone(), id);
            if let Some(repository) = repository {
                repository_documents[repository].push(id);
            } else {
                loose_documents.push(id);
            }
            documents.push(Document {
                id,
                relative_path,
                title: document.title,
                repository,
            });
        }
        for (repository_id, ids) in repository_documents.iter_mut().enumerate() {
            let collection_root = &repository_roots[repository_id];
            ids.sort_by_cached_key(|id| {
                let relative = documents[*id]
                    .relative_path
                    .strip_prefix(collection_root)
                    .unwrap_or(&documents[*id].relative_path);
                default_document_sort_key(relative)
            });
        }
        loose_documents
            .sort_by_cached_key(|id| default_document_sort_key(&documents[*id].relative_path));

        let mut repositories: Vec<Repository> = repository_roots
            .into_iter()
            .enumerate()
            .map(|(id, root_relative)| {
                let name = if root_relative.as_os_str().is_empty() {
                    root.file_name()
                        .and_then(OsStr::to_str)
                        .unwrap_or("Repository")
                        .to_owned()
                } else {
                    root_relative
                        .file_name()
                        .and_then(OsStr::to_str)
                        .unwrap_or("Repository")
                        .to_owned()
                };
                let ids = std::mem::take(&mut repository_documents[id]);
                let default_document = choose_default_document(&documents, &ids, &root_relative);
                Repository {
                    id,
                    root_relative,
                    name,
                    qualifier: None,
                    documents: ids,
                    default_document,
                }
            })
            .collect();
        qualify_duplicate_repository_names(&mut repositories);

        let mut catalog = Self {
            root,
            mode: CatalogMode::SingleLibrary,
            repositories,
            documents,
            loose_documents,
            by_path,
            diagnostics,
        };
        catalog.mode = if catalog.collection_count() <= 1 {
            CatalogMode::SingleLibrary
        } else {
            CatalogMode::Shelf
        };
        Ok(catalog)
    }

    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    #[must_use]
    pub fn mode(&self) -> CatalogMode {
        self.mode
    }

    #[must_use]
    pub fn repositories(&self) -> &[Repository] {
        &self.repositories
    }

    #[must_use]
    pub fn documents(&self) -> &[Document] {
        &self.documents
    }

    #[must_use]
    pub fn loose_documents(&self) -> &[DocumentId] {
        &self.loose_documents
    }

    #[must_use]
    pub fn loose_default_document(&self) -> Option<&Document> {
        choose_default_document(&self.documents, &self.loose_documents, Path::new(""))
            .and_then(|id| self.document(id))
    }

    #[must_use]
    pub fn diagnostics(&self) -> &[CatalogDiagnostic] {
        &self.diagnostics
    }

    #[must_use]
    pub fn collection_count(&self) -> usize {
        self.repositories.len() + usize::from(!self.loose_documents.is_empty())
    }

    #[must_use]
    pub fn document(&self, id: DocumentId) -> Option<&Document> {
        self.documents.get(id)
    }

    #[must_use]
    pub fn document_by_path(&self, relative_path: &Path) -> Option<&Document> {
        self.by_path
            .get(relative_path)
            .and_then(|id| self.documents.get(*id))
    }

    #[must_use]
    pub fn resolve_document_target(&self, target: &Path) -> Option<&Document> {
        if let Some(document) = self.document_by_path(target) {
            return Some(document);
        }

        if target.extension().is_none() {
            for extension in ["md", "markdown"] {
                let candidate = target.with_extension(extension);
                if let Some(document) = self.document_by_path(&candidate) {
                    return Some(document);
                }
            }
            if let Some(document) = self.documents.iter().find(|document| {
                document.relative_path.parent() == target.parent()
                    && document.relative_path.file_stem() == target.file_name()
                    && is_markdown_path(&document.relative_path)
            }) {
                return Some(document);
            }
        }

        self.documents
            .iter()
            .filter(|document| document.relative_path.starts_with(target))
            .min_by_key(|document| {
                default_document_sort_key(
                    document
                        .relative_path
                        .strip_prefix(target)
                        .unwrap_or(&document.relative_path),
                )
            })
    }

    #[must_use]
    pub fn landing(&self) -> Landing {
        if self.mode == CatalogMode::Shelf {
            return Landing::Shelf;
        }
        if let Some(repository) = self.repositories.first() {
            return repository
                .default_document
                .map_or(Landing::Shelf, Landing::Document);
        }
        self.loose_default_document()
            .map_or(Landing::Shelf, |document| Landing::Document(document.id))
    }
}

fn is_markdown_path(path: &Path) -> bool {
    path.extension()
        .and_then(OsStr::to_str)
        .is_some_and(|extension| {
            extension.eq_ignore_ascii_case("md") || extension.eq_ignore_ascii_case("markdown")
        })
}

fn ignore_error_path(error: &ignore::Error) -> Option<PathBuf> {
    match error {
        ignore::Error::Partial(errors) => errors.iter().find_map(ignore_error_path),
        ignore::Error::WithLineNumber { err, .. } | ignore::Error::WithDepth { err, .. } => {
            ignore_error_path(err)
        }
        ignore::Error::WithPath { path, .. } => Some(path.clone()),
        ignore::Error::Loop { child, .. } => Some(child.clone()),
        ignore::Error::Io(_)
        | ignore::Error::Glob { .. }
        | ignore::Error::UnrecognizedFileType(_)
        | ignore::Error::InvalidDefinition => None,
    }
}

fn is_git_boundary(path: &Path) -> bool {
    let marker = path.join(".git");
    let Ok(metadata) = fs::symlink_metadata(&marker) else {
        return false;
    };

    if metadata.file_type().is_dir() {
        return is_git_admin_directory(&marker);
    }
    if !metadata.file_type().is_file() || metadata.len() > MAX_GITFILE_BYTES {
        return false;
    }

    let Ok(contents) = fs::read_to_string(marker) else {
        return false;
    };
    let Some(git_dir) = contents
        .strip_prefix("gitdir: ")
        .map(|path| path.trim_end_matches(['\r', '\n']))
        .filter(|path| !path.is_empty() && !path.contains(['\r', '\n']))
    else {
        return false;
    };
    let git_dir = Path::new(git_dir);
    let git_dir = if git_dir.is_absolute() {
        git_dir.to_path_buf()
    } else {
        path.join(git_dir)
    };
    is_git_admin_directory(&git_dir)
}

fn is_git_admin_directory(path: &Path) -> bool {
    fs::symlink_metadata(path.join("HEAD")).is_ok_and(|metadata| metadata.file_type().is_file())
}

fn is_hard_excluded(name: &OsStr) -> bool {
    name.to_str().is_some_and(|name| {
        HARD_EXCLUDED_DIRECTORIES
            .iter()
            .any(|excluded| name.eq_ignore_ascii_case(excluded))
    })
}

fn nearest_repository_root(
    root: &Path,
    document_path: &Path,
    boundaries: &mut HashMap<PathBuf, bool>,
) -> Option<PathBuf> {
    let mut current = document_path.parent()?;
    loop {
        let is_boundary = match boundaries.get(current) {
            Some(&is_boundary) => is_boundary,
            None => {
                let is_boundary = is_git_boundary(current);
                boundaries.insert(current.to_path_buf(), is_boundary);
                is_boundary
            }
        };
        if is_boundary {
            return current.strip_prefix(root).ok().map(Path::to_path_buf);
        }
        if current == root {
            return None;
        }
        current = current.parent()?;
    }
}

fn read_title(path: &Path, buf: &mut Vec<u8>) -> io::Result<Option<String>> {
    buf.clear();
    let file = File::open(path)?;
    file.take(TITLE_PREFIX_BYTES).read_to_end(buf)?;
    Ok(extract_title(&String::from_utf8_lossy(buf)))
}

fn extract_title(markdown: &str) -> Option<String> {
    let mut previous: Option<&str> = None;
    for line in markdown.lines() {
        if let Some(previous_line) = previous {
            let underline = line.trim();
            let is_setext = !underline.is_empty()
                && (underline.bytes().all(|byte| byte == b'=')
                    || underline.bytes().all(|byte| byte == b'-'));
            let title = previous_line.trim();
            if is_setext && !title.is_empty() {
                return plain_inline_title(title);
            }
        }

        let trimmed = line.trim_start();
        let hashes = trimmed.bytes().take_while(|byte| *byte == b'#').count();
        if (1..=6).contains(&hashes)
            && trimmed
                .as_bytes()
                .get(hashes)
                .is_some_and(u8::is_ascii_whitespace)
        {
            let title = trimmed[hashes..].trim().trim_end_matches('#').trim();
            if !title.is_empty() {
                return plain_inline_title(title);
            }
        }
        previous = Some(line);
    }
    None
}

fn heading_has_inline_markup(markdown: &str) -> bool {
    markdown
        .bytes()
        .any(|byte| matches!(byte, b'*' | b'_' | b'`' | b'[' | b']' | b'<' | b'&' | b'\\'))
}

/// Comrak treats these as block openers inside the heading, so the marker
/// never becomes a text node. The fast path must not keep them as title text.
fn heading_looks_like_block(markdown: &str) -> bool {
    let trimmed = markdown.trim_start();
    if trimmed.starts_with('>') || trimmed.starts_with("- ") || trimmed.starts_with("+ ") {
        return true;
    }
    let digits = trimmed.bytes().take_while(u8::is_ascii_digit).count();
    if digits == 0 {
        return false;
    }
    matches!(
        trimmed.as_bytes().get(digits..),
        Some([b'.' | b')', b' ' | b'\t', ..])
    )
}

fn collapse_heading_whitespace(markdown: &str) -> Option<String> {
    let title = markdown.split_whitespace().collect::<Vec<_>>().join(" ");
    (!title.is_empty()).then_some(title)
}

fn plain_inline_title(markdown: &str) -> Option<String> {
    if !heading_has_inline_markup(markdown) && !heading_looks_like_block(markdown) {
        return collapse_heading_whitespace(markdown);
    }
    let arena = Arena::new();
    let root = parse_document(&arena, markdown, &Options::default());
    let mut title = String::new();
    for node in root.descendants() {
        match &node.data.borrow().value {
            NodeValue::Text(text) => title.push_str(text),
            NodeValue::Code(code) => title.push_str(&code.literal),
            NodeValue::SoftBreak | NodeValue::LineBreak => title.push(' '),
            _ => {}
        }
    }
    collapse_heading_whitespace(&title)
}

fn file_stem_title(path: &Path) -> String {
    path.file_stem()
        .and_then(OsStr::to_str)
        .unwrap_or("Untitled")
        .replace(['-', '_'], " ")
}

fn choose_default_document(
    documents: &[Document],
    ids: &[DocumentId],
    collection_root: &Path,
) -> Option<DocumentId> {
    ids.iter().copied().min_by_key(|id| {
        let relative = documents[*id]
            .relative_path
            .strip_prefix(collection_root)
            .unwrap_or(&documents[*id].relative_path);
        default_document_sort_key(relative)
    })
}

fn default_document_sort_key(path: &Path) -> (u8, usize, String) {
    let is_root = path
        .parent()
        .is_none_or(|parent| parent.as_os_str().is_empty());
    let file_name = path.file_name().and_then(OsStr::to_str).unwrap_or("");
    let stem = path.file_stem().and_then(OsStr::to_str).unwrap_or("");
    let priority = if is_root && file_name == "README.md" {
        0
    } else if is_root && stem.eq_ignore_ascii_case("readme") {
        1
    } else if is_root && file_name == "index.md" {
        2
    } else if is_root && stem.eq_ignore_ascii_case("index") {
        3
    } else {
        4
    };
    (priority, path.components().count(), path_sort_key(path))
}

fn path_sort_key(path: &Path) -> String {
    path.to_string_lossy().to_ascii_lowercase()
}

fn qualify_duplicate_repository_names(repositories: &mut [Repository]) {
    let mut name_counts: HashMap<String, usize> = HashMap::new();
    for repository in repositories.iter() {
        *name_counts
            .entry(repository.name.to_ascii_lowercase())
            .or_default() += 1;
    }
    for repository in repositories {
        if name_counts
            .get(&repository.name.to_ascii_lowercase())
            .copied()
            .unwrap_or_default()
            > 1
        {
            repository.qualifier = repository
                .root_relative
                .parent()
                .filter(|parent| !parent.as_os_str().is_empty())
                .map(|parent| parent.to_string_lossy().into_owned());
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{fs, path::Path};

    use tempfile::TempDir;

    use super::{Catalog, CatalogMode, Landing};

    fn write(root: &Path, path: &str, contents: &str) {
        let path = root.join(path);
        fs::create_dir_all(path.parent().expect("test file has a parent")).unwrap();
        fs::write(path, contents).unwrap();
    }

    fn git(root: &Path, path: &str) {
        let git_dir = root.join(path).join(".git");
        fs::create_dir_all(&git_dir).unwrap();
        fs::write(git_dir.join("HEAD"), "ref: refs/heads/main\n").unwrap();
    }

    #[test]
    fn empty_git_placeholder_is_not_a_repository_boundary() {
        let temp = TempDir::new().unwrap();
        fs::create_dir(temp.path().join(".git")).unwrap();
        write(temp.path(), "README.md", "# Loose");

        let catalog = Catalog::scan(temp.path()).unwrap();

        assert!(catalog.repositories().is_empty());
        assert_eq!(catalog.loose_documents().len(), 1);
    }

    #[test]
    fn git_directory_with_head_is_a_repository_boundary() {
        let temp = TempDir::new().unwrap();
        git(temp.path(), "project");
        write(temp.path(), "project/README.md", "# Project");

        let catalog = Catalog::scan(temp.path()).unwrap();

        assert_eq!(catalog.repositories().len(), 1);
        assert!(catalog.loose_documents().is_empty());
        assert_eq!(
            catalog.repositories()[0].root_relative,
            Path::new("project")
        );
    }

    #[test]
    fn valid_gitfile_is_a_repository_boundary() {
        let temp = TempDir::new().unwrap();
        write(temp.path(), "admin/worktree/HEAD", "ref: refs/heads/main\n");
        write(temp.path(), "project/.git", "gitdir: ../admin/worktree\n");
        write(temp.path(), "project/README.md", "# Worktree");

        let catalog = Catalog::scan(temp.path()).unwrap();

        assert_eq!(catalog.repositories().len(), 1);
        assert_eq!(
            catalog.repositories()[0].root_relative,
            Path::new("project")
        );
    }

    #[test]
    fn malformed_gitfile_is_not_a_repository_boundary() {
        let temp = TempDir::new().unwrap();
        write(temp.path(), "project/.git", "not a gitdir pointer\n");
        write(temp.path(), "project/README.md", "# Loose");

        let catalog = Catalog::scan(temp.path()).unwrap();

        assert!(catalog.repositories().is_empty());
        assert_eq!(catalog.loose_documents().len(), 1);
    }

    #[test]
    fn root_repository_and_nested_repository_form_a_shelf() {
        let temp = TempDir::new().unwrap();
        git(temp.path(), "");
        git(temp.path(), "tools/nested");
        write(temp.path(), "README.md", "# Root");
        write(temp.path(), "tools/nested/README.md", "# Nested");

        let catalog = Catalog::scan(temp.path()).unwrap();

        assert_eq!(catalog.mode(), CatalogMode::Shelf);
        assert_eq!(catalog.repositories().len(), 2);
        assert_eq!(catalog.landing(), Landing::Shelf);
    }

    #[test]
    fn shelf_groups_nearest_repositories_and_loose_files() {
        let temp = TempDir::new().unwrap();
        git(temp.path(), "work/alpha");
        git(temp.path(), "archive/alpha");
        write(temp.path(), "work/alpha/README.md", "# Current");
        write(temp.path(), "archive/alpha/readme.MD", "# Archived");
        write(temp.path(), "notes/today.md", "# Today");

        let catalog = Catalog::scan(temp.path()).unwrap();

        assert_eq!(catalog.mode(), CatalogMode::Shelf);
        assert_eq!(catalog.repositories().len(), 2);
        assert_eq!(catalog.loose_documents().len(), 1);
        assert!(
            catalog
                .repositories()
                .iter()
                .all(|repo| repo.qualifier.is_some())
        );
        assert_eq!(catalog.landing(), Landing::Shelf);
    }

    #[test]
    fn sole_repository_opens_its_readme() {
        let temp = TempDir::new().unwrap();
        git(temp.path(), "project");
        write(temp.path(), "project/guide.md", "# Guide");
        write(temp.path(), "project/README.md", "# Home");

        let catalog = Catalog::scan(temp.path()).unwrap();
        let Landing::Document(id) = catalog.landing() else {
            panic!("sole repository should open directly");
        };

        assert_eq!(
            catalog.document(id).unwrap().relative_path,
            Path::new("project/README.md")
        );
        assert_eq!(catalog.repositories()[0].documents[0], id);
    }

    #[test]
    fn readme_choice_is_case_insensitive_and_deterministic() {
        let temp = TempDir::new().unwrap();
        git(temp.path(), "");
        write(temp.path(), "guide.md", "# Guide");
        write(temp.path(), "readme.MD", "# Lower");
        write(temp.path(), "README.md", "# Exact");

        let catalog = Catalog::scan(temp.path()).unwrap();
        let Landing::Document(id) = catalog.landing() else {
            panic!("root repository should open directly");
        };

        assert_eq!(
            catalog.document(id).unwrap().relative_path,
            Path::new("README.md")
        );
    }

    #[test]
    fn hard_excludes_caches_but_keeps_hidden_authoring_directories() {
        let temp = TempDir::new().unwrap();
        write(temp.path(), "target/no.md", "# No");
        write(temp.path(), "node_modules/no.md", "# No");
        write(temp.path(), ".agents/yes.md", "# Yes");

        let catalog = Catalog::scan(temp.path()).unwrap();

        assert_eq!(catalog.documents().len(), 1);
        assert_eq!(
            catalog.documents()[0].relative_path,
            Path::new(".agents/yes.md")
        );
    }

    #[test]
    fn title_uses_first_heading_and_falls_back_to_stem() {
        let temp = TempDir::new().unwrap();
        write(temp.path(), "setext.md", "A Setext Title\n==============\n");
        write(temp.path(), "plain-note.md", "No heading here.");
        write(
            temp.path(),
            "formatted.md",
            "# **A Quiet** [Book](https://example.com)",
        );

        let catalog = Catalog::scan(temp.path()).unwrap();

        assert_eq!(
            catalog
                .document_by_path(Path::new("setext.md"))
                .unwrap()
                .title,
            "A Setext Title"
        );
        assert_eq!(
            catalog
                .document_by_path(Path::new("plain-note.md"))
                .unwrap()
                .title,
            "plain note"
        );
        assert_eq!(
            catalog
                .document_by_path(Path::new("formatted.md"))
                .unwrap()
                .title,
            "A Quiet Book"
        );
    }

    #[test]
    fn atx_title_takes_precedence_over_a_setext_underline() {
        let temp = TempDir::new().unwrap();
        write(temp.path(), "precedence.md", "# A##\n===\n");

        let catalog = Catalog::scan(temp.path()).unwrap();

        assert_eq!(
            catalog
                .document_by_path(Path::new("precedence.md"))
                .unwrap()
                .title,
            "A"
        );
    }

    #[test]
    fn setext_title_wins_over_a_later_atx_heading() {
        let temp = TempDir::new().unwrap();
        write(temp.path(), "setext-first.md", "Title\n===\n# Other\n");

        let catalog = Catalog::scan(temp.path()).unwrap();

        assert_eq!(
            catalog
                .document_by_path(Path::new("setext-first.md"))
                .unwrap()
                .title,
            "Title"
        );
    }

    #[test]
    fn empty_atx_heading_falls_through_to_the_next_candidate() {
        let temp = TempDir::new().unwrap();
        write(temp.path(), "empty-atx.md", "# \nTitle\n===\n");

        let catalog = Catalog::scan(temp.path()).unwrap();

        assert_eq!(
            catalog
                .document_by_path(Path::new("empty-atx.md"))
                .unwrap()
                .title,
            "Title"
        );
    }

    #[test]
    fn list_and_quote_markers_in_headings_do_not_become_title_text() {
        let temp = TempDir::new().unwrap();
        write(temp.path(), "numbered.md", "# 1. Introduction\n");
        write(temp.path(), "dash.md", "# - item\n");
        write(temp.path(), "quote.md", "# > note\n");

        let catalog = Catalog::scan(temp.path()).unwrap();

        assert_eq!(
            catalog
                .document_by_path(Path::new("numbered.md"))
                .unwrap()
                .title,
            "Introduction"
        );
        assert_eq!(
            catalog
                .document_by_path(Path::new("dash.md"))
                .unwrap()
                .title,
            "item"
        );
        assert_eq!(
            catalog
                .document_by_path(Path::new("quote.md"))
                .unwrap()
                .title,
            "note"
        );
    }

    #[test]
    fn entity_reference_in_heading_decodes() {
        let temp = TempDir::new().unwrap();
        write(temp.path(), "entities.md", "# Q&amp;A\n");

        let catalog = Catalog::scan(temp.path()).unwrap();

        assert_eq!(
            catalog
                .document_by_path(Path::new("entities.md"))
                .unwrap()
                .title,
            "Q&A"
        );
    }

    #[test]
    fn image_only_first_heading_falls_back_to_the_stem() {
        let temp = TempDir::new().unwrap();
        write(temp.path(), "image-only.md", "# ![](x.png)\n# Real\n");

        let catalog = Catalog::scan(temp.path()).unwrap();

        assert_eq!(
            catalog
                .document_by_path(Path::new("image-only.md"))
                .unwrap()
                .title,
            "image only"
        );
    }

    #[test]
    fn nested_gitignore_is_honored_at_a_shelf_root() {
        let temp = TempDir::new().unwrap();
        git(temp.path(), "project");
        write(temp.path(), "project/.gitignore", "private.md\n");
        write(temp.path(), "project/private.md", "# Private");
        write(temp.path(), "project/public.md", "# Public");

        let catalog = Catalog::scan(temp.path()).unwrap();

        assert_eq!(catalog.documents().len(), 1);
        assert_eq!(
            catalog.documents()[0].relative_path,
            Path::new("project/public.md")
        );
    }

    #[test]
    fn repository_info_exclude_is_honored() {
        let temp = TempDir::new().unwrap();
        git(temp.path(), "");
        write(temp.path(), ".git/info/exclude", "private.md\n");
        write(temp.path(), "private.md", "# Private");
        write(temp.path(), "public.md", "# Public");

        let catalog = Catalog::scan(temp.path()).unwrap();

        assert_eq!(catalog.documents().len(), 1);
        assert_eq!(catalog.documents()[0].relative_path, Path::new("public.md"));
    }

    #[test]
    fn title_scan_is_bounded_to_the_first_64_kib() {
        let temp = TempDir::new().unwrap();
        let mut markdown = "ordinary text ".repeat(6_000);
        markdown.push_str("\n# Too Late");
        write(temp.path(), "late-heading.md", &markdown);

        let catalog = Catalog::scan(temp.path()).unwrap();

        assert_eq!(catalog.documents()[0].title, "late heading");
    }

    #[cfg(unix)]
    #[test]
    fn invalid_utf8_markdown_paths_are_diagnostic_only() {
        use std::{ffi::OsString, os::unix::ffi::OsStringExt};

        let temp = TempDir::new().unwrap();
        let name = OsString::from_vec(vec![0xff, b'.', b'm', b'd']);
        fs::write(temp.path().join(name), "# Hidden").unwrap();

        let catalog = Catalog::scan(temp.path()).unwrap();

        assert!(catalog.documents().is_empty());
        assert_eq!(catalog.diagnostics().len(), 1);
        assert!(catalog.diagnostics()[0].message.contains("not valid UTF-8"));
    }

    #[test]
    fn empty_root_is_a_valid_empty_shelf() {
        let temp = TempDir::new().unwrap();

        let catalog = Catalog::scan(temp.path()).unwrap();

        assert!(catalog.documents().is_empty());
        assert!(catalog.repositories().is_empty());
        assert_eq!(catalog.landing(), Landing::Shelf);
    }

    #[test]
    fn repository_subdirectory_is_one_loose_library() {
        let temp = TempDir::new().unwrap();
        git(temp.path(), "");
        write(temp.path(), "docs/README.md", "# Focused docs");
        write(temp.path(), "outside.md", "# Outside");

        let catalog = Catalog::scan(temp.path().join("docs")).unwrap();

        assert_eq!(catalog.mode(), CatalogMode::SingleLibrary);
        assert!(catalog.repositories().is_empty());
        assert_eq!(catalog.loose_documents().len(), 1);
        let Landing::Document(id) = catalog.landing() else {
            panic!("one loose library should open directly");
        };
        assert_eq!(
            catalog.document(id).unwrap().relative_path,
            Path::new("README.md")
        );
    }
}
