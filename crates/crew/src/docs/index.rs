//! Bounded source registry and incremental metadata index. All roots arrive from
//! the daemon's validated workspace/worktree lifecycle, never from an RPC.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crew_core::rpc::docs::*;
use crew_core::rpc::features::{Id, List, Text, Timestamp, Wide};

use super::config::{Policy, Relative};
use super::markdown::{hash, label};
use super::source::{Root, Scan, Stamp, STEP_ENTRIES, STEP_READ_BYTES};

pub const MAX_SOURCES: usize = 64;
pub const MAX_DOCUMENTS: usize = 4000;
pub const MAX_VARIANTS: usize = 16_000;
pub const INDEX_BYTES: usize = 16 * 1024 * 1024;

pub fn id() -> Id {
    Id::new(uuid::Uuid::new_v4().to_string()).expect("UUID")
}
pub fn now() -> Timestamp {
    let ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    Timestamp::new(ms.min(Timestamp::MAX as u128) as i64).expect("bounded timestamp")
}

/// This is a trusted host API, not a wire DTO. The caller validates every path
/// against the daemon's actual workspace registrations before passing it here.
#[derive(Clone)]
pub struct Registration {
    pub key: Id,
    pub root: PathBuf,
    pub kind: DocSourceKind,
    pub member_ids: Vec<String>,
    pub display_name: String,
    pub branch: Option<String>,
    /// A published tip observed by the daemon, not a revision supplied by a reader.
    pub head_commit: Option<Id>,
    pub history: Option<Arc<super::history::Reader>>,
}

pub struct Source {
    pub key: Id,
    pub root: Arc<Root>,
    pub record: DocSource,
    pub history: Option<Arc<super::history::Reader>>,
    pub directories: BTreeSet<Relative>,
    pub watch_limited: bool,
    scan: Option<Scan>,
    reads: VecDeque<Relative>,
    seen: BTreeSet<Relative>,
    stamps: BTreeMap<Relative, Stamp>,
    scan_done: bool,
    limited: bool,
}

pub struct Index {
    pub epoch: Id,
    pub revision: Wide,
    pub policy: Policy,
    pub sources: BTreeMap<Id, Source>,
    pub rows: BTreeMap<Relative, DocEntry>,
    pub phase: DocPhase,
    pub limited: bool,
    pub config_error: bool,
    pub changed: BTreeSet<Id>,
    next_source: usize,
    metadata_bytes: usize,
    generation: u128,
}

impl Index {
    pub fn new(policy: Policy) -> Self {
        Self {
            epoch: id(),
            revision: Wide(0),
            phase: if policy.enabled {
                DocPhase::Building
            } else {
                DocPhase::Disabled
            },
            policy,
            sources: BTreeMap::new(),
            rows: BTreeMap::new(),
            limited: false,
            config_error: false,
            changed: BTreeSet::new(),
            next_source: 0,
            metadata_bytes: 0,
            generation: 0,
        }
    }

