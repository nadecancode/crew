import Accessibility
import SwiftUI

/// Presentation of already supplied values. Formatting and domain projection
/// belong to the caller; this view does not infer a missing value or a status.
public struct CrewLabeledValue: View {
    private let label: String
    private let value: String
    private let monospaced: Bool

    public init(_ label: String, value: String, monospaced: Bool = false) {
        self.label = label
        self.value = value
        self.monospaced = monospaced
    }

    public var body: some View {
        LabeledContent {
            Text(verbatim: value)
                .font(monospaced ? .body.monospaced() : .body)
                .fixedSize(horizontal: false, vertical: true)
                .textSelection(.enabled)
        } label: {
            Text(LocalizedStringKey(label))
        }
    }
}

/// Selectable evidence with explicit copy. Dynamic strings are displayed
/// verbatim, so evidence never becomes a Markdown link or a file fetch.
public struct CrewTextEvidence: View {
    private let text: String
    private let copy: () -> Void
    @State private var wrapsLines = true

    public init(text: String, copy: @escaping () -> Void) {
        self.text = text
        self.copy = copy
    }

    public var body: some View {
        VStack(spacing: 0) {
            HStack {
                Toggle("Wrap lines", isOn: $wrapsLines)
                    .toggleStyle(.switch)
                Spacer()
                Button(action: copy) {
                    Label("Copy", systemImage: "doc.on.doc")
                        #if os(iOS)
                        .frame(minWidth: 44, minHeight: 44)
                        #endif
                }
                .accessibilityLabel("Copy evidence text")
            }
            .padding(16)
            Divider()
            ScrollView(wrapsLines ? .vertical : [.vertical, .horizontal]) {
                Text(verbatim: text)
                    .font(.body.monospaced())
                    .textSelection(.enabled)
                    .frame(maxWidth: wrapsLines ? .infinity : nil, alignment: .leading)
                    .padding(16)
            }
        }
        .background(CrewPalette.canvas)
    }
}

/// Eligibility of one exact captured action, as answered by the shared core's
/// read-only availability query. The reason is the core's canonical text.
/// While the answer for the current input is outstanding, the control is
/// disabled without a reason, so typing does not flash explanatory text.
public struct CrewControlAvailability: Equatable {
    public let available: Bool
    public let reason: String?

    public init(available: Bool, reason: String?) {
        self.available = available
        self.reason = reason
    }

    public static let checking = CrewControlAvailability(available: false, reason: nil)
}

/// A message that has already been submitted, shown read-only and separate
/// from the editable draft. The caller supplies the receipt status; closing
/// the containing sheet does not cancel a pending send.
public struct CrewSubmittedMessageView: View {
    private let recipient: String
    private let messageBody: String
    private let status: String
    private let pending: Bool
    @ScaledMetric(relativeTo: .body) private var receiptHeight: CGFloat = 120

    public init(recipient: String, body: String, status: String, pending: Bool) {
        self.recipient = recipient
        self.messageBody = body
        self.status = status
        self.pending = pending
    }

    public var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack(spacing: 8) {
                if pending { ProgressView().accessibilityHidden(true) }
                Text(pending ? "Submitted message" : "Last submitted message")
                    .font(.headline)
                    .accessibilityAddTraits(.isHeader)
            }
            Text(verbatim: "To \(recipient)")
                .font(.callout)
                .foregroundStyle(.secondary)
            // Bounded so a long submitted message cannot push the editable
            // draft out of the sheet; the editor keeps its own scrolling.
            ViewThatFits(in: .vertical) {
                submittedText
                ScrollView { submittedText }
            }
            .frame(maxHeight: receiptHeight)
            .background(CrewPalette.canvas, in: RoundedRectangle(cornerRadius: 6))
            Text(verbatim: status)
                .font(.callout)
            if pending {
                Text("Closing this sheet does not cancel it. Later edits below are a separate draft.")
                    .font(.footnote)
                    .foregroundStyle(.secondary)
            }
        }
        .padding(16)
        .accessibilityElement(children: .contain)
        // Receipt changes arrive without user action; announce them.
        .onChange(of: status) { _, status in AccessibilityNotification.Announcement(status).post() }
    }

    private var submittedText: some View {
        Text(verbatim: messageBody)
            .font(.body)
            .fixedSize(horizontal: false, vertical: true)
            .textSelection(.enabled)
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding(8)
    }
}

/// A UI-owned draft. Return remains a newline in the native editor. Only the
/// explicit button calls the action; its caller supplies shared eligibility
/// and pending state. A failed or unknown outcome cannot clear the draft here.
public struct CrewDraftEditor: View {
    @Binding private var text: String
    private let title: String
    private let submitLabel: String
    private let targetDescription: String
    private let submitAvailable: Bool
    private let pending: Bool
    private let unavailableReason: String?
    private let submit: () -> Void
    @ScaledMetric(relativeTo: .body) private var editorHeight: CGFloat = 160

    public init(
        title: String,
        text: Binding<String>,
        submitLabel: String,
        targetDescription: String,
        submitAvailable: Bool,
        pending: Bool,
        unavailableReason: String?,
        submit: @escaping () -> Void
    ) {
        self.title = title
        self._text = text
        self.submitLabel = submitLabel
        self.targetDescription = targetDescription
        self.submitAvailable = submitAvailable
        self.pending = pending
        self.unavailableReason = unavailableReason
        self.submit = submit
    }

    public var body: some View {
        VStack(alignment: .leading, spacing: 16) {
            Text(LocalizedStringKey(title))
                .font(.headline)
                .accessibilityAddTraits(.isHeader)
            Text(verbatim: targetDescription)
                .font(.callout)
                .textSelection(.enabled)
            TextEditor(text: $text)
                .font(.body)
                .frame(minHeight: editorHeight)
                .accessibilityLabel(LocalizedStringKey(title))
                .accessibilityHint("Return inserts a new line. Use the explicit submit button to send.")
            if let unavailableReason {
                Text(verbatim: unavailableReason)
                    .font(.callout)
                    .foregroundStyle(.secondary)
            }
            HStack {
                if pending {
                    ProgressView().accessibilityLabel("Submission pending")
                }
                Spacer()
                Button(action: submit) {
                    Text(LocalizedStringKey(submitLabel))
                        #if os(iOS)
                        .frame(minWidth: 44, minHeight: 44)
                        #endif
                }
                .buttonStyle(.borderedProminent)
                .disabled(!submitAvailable || pending)
            }
        }
        .padding(16)
    }
}
