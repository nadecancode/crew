//! CommonMark events become a closed data tree, never HTML. Unsupported
//! extensions are not enabled; HTML is preserved as literal text/code.

use std::collections::{BTreeMap, BTreeSet};

use crew_core::rpc::docs::{self, *};
use crew_core::rpc::features::{Id, Label, List, Text};
use pulldown_cmark::{CodeBlockKind, Event, Options, Parser, Tag, TagEnd};
use sha2::{Digest, Sha256};

pub struct Parsed {
    pub blocks: Vec<DocBlock>,
    pub outline: Vec<DocHeading>,
    pub headings_limited: bool,
    pub images: Vec<DocImage>,
}

pub trait Resolve {
    fn link(&self, destination: &str, own_anchors: &BTreeSet<String>) -> LinkTarget;
    fn image(&mut self, destination: &str, alt: Label) -> DocImage;
}

pub fn hash(bytes: &[u8]) -> Id {
    Id::new(format!("sha256:{:x}", Sha256::digest(bytes))).expect("fixed-size digest")
}

pub fn label(value: &str) -> Label {
    Label::new(prefix(value, 512)).expect("bounded label")
}

fn prefix(value: &str, limit: usize) -> &str {
    let mut end = value.len().min(limit);
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    &value[..end]
}

fn bounded<T, const N: usize>(values: Vec<T>) -> Result<List<T, N>, DocFailureCode> {
    List::new(values).map_err(|_| DocFailureCode::TooLarge)
}

fn text(value: impl Into<String>) -> Result<Text<MAX_MARKDOWN_BYTES>, DocFailureCode> {
    Text::new(value).map_err(|_| DocFailureCode::TooLarge)
}

fn options() -> Options {
    Options::ENABLE_TABLES | Options::ENABLE_TASKLISTS
}

struct Lines(Vec<usize>);
impl Lines {
    fn new(source: &str) -> Self {
        let mut starts = vec![0];
        for (i, b) in source.bytes().enumerate() {
            if b == b'\n' || (b == b'\r' && source.as_bytes().get(i + 1) != Some(&b'\n')) {
                starts.push(i + 1);
            }
        }
        Self(starts)
    }
    fn at(&self, offset: usize) -> u32 {
        self.0.partition_point(|start| *start <= offset) as u32
    }
}

struct Heading {
    start: usize,
    level: u8,
    title: String,
    anchor: String,
}

fn headings(source: &str) -> Result<Vec<Heading>, DocFailureCode> {
    let mut result = Vec::new();
    let mut active: Option<(usize, u8, String)> = None;
    let mut used: BTreeMap<String, usize> = BTreeMap::new();
    for (event, range) in Parser::new_ext(source, options()).into_offset_iter() {
        match event {
            Event::Start(Tag::Heading { level, .. }) => {
                active = Some((range.start, level as u8, String::new()))
            }
            Event::End(TagEnd::Heading(_)) => {
                let (start, level, title) = active.take().ok_or(DocFailureCode::TooLarge)?;
                let mut anchor = String::new();
                for c in title.trim().chars().flat_map(char::to_lowercase) {
                    if c.is_alphanumeric() || matches!(c, '-' | '_') {
                        anchor.push(c);
                    } else if c.is_whitespace() && !anchor.ends_with('-') {
                        anchor.push('-');
                    }
                }
                if anchor.is_empty() {
                    anchor = "section".into();
                }
                if anchor.len() > 480 {
                    anchor = format!(
                        "{}-{}",
                        prefix(&anchor, 480),
                        &hash(title.as_bytes()).as_str()[7..23]
                    );
                }
                let occurrence = used.entry(anchor.clone()).or_default();
                *occurrence += 1;
                if *occurrence > 1 {
                    anchor.push_str(&format!("-{occurrence}"));
                }
                if result.len() == MAX_AST_NODES {
                    return Err(DocFailureCode::TooLarge);
                }
                result.push(Heading {
                    start,
                    level,
                    title,
                    anchor,
                });
            }
            Event::Text(s) | Event::Code(s) | Event::Html(s) | Event::InlineHtml(s) => {
                if let Some((_, _, title)) = &mut active {
                    title.push_str(&s);
                }
            }
            Event::SoftBreak | Event::HardBreak => {
                if let Some((_, _, title)) = &mut active {
                    title.push(' ');
                }
            }
            _ => {}
        }
    }
    Ok(result)
}