    pub fn register(&mut self, registrations: Vec<Registration>) -> Result<bool, DocFailureCode> {
        if registrations.len() > MAX_SOURCES {
            return Err(DocFailureCode::TooLarge);
        }
        // Open the entire proposed registry before changing the last good one.
        let mut opened = Vec::new();
        let mut keys = BTreeSet::new();
        for registration in registrations {
            if registration.branch.as_ref().is_some_and(|s| s.len() > 1024) {
                return Err(DocFailureCode::TooLarge);
            }
            if !keys.insert(registration.key.clone())
                || registration.member_ids.len() > 64
                || registration
                    .member_ids
                    .iter()
                    .any(|s| s.len() > 256 || s.chars().any(char::is_control))
            {
                return Err(DocFailureCode::ForbiddenPath);
            }
            let root = Arc::new(Root::open(&registration.root)?);
            opened.push((registration, root));
        }
        // Workspace wins the canonical label for shared roots.
        opened.sort_by_key(|(r, _)| (r.kind != DocSourceKind::Workspace, r.key.clone()));
        let mut shared: Vec<(Registration, Arc<Root>)> = Vec::new();
        for (registration, root) in opened {
            if let Some((existing, _)) = shared
                .iter_mut()
                .find(|(_, r)| r.identity() == root.identity())
            {
                existing.member_ids.extend(registration.member_ids);
                existing.member_ids.sort();
                existing.member_ids.dedup();
                if existing.member_ids.len() > 64 {
                    return Err(DocFailureCode::TooLarge);
                }
            } else {
                shared.push((registration, root));
            }
        }
        let mut next = BTreeMap::new();
        let mut registry_bytes = 0;
        for (registration, _) in &shared {
            // Bound all repeated page metadata before mutating the registry.
            registry_bytes += super::bounded_size(
                &(
                    &registration.member_ids,
                    label(&registration.display_name),
                    &registration.branch,
                    &registration.head_commit,
                ),
                128 * 1024,
            )?
            .saturating_add(1024);
        }
        if registry_bytes > 128 * 1024 {
            return Err(DocFailureCode::TooLarge);
        }
        let mut changed = false;
        for (registration, root) in shared {
            let previous = self
                .sources
                .iter()
                .find(|(_, s)| s.key == registration.key && s.root.identity() == root.identity())
                .map(|(id, _)| id.clone());
            let mut source = if let Some(previous) = previous {
                self.sources.remove(&previous).expect("existing source")
            } else {
                self.generation += 1;
                changed = true;
                Source {
                    key: registration.key,
                    root,
                    record: DocSource {
                        id: id(),
                        generation: Wide(self.generation),
                        kind: registration.kind,
                        member_ids: List::default(),
                        display_name: label(&registration.display_name),
                        branch: None,
                        head_commit: None,
                        state: DocSourceState::Ready,
                        reason: None,
                    },
                    history: None,
                    directories: BTreeSet::new(),
                    watch_limited: false,
                    scan: None,
                    reads: VecDeque::new(),
                    seen: BTreeSet::new(),
                    stamps: BTreeMap::new(),
                    scan_done: false,
                    limited: false,
                }
            };
            let old = source.record.clone();
            source.record.kind = registration.kind;
            source.record.member_ids =
                List::new(registration.member_ids).map_err(|_| DocFailureCode::TooLarge)?;
            source.record.display_name = label(&registration.display_name);
            source.record.branch = registration
                .branch
                .map(Text::new)
                .transpose()
                .map_err(|_| DocFailureCode::TooLarge)?;
            source.record.head_commit = registration.head_commit;
            source.history = registration.history;
            changed |= old != source.record;
            next.insert(source.record.id.clone(), source);
        }
        changed |= !self.sources.is_empty();
        self.sources = next;
        if changed {
            self.rows.values_mut().for_each(|entry| {
                entry.variants = List::new(
                    entry
                        .variants
                        .iter()
                        .filter(|v| self.sources.contains_key(&v.source_id))
                        .cloned()
                        .collect(),
                )
                .expect("subset");
            });
            self.rows.retain(|_, e| !e.variants.is_empty());
            self.recount();
            self.revision.0 += 1;
            self.rescan();
        }
        Ok(changed)
    }

    pub fn set_policy(&mut self, policy: Policy) -> bool {
        if policy == self.policy {
            self.config_error = false;
            return false;
        }
        self.policy = policy;
        self.epoch = id();
        self.revision.0 += 1;
        self.rows.clear();
        self.metadata_bytes = 0;
        self.limited = false;
        for source in self.sources.values_mut() {
            source.stamps.clear();
            source.reads.clear();
            source.scan = None;
            source.seen.clear();
        }
        self.phase = if self.policy.enabled {
            DocPhase::Building
        } else {
            DocPhase::Disabled
        };
        self.rescan();
        true
    }

    pub fn rescan(&mut self) {
        if !self.policy.enabled {
            return;
        }
        for source in self.sources.values_mut() {
            if source.scan.is_some() || !source.reads.is_empty() {
                continue;
            }
            source.seen.clear();
            source.scan_done = false;
            source.limited = false;
        }
        self.phase = DocPhase::Building;
    }

