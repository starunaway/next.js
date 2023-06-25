use anyhow::Result;
use turbo_tasks::{Completion, Vc};
use turbopack_binding::{
    turbo::tasks_fs::{DirectoryContent, DirectoryEntry, FileSystemEntryType, FileSystemPath},
    turbopack::dev_server::source::specificity::Specificity,
};

use crate::{embed_js::next_js_file_path, next_config::NextConfig};

/// A final route in the pages directory.
#[turbo_tasks::value]
pub struct PagesStructureItem {
    pub project_path: Vc<FileSystemPath>,
    pub next_router_path: Vc<FileSystemPath>,
    pub specificity: Vc<Specificity>,
}

#[turbo_tasks::value_impl]
impl PagesStructureItem {
    #[turbo_tasks::function]
    async fn new(
        project_path: Vc<FileSystemPath>,
        next_router_path: Vc<FileSystemPath>,
        specificity: Vc<Specificity>,
    ) -> Result<Vc<Self>> {
        Ok(PagesStructureItem {
            project_path,
            next_router_path,
            specificity,
        }
        .cell())
    }

    /// Returns a completion that changes when any route in the whole tree
    /// changes.
    #[turbo_tasks::function]
    pub async fn routes_changed(self: Vc<Self>) -> Result<Vc<Completion>> {
        let this = self.await?;
        this.next_router_path.await?;
        Ok(Completion::new())
    }
}

/// A (sub)directory in the pages directory with all analyzed routes and
/// folders.
#[turbo_tasks::value]
pub struct PagesStructure {
    pub app: Vc<PagesStructureItem>,
    pub document: Vc<PagesStructureItem>,
    pub error: Vc<PagesStructureItem>,
    pub api: Option<Vc<PagesDirectoryStructure>>,
    pub pages: Vc<PagesDirectoryStructure>,
}

#[turbo_tasks::value_impl]
impl PagesStructure {
    /// Returns the path to the directory of this structure in the project file
    /// system.
    #[turbo_tasks::function]
    pub async fn project_path(self: Vc<Self>) -> Result<Vc<FileSystemPath>> {
        Ok(self.await?.pages.project_path())
    }

    /// Returns a completion that changes when any route in the whole tree
    /// changes.
    #[turbo_tasks::function]
    pub async fn routes_changed(self: Vc<Self>) -> Result<Vc<Completion>> {
        let PagesStructure {
            ref app,
            ref document,
            ref error,
            ref api,
            ref pages,
        } = &*self.await?;
        app.routes_changed().await?;
        document.routes_changed().await?;
        error.routes_changed().await?;
        if let Some(api) = api {
            api.routes_changed().await?;
        }
        pages.routes_changed().await?;
        Ok(Completion::new())
    }
}

#[turbo_tasks::value(transparent)]
pub struct OptionPagesStructure(Option<Vc<PagesStructure>>);

#[turbo_tasks::value_impl]
impl OptionPagesStructure {
    #[turbo_tasks::function]
    pub async fn routes_changed(self: Vc<Self>) -> Result<Vc<Completion>> {
        if let Some(pages_structure) = *self.await? {
            pages_structure.routes_changed().await?;
        }
        Ok(Completion::new())
    }
}

#[turbo_tasks::value]
pub struct PagesDirectoryStructure {
    pub project_path: Vc<FileSystemPath>,
    pub next_router_path: Vc<FileSystemPath>,
    pub items: Vec<Vc<PagesStructureItem>>,
    pub children: Vec<Vc<PagesDirectoryStructure>>,
}

#[turbo_tasks::value_impl]
impl PagesDirectoryStructure {
    /// Returns the router path of this directory.
    #[turbo_tasks::function]
    pub async fn next_router_path(self: Vc<Self>) -> Result<Vc<FileSystemPath>> {
        Ok(self.await?.next_router_path)
    }

    /// Returns the path to the directory of this structure in the project file
    /// system.
    #[turbo_tasks::function]
    pub async fn project_path(self: Vc<Self>) -> Result<Vc<FileSystemPath>> {
        Ok(self.await?.project_path)
    }