/// Metadata parsing performs no image or link reads.
pub fn outline(source: &str) -> Result<(Vec<DocHeading>, bool), DocFailureCode> {
    if source.len() > MAX_MARKDOWN_BYTES {
        return Err(DocFailureCode::TooLarge);
    }
    let all = headings(source)?;
    let lines = Lines::new(source);
    let limited = all.len() > 128 || all.iter().any(|h| h.title.len() > 512);
    Ok((
        all.into_iter()
            .take(128)
            .map(|h| DocHeading {
                anchor: label(&h.anchor),
                title: label(&h.title),
                level: h.level,
                start_line: lines.at(h.start),
            })
            .collect(),
        limited,
    ))
}

enum Kind {
    Root,
    Paragraph,
    Heading(u8),
    Quote,
    Code(Option<Label>),
    Html,
    List(Option<u32>),
    Item,
    Table(Vec<docs::Alignment>),
    Head,
    Row,
    Cell,
    Emphasis,
    Strong,
    Link(String),
    Image(String),
    Literal,
}

struct Frame {
    kind: Kind,
    start: usize,
    end_tag: Option<TagEnd>,
    inline: Vec<Inline>,
    blocks: Vec<Block>,
    items: Vec<ListItem>,
    cells: Vec<TableCell>,
    rows: Vec<TableRow>,
    header: Vec<TableCell>,
    checked: Option<bool>,
    literal: String,
}

impl Frame {
    fn new(kind: Kind, start: usize, end_tag: Option<TagEnd>) -> Self {
        Self {
            kind,
            start,
            end_tag,
            inline: Vec::new(),
            blocks: Vec::new(),
            items: Vec::new(),
            cells: Vec::new(),
            rows: Vec::new(),
            header: Vec::new(),
            checked: None,
            literal: String::new(),
        }
    }
}

struct Build<'a, R> {
    source: &'a str,
    resolver: &'a mut R,
    frames: Vec<Frame>,
    blocks: Vec<DocBlock>,
    headings: Vec<Heading>,
    anchors: BTreeSet<String>,
    keys: BTreeMap<String, usize>,
    images: Vec<DocImage>,
    nodes: usize,
    image_references: usize,
    lines: Lines,
}

