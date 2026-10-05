import CrewUI
import SwiftUI

/// The foundation persists only an appearance preference. Navigation,
/// workspace evidence and connection authority do not enter UserDefaults.
enum CrewPreferences {
    static let appearanceKey = "crew.appearance"

    static func appearance(_ raw: Binding<String>) -> Binding<CrewAppearance> {
        Binding(
            get: { CrewAppearance(rawValue: raw.wrappedValue) ?? .system },
            set: { raw.wrappedValue = $0.rawValue }
        )
    }
}

struct CrewAppearanceSettings: View {
    @AppStorage(CrewPreferences.appearanceKey) private var appearanceRaw = CrewAppearance.system.rawValue

    var body: some View {
        Form {
            CrewAppearancePicker(appearance: CrewPreferences.appearance($appearanceRaw))
            Text("System follows this device's appearance. Native controls also follow accessibility and contrast settings.")
                .font(.callout)
                .foregroundStyle(.secondary)
        }
        .formStyle(.grouped)
        .navigationTitle("Appearance")
        .preferredColorScheme(CrewAppearance(rawValue: appearanceRaw)?.colorScheme)
        .tint(CrewPalette.accent)
    }
}
