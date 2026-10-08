//! Resolve only indexed links and capture only referenced images. The returned
//! asset map contains immutable bytes; chunk reads never reopen these paths.

use super::{
    assets,
    config::{Policy, Relative},
    history,
    index::{id, Index},
    markdown::{self, Resolve},
    source::Root,
};
use crew_core::rpc::docs::*;
use crew_core::rpc::features::{Id, Label, List, Text};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::Instant;

#[derive(Clone)]
pub struct LinkRow {
    pub id: Id,
    pub sources: Vec<(Id, Vec<DocHeading>)>,
    pub workspace: Option<Id>,
}
pub type Links = BTreeMap<Relative, LinkRow>;
pub fn links(index: &Index) -> Links {
    index
        .rows
        .iter()
        .map(|(p, e)| {
            let mut sources: Vec<_> = e
                .variants
                .iter()
                .filter(|v| v.state == DocState::Present)
                .map(|v| (v.source_id.clone(), v.headings.to_vec()))
                .collect();
            sources.sort_by_key(|(id, _)| index.sources.get(id).map(|s| s.key.clone()));
            (
                p.clone(),
                LinkRow {
                    id: e.id.clone(),
                    sources,
                    workspace: index
                        .sources
                        .values()
                        .find(|s| s.record.kind == DocSourceKind::Workspace)
                        .map(|s| s.record.id.clone()),
                },
            )
        })
        .collect()
}

#[derive(Clone)]
pub enum Content {
    Working(Arc<Root>),
    Recorded {
        reader: Arc<history::Reader>,
        commit: Id,
        deadline: Instant,
    },
}
impl Content {
    pub fn read(
        &self,
        path: &Relative,
        policy: &Policy,
        max: usize,
    ) -> Result<Vec<u8>, DocFailureCode> {
        if !policy.allows(path) {
            return Err(DocFailureCode::ForbiddenPath);
        }
        match self {
            Self::Working(root) => root.read(path, policy, max).map(|r| r.bytes),
            Self::Recorded {
                reader,
                commit,
                deadline,
            } => reader.blob(commit, path, max, *deadline),
        }
    }
}
#[derive(Clone)]
pub struct Asset {
    pub bytes: Arc<[u8]>,
    pub hash: Id,
    pub raster: assets::Raster,
    pub path: Relative,
}
pub struct Rendered {
    pub snapshot: DocSnapshot,
    pub assets: BTreeMap<Id, Asset>,
    pub charge: usize,
}

pub fn capture(
    epoch: Id,
    version: DocVersion,
    markdown: String,
    path: Relative,
    policy: Policy,
    content: Content,
    links: Links,
) -> Result<Rendered, DocFailureCode> {
    let mut resolver = Resolver {
        path,
        source: version.document.source_id.clone(),
        policy,
        content,
        links,
        assets: BTreeMap::new(),
        asset_paths: BTreeMap::new(),
        asset_bytes: 0,
    };
    let parsed = markdown::parse(&markdown, &mut resolver)?;
    let image_content: Vec<_> = parsed
        .images
        .iter()
        .map(|image| {
            (
                &image.relative_path,
                &image.hash,
                image.media_type,
                image.bytes,
                image.width,
                image.height,
                image.state,
                image.reason,
            )
        })
        .collect();
    let render_hash = markdown::hash(
        &serde_json::to_vec(&(&version.markdown_hash, image_content))
            .map_err(|_| DocFailureCode::TooLarge)?,
    );
    let snapshot = DocSnapshot::try_from(DocSnapshotData {
        epoch,
        snapshot_id: id(),
        version,
        markdown: Text::new(markdown).map_err(|_| DocFailureCode::TooLarge)?,
        blocks: List::new(parsed.blocks).map_err(|_| DocFailureCode::TooLarge)?,
        outline: List::new(parsed.outline).expect("bounded outline"),
        images: List::new(parsed.images).expect("bounded images"),
        render_hash,
        notices: List::new(if parsed.headings_limited {
            vec![markdown::label("Outline shortened to its display limit")]
        } else {
            Vec::new()
        })
        .expect("one notice"),
    })
    .map_err(|_| DocFailureCode::TooLarge)?;
    // Leave envelope margin within the 512 KiB target, not merely the 1 MiB cap.
    let bytes = super::bounded_size(&snapshot, 500 * 1024)?;
    let charge = bytes.saturating_mul(3) + resolver.asset_bytes + 4096;
    Ok(Rendered {
        snapshot,
        assets: resolver.assets,
        charge,
    })
}