    /// Returns a completion that changes when any route in the whole tree
    /// changes.
    #[turbo_tasks::function]
    pub async fn routes_changed(self: Vc<Self>) -> Result<Vc<Completion>> {
        for item in self.await?.items.iter() {
            item.routes_changed().await?;
        }
        for child in self.await?.children.iter() {
            child.routes_changed().await?;
        }
        Ok(Completion::new())
    }
}

/// Finds and returns the [PagesStructure] of the pages directory if existing.
#[turbo_tasks::function]
pub async fn find_pages_structure(
    project_root: Vc<FileSystemPath>,
    next_router_root: Vc<FileSystemPath>,
    next_config: Vc<NextConfig>,
) -> Result<Vc<OptionPagesStructure>> {
    let pages_root = project_root.join("pages".to_string());
    let pages_root = if *pages_root.get_type().await? == FileSystemEntryType::Directory {
        pages_root
    } else {
        let src_pages_root = project_root.join("src/pages".to_string());
        if *src_pages_root.get_type().await? == FileSystemEntryType::Directory {
            src_pages_root
        } else {
            return Ok(Vc::cell(None));
        }
    }
    .resolve()
    .await?;

    Ok(Vc::cell(Some(get_pages_structure_for_root_directory(
        pages_root,
        next_router_root,
        next_config.page_extensions(),
    ))))
}

/// Handles the root pages directory.
#[turbo_tasks::function]
async fn get_pages_structure_for_root_directory(
    project_path: Vc<FileSystemPath>,
    next_router_path: Vc<FileSystemPath>,
    page_extensions: Vc<Vec<String>>,
) -> Result<Vc<PagesStructure>> {
    let page_extensions_raw = &*page_extensions.await?;

    let mut children = vec![];
    let mut items = vec![];
    let mut app_item = None;
    let mut document_item = None;
    let mut error_item = None;
    let mut api_directory = None;
    let specificity = Specificity::exact();
    let dir_content = project_path.read_dir().await?;
    if let DirectoryContent::Entries(entries) = &*dir_content {
        for (name, entry) in entries.iter() {
            match entry {
                DirectoryEntry::File(file_project_path) => {
                    let Some(basename) = page_basename(name, page_extensions_raw) else {
                        continue;
                    };
                    match basename {
                        "_app" => {
                            let _ = app_item.insert(PagesStructureItem::new(
                                *file_project_path,
                                next_router_path.join("_app".to_string()),
                                specificity,
                            ));
                        }
                        "_document" => {
                            let _ = document_item.insert(PagesStructureItem::new(
                                *file_project_path,
                                next_router_path.join("_document".to_string()),
                                specificity,
                            ));
                        }
                        "_error" => {
                            let _ = error_item.insert(PagesStructureItem::new(
                                *file_project_path,
                                next_router_path.join("_error".to_string()),
                                specificity,
                            ));
                        }
                        basename => {
                            let specificity = entry_specificity(specificity, name, 0);
                            let next_router_path =
                                next_router_path_for_basename(next_router_path, basename);
                            items.push((
                                basename,
                                PagesStructureItem::new(
                                    *file_project_path,
                                    next_router_path,
                                    specificity,
                                ),
                            ));
                        }
                    }
                }
                DirectoryEntry::Directory(dir_project_path) => match name.as_ref() {
                    "api" => {
                        let _ = api_directory.insert(get_pages_structure_for_directory(
                            *dir_project_path,
                            next_router_path.join(name.clone()),
                            specificity,
                            1,
                            page_extensions,
                        ));
                    }
                    _ => {
                        let specificity = entry_specificity(Specificity::exact(), name, 0);
                        children.push((
                            name,
                            get_pages_structure_for_directory(
                                *dir_project_path,
                                next_router_path.join(name.clone()),
                                specificity,
                                1,
                                page_extensions,
                            ),
                        ));
                    }
                },
                _ => {}
            }
        }
    }

    // Ensure deterministic order since read_dir is not deterministic
    items.sort_by_key(|(k, _)| *k);
    children.sort_by_key(|(k, _)| *k);

    let app_item = if let Some(app_item) = app_item {
        app_item
    } else {
        PagesStructureItem::new(
            next_js_file_path("entry/pages/_app.tsx".to_string()),
            next_router_path.join("_app".to_string()),
            specificity,
        )
    };

    let document_item = if let Some(document_item) = document_item {
        document_item
    } else {
        PagesStructureItem::new(
            next_js_file_path("entry/pages/_document.tsx".to_string()),
            next_router_path.join("_document".to_string()),
            specificity,
        )
    };

    let error_item = if let Some(error_item) = error_item {
        error_item
    } else {
        PagesStructureItem::new(
            next_js_file_path("entry/pages/_error.tsx".to_string()),
            next_router_path.join("_error".to_string()),
            specificity,
        )
    };

    Ok(PagesStructure {
        app: app_item,
        document: document_item,
        error: error_item,
        api: api_directory,
        pages: PagesDirectoryStructure {
            project_path,
            next_router_path,
            items: items.into_iter().map(|(_, v)| v).collect(),
            children: children.into_iter().map(|(_, v)| v).collect(),
        }
        .cell(),
    }
    .cell())
}