pub fn parse(source: &str, resolver: &mut impl Resolve) -> Result<Parsed, DocFailureCode> {
    if source.len() > MAX_MARKDOWN_BYTES {
        return Err(DocFailureCode::TooLarge);
    }
    let headings = headings(source)?;
    let anchors = headings.iter().map(|h| h.anchor.clone()).collect();
    let mut build = Build {
        source,
        resolver,
        frames: vec![Frame::new(Kind::Root, 0, None)],
        blocks: Vec::new(),
        headings,
        anchors,
        keys: BTreeMap::new(),
        images: Vec::new(),
        nodes: 0,
        image_references: 0,
        lines: Lines::new(source),
    };
    for (event, range) in Parser::new_ext(source, options()).into_offset_iter() {
        match event {
            Event::Start(tag) => build.start(tag, range.start)?,
            Event::End(end) => build.end(end, range.end)?,
            Event::Text(s) | Event::Html(s) | Event::InlineHtml(s) => {
                if matches!(build.current()?.kind, Kind::Code(_) | Kind::Html) {
                    let frame = build.current()?;
                    if frame.literal.len().saturating_add(s.len()) > MAX_MARKDOWN_BYTES {
                        return Err(DocFailureCode::TooLarge);
                    }
                    frame.literal.push_str(&s);
                } else {
                    build.inline(Inline::Text {
                        text: text(s.into_string())?,
                    })?;
                }
            }
            Event::Code(s) => build.inline(Inline::Code {
                text: text(s.into_string())?,
            })?,
            Event::SoftBreak => build.inline(Inline::SoftBreak)?,
            Event::HardBreak => build.inline(Inline::HardBreak)?,
            Event::Rule => {
                build.node()?;
                build.block(Block::Rule, range.start, range.end)?;
            }
            Event::TaskListMarker(checked) => {
                let item = build
                    .frames
                    .iter_mut()
                    .rev()
                    .find(|f| matches!(f.kind, Kind::Item))
                    .ok_or(DocFailureCode::TooLarge)?;
                item.checked = Some(checked);
            }
            // These extensions are disabled. If a future pinned parser emits
            // one, retain its exact source spelling instead of interpreting it.
            Event::InlineMath(_) | Event::DisplayMath(_) | Event::FootnoteReference(_) => {
                build.inline(Inline::Text {
                    text: text(&source[range])?,
                })?;
            }
        }
    }
    if build.frames.len() != 1 {
        return Err(DocFailureCode::TooLarge);
    }
    let limited = build.headings.len() > 128 || build.headings.iter().any(|h| h.title.len() > 512);
    let outline = build
        .headings
        .iter()
        .take(128)
        .map(|h| DocHeading {
            anchor: label(&h.anchor),
            title: label(&h.title),
            level: h.level,
            start_line: build.lines.at(h.start),
        })
        .collect();
    Ok(Parsed {
        blocks: build.blocks,
        outline,
        headings_limited: limited,
        images: build.images,
    })
}