    /// One fair slice. Files are read only after safe enumeration selected them.
    pub fn advance(&mut self) -> bool {
        if !self.policy.enabled || self.sources.is_empty() {
            self.phase = if self.policy.enabled {
                DocPhase::Ready
            } else {
                DocPhase::Disabled
            };
            return false;
        }
        let started = Instant::now();
        let keys: Vec<_> = self.sources.keys().cloned().collect();
        let key = &keys[self.next_source % keys.len()];
        let source = self.sources.get_mut(key).expect("key captured");
        let mut changed = false;
        if source.scan.is_none() && !source.scan_done {
            source.directories.clear();
            source.watch_limited = false;
            match Scan::new(&source.root) {
                Ok(scan) => {
                    source.scan = Some(scan);
                    source.record.state = DocSourceState::Ready;
                    source.record.reason = None;
                }
                Err(reason) => {
                    source.record.state = DocSourceState::Unavailable;
                    source.record.reason = Some(reason);
                    source.scan_done = true;
                    source.limited = true;
                    changed = true;
                }
            }
        }
        if let Some(scan) = source.scan.as_mut().filter(|_| source.reads.is_empty()) {
            match scan.step(&source.root, &self.policy) {
                Ok(step) => {
                    let remaining = 63usize.saturating_sub(source.directories.len());
                    source.watch_limited |= step.directories.len() > remaining;
                    source
                        .directories
                        .extend(step.directories.into_iter().take(remaining));
                    source.reads.extend(step.documents);
                    source.limited |= step.limited;
                    if step.done {
                        source.scan = None;
                        source.scan_done = true;
                    }
                }
                Err(reason) => {
                    source.scan = None;
                    source.scan_done = true;
                    source.limited = true;
                    source.record.state = DocSourceState::Unavailable;
                    source.record.reason = Some(reason);
                    changed = true;
                }
            }
        }
        let mut bytes = 0;
        let mut updates = Vec::new();
        for _ in 0..STEP_ENTRIES {
            if bytes >= STEP_READ_BYTES || started.elapsed() >= Duration::from_millis(25) {
                break;
            }
            let Some(path) = source.reads.pop_front() else {
                break;
            };
            source.seen.insert(path.clone());
            let stamp = source
                .root
                .metadata(&path, &self.policy)
                .ok()
                .map(|m| Stamp::from(&m));
            if stamp.is_some() && source.stamps.get(&path) == stamp.as_ref() {
                continue;
            }
            let (mut variant, stored_stamp) = variant(source, &path, &self.policy);
            bytes += variant.bytes.unwrap_or(0).min(MAX_MARKDOWN_BYTES as u32) as usize;
            // Index metadata has its own allocation cap. Long outlines degrade
            // to explicit limited headings, not an unbounded tree of labels.
            while super::bounded_size(&variant, 4096).is_err() && !variant.headings.is_empty() {
                variant.headings =
                    List::new(variant.headings[..variant.headings.len() / 2].to_vec())
                        .expect("subset");
                variant.headings_limited = true;
            }
            if let Some(stamp) = stored_stamp {
                source.stamps.insert(path.clone(), stamp);
            }
            updates.push((path, variant));
        }
        if source.scan_done && source.reads.is_empty() && !source.limited {
            let missing: Vec<_> = source
                .stamps
                .keys()
                .filter(|p| !source.seen.contains(*p))
                .cloned()
                .collect();
            for path in missing {
                source.stamps.remove(&path);
                if let Some(entry) = self.rows.get(&path) {
                    if let Some(old) = entry.variants.iter().find(|v| v.source_id == *key) {
                        let mut v = old.clone();
                        v.state = DocState::Missing;
                        v.reason = Some(DocFailureCode::ObjectMissing);
                        updates.push((path, v));
                    }
                }
            }
        }
        self.limited |= source.limited;
        if source.scan_done && source.reads.is_empty() {
            self.next_source = (self.next_source + 1) % keys.len();
        }
        for (path, v) in updates {
            changed |= self.upsert(path, v);
        }
        let source = self.sources.get_mut(key).expect("captured source");
        source.stamps.retain(|p, _| {
            self.rows
                .get(p)
                .is_some_and(|e| e.variants.iter().any(|v| &v.source_id == key))
        });
        source.seen.retain(|p| {
            self.rows
                .get(p)
                .is_some_and(|e| e.variants.iter().any(|v| &v.source_id == key))
        });
        self.phase = if self
            .sources
            .values()
            .any(|s| !s.scan_done || !s.reads.is_empty())
        {
            DocPhase::Building
        } else {
            DocPhase::Ready
        };
        if changed {
            self.revision.0 += 1;
        }
        changed
    }