struct Resolver {
    path: Relative,
    source: Id,
    policy: Policy,
    content: Content,
    links: Links,
    assets: BTreeMap<Id, Asset>,
    asset_paths: BTreeMap<Relative, DocImage>,
    asset_bytes: usize,
}
impl Resolve for Resolver {
    fn link(&self, destination: &str, own_anchors: &BTreeSet<String>) -> LinkTarget {
        if let Some(href) = external(destination) {
            return LinkTarget::External { href };
        }
        if let Some(fragment) = destination.strip_prefix('#') {
            return assets::decode(fragment)
                .ok()
                .and_then(|fragment| anchor(&fragment, own_anchors))
                .map_or_else(
                    || inert("Heading is not present in this version"),
                    |anchor| LinkTarget::Anchor { anchor },
                );
        }
        let Ok((path, fragment)) = assets::local_target(&self.path, destination) else {
            return inert("Link is outside the docs folders");
        };
        if !self.policy.selects(&path) {
            return inert("Link is outside the docs folders");
        }
        let Some(row) = self.links.get(&path) else {
            return inert("Document is not indexed");
        };
        let preferred = row
            .sources
            .iter()
            .find(|(s, _)| s == &self.source)
            .or_else(|| {
                row.sources
                    .iter()
                    .find(|(s, _)| Some(s) == row.workspace.as_ref())
            })
            .or_else(|| row.sources.first());
        let Some((source, headings)) = preferred else {
            return inert("Document is unavailable");
        };
        // Same-document headings use the opened version, never another source's
        // current outline. Other targets verify their anchor on opening too.
        let anchor = fragment.and_then(|f| {
            if path == self.path {
                anchor(&f, own_anchors)
            } else {
                heading(&f, headings)
            }
        });
        LinkTarget::Doc {
            doc_id: row.id.clone(),
            preferred_source: Some(source.clone()),
            anchor,
        }
    }
    fn image(&mut self, destination: &str, alt: Label) -> DocImage {
        if let Some(href) = external(destination) {
            let mut image = placeholder(alt, DocImageReason::Remote);
            image.safe_external_href = Some(href);
            return image;
        }
        let Ok((path, fragment)) = assets::local_target(&self.path, destination) else {
            return placeholder(
                alt,
                DocImageReason::Blocked(BlockedImageKind::OutsideSource),
            );
        };
        if fragment.is_some() || !self.policy.allows(&path) {
            return placeholder(alt, DocImageReason::Blocked(BlockedImageKind::NotAllowed));
        }
        if let Some(image) = self.asset_paths.get(&path) {
            let mut image = image.clone();
            image.alt = alt;
            return image;
        }
        let mut image = placeholder(alt, DocImageReason::Invalid);
        image.relative_path = Some(Text::new(path.as_str()).expect("relative bound"));
        let bytes = match self
            .content
            .read(&path, &self.policy, MAX_IMAGE_BYTES as usize)
        {
            Ok(bytes) => bytes,
            Err(code) => {
                image.reason = Some(match code {
                    DocFailureCode::TooLarge => {
                        DocImageReason::TooLarge(ImageLimitKind::ImageBytes)
                    }
                    DocFailureCode::ObjectMissing => DocImageReason::Missing,
                    DocFailureCode::UnstableFile => DocImageReason::Unstable,
                    _ => DocImageReason::Blocked(BlockedImageKind::NotAllowed),
                });
                set_state(&mut image);
                return image;
            }
        };
        image.bytes = Some(bytes.len() as u32);
        let raster = match assets::inspect(&bytes) {
            Ok(r) => r,
            Err(reason) => {
                image.reason = Some(reason);
                set_state(&mut image);
                return image;
            }
        };
        image.width = Some(raster.width);
        image.height = Some(raster.height);
        image.media_type = Some(raster.kind);
        let hash = markdown::hash(&bytes);
        let existing = self
            .assets
            .iter()
            .find(|(_, a)| a.hash == hash)
            .map(|(id, _)| id.clone());
        if existing.is_none() && self.asset_bytes + bytes.len() > MAX_DOCUMENT_IMAGE_BYTES as usize
        {
            image.reason = Some(DocImageReason::TooLarge(ImageLimitKind::DocumentBudget));
            set_state(&mut image);
            return image;
        }
        let asset_id = existing.unwrap_or_else(|| {
            let key = id();
            self.asset_bytes += bytes.len();
            self.assets.insert(
                key.clone(),
                Asset {
                    bytes: bytes.into(),
                    hash: hash.clone(),
                    raster,
                    path: path.clone(),
                },
            );
            key
        });
        image.asset_id = Some(asset_id);
        image.hash = Some(hash);
        image.state = DocImageState::Ready;
        image.reason = None;
        self.asset_paths.insert(path, image.clone());
        image
    }
}
fn external(value: &str) -> Option<ExternalHref> {
    Text::new(value)
        .ok()
        .and_then(|s| ExternalHref::try_from(s).ok())
}
fn inert(reason: &str) -> LinkTarget {
    LinkTarget::Text {
        reason: markdown::label(reason),
    }
}
fn anchor(value: &str, anchors: &BTreeSet<String>) -> Option<Label> {
    anchors.contains(value).then(|| markdown::label(value))
}
fn heading(value: &str, headings: &[DocHeading]) -> Option<Label> {
    headings
        .iter()
        .find(|h| &*h.anchor == value)
        .map(|h| h.anchor.clone())
}
fn placeholder(alt: Label, reason: DocImageReason) -> DocImage {
    let mut image = DocImage {
        asset_id: None,
        relative_path: None,
        hash: None,
        media_type: None,
        bytes: None,
        width: None,
        height: None,
        alt,
        state: DocImageState::Blocked,
        reason: Some(reason),
        safe_external_href: None,
    };
    set_state(&mut image);
    image
}
fn set_state(image: &mut DocImage) {
    image.state = match image.reason {
        Some(DocImageReason::Remote) => DocImageState::Remote,
        Some(DocImageReason::TooLarge(_)) => DocImageState::TooLarge,
        Some(DocImageReason::Unsupported(_)) => DocImageState::Unsupported,
        Some(DocImageReason::Expired) => DocImageState::Expired,
        _ => DocImageState::Blocked,
    };
}
