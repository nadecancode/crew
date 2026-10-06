import Accessibility
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

    var body: some View {
        NavigationStack {
            Group {
                switch sheet {
                case .compose(let key):
                    let recipient: String = { if case .agent(let id) = key.entity { return id }; return "all" }()
                    let draftKey = CrewDraftKey(scope: key.scope, purpose: "message:\(recipient)")
                    let control = "send:\(draftKey.purpose)"
                    let intent = model.sendIntent(draftKey, to: recipient)
                    let availability = model.availability(intent, control: control)
                    // Scrolls like the question sheet, so Send stays reachable
                    // with the keyboard up and at accessibility text sizes.
                    ScrollView {
                        VStack(alignment: .leading) {
                            // The Core's held message is shown as its immutable
                            // receipt; the editor below may already hold new text.
                            if let receipt = model.sendReceipt(draftKey) {
                                CrewSubmittedMessageView(recipient: receipt.recipient, body: receipt.body, status: CrewFailure.receipt(receipt), pending: receipt.isPending)
                            }
                            CrewDraftEditor(title: "Message", text: model.sendDraft(draftKey), submitLabel: "Send", targetDescription: "\(recipient) · \(key.scope.workspaceId)", submitAvailable: availability.available, pending: model.sendPending(draftKey), unavailableReason: model.sendPending(draftKey) ? "Send after the submitted message above resolves." : availability.reason) {
                                model.send(draftKey, to: recipient)
                            }
                            if let explanation = model.sendExplanation(draftKey) { Text(verbatim: explanation).font(.callout).padding(16) }
                        }
                    }
                    .onAppear { model.prepareSendDraft(draftKey) }
                    .onChange(of: model.sendExplanation(draftKey)) { _, explanation in
                        if let explanation { AccessibilityNotification.Announcement(explanation).post() }
                    }
                    .crewAvailability(model, intent.map { [CrewWatchedIntent(control: control, intent: $0)] } ?? [])
                case .prompt(let key):
                    if case .agent(let id) = key.entity {
                        let draft = model.draft(CrewDraftKey(scope: key.scope, purpose: "prompt:\(id)"))
                        let control = "prompt:\(id)"
                        let intent = ActionIntent.agentInput(scope: key.scope, id: id, text: draft.wrappedValue, submit: true)
                        let availability = model.availability(intent, control: control)
                        ScrollView {
                            CrewDraftEditor(title: "Terminal prompt", text: draft, submitLabel: "Submit prompt", targetDescription: "Terminal input to \(id) · \(key.scope.workspaceId)", submitAvailable: availability.available, pending: model.pending, unavailableReason: availability.reason) {
                                model.perform(intent)
                            }
                        }
                        .crewAvailability(model, [CrewWatchedIntent(control: control, intent: intent)])
                    }
                case .question(let key, let message, let info):
                    let draft = model.draft(CrewDraftKey(scope: key.scope, purpose: "answer:\(message.id)"))
                    let answer = { (body: String) in ActionIntent.answer(scope: key.scope, questionId: message.id, body: body) }
                    let draftControl = "answer:\(message.id)"
                    let optionControl = { (option: String) in "answer:\(message.id):option:\(option)" }
                    CrewQuestionView(question: message, info: info, draft: draft, answerAvailability: model.availability(answer(draft.wrappedValue), control: draftControl), optionAvailability: { model.availability(answer($0), control: optionControl($0)) }, pending: model.pending, answer: { model.perform(answer($0)) }, later: { dismiss() })
                        .crewAvailability(model, [CrewWatchedIntent(control: draftControl, intent: answer(draft.wrappedValue))] + info.options.map { CrewWatchedIntent(control: optionControl($0), intent: answer($0)) })
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
                        } else if model.terminalPending {
                            ProgressView("Waiting for the attachment outcome")
                        } else {
                            ContentUnavailableView {
                                Label("Terminal is not open", systemImage: "terminal")
                            } description: {
                                Text(verbatim: model.terminalStatus ?? "The terminal attempt did not complete.")
                            }
                            Button("Close") { dismiss() }
                            if model.scope == sheet.id.scope {
                                Button("Try opening a new terminal") { model.attach(sheet.id) }
                            }
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
        .sheet(item: $model.externalLink) { preview in CrewLinkPreviewView(preview: preview) }
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