    fn upsert(&mut self, path: Relative, mut value: DocVariant) -> bool {
        let previous = self
            .rows
            .get(&path)
            .and_then(|e| e.variants.iter().find(|v| v.source_id == value.source_id));
        if let Some(previous) = previous {
            if previous.markdown_hash == value.markdown_hash
                && previous.state == value.state
                && previous.reason == value.reason
            {
                value.observed_at_ms = previous.observed_at_ms;
            }
        }
        let total: usize = self.rows.values().map(|e| e.variants.len()).sum();
        if (!self.rows.contains_key(&path) && self.rows.len() >= MAX_DOCUMENTS)
            || (previous.is_none() && total >= MAX_VARIANTS)
        {
            self.limited = true;
            return false;
        }
        let mut entry = self.rows.get(&path).cloned().unwrap_or_else(|| DocEntry {
            id: hash(format!("{}:{}", self.epoch.as_str(), path.as_str()).as_bytes()),
            relative_path: Text::new(path.as_str()).expect("relative bound"),
            variants: List::default(),
        });
        let old_size = entry_charge(&entry);
        let mut variants = entry.variants.to_vec();
        variants.retain(|v| v.source_id != value.source_id);
        variants.push(value);
        variants.sort_by(|a, b| a.source_id.cmp(&b.source_id));
        let workspace = self
            .sources
            .values()
            .find(|s| s.record.kind == DocSourceKind::Workspace)
            .map(|s| &s.record.id);
        let workspace_hash = variants
            .iter()
            .find(|v| Some(&v.source_id) == workspace && v.state == DocState::Present)
            .and_then(|v| v.markdown_hash.clone());
        for v in &mut variants {
            v.comparison = if v.state != DocState::Present {
                DocComparison::Unknown
            } else if let Some(base) = &workspace_hash {
                if v.markdown_hash.as_ref() == Some(base) {
                    DocComparison::SameMarkdown
                } else {
                    DocComparison::Different
                }
            } else {
                DocComparison::OnlyHere
            };
        }
        entry.variants = List::new(variants).expect("source bound");
        if self.rows.get(&path) == Some(&entry) {
            return false;
        }
        let new_size = entry_charge(&entry);
        let size = self
            .metadata_bytes
            .saturating_sub(if self.rows.contains_key(&path) {
                old_size
            } else {
                0
            })
            .saturating_add(new_size);
        if size > INDEX_BYTES {
            self.limited = true;
            return false;
        }
        self.metadata_bytes = size;
        self.changed.insert(entry.id.clone());
        self.rows.insert(path, entry);
        true
    }

    fn recount(&mut self) {
        self.metadata_bytes = self.rows.values().map(entry_charge).sum();
    }
    pub fn known(&self, doc: &Id) -> Option<(&Relative, &DocEntry)> {
        self.rows.iter().find(|(_, e)| &e.id == doc)
    }
    pub fn resolve(&self, reference: &DocRef) -> Result<(&Relative, &Source), DocFailureCode> {
        if !self.policy.enabled {
            return Err(DocFailureCode::Disabled);
        }
        let (path, entry) = self
            .known(&reference.doc_id)
            .ok_or(DocFailureCode::ForbiddenPath)?;
        let source = self
            .sources
            .get(&reference.source_id)
            .filter(|s| s.record.generation == reference.source_generation)
            .ok_or(DocFailureCode::SourceGone)?;
        if !self.policy.selects(path)
            || !entry
                .variants
                .iter()
                .any(|v| v.source_id == reference.source_id)
        {
            return Err(DocFailureCode::ForbiddenPath);
        }
        source.root.check_identity()?;
        Ok((path, source))
    }
}

fn variant(source: &Source, path: &Relative, policy: &Policy) -> (DocVariant, Option<Stamp>) {
    let mut variant = DocVariant {
        source_id: source.record.id.clone(),
        source_generation: source.record.generation,
        markdown_hash: None,
        bytes: None,
        observed_at_ms: Some(now()),
        title: None,
        headings: List::default(),
        headings_limited: false,
        state: DocState::Unavailable,
        reason: None,
        comparison: DocComparison::Unknown,
    };
    let read = match source.root.read(path, policy, MAX_MARKDOWN_BYTES) {
        Ok(read) => read,
        Err(code) => {
            variant.reason = Some(code);
            return (variant, None);
        }
    };
    variant.bytes = Some(read.bytes.len() as u32);
    let parsed = std::str::from_utf8(&read.bytes)
        .map_err(|_| DocFailureCode::InvalidUtf8)
        .and_then(super::markdown::outline);
    match parsed {
        Ok((headings, limited)) => {
            variant.title = headings.first().map(|h| h.title.clone());
            variant.headings = List::new(headings).expect("outline bound");
            variant.headings_limited = limited;
            variant.markdown_hash = Some(hash(&read.bytes));
            variant.state = DocState::Present;
        }
        Err(code) => variant.reason = Some(code),
    }
    (variant, Some(read.stamp))
}

fn entry_charge(entry: &DocEntry) -> usize {
    // Include the per-source stamp/seen path copies, not only the display DTO.
    super::bounded_size(entry, INDEX_BYTES)
        .unwrap_or(INDEX_BYTES)
        .saturating_mul(3)
        .saturating_add(
            entry
                .variants
                .len()
                .saturating_mul(entry.relative_path.len() * 3 + 512),
        )
        .saturating_add(512)
}
