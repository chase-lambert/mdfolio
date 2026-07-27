use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};

use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher, event::ModifyKind};
use thiserror::Error;
use tokio::{
    sync::{mpsc, watch},
    task::JoinHandle,
};

use crate::{
    catalog::Catalog,
    markdown::asset_kind,
    server::{AppState, ReloadEvent},
};

const HARD_EXCLUDED_DIRECTORIES: [&str; 6] = [
    ".git",
    "target",
    "node_modules",
    ".venv",
    ".direnv",
    "vendor",
];

#[derive(Debug, Error)]
pub enum WatchError {
    #[error("could not create filesystem watcher: {0}")]
    Create(#[source] notify::Error),
    #[error("could not watch {path}: {source}")]
    Root {
        path: PathBuf,
        #[source]
        source: notify::Error,
    },
}

#[derive(Default, Debug)]
struct PendingChanges {
    rescan: bool,
    documents: BTreeSet<String>,
    assets: BTreeSet<String>,
}

pub struct WatchRuntime {
    shutdown: watch::Sender<bool>,
    task: Option<JoinHandle<()>>,
    _watcher: Arc<Mutex<RecommendedWatcher>>,
}

impl WatchRuntime {
    pub async fn shutdown(mut self) {
        let _ = self.shutdown.send(true);
        if let Some(task) = self.task.take() {
            let _ = task.await;
        }
    }
}

impl Drop for WatchRuntime {
    fn drop(&mut self) {
        let _ = self.shutdown.send(true);
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

pub async fn start(state: AppState) -> Result<WatchRuntime, WatchError> {
    let catalog = state.catalog().await;
    let root = catalog.root().to_path_buf();
    let pending = Arc::new(Mutex::new(PendingChanges::default()));
    let (signal_tx, mut signal_rx) = mpsc::channel(1);

    let callback_root = root.clone();
    let callback_pending = Arc::clone(&pending);
    let mut watcher = notify::recommended_watcher(move |result| match result {
        Ok(event) => {
            let changes = classify_event(&callback_root, &event);
            if changes.rescan || !changes.documents.is_empty() || !changes.assets.is_empty() {
                if let Ok(mut pending) = callback_pending.lock() {
                    pending.rescan |= changes.rescan;
                    pending.documents.extend(changes.documents);
                    pending.assets.extend(changes.assets);
                }
                let _ = signal_tx.try_send(());
            }
        }
        Err(error) => tracing::warn!("filesystem watcher error: {error}"),
    })
    .map_err(WatchError::Create)?;

    let mut watched = BTreeSet::new();
    for directory in catalog.watch_directories() {
        let absolute = root.join(directory);
        match watcher.watch(&absolute, RecursiveMode::NonRecursive) {
            Ok(()) => {
                watched.insert(absolute);
            }
            Err(error) if directory.as_os_str().is_empty() => {
                return Err(WatchError::Root {
                    path: absolute,
                    source: error,
                });
            }
            Err(error) => tracing::warn!("could not watch {}: {error}", absolute.display()),
        }
    }

    let watcher = Arc::new(Mutex::new(watcher));
    let task_watcher = Arc::clone(&watcher);
    let (shutdown, mut shutdown_rx) = watch::channel(false);
    let task = tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = shutdown_rx.changed() => break,
                signal = signal_rx.recv() => {
                    if signal.is_none() {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(140)).await;
                    while signal_rx.try_recv().is_ok() {}
                    let changes = {
                        let Ok(mut pending) = pending.lock() else {
                            tracing::warn!("filesystem change buffer was poisoned");
                            continue;
                        };
                        std::mem::take(&mut *pending)
                    };
                    process_changes(
                        &root,
                        &state,
                        &task_watcher,
                        &mut watched,
                        changes,
                    )
                    .await;
                }
            }
        }
    });

    Ok(WatchRuntime {
        shutdown,
        task: Some(task),
        _watcher: watcher,
    })
}

