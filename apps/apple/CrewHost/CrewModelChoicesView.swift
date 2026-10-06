import CrewBindings
import CrewUI
import SwiftUI

@MainActor
struct CrewModelChoicesView: View {
    let key: EntityKey
    let models: ModelsResult
    @Bindable var model: CrewSessionModel
    @State private var note = ""

    private var agent: AgentPresentation? {
        guard case .agentDetail(let data) = model.detail?.payload else { return nil }
        return data.agent
    }

    var body: some View {
        List {
            Section("Current choice") {
                if let agent {
                    CrewLabeledValue("CLI", value: String(describing: agent.agent.runtime))
                    CrewLabeledValue("Model", value: agent.agent.model ?? "CLI default")
                }
                TextField("Optional note for this change", text: $note, axis: .vertical)
                Text("A model change using the current CLI resumes the session. Changing the CLI starts a fresh session.")
            }
            Section("Models and CLIs") {
                ForEach(Array(models.models.enumerated()), id: \.offset) { _, choice in
                    let availability = model.availability(intent(choice))
                    VStack(alignment: .leading, spacing: 8) {
                        Text(verbatim: choice.name).font(.headline)
                        CrewLabeledValue("CLI", value: String(describing: choice.runtime))
                        Text(verbatim: choice.model.isEmpty ? "CLI default" : choice.model).font(.body.monospaced())
                        Text(verbatim: choice.summary)
                        if !choice.tier.isEmpty { CrewLabeledValue("Tier", value: choice.tier) }
                        if !choice.price.isEmpty { CrewLabeledValue("Price", value: choice.price) }
                        if let unavailable = choice.unavailable { Text(verbatim: unavailable).font(.callout).foregroundStyle(.secondary) }
                        else if let reason = availability.reason { Text(verbatim: reason).font(.callout).foregroundStyle(.secondary) }
                        Button("Use \(choice.name)") {
                            if let intent = intent(choice) { model.perform(intent) }
                        }
                        .disabled(choice.unavailable != nil || model.pending || !availability.available)
                    }
                    .padding(.vertical, 8)
                }
            }
            if let feedback = model.feedback { Section("Result") { Text(verbatim: feedback) } }
        }
        .crewAvailability(model, models.models.compactMap(intent))
    }

    /// The exact setModel intent this choice and note would submit.
    private func intent(_ choice: ModelChoice) -> ActionIntent? {
        guard case .agent(let id) = key.entity else { return nil }
        return .setModel(scope: key.scope, id: id, runtime: choice.runtime, model: choice.model, note: note.isEmpty ? nil : note)
    }
}
