import CrewBindings
import CrewUI
import SwiftUI

/// Ephemeral, fully scoped presentation targets. The live scope is never
/// substituted while a sheet is open; the core rejects retired intents.
enum CrewActionSheet: Identifiable {
    case compose(EntityKey)
    case prompt(EntityKey)
    case question(EntityKey, Message, QuestionInfo)
    case terminal(EntityKey)

    var id: EntityKey {
        switch self {
        case .compose(let key), .prompt(let key), .question(let key, _, _), .terminal(let key): return key
        }
    }
}

@MainActor
struct CrewActionSheetView: View {
    let sheet: CrewActionSheet
    @Bindable var model: CrewSessionModel
    @Environment(\.dismiss) private var dismiss
    @State private var messageId = UUID().uuidString

    var body: some View {
        NavigationStack {
            Group {
                switch sheet {
                case .compose(let key):
                    let recipient: String = { if case .agent(let id) = key.entity { return id }; return "all" }()
                    CrewDraftEditor(title: "Message", text: model.draft(CrewDraftKey(scope: key.scope, purpose: "message:\(recipient)")), submitLabel: "Send", targetDescription: "\(recipient) · \(key.scope.workspaceId)", submitAvailable: model.scope == key.scope && model.isLive, pending: model.pending, unavailableReason: model.scope == key.scope && model.isLive ? nil : "Reconnect and reopen this target before sending") {
                        let text = model.draft(CrewDraftKey(scope: key.scope, purpose: "message:\(recipient)")).wrappedValue
                        model.perform(.sendMessage(scope: key.scope, to: recipient, body: text, messageId: messageId))
                    }
                case .prompt(let key):
                    if case .agent(let id) = key.entity {
                        CrewDraftEditor(title: "Terminal prompt", text: model.draft(CrewDraftKey(scope: key.scope, purpose: "prompt:\(id)")), submitLabel: "Submit prompt", targetDescription: "Terminal input to \(id) · \(key.scope.workspaceId)", submitAvailable: model.scope == key.scope && model.isLive, pending: model.pending, unavailableReason: model.scope == key.scope && model.isLive ? nil : "Reconnect and reopen this terminal target before submitting") {
                            let text = model.draft(CrewDraftKey(scope: key.scope, purpose: "prompt:\(id)")).wrappedValue
                            model.perform(.agentInput(scope: key.scope, id: id, text: text, submit: true))
                        }
                    }
                case .question(let key, let message, let info):
                    CrewQuestionView(question: message, info: info, draft: model.draft(CrewDraftKey(scope: key.scope, purpose: "answer:\(message.id)")), answerAvailable: model.scope == key.scope && model.isLive, pending: model.pending, reason: model.scope == key.scope && model.isLive ? nil : "This question's scope is no longer live", answer: { model.perform(.answer(scope: key.scope, questionId: message.id, body: $0)) }, later: { dismiss() })
                case .terminal:
                    VStack(alignment: .leading, spacing: 16) {
                        Text(verbatim: model.terminalStatus ?? "Opening terminal").font(.callout)
                        if let renderer = model.terminal, renderer.identity.scope == sheet.id.scope {
                            CrewTerminalSurface(renderer: renderer)
                                .frame(minHeight: 240)
                            HStack {
                                Button("Focus hardware input") { renderer.focusInput() }
                                Button("Read only") { renderer.releaseInputFocus() }
                                Spacer()
                                Button("Close terminal") { model.closeTerminal(); dismiss() }
                            }
                        } else {
                            ProgressView("Waiting for the attachment outcome")
                        }
                        Text("Software keyboard Return inserts a newline in the separate prompt draft. Raw keys go only to a focused, live terminal and are never replayed after reconnect.")
                            .font(.callout).foregroundStyle(.secondary)
                    }
                    .padding(16)
                }
            }
            .safeAreaInset(edge: .bottom) {
                if let feedback = model.feedback {
                    Text(verbatim: feedback).font(.callout).padding(16).frame(maxWidth: .infinity, alignment: .leading).background(CrewPalette.surface)
                }
            }
            .navigationTitle(title)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Done") { dismiss() }.keyboardShortcut(.cancelAction)
                }
            }
        }
        #if os(macOS)
        .frame(minWidth: 420, idealWidth: 700, minHeight: 420, idealHeight: 640)
        #endif
        .onDisappear { if case .terminal = sheet { model.closeTerminal() } }
    }

    private var title: String {
        switch sheet {
        case .compose: return "Compose"
        case .prompt: return "Terminal prompt"
        case .question: return "Answer question"
        case .terminal: return "Terminal"
        }
    }
}

extension CrewConfirmation {
    var title: String {
        switch kind {
        case .killAgent: return "Kill agent?"
        case .restartFresh: return "Restart fresh?"
        case .restartLimited: return "Restart agents paused at limit?"
        case .stopTeam: return "Stop team?"
        }
    }
    var consequence: String {
        let names = targets.joined(separator: ", ")
        switch kind {
        case .killAgent: return "Kill \(names). Its task stays open."
        case .restartFresh: return "Restart \(names) without resuming the old conversation."
        case .restartLimited: return "Restart \(targets.count) paused agents using the current login: \(names)."
        case .stopTeam: return "Stop every agent and the daemon. Committed work remains. Targets: \(names)."
        }
    }
}
