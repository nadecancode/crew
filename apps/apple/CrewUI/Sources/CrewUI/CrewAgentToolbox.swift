import CrewBindings
import SwiftUI

/// A member's effective toolbox (notes/toolboxes.md §Display): the summary, a
/// pending change with the caller's Restart, the items by kind, then the
/// runtime's sources. The daemon supplies every status text; this view only
/// arranges it, in the same words as the Rust `AgentToolbox` helpers.
struct CrewAgentToolbox<Restart: View>: View {
    let toolbox: AgentToolbox
    @ViewBuilder let restart: () -> Restart

    var body: some View {
        Section {
            // The header says Toolbox; VoiceOver hears what the fingerprint is.
            (Text(verbatim: CrewToolboxWords.shortFingerprint(toolbox)).font(.body.monospaced())
                + Text(verbatim: CrewToolboxWords.counts(toolbox)))
                .textSelection(.enabled)
                .accessibilityLabel(
                    crewSpelledLabel("Toolbox fingerprint", CrewToolboxWords.shortFingerprint(toolbox))
                        + Text(verbatim: CrewToolboxWords.counts(toolbox))
                )
            if toolbox.pendingFingerprint != nil {
                Label(CrewToolboxWords.pending, systemImage: "exclamationmark.triangle")
                restart()
            }
            if CrewToolboxWords.unpinned(toolbox) {
                Text(verbatim: CrewToolboxWords.unpinnedNotice).foregroundStyle(.secondary)
            }
            DisclosureGroup("Definitions") {
                CrewLabeledValue("Fingerprint", value: toolbox.fingerprint, monospaced: true, spellsOut: true)
                if let next = toolbox.pendingFingerprint {
                    CrewLabeledValue("After restart", value: next, monospaced: true, spellsOut: true)
                }
                CrewLabeledValue("Definition digest", value: toolbox.definitionDigest ?? "unavailable", monospaced: toolbox.definitionDigest != nil)
                CrewLabeledValue("Tools", value: toolbox.toolCount.map { $0.formatted() } ?? "unknown")
                CrewLabeledValue("Definition bytes", value: toolbox.definitionBytes.map { "\($0.formatted()) bytes" } ?? "unknown")
            }
        } header: {
            Text("Toolbox").accessibilityAddTraits(.isHeader)
        }
        // Groups appear only when they have entries.
        ForEach(CrewToolboxWords.groups(toolbox)) { group in
            Section {
                ForEach(Array(group.items.enumerated()), id: \.offset) { _, item in
                    itemRow(item)
                }
            } header: {
                Text(verbatim: group.heading).accessibilityAddTraits(.isHeader)
            }
        }
        if !toolbox.sources.isEmpty {
            Section {
                ForEach(Array(toolbox.sources.enumerated()), id: \.offset) { _, source in
                    sourceRow(source)
                }
            } header: {
                Text("Sources").accessibilityAddTraits(.isHeader)
            }
        }
    }

    /// The name, then origin and status in words (a warning when the item is
    /// not usable), then its command, URL and references, wrapped.
    private func itemRow(_ item: ToolboxItem) -> some View {
        let facts = "\(CrewToolboxWords.originLabel(item.origin)) · \(item.status.text)"
            + (item.required ? " · required" : "")
        return VStack(alignment: .leading, spacing: 4) {
            Text(verbatim: item.name)
            if CrewToolboxWords.usable(item.status.code) {
                Text(verbatim: facts).font(.callout).foregroundStyle(.secondary)
            } else {
                Label(facts, systemImage: "exclamationmark.triangle").font(.callout)
            }
            if let detail = item.detail {
                Text(verbatim: detail)
                    .font(.callout.monospaced())
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
                    .textSelection(.enabled)
            }
        }
    }

    /// `Your Codex MCP servers: Disabled (ambient, docs-local)`, its reason below.
    private func sourceRow(_ source: ToolboxSource) -> some View {
        let names = source.names.isEmpty ? "" : " (\(source.names.joined(separator: ", ")))"
        let line = "\(source.label): \(source.status.text)\(names)"
        return VStack(alignment: .leading, spacing: 4) {
            if source.status.code == .cannotBeExcluded {
                Label(line, systemImage: "exclamationmark.triangle")
            } else {
                Text(verbatim: line)
            }
            if let detail = source.detail {
                Text(verbatim: detail).font(.callout).foregroundStyle(.secondary)
            }
        }
    }
}

struct CrewToolboxGroup: Identifiable {
    let heading: String
    let items: [ToolboxItem]
    var id: String { heading }
}

/// The toolbox in words, matching `AgentToolbox::summary`, `attention` and
/// `toolbox_origin_label` in crew-protocol.
enum CrewToolboxWords {
    static let pending = "Toolbox changed: restart to apply"
    static let unpinnedNotice = "MCP definitions not pinned (proxy pending)"
    /// The shared warning code the Toolbox section replaces in the inspector.
    static let warningCode = "toolbox"

    static func shortFingerprint(_ toolbox: AgentToolbox) -> String {
        String(toolbox.fingerprint.prefix(12))
    }

    /// ` · 4 items · 1 not available`: the summary after its fingerprint.
    static func counts(_ toolbox: AgentToolbox) -> String {
        let count = toolbox.items.count
        let inactive = toolbox.items.filter { !usable($0.status.code) }.count
        return " · \(count) item\(count == 1 ? "" : "s")" + (inactive > 0 ? " · \(inactive) not available" : "")
    }

    /// Items that are not applicable count as items but not as a problem.
    static func usable(_ code: ToolboxItemCode) -> Bool {
        code == .active || code == .planned || code == .notApplicable
    }

    static func unpinned(_ toolbox: AgentToolbox) -> Bool {
        toolbox.definitionDigest == nil && toolbox.items.contains { $0.kind == .mcp }
    }

    static func originLabel(_ origin: String) -> String {
        switch origin {
        case "default": return "Team default"
        case "member": return "Member"
        default:
            return origin.hasPrefix("bundle:") ? "Bundle \(origin.dropFirst("bundle:".count))" : origin
        }
    }

    static func groups(_ toolbox: AgentToolbox) -> [CrewToolboxGroup] {
        let kinds: [(ToolboxItemKind, String)] = [
            (.mcp, "MCP servers"), (.skills, "Skills"), (.plugins, "Plugins"),
            (.subagents, "Subagents"), (.hooks, "Hooks"), (.unknown, "Other"),
        ]
        return kinds.compactMap { kind -> CrewToolboxGroup? in
            let items = toolbox.items.filter { $0.kind == kind.0 }
            return items.isEmpty ? nil : CrewToolboxGroup(heading: kind.1, items: items)
        }
    }
}
