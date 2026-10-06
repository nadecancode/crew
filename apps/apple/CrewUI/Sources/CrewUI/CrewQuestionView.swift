import CrewBindings
import SwiftUI

/// QuestionInfo comes from messageQuestionInfo in the shared Rust bridge.
/// Permission and outcomes come from the core; this view owns only a draft.
public struct CrewQuestionView: View {
    private let question: Message
    private let info: QuestionInfo
    private let answerAvailable: Bool
    private let pending: Bool
    private let reason: String?
    private let answer: (String) -> Void
    private let later: () -> Void
    @Binding private var draft: String

    public init(
        question: Message,
        info: QuestionInfo,
        draft: Binding<String>,
        answerAvailable: Bool,
        pending: Bool,
        reason: String?,
        answer: @escaping (String) -> Void,
        later: @escaping () -> Void
    ) {
        self.question = question
        self.info = info
        self._draft = draft
        self.answerAvailable = answerAvailable
        self.pending = pending
        self.reason = reason
        self.answer = answer
        self.later = later
    }

    public var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 24) {
                Text("Question from \(info.asker)").font(.title2.bold()).accessibilityAddTraits(.isHeader)
                Text(verbatim: question.body).textSelection(.enabled)
                if !info.options.isEmpty {
                    VStack(alignment: .leading, spacing: 8) {
                        Text("Suggested answers").font(.headline)
                        ForEach(Array(info.options.enumerated()), id: \.offset) { _, option in
                            Button {
                                draft = option
                                answer(option)
                            } label: {
                                Text(verbatim: option)
                                    .frame(maxWidth: .infinity, minHeight: 44, alignment: .leading)
                            }
                            .buttonStyle(.bordered)
                            .disabled(!answerAvailable || pending)
                        }
                    }
                }
                CrewDraftEditor(
                    title: "Your answer",
                    text: $draft,
                    submitLabel: "Answer",
                    targetDescription: info.asker,
                    submitAvailable: answerAvailable,
                    pending: pending,
                    unavailableReason: reason
                ) { answer(draft) }
                Button(action: later) {
                    Text("Later").frame(minWidth: 44, minHeight: 44)
                }
            }
            .frame(maxWidth: 640, alignment: .leading)
            .padding(24)
            .frame(maxWidth: .infinity)
        }
        .background(CrewPalette.canvas)
    }
}
