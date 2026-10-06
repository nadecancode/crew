import CrewBindings
import SwiftUI

public struct CrewOverviewSummary: View {
    private let data: OverviewData
    private let copy: (String) -> Void
    public init(data: OverviewData, copy: @escaping (String) -> Void) { self.data = data; self.copy = copy }

    public var body: some View {
        DisclosureGroup("Session and warnings") {
            VStack(alignment: .leading, spacing: 12) {
                Text("\(data.agentCount) agents · \(data.workingCount) working · \(data.doneCount) of \(data.taskCount) tasks done · \(data.questionCount) questions")
                if let session = data.session {
                    Text(verbatim: session.objective).textSelection(.enabled)
                    if let project = session.project { CrewLabeledValue("Project", value: project) }
                    CrewLabeledValue("Workspace", value: session.workspace, monospaced: true)
                    Button("Copy workspace") { copy(session.workspace) }
                    CrewLabeledValue("Backend", value: session.backend)
                    CrewLabeledValue("Isolation", value: session.isolation)
                    CrewLabeledValue("Sandbox", value: session.sandbox)
                    CrewLabeledValue("VCS", value: session.vcs)
                    if !session.vcsNote.isEmpty { Text(verbatim: session.vcsNote).textSelection(.enabled) }
                    if let style = session.style { CrewLabeledValue("Work style", value: style) }
                    if let at = session.teamStartedAt { CrewLabeledValue("Team started", value: CrewLabels.timestamp(at)) }
                    if let at = session.completedAt { CrewLabeledValue("Completed", value: CrewLabels.timestamp(at)) }
                    if let summary = session.summary { Text(verbatim: summary).textSelection(.enabled) }
                }
                ForEach(Array(data.warnings.enumerated()), id: \.offset) { _, warning in
                    Label(warning.text, systemImage: "exclamationmark.triangle")
                        .fixedSize(horizontal: false, vertical: true)
                }
            }
            .padding(.vertical, 8)
            .frame(maxWidth: 720, alignment: .leading)
        }
        .padding(12)
    }
}