async fn process_changes(
    root: &Path,
    state: &AppState,
    watcher: &Arc<Mutex<RecommendedWatcher>>,
    watched: &mut BTreeSet<PathBuf>,
    changes: PendingChanges,
) {
    if changes.rescan {
        let (directories, changed) = match replace_from_scan(root, state).await {
            Ok(result) => result,
            Err(error) => {
                tracing::warn!("rescan failed; keeping the previous catalog: {error}");
                return;
            }
        };
        reconcile_watches(root, watcher, watched, &directories);
        if changed {
            state.notify(ReloadEvent::catalog());
            return;
        }
    }

    if !changes.documents.is_empty() {
        state.notify(ReloadEvent::documents(
            changes.documents.into_iter().collect(),
        ));
    }
    if !changes.assets.is_empty() {
        state.notify(ReloadEvent::asset(changes.assets.into_iter().collect()));
    }
}

async fn replace_from_scan(root: &Path, state: &AppState) -> Result<(Vec<PathBuf>, bool), String> {
    let scan_root = root.to_path_buf();
    let catalog = tokio::task::spawn_blocking(move || Catalog::scan(scan_root))
        .await
        .map_err(|error| format!("scan task stopped unexpectedly: {error}"))?
        .map_err(|error| error.to_string())?;
    let directories = catalog.watch_directories().to_vec();
    let changed = state.replace_catalog(catalog).await;
    Ok((directories, changed))
}

fn reconcile_watches(
    root: &Path,
    watcher: &Arc<Mutex<RecommendedWatcher>>,
    watched: &mut BTreeSet<PathBuf>,
    directories: &[PathBuf],
) {
    let desired: BTreeSet<PathBuf> = directories.iter().map(|path| root.join(path)).collect();
    let Ok(mut watcher) = watcher.lock() else {
        tracing::warn!("filesystem watcher lock was poisoned");
        return;
    };

    let mut active: BTreeSet<PathBuf> = watched.intersection(&desired).cloned().collect();
    for removed in watched.difference(&desired) {
        let _ = watcher.unwatch(removed);
    }
    for added in desired.difference(watched) {
        match watcher.watch(added, RecursiveMode::NonRecursive) {
            Ok(()) => {
                active.insert(added.clone());
            }
            Err(error) => {
                tracing::warn!("could not watch {}: {error}", added.display());
            }
        }
    }
    *watched = active;
}

fn classify_event(root: &Path, event: &Event) -> PendingChanges {
    if matches!(event.kind, EventKind::Access(_)) {
        return PendingChanges::default();
    }

    let mut changes = PendingChanges::default();
    let explicit_directory_event = matches!(
        event.kind,
        EventKind::Create(notify::event::CreateKind::Folder)
            | EventKind::Remove(notify::event::RemoveKind::Folder)
    );
    let rename_event = matches!(event.kind, EventKind::Modify(ModifyKind::Name(_)));

    for path in &event.paths {
        let Ok(relative) = path.strip_prefix(root) else {
            continue;
        };
        if is_git_exclude(relative) {
            changes.rescan = true;
            continue;
        }
        if is_inside_hard_excluded(relative) {
            if relative.file_name().is_some_and(|name| name == ".git")
                && relative.components().count() > 0
            {
                changes.rescan = true;
            }
            continue;
        }
        if explicit_directory_event
            || (rename_event && (path.is_dir() || relative.extension().is_none()))
        {
            changes.rescan = true;
        }
        if is_ignore_file(relative) {
            changes.rescan = true;
            continue;
        }
        if is_markdown(relative) {
            changes.rescan = true;
            if let Some(path) = relative.to_str() {
                changes.documents.insert(path.to_owned());
            }
            continue;
        }
        if asset_kind(relative).is_some()
            && let Some(path) = relative.to_str()
        {
            changes.assets.insert(path.to_owned());
        }
    }
    changes
}

fn is_inside_hard_excluded(path: &Path) -> bool {
    path.components().any(|component| {
        component.as_os_str().to_str().is_some_and(|part| {
            HARD_EXCLUDED_DIRECTORIES
                .iter()
                .any(|excluded| part.eq_ignore_ascii_case(excluded))
        })
    })
}

