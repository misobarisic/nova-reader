//! Page identities and resources shared by overview loading and the viewer.
use crate::page_store::PageImage;
use reader_domain::ChapterId;
use reader_sources::{Page, Source};
use std::{path::PathBuf, sync::Arc};

#[derive(Clone)]
pub struct Resource {
    pub chapter: ChapterId,
    pub id: String,
    pub kind: ResourceKind,
}

#[derive(Clone)]
pub enum ResourceKind {
    Unavailable {
        message: String,
    },
    Local {
        path: PathBuf,
        expected_bytes: u64,
        sha256: String,
    },
    Remote {
        source: Arc<Source>,
        page: Page,
    },
}

impl Resource {
    /// Read the original page only. A cached thumbnail is never sufficient here.
    /// A downloaded page may be stored in any layout; it is verified as the file
    /// it is, then presented as a page.
    pub fn read(&self, cancel: &reader_sources::CancelToken) -> Result<PageImage, String> {
        use sha2::{Digest, Sha256};
        use std::io::Read;
        cancel.check().map_err(crate::services::error)?;
        let cap = reader_engine::current_limits().max_encoded_bytes;
        match &self.kind {
            ResourceKind::Unavailable { message } => Err(message.clone()),
            ResourceKind::Remote { source, page } => source
                .fetch_page_limited(&page.url, &page.headers, cancel, cap)
                .map(PageImage::from_encoded)
                .map_err(crate::services::error),
            ResourceKind::Local {
                path,
                expected_bytes,
                sha256,
            } => {
                let unavailable = || {
                    nova_reader_ui::backend_text::tr("Full-resolution image is unavailable offline")
                };
                let metadata = std::fs::symlink_metadata(path).map_err(|_| unavailable())?;
                if !metadata.is_file()
                    || metadata.file_type().is_symlink()
                    || metadata.len() != *expected_bytes
                {
                    return Err(unavailable());
                }
                if *expected_bytes > cap as u64 {
                    return Err(nova_reader_ui::backend_text::tr(
                        "Image exceeds the configured file size limit",
                    ));
                }
                let mut file = std::fs::File::open(path).map_err(|_| unavailable())?;
                let mut bytes = Vec::new();
                let mut hash = Sha256::new();
                let mut buffer = [0; 65536];
                loop {
                    cancel.check().map_err(crate::services::error)?;
                    let count = file.read(&mut buffer).map_err(crate::services::error)?;
                    if count == 0 {
                        break;
                    }
                    if count > cap.saturating_sub(bytes.len()) {
                        return Err(nova_reader_ui::backend_text::tr(
                            "Image exceeds the configured file size limit",
                        ));
                    }
                    hash.update(&buffer[..count]);
                    bytes.extend_from_slice(&buffer[..count]);
                }
                cancel.check().map_err(crate::services::error)?;
                if bytes.len() as u64 != *expected_bytes
                    || format!("{:x}", hash.finalize()) != *sha256
                {
                    return Err(unavailable());
                }
                PageImage::from_stored(bytes).map_err(|_| unavailable())
            }
        }
    }
}

#[derive(Clone)]
pub struct Inventory {
    pub chapter: ChapterId,
    pub pages: Arc<Vec<Resource>>,
}

/// Loading an inventory never constructs a reader, writes progress/history,
/// starts fullscreen lifecycle work or enqueues chapter downloads.
pub fn load(
    services: &crate::services::Services,
    chapter: &ChapterId,
    cancel: &reader_sources::CancelToken,
) -> Result<Inventory, String> {
    cancel.check().map_err(crate::services::error)?;
    let source = services
        .source(&chapter.series.source_id)
        .ok()
        .map(Arc::new);
    let pages = match services.downloads.page_inventory(chapter) {
        Ok(stored) => stored
            .into_iter()
            .map(|stored| {
                let id = stored.resource.id.clone();
                let kind = match stored.sha256 {
                    Some(sha256) => ResourceKind::Local {
                        path: stored.path,
                        expected_bytes: stored.bytes,
                        sha256,
                    },
                    None => match &source {
                        Some(source) => ResourceKind::Remote {
                            source: source.clone(),
                            page: Page {
                                id: id.clone(),
                                url: stored.resource.url,
                                headers: stored.resource.headers,
                                width: None,
                                height: None,
                            },
                        },
                        None => ResourceKind::Unavailable {
                            message: nova_reader_ui::backend_text::tr(
                                "Full-resolution image is unavailable offline",
                            ),
                        },
                    },
                };
                Resource {
                    chapter: chapter.clone(),
                    id,
                    kind,
                }
            })
            .collect::<Vec<_>>(),
        Err(_) => {
            let (source, pages) = services.pages(chapter, cancel)?;
            let source = Arc::new(source);
            pages
                .into_iter()
                .map(|page| Resource {
                    chapter: chapter.clone(),
                    id: page.id.clone(),
                    kind: ResourceKind::Remote {
                        source: source.clone(),
                        page,
                    },
                })
                .collect()
        }
    };
    cancel.check().map_err(crate::services::error)?;
    if pages.is_empty()
        || pages
            .iter()
            .map(|p| &p.id)
            .collect::<std::collections::BTreeSet<_>>()
            .len()
            != pages.len()
    {
        return Err(nova_reader_ui::backend_text::tr("Invalid page inventory"));
    }
    Ok(Inventory {
        chapter: chapter.clone(),
        pages: Arc::new(pages),
    })
}
