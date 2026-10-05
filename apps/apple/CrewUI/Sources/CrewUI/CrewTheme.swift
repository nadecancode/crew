import SwiftUI

public enum CrewAppearance: String, CaseIterable, Hashable, Identifiable {
    case system, light, dark
    public var id: String { rawValue }

    public var title: String {
        switch self {
        case .system: return "System"
        case .light: return "Light"
        case .dark: return "Dark"
        }
    }

    public var colorScheme: ColorScheme? {
        switch self {
        case .system: return nil
        case .light: return .light
        case .dark: return .dark
        }
    }
}

/// Semantic surfaces from the approved neutral palette. Text, selection and
/// focus use system controls; asset variants also respect increased contrast.
public enum CrewPalette {
    public static let canvas = Color("CrewCanvas", bundle: .module)
    public static let surface = Color("CrewSurface", bundle: .module)
    public static let raised = Color("CrewRaised", bundle: .module)
    public static let accent = Color("CrewAccent", bundle: .module)
}

public struct CrewAppearancePicker: View {
    @Binding private var appearance: CrewAppearance

    public init(appearance: Binding<CrewAppearance>) {
        self._appearance = appearance
    }

    public var body: some View {
        Picker("Appearance", selection: $appearance) {
            ForEach(CrewAppearance.allCases) { value in
                Text(LocalizedStringKey(value.title)).tag(value)
            }
        }
        .accessibilityIdentifier("crew.appearance")
    }
}