impl<R: Resolve> Build<'_, R> {
    fn current(&mut self) -> Result<&mut Frame, DocFailureCode> {
        self.frames.last_mut().ok_or(DocFailureCode::TooLarge)
    }
    fn node(&mut self) -> Result<(), DocFailureCode> {
        self.nodes += 1;
        if self.nodes > MAX_AST_NODES {
            Err(DocFailureCode::TooLarge)
        } else {
            Ok(())
        }
    }
    fn inline(&mut self, node: Inline) -> Result<(), DocFailureCode> {
        self.node()?;
        self.current()?.inline.push(node);
        Ok(())
    }
    fn flush_inline(&mut self) -> Result<(), DocFailureCode> {
        if !self.current()?.inline.is_empty() {
            self.node()?;
            let inline = std::mem::take(&mut self.current()?.inline);
            self.current()?.blocks.push(Block::Paragraph {
                children: bounded(inline)?,
            });
        }
        Ok(())
    }
    fn block(&mut self, block: Block, start: usize, end: usize) -> Result<(), DocFailureCode> {
        if matches!(self.current()?.kind, Kind::Root) {
            let digest = hash(&serde_json::to_vec(&block).map_err(|_| DocFailureCode::TooLarge)?);
            let occurrence = self.keys.entry(digest.as_str().to_owned()).or_default();
            *occurrence += 1;
            let key = Id::new(format!("{}-{occurrence}", digest.as_str()))
                .map_err(|_| DocFailureCode::TooLarge)?;
            self.blocks.push(DocBlock {
                block_key: key,
                start_line: self.lines.at(start),
                end_line: self.lines.at(end.saturating_sub(1).max(start)),
                block,
            });
        } else {
            self.flush_inline()?;
            self.current()?.blocks.push(block);
        }
        Ok(())
    }
    fn start(&mut self, tag: Tag<'_>, start: usize) -> Result<(), DocFailureCode> {
        if self.frames.len() >= MAX_AST_DEPTH {
            return Err(DocFailureCode::TooLarge);
        }
        let end = tag.to_end();
        let kind = match tag {
            Tag::Paragraph => Kind::Paragraph,
            Tag::Heading { level, .. } => Kind::Heading(level as u8),
            Tag::BlockQuote(_) => Kind::Quote,
            Tag::CodeBlock(kind) => Kind::Code(match kind {
                CodeBlockKind::Fenced(info) => info
                    .split_whitespace()
                    .next()
                    .filter(|s| {
                        s.len() <= 32
                            && s.bytes().all(|b| {
                                b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'+')
                            })
                    })
                    .map(label),
                CodeBlockKind::Indented => None,
            }),
            Tag::HtmlBlock => Kind::Html,
            Tag::List(start) => Kind::List(
                start
                    .map(u32::try_from)
                    .transpose()
                    .map_err(|_| DocFailureCode::TooLarge)?,
            ),
            Tag::Item => Kind::Item,
            Tag::Table(align) => Kind::Table(
                align
                    .into_iter()
                    .map(|a| match a {
                        pulldown_cmark::Alignment::None => docs::Alignment::None,
                        pulldown_cmark::Alignment::Left => docs::Alignment::Left,
                        pulldown_cmark::Alignment::Center => docs::Alignment::Center,
                        pulldown_cmark::Alignment::Right => docs::Alignment::Right,
                    })
                    .collect(),
            ),
            Tag::TableHead => Kind::Head,
            Tag::TableRow => Kind::Row,
            Tag::TableCell => Kind::Cell,
            Tag::Emphasis => Kind::Emphasis,
            Tag::Strong => Kind::Strong,
            Tag::Link { dest_url, .. } => Kind::Link(dest_url.into_string()),
            Tag::Image { dest_url, .. } => Kind::Image(dest_url.into_string()),
            _ => Kind::Literal,
        };
        self.node()?;
        self.frames.push(Frame::new(kind, start, Some(end)));
        Ok(())
    }
    fn end(&mut self, tag: TagEnd, end: usize) -> Result<(), DocFailureCode> {
        let mut frame = self.frames.pop().ok_or(DocFailureCode::TooLarge)?;
        if frame.end_tag != Some(tag) {
            return Err(DocFailureCode::TooLarge);
        }
        let start = frame.start;
        let block = match frame.kind {
            Kind::Paragraph => Some(Block::Paragraph {
                children: bounded(frame.inline)?,
            }),
            Kind::Heading(level) => {
                let heading = self
                    .headings
                    .iter()
                    .find(|h| h.start == start)
                    .ok_or(DocFailureCode::TooLarge)?;
                Some(Block::Heading {
                    level,
                    anchor: label(&heading.anchor),
                    children: bounded(frame.inline)?,
                })
            }
            Kind::Code(language_hint) => Some(Block::CodeBlock {
                language_hint,
                literal: text(frame.literal)?,
            }),
            Kind::Html => Some(Block::CodeBlock {
                language_hint: None,
                literal: text(frame.literal)?,
            }),
            Kind::Quote => Some(Block::Quote {
                blocks: bounded(frame.blocks)?,
            }),
            Kind::List(first) => Some(Block::List {
                ordered: first.is_some(),
                start: first,
                items: bounded(frame.items)?,
            }),
            Kind::Item => {
                if !frame.inline.is_empty() {
                    self.node()?;
                    frame.blocks.push(Block::Paragraph {
                        children: bounded(frame.inline)?,
                    });
                }
                self.current()?.items.push(ListItem {
                    checked: frame.checked,
                    blocks: bounded(frame.blocks)?,
                });
                None
            }
            Kind::Table(alignments) => Some(Block::Table {
                alignments: bounded(alignments)?,
                header: bounded(frame.header)?,
                rows: bounded(frame.rows)?,
            }),
            Kind::Head => {
                self.current()?.header = frame.cells;
                None
            }
            Kind::Row => {
                self.current()?.rows.push(bounded(frame.cells)?);
                None
            }
            Kind::Cell => {
                self.current()?.cells.push(bounded(frame.inline)?);
                None
            }
            Kind::Emphasis => {
                self.current()?.inline.push(Inline::Emphasis {
                    children: bounded(frame.inline)?,
                });
                None
            }
            Kind::Strong => {
                self.current()?.inline.push(Inline::Strong {
                    children: bounded(frame.inline)?,
                });
                None
            }
            Kind::Link(destination) => {
                let target = self.resolver.link(&destination, &self.anchors);
                self.current()?.inline.push(Inline::Link {
                    label: bounded(frame.inline)?,
                    target,
                });
                None
            }
            Kind::Image(destination) => {
                self.image_references += 1;
                if self.image_references > MAX_IMAGES {
                    return Err(DocFailureCode::TooLarge);
                }
                let alt = label(&inline_text(&frame.inline));
                let image = self.resolver.image(&destination, alt.clone());
                let index = self.images.len();
                self.images.push(image);
                self.current()?.inline.push(Inline::Image {
                    alt,
                    asset: index as u16,
                });
                None
            }
            Kind::Literal => Some(Block::CodeBlock {
                language_hint: None,
                literal: text(&self.source[start..end])?,
            }),
            Kind::Root => return Err(DocFailureCode::TooLarge),
        };
        if let Some(block) = block {
            self.block(block, start, end)?;
        }
        Ok(())
    }
}

