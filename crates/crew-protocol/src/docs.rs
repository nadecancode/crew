//! Closed document AST and connection-bound read handles. No raw HTML nodes,
//! arbitrary paths, filesystem roots or host commands are accepted as read authority.

use std::ops::Deref;

use serde::{Deserialize, Serialize};

use crate::features::{Id, Label, List, PageRequest, Text, Timestamp, Wide};

pub const MAX_MARKDOWN_BYTES: usize = 256 * 1024;
pub const MAX_AST_NODES: usize = 4096;
pub const MAX_AST_DEPTH: usize = 32;
pub const MAX_IMAGES: usize = 64;
pub const MAX_IMAGE_BYTES: u32 = 2 * 1024 * 1024;
pub const MAX_DOCUMENT_IMAGE_BYTES: u32 = 8 * 1024 * 1024;
pub const MAX_ASSET_CHUNK_BYTES: u32 = 192 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DocSourceKind {
    Workspace,
    Member,
    Shared,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DocSourceState {
    Ready,
    Missing,
    Removed,
    Unavailable,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DocState {
    Present,
    Missing,
    Removed,
    Unavailable,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DocFailureCode {
    Disabled,
    ForbiddenPath,
    SourceGone,
    TooLarge,
    InvalidUtf8,
    UnsupportedImage,
    UnstableFile,
    HistoryUnavailable,
    ObjectMissing,
    SnapshotExpired,
    StaleCursor,
    Busy,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocSource {
    pub id: Id,
    pub generation: Wide,
    pub kind: DocSourceKind,
    pub member_ids: List<String, 64>,
    pub display_name: Label,
    pub branch: Option<Text<1024>>,
    pub head_commit: Option<Id>,
    pub state: DocSourceState,
    pub reason: Option<DocFailureCode>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DocRef {
    pub doc_id: Id,
    pub source_id: Id,
    pub source_generation: Wide,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocHeading {
    pub anchor: Label,
    pub title: Label,
    pub level: u8,
    pub start_line: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DocComparison {
    SameMarkdown,
    Different,
    OnlyHere,
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocVariant {
    pub source_id: Id,
    pub source_generation: Wide,
    pub markdown_hash: Option<Id>,
    pub bytes: Option<u32>,
    pub observed_at_ms: Option<Timestamp>,
    pub title: Option<Label>,
    pub headings: List<DocHeading, 128>,
    pub headings_limited: bool,
    pub state: DocState,
    pub reason: Option<DocFailureCode>,
    pub comparison: DocComparison,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocEntry {
    pub id: Id,
    pub relative_path: Text<1024>,
    pub variants: List<DocVariant, 64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DocPhase {
    Building,
    Ready,
    Disabled,
    Unavailable,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DocWatchMode {
    Events,
    ReconcileOnly,
    None,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocIndexPage {
    pub epoch: Id,
    pub revision: Wide,
    pub sources: List<DocSource, 64>,
    pub entries: List<DocEntry, 200>,
    pub next_cursor: Option<Id>,
    pub phase: DocPhase,
    pub watch_mode: DocWatchMode,
    pub limited: bool,
    pub omitted_entries: Option<Wide>,
    pub reason: Option<DocFailureCode>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DocVersionKind {
    Working,
    Recorded,
    Seen,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocVersionData {
    pub id: Id,
    pub document: DocRef,
    pub kind: DocVersionKind,
    pub markdown_hash: Id,
    pub bytes: u32,
    pub observed_at_ms: Option<Timestamp>,
    pub recorded_at_ms: Option<Timestamp>,
    pub commit_id: Option<Id>,
    pub change_id: Option<Id>,
    pub author: Option<Label>,
    pub subject: Option<Label>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "DocVersionData")]
pub struct DocVersion(DocVersionData);

impl Deref for DocVersion {
    type Target = DocVersionData;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl TryFrom<DocVersionData> for DocVersion {
    type Error = &'static str;
    fn try_from(value: DocVersionData) -> Result<Self, Self::Error> {
        if value.bytes as usize > MAX_MARKDOWN_BYTES {
            return Err("document version exceeds byte limit");
        }
        if value.kind == DocVersionKind::Recorded && value.commit_id.is_none() {
            return Err("recorded document version requires commit id");
        }
        Ok(Self(value))
    }
}

/// A server-approved HTTP(S) link. It is display data, never a daemon fetch target.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "Text<2048>")]
pub struct ExternalHref(Text<2048>);

impl TryFrom<Text<2048>> for ExternalHref {
    type Error = &'static str;
    fn try_from(href: Text<2048>) -> Result<Self, Self::Error> {
        let tail = href
            .strip_prefix("https://")
            .or_else(|| href.strip_prefix("http://"));
        if href
            .chars()
            .any(|c| c.is_control() || c.is_whitespace() || c == '\\')
            || tail.is_none_or(|s| s.split(['/', '?', '#']).next().unwrap_or("").is_empty())
        {
            return Err("external link must be an absolute HTTP(S) URL");
        }
        Ok(Self(href))
    }
}

impl ExternalHref {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LinkTarget {
    Doc {
        doc_id: Id,
        preferred_source: Option<Id>,
        anchor: Option<Label>,
    },
    Anchor {
        anchor: Label,
    },
    External {
        href: ExternalHref,
    },
    Text {
        reason: Label,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Inline {
    Text {
        text: Text<MAX_MARKDOWN_BYTES>,
    },
    Emphasis {
        children: List<Inline, MAX_AST_NODES>,
    },
    Strong {
        children: List<Inline, MAX_AST_NODES>,
    },
    Code {
        text: Text<MAX_MARKDOWN_BYTES>,
    },
    SoftBreak,
    HardBreak,
    Link {
        label: List<Inline, MAX_AST_NODES>,
        target: LinkTarget,
    },
    Image {
        alt: Label,
        asset: u16,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Alignment {
    None,
    Left,
    Center,
    Right,
}

pub type TableCell = List<Inline, MAX_AST_NODES>;
pub type TableRow = List<TableCell, MAX_AST_NODES>;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListItem {
    pub checked: Option<bool>,
    pub blocks: List<Block, MAX_AST_NODES>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Block {
    Heading {
        level: u8,
        anchor: Label,
        children: List<Inline, MAX_AST_NODES>,
    },
    Paragraph {
        children: List<Inline, MAX_AST_NODES>,
    },
    CodeBlock {
        language_hint: Option<Label>,
        literal: Text<MAX_MARKDOWN_BYTES>,
    },
    Quote {
        blocks: List<Block, MAX_AST_NODES>,
    },
    List {
        ordered: bool,
        start: Option<u32>,
        items: List<ListItem, MAX_AST_NODES>,
    },
    Table {
        alignments: List<Alignment, MAX_AST_NODES>,
        header: TableRow,
        rows: List<TableRow, MAX_AST_NODES>,
    },
    Rule,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocBlock {
    pub block_key: Id,
    pub start_line: u32,
    pub end_line: u32,
    pub block: Block,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RasterType {
    Png,
    Jpeg,
    Gif,
    Webp,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DocImageState {
    Ready,
    Remote,
    TooLarge,
    Blocked,
    Unsupported,
    Expired,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UnsupportedImageKind {
    Svg,
    Animated,
    Format,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BlockedImageKind {
    OutsideSource,
    NotAllowed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImageLimitKind {
    ImageBytes,
    DocumentBudget,
    AxisPixels,
    TotalPixels,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "cause", rename_all = "snake_case")]
pub enum DocImageReason {
    Unsupported(UnsupportedImageKind),
    Blocked(BlockedImageKind),
    TooLarge(ImageLimitKind),
    Remote,
    Expired,
    Missing,
    Invalid,
    Unstable,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocImage {
    pub asset_id: Option<Id>,
    pub relative_path: Option<Text<1024>>,
    pub hash: Option<Id>,
    pub media_type: Option<RasterType>,
    pub bytes: Option<u32>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub alt: Label,
    pub state: DocImageState,
    pub reason: Option<DocImageReason>,
    pub safe_external_href: Option<ExternalHref>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocSnapshotData {
    pub epoch: Id,
    pub snapshot_id: Id,
    pub version: DocVersion,
    pub markdown: Text<MAX_MARKDOWN_BYTES>,
    pub blocks: List<DocBlock, MAX_AST_NODES>,
    pub outline: List<DocHeading, 128>,
    pub images: List<DocImage, MAX_IMAGES>,
    pub render_hash: Id,
    pub notices: List<Label, 64>,
}

/// Construct through TryFrom, including on JSON decode, so renderers never receive
/// an unchecked recursive tree. The surrounding transport still caps frame bytes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "DocSnapshotData")]
pub struct DocSnapshot(DocSnapshotData);

impl Deref for DocSnapshot {
    type Target = DocSnapshotData;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl TryFrom<DocSnapshotData> for DocSnapshot {
    type Error = &'static str;
    fn try_from(value: DocSnapshotData) -> Result<Self, Self::Error> {
        let mut budget = AstBudget {
            nodes: 0,
            images: 0,
            image_count: value.images.len(),
        };
        for block in value.blocks.iter() {
            if block.start_line == 0 || block.end_line < block.start_line {
                return Err("invalid block source lines");
            }
            budget.block(&block.block, 1)?;
        }
        for heading in value.outline.iter() {
            if !(1..=6).contains(&heading.level) || heading.start_line == 0 {
                return Err("invalid outline heading");
            }
        }
        if value.version.bytes as usize != value.markdown.len() {
            return Err("markdown byte count differs from snapshot");
        }
        let mut image_bytes = 0u32;
        let mut assets = std::collections::BTreeMap::new();
        for image in value.images.iter() {
            if image.state != DocImageState::Ready {
                continue;
            }
            let (Some(id), Some(bytes), Some(width), Some(height)) =
                (&image.asset_id, image.bytes, image.width, image.height)
            else {
                return Err("ready image lacks metadata");
            };
            if image.hash.is_none()
                || image.media_type.is_none()
                || bytes > MAX_IMAGE_BYTES
                || width == 0
                || height == 0
                || width > 8192
                || height > 8192
                || u64::from(width) * u64::from(height) > 16_000_000
            {
                return Err("invalid raster image metadata");
            }
            if let Some(old) =
                assets.insert(id, (bytes, &image.hash, image.media_type, width, height))
            {
                if old != (bytes, &image.hash, image.media_type, width, height) {
                    return Err("conflicting metadata for image asset");
                }
            } else {
                image_bytes = image_bytes
                    .checked_add(bytes)
                    .ok_or("image size overflow")?;
            }
        }
        if image_bytes > MAX_DOCUMENT_IMAGE_BYTES {
            return Err("document image byte limit");
        }
        Ok(Self(value))
    }
}

struct AstBudget {
    nodes: usize,
    images: usize,
    image_count: usize,
}

impl AstBudget {
    fn node(&mut self, depth: usize) -> Result<(), &'static str> {
        self.nodes += 1;
        if self.nodes > MAX_AST_NODES || depth > MAX_AST_DEPTH {
            Err("document AST budget exceeded")
        } else {
            Ok(())
        }
    }
    fn inline(&mut self, nodes: &[Inline], depth: usize) -> Result<(), &'static str> {
        for node in nodes {
            self.node(depth)?;
            match node {
                Inline::Emphasis { children }
                | Inline::Strong { children }
                | Inline::Link {
                    label: children, ..
                } => self.inline(children, depth + 1)?,
                Inline::Image { asset, .. } => {
                    self.images += 1;
                    if usize::from(*asset) >= self.image_count || self.images > MAX_IMAGES {
                        return Err("invalid or excessive image references");
                    }
                }
                _ => {}
            }
        }
        Ok(())
    }
    fn block(&mut self, block: &Block, depth: usize) -> Result<(), &'static str> {
        self.node(depth)?;
        match block {
            Block::Heading {
                level, children, ..
            } => {
                if !(1..=6).contains(level) {
                    return Err("invalid heading level");
                }
                self.inline(children, depth + 1)?;
            }
            Block::Paragraph { children } => self.inline(children, depth + 1)?,
            Block::Quote { blocks } => {
                for child in blocks.iter() {
                    self.block(child, depth + 1)?;
                }
            }
            Block::List { items, .. } => {
                for item in items.iter() {
                    self.node(depth + 1)?;
                    for child in item.blocks.iter() {
                        self.block(child, depth + 2)?;
                    }
                }
            }
            Block::Table {
                alignments,
                header,
                rows,
            } => {
                for row in std::iter::once(header).chain(rows.iter()) {
                    self.node(depth + 1)?;
                    if row.len() != alignments.len() {
                        return Err("table column count differs");
                    }
                    for cell in row.iter() {
                        self.node(depth + 2)?;
                        self.inline(cell, depth + 3)?;
                    }
                }
            }
            Block::CodeBlock { .. } | Block::Rule => {}
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocFailure {
    pub document: Option<DocRef>,
    pub code: DocFailureCode,
    pub detail: Label,
    pub bytes: Option<u32>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", content = "data", rename_all = "snake_case")]
pub enum DocReadResult {
    Available(Box<DocSnapshot>),
    Unavailable(DocFailure),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocAssetChunkData {
    pub snapshot_id: Id,
    pub asset_id: Id,
    pub hash: Id,
    pub media_type: RasterType,
    pub offset: u32,
    pub total_bytes: u32,
    pub data_base64: Text<262144>,
    pub next_offset: Option<u32>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "DocAssetChunkData")]
pub struct DocAssetChunk(DocAssetChunkData);

impl Deref for DocAssetChunk {
    type Target = DocAssetChunkData;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl TryFrom<DocAssetChunkData> for DocAssetChunk {
    type Error = &'static str;
    fn try_from(value: DocAssetChunkData) -> Result<Self, Self::Error> {
        let bytes = value.data_base64.as_bytes();
        let padding = bytes.iter().rev().take_while(|b| **b == b'=').count();
        let content = &bytes[..bytes.len() - padding];
        let sextet = |b: u8| match b {
            b'A'..=b'Z' => Some(b - b'A'),
            b'a'..=b'z' => Some(b - b'a' + 26),
            b'0'..=b'9' => Some(b - b'0' + 52),
            b'+' => Some(62),
            b'/' => Some(63),
            _ => None,
        };
        if bytes.len() % 4 != 0
            || padding > 2
            || content.iter().any(|b| sextet(*b).is_none())
            || (padding > 0
                && content.last().is_none_or(|b| {
                    let mask = if padding == 2 { 15 } else { 3 };
                    sextet(*b).is_none_or(|v| v & mask != 0)
                }))
        {
            return Err("invalid asset base64");
        }
        let decoded = (bytes.len() / 4 * 3)
            .checked_sub(padding)
            .ok_or("invalid asset base64")? as u32;
        let end = value
            .offset
            .checked_add(decoded)
            .ok_or("asset offset overflow")?;
        if decoded > MAX_ASSET_CHUNK_BYTES
            || value.total_bytes > MAX_IMAGE_BYTES
            || end > value.total_bytes
            || (decoded == 0 && end < value.total_bytes)
            || value.next_offset != (end < value.total_bytes).then_some(end)
        {
            return Err("invalid asset chunk range");
        }
        Ok(Self(value))
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", content = "data", rename_all = "snake_case")]
pub enum DocAssetResult {
    Available(Box<DocAssetChunk>),
    Unavailable(DocFailure),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocCapturedHead {
    pub source_id: Id,
    pub source_generation: Wide,
    pub commit_id: Id,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocHistoryPage {
    pub epoch: Id,
    pub doc_id: Id,
    pub captured_heads: List<DocCapturedHead, 64>,
    pub recorded_versions: List<DocVersion, 50>,
    pub next_cursor: Option<Id>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", content = "data", rename_all = "snake_case")]
pub enum DocHistoryResult {
    Available(Box<DocHistoryPage>),
    Unavailable(DocFailure),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocDiff {
    pub left: DocVersion,
    pub right: DocVersion,
    pub unified_diff: Text<65536>,
    pub added_lines: u32,
    pub removed_lines: u32,
    pub truncated: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", content = "data", rename_all = "snake_case")]
pub enum DocDiffResult {
    Available(Box<DocDiff>),
    Unavailable(DocFailure),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DocsWatch {
    pub selected: List<DocRef, 2>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "id", rename_all = "snake_case")]
pub enum DocVersionSelector {
    Current,
    Id(Id),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DocsRead {
    pub document: DocRef,
    pub version: DocVersionSelector,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DocsAsset {
    pub snapshot_id: Id,
    pub asset_id: Id,
    pub offset: u32,
    pub max_bytes: u32,
}

impl DocsAsset {
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.max_bytes == 0 || self.max_bytes > MAX_ASSET_CHUNK_BYTES {
            Err("invalid asset chunk limit")
        } else {
            Ok(())
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DocsHistory {
    pub doc_id: Id,
    pub source_ids: Option<List<Id, 64>>,
    pub page: PageRequest,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DocsDiff {
    pub left: Id,
    pub right: Id,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DocChangeKind {
    Content,
    Image,
    Removed,
    Policy,
    Availability,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocSelectedChange {
    pub document: DocRef,
    pub kind: DocChangeKind,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocsChanged {
    pub epoch: Id,
    pub watch_id: Id,
    pub from_revision: Wide,
    pub to_revision: Wide,
    pub upserts: List<DocEntry, 200>,
    pub removed_doc_ids: List<Id, 200>,
    pub changed_selected: List<DocSelectedChange, 2>,
    pub history_sources_changed: List<Id, 64>,
    /// Includes versions belonging to retained, currently unselected views.
    pub expired_seen_versions: List<Id, 200>,
    pub resync_required: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocsWatchResult {
    pub watch_id: Id,
    pub initial_index_page: DocIndexPage,
}