fn is_git_exclude(path: &Path) -> bool {
    let parts: Vec<_> = path.iter().collect();
    parts.len() >= 3
        && parts[parts.len() - 3] == ".git"
        && parts[parts.len() - 2] == "info"
        && parts[parts.len() - 1] == "exclude"
}

fn is_ignore_file(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| matches!(name, ".gitignore" | ".ignore"))
}

fn is_markdown(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            extension.eq_ignore_ascii_case("md") || extension.eq_ignore_ascii_case("markdown")
        })
}

#[cfg(test)]
mod tests {
    use std::{fs, path::Path, time::Duration};

    use notify::{
        Event, EventKind,
        event::{CreateKind, ModifyKind},
    };
    use tempfile::TempDir;

    use super::{classify_event, replace_from_scan, start};
    use crate::{catalog::Catalog, server::AppState};

    fn event(kind: EventKind, path: &str) -> Event {
        Event {
            kind,
            paths: vec![Path::new("/library").join(path)],
            attrs: Default::default(),
        }
    }

    #[test]
    fn ignores_source_build_and_git_internal_activity() {
        let root = Path::new("/library");
        for (kind, path) in [
            (EventKind::Modify(ModifyKind::Any), "src/main.rs"),
            (
                EventKind::Modify(ModifyKind::Name(notify::event::RenameMode::Both)),
                "src/main.rs",
            ),
            (EventKind::Modify(ModifyKind::Any), "target/debug/build.log"),
            (EventKind::Modify(ModifyKind::Any), ".git/index"),
        ] {
            let changes = classify_event(root, &event(kind, path));
            assert!(!changes.rescan, "{path}");
            assert!(changes.documents.is_empty(), "{path}");
            assert!(changes.assets.is_empty(), "{path}");
        }
    }

    #[test]
    fn markdown_ignore_and_directory_events_request_rescan() {
        let root = Path::new("/library");
        for (kind, path) in [
            (EventKind::Modify(ModifyKind::Any), "notes/today.md"),
            (EventKind::Modify(ModifyKind::Any), ".gitignore"),
            (EventKind::Create(CreateKind::Folder), "new-section"),
        ] {
            assert!(classify_event(root, &event(kind, path)).rescan, "{path}");
        }
    }

    #[test]
    fn assets_are_targeted_without_catalog_rescan() {
        let changes = classify_event(
            Path::new("/library"),
            &event(EventKind::Modify(ModifyKind::Any), "images/cover.png"),
        );

        assert!(!changes.rescan);
        assert_eq!(
            changes.assets.into_iter().collect::<Vec<_>>(),
            ["images/cover.png"]
        );
    }

    #[tokio::test]
    async fn an_actual_markdown_save_notifies_the_reader() {
        let temp = TempDir::new().unwrap();
        fs::create_dir(temp.path().join(".git")).unwrap();
        fs::write(temp.path().join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
        fs::write(temp.path().join("README.md"), "# Home\n\nBefore").unwrap();
        let state = AppState::new(Catalog::scan(temp.path()).unwrap());
        let mut reloads = state.subscribe();
        let runtime = start(state).await.unwrap();

        fs::write(temp.path().join("README.md"), "# Home\n\nAfter").unwrap();
        let event = tokio::time::timeout(Duration::from_secs(3), reloads.recv())
            .await
            .expect("watcher timed out")
            .expect("reload channel closed");

        assert_eq!(event.kind(), "documents");
        assert_eq!(event.paths(), ["README.md"]);
        runtime.shutdown().await;
    }

    #[tokio::test]
    async fn failed_rescan_preserves_catalog_and_emits_nothing() {
        let temp = TempDir::new().unwrap();
        fs::write(temp.path().join("README.md"), "# Still here").unwrap();
        let state = AppState::new(Catalog::scan(temp.path()).unwrap());
        let mut reloads = state.subscribe();

        let result = replace_from_scan(&temp.path().join("missing"), &state).await;

        assert!(result.is_err());
        assert!(
            state
                .catalog()
                .await
                .document_by_path(Path::new("README.md"))
                .is_some()
        );
        assert!(matches!(
            reloads.try_recv(),
            Err(tokio::sync::broadcast::error::TryRecvError::Empty)
        ));
    }
}