fn inline_text(nodes: &[Inline]) -> String {
    let mut text = String::new();
    for node in nodes {
        match node {
            Inline::Text { text: value } | Inline::Code { text: value } => text.push_str(value),
            Inline::Emphasis { children }
            | Inline::Strong { children }
            | Inline::Link {
                label: children, ..
            } => text.push_str(&inline_text(children)),
            Inline::Image { alt, .. } => text.push_str(alt),
            Inline::SoftBreak | Inline::HardBreak => text.push(' '),
        }
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Resolver;
    impl Resolve for Resolver {
        fn link(&self, destination: &str, own: &BTreeSet<String>) -> LinkTarget {
            if let Some(anchor) = destination.strip_prefix('#').filter(|s| own.contains(*s)) {
                return LinkTarget::Anchor {
                    anchor: label(anchor),
                };
            }
            match Text::new(destination).and_then(ExternalHref::try_from) {
                Ok(href) => LinkTarget::External { href },
                Err(_) => LinkTarget::Text {
                    reason: label("unknown or disallowed link"),
                },
            }
        }
        fn image(&mut self, _: &str, alt: Label) -> DocImage {
            DocImage {
                asset_id: None,
                relative_path: None,
                hash: None,
                media_type: None,
                bytes: None,
                width: None,
                height: None,
                alt,
                state: DocImageState::Unsupported,
                reason: Some(DocImageReason::Unsupported(UnsupportedImageKind::Format)),
                safe_external_href: None,
            }
        }
    }

    fn checked(source: &str) -> DocSnapshot {
        let parsed = parse(source, &mut Resolver).unwrap();
        DocSnapshotData {
            epoch: Id::new("epoch").unwrap(),
            snapshot_id: Id::new("snapshot").unwrap(),
            version: DocVersionData {
                id: Id::new("version").unwrap(),
                document: DocRef {
                    doc_id: Id::new("doc").unwrap(),
                    source_id: Id::new("source").unwrap(),
                    source_generation: crew_core::rpc::features::Wide(1),
                },
                kind: DocVersionKind::Working,
                markdown_hash: hash(source.as_bytes()),
                bytes: source.len() as u32,
                observed_at_ms: None,
                recorded_at_ms: None,
                commit_id: None,
                change_id: None,
                author: None,
                subject: None,
            }
            .try_into()
            .unwrap(),
            markdown: Text::new(source).unwrap(),
            blocks: List::new(parsed.blocks).unwrap(),
            outline: List::new(parsed.outline).unwrap(),
            images: List::new(parsed.images).unwrap(),
            render_hash: hash(source.as_bytes()),
            notices: List::default(),
        }
        .try_into()
        .unwrap()
    }

    #[test]
    fn k06_supported_nodes_form_a_valid_closed_tree() {
        let source = "# Heading\n\nA *soft* **strong** `code` [link](https://example.test)\nline.  \nBreak.\n\n> Quote\n\n- [x] done\n- [ ] later\n  - nested\n\n1. numbered\n\n| A | B |\n| :- | -: |\n| X | Y |\n\n```rust\nlet x = 1;\n```\n\n---\n\n![alt](image.png)\n";
        let value = checked(source);
        let json = serde_json::to_string(&value).unwrap();
        for kind in [
            "heading",
            "paragraph",
            "emphasis",
            "strong",
            "code",
            "link",
            "soft_break",
            "hard_break",
            "quote",
            "list",
            "table",
            "code_block",
            "rule",
            "image",
        ] {
            assert!(
                json.contains(&format!("\"kind\":\"{kind}\"")),
                "missing {kind}"
            );
        }
        assert!(json.contains("\"checked\":true"));
        assert_eq!(value.blocks[0].start_line, 1);
        assert_eq!(value.outline[0].anchor.to_string(), "heading");
    }

    #[test]
    fn k06_html_and_scripts_are_literal_and_bad_links_are_inert() {
        let source = "# Links\n\n<script src=\"https://example.test/x\">alert(1)</script>\n\nHello <b onclick='run()'>text</b>. [bad](javascript:run) [data](data:text/html,x) [local](#links)\n";
        let value = checked(source);
        assert!(value.blocks.iter().any(
            |b| matches!(&b.block, Block::CodeBlock { literal, .. } if literal.contains("<script"))
        ));
        let json = serde_json::to_string(&value).unwrap();
        assert!(!json.contains("\"kind\":\"external\""));
        assert!(json.contains("\"kind\":\"anchor\""));
        assert!(json.contains("onclick")); // Only a literal string in a typed text node.
        assert!(!json.contains("\"kind\":\"html\""));
    }

    #[test]
    fn k05_k08_node_depth_and_outline_limits_are_explicit() {
        assert!(parse(&"x".repeat(MAX_MARKDOWN_BYTES + 1), &mut Resolver).is_err());
        assert!(parse(&"paragraph\n\n".repeat(MAX_AST_NODES), &mut Resolver).is_err());
        assert!(parse(
            &format!("{}x\n", "> ".repeat(MAX_AST_DEPTH + 1)),
            &mut Resolver
        )
        .is_err());
        assert!(parse(&"![x](y) ".repeat(MAX_IMAGES + 1), &mut Resolver).is_err());
        let source = "# repeated\n\n".repeat(130);
        let parsed = parse(&source, &mut Resolver).unwrap();
        assert!(parsed.headings_limited);
        assert_eq!(parsed.outline.len(), 128);
        assert_eq!(parsed.outline[1].anchor.to_string(), "repeated-2");
        checked(&source);
    }

    #[test]
    fn k09_k18_block_keys_anchors_and_line_ranges_are_deterministic() {
        let source = "# Heading\r\n\r\nsame\r\n\r\nsame\r\n";
        let a = checked(source);
        let b = checked(source);
        assert_eq!(a.blocks, b.blocks);
        assert_eq!(a.blocks[1].start_line, 3);
        assert_eq!(a.blocks[2].start_line, 5);
        assert!(a.blocks[1].block_key.as_str().ends_with("-1"));
        assert!(a.blocks[2].block_key.as_str().ends_with("-2"));
        let long = format!("# {}\n", "界".repeat(700));
        let parsed = parse(&long, &mut Resolver).unwrap();
        assert!(parsed.headings_limited);
        assert!(parsed.outline[0].anchor.len() <= 512);
        checked(&long);
    }
}