/// Handles a directory in the pages directory (or the pages directory itself).
/// Calls itself recursively for sub directories or the
/// [create_page_source_for_file] method for files.
#[turbo_tasks::function]
async fn get_pages_structure_for_directory(
    project_path: Vc<FileSystemPath>,
    next_router_path: Vc<FileSystemPath>,
    specificity: Vc<Specificity>,
    position: u32,
    page_extensions: Vc<Vec<String>>,
) -> Result<Vc<PagesDirectoryStructure>> {
    let page_extensions_raw = &*page_extensions.await?;

    let mut children = vec![];
    let mut items = vec![];
    let dir_content = project_path.read_dir().await?;
    if let DirectoryContent::Entries(entries) = &*dir_content {
        for (name, entry) in entries.iter() {
            let specificity = entry_specificity(specificity, name, position);
            match entry {
                DirectoryEntry::File(file_project_path) => {
                    let Some(basename) = page_basename(name, page_extensions_raw) else {
                        continue;
                    };
                    let next_router_path = match basename {
                        "index" => next_router_path,
                        _ => next_router_path.join(basename.to_string()),
                    };
                    items.push((
                        basename,
                        PagesStructureItem::new(*file_project_path, next_router_path, specificity),
                    ));
                }
                DirectoryEntry::Directory(dir_project_path) => {
                    children.push((
                        name,
                        get_pages_structure_for_directory(
                            *dir_project_path,
                            next_router_path.join(name.clone()),
                            specificity,
                            position + 1,
                            page_extensions,
                        ),
                    ));
                }
                _ => {}
            }
        }
    }

    // Ensure deterministic order since read_dir is not deterministic
    items.sort_by_key(|(k, _)| *k);

    // Ensure deterministic order since read_dir is not deterministic
    children.sort_by_key(|(k, _)| *k);

    Ok(PagesDirectoryStructure {
        project_path,
        next_router_path,
        items: items.into_iter().map(|(_, v)| v).collect(),
        children: children.into_iter().map(|(_, v)| v).collect(),
    }
    .cell())
}

fn entry_specificity(specificity: Vc<Specificity>, name: &str, position: u32) -> Vc<Specificity> {
    if name.starts_with("[[") || name.starts_with("[...") {
        specificity.with_catch_all(position)
    } else if name.starts_with('[') {
        specificity.with_dynamic_segment(position)
    } else {
        specificity
    }
}

fn page_basename<'a>(name: &'a str, page_extensions: &'a [String]) -> Option<&'a str> {
    if let Some((basename, extension)) = name.rsplit_once('.') {
        if page_extensions.iter().any(|allowed| allowed == extension) {
            return Some(basename);
        }
    }
    None
}

fn next_router_path_for_basename(
    next_router_path: Vc<FileSystemPath>,
    basename: &str,
) -> Vc<FileSystemPath> {
    if basename == "index" {
        next_router_path
    } else {
        next_router_path.join(basename.to_string())
    }
}
