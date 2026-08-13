use std::path::{Component, Path, PathBuf};

use percent_encoding::{AsciiSet, CONTROLS, percent_decode_str, utf8_percent_encode};

const URL_PATH_ENCODE_SET: &AsciiSet = &CONTROLS
    .add(b' ')
    .add(b'"')
    .add(b'#')
    .add(b'%')
    .add(b'<')
    .add(b'>')
    .add(b'?')
    .add(b'[')
    .add(b'\\')
    .add(b']')
    .add(b'^')
    .add(b'`')
    .add(b'{')
    .add(b'|')
    .add(b'}');

pub const APP_PREFIX: &str = "/_mdfolio";

#[must_use]
pub fn document_url(relative_path: &Path) -> String {
    format!("{APP_PREFIX}/doc/{}", encode_relative_path(relative_path))
}

#[must_use]
pub fn asset_url(relative_path: &Path) -> String {
    format!("{APP_PREFIX}/asset/{}", encode_relative_path(relative_path))
}

#[must_use]
pub fn missing_url(target: &Path) -> String {
    format!("{APP_PREFIX}/missing/{}", encode_relative_path(target))
}

#[must_use]
pub fn encode_relative_path(path: &Path) -> String {
    let mut encoded = String::new();
    for component in path.iter().filter_map(|component| component.to_str()) {
        if !encoded.is_empty() {
            encoded.push('/');
        }
        for piece in utf8_percent_encode(component, URL_PATH_ENCODE_SET) {
            encoded.push_str(piece);
        }
    }
    encoded
}

pub fn normalize_library_path(base_directory: &Path, raw: &str) -> Option<PathBuf> {
    let decoded = percent_decode_str(raw).decode_utf8().ok()?;
    if decoded.contains('\0') {
        return None;
    }

    let absolute_from_library_root = decoded.starts_with('/');
    let mut normalized = if absolute_from_library_root {
        PathBuf::new()
    } else {
        base_directory.to_path_buf()
    };

    for component in Path::new(decoded.trim_start_matches('/')).components() {
        match component {
            Component::CurDir => {}
            Component::Normal(part) => normalized.push(part),
            Component::ParentDir => {
                if !normalized.pop() {
                    return None;
                }
            }
            Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    Some(normalized)
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{document_url, normalize_library_path};

    #[test]
    fn joins_and_normalizes_without_escaping_the_library() {
        assert_eq!(
            normalize_library_path(Path::new("docs/guide"), "../intro.md"),
            Some(Path::new("docs/intro.md").to_path_buf())
        );
        assert_eq!(
            normalize_library_path(Path::new("docs"), "../../secret.md"),
            None
        );
        assert_eq!(
            normalize_library_path(Path::new("docs"), "/README.md"),
            Some(Path::new("README.md").to_path_buf())
        );
    }

    #[test]
    fn document_urls_encode_each_path_segment() {
        assert_eq!(
            document_url(Path::new("notes/a quiet day.md")),
            "/_mdfolio/doc/notes/a%20quiet%20day.md"
        );
    }
}
