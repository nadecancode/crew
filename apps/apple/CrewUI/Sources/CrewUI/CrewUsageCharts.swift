import Charts
import CrewBindings
import SwiftUI

/// Charts render only the supplied counters and bounded shared samples. All
/// labels remain available as native text; no unknown value becomes a zero,
/// no runtime polling or locally reconstructed usage history is added.
public struct CrewTokenChart: View {
    private let usage: Usage
    public init(usage: Usage) { self.usage = usage }

    public var body: some View {
        Chart {
            BarMark(x: .value("Tokens", Double(usage.inputTokens)), y: .value("Series", "Input"))
                .foregroundStyle(CrewPalette.info)
                .accessibilityLabel("Input tokens")
                .accessibilityValue(usage.inputTokens.formatted())
            BarMark(x: .value("Tokens", Double(usage.outputTokens)), y: .value("Series", "Output"))
                .foregroundStyle(Color("CrewTokenOutput", bundle: .module))
                .accessibilityLabel("Output tokens")
                .accessibilityValue(usage.outputTokens.formatted())
            BarMark(x: .value("Tokens", Double(usage.cacheReadTokens)), y: .value("Series", "Cache read"))
                .foregroundStyle(Color("CrewTokenCacheRead", bundle: .module))
                .accessibilityLabel("Cache read tokens")
                .accessibilityValue(usage.cacheReadTokens.formatted())
            BarMark(x: .value("Tokens", Double(usage.cacheCreationTokens)), y: .value("Series", "Cache written"))
                .foregroundStyle(CrewPalette.warning)
                .accessibilityLabel("Cache written tokens")
                .accessibilityValue(usage.cacheCreationTokens.formatted())
        }
        .chartXAxisLabel("Reported tokens")
        .frame(height: 160)
    }
}

public struct CrewContextHistory: View {
    private let agent: AgentUsage
    public init(agent: AgentUsage) { self.agent = agent }

    public var body: some View {
        if agent.samples.isEmpty {
            Text("No context samples collected during this client session")
        } else {
            Chart(Array(agent.samples.enumerated()), id: \.offset) { _, sample in
                LineMark(
                    x: .value("Time", Date(timeIntervalSince1970: Double(sample.atMs) / 1_000)),
                    y: .value("Context percent", sample.percent)
                )
                .foregroundStyle(CrewPalette.info)
                .symbol(.circle)
                .accessibilityLabel(CrewLabels.timestamp(sample.atMs))
                .accessibilityValue("\(sample.percent) percent context")
            }
            .chartYAxisLabel("Context percent")
            .frame(height: 160)
            DisclosureGroup("Context sample values") {
                ForEach(Array(agent.samples.enumerated()), id: \.offset) { _, sample in
                    CrewLabeledValue(CrewLabels.timestamp(sample.atMs), value: "\(sample.percent)%")
                }
            }
        }
    }
}
