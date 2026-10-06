import CrewBindings
import Observation
import SwiftUI

/// A single per-window focus request, issued only by an explicit close/back or
/// a retired selection. Requests carry complete entity identity and expire on
/// scope changes. Background content updates never request reading focus.
@MainActor
@Observable
public final class CrewFocus {
    public private(set) var request: CrewFocusRequest?
    public init() {}

    public func restore(opener: EntityKey?, closed: EntityKey?, rendered: Set<EntityKey>, destination: CrewDestination) {
        let target = opener.flatMap { rendered.contains($0) ? $0 : nil }
            ?? closed.flatMap { rendered.contains($0) ? $0 : nil }
        request = CrewFocusRequest(id: UUID(), destination: destination, target: target)
    }
    public func cancel() { request = nil }
    public func consume(_ id: UUID?) { if request?.id == id { request = nil } }
}

public struct CrewFocusRequest: Equatable {
    public let id: UUID
    public let destination: CrewDestination
    public let target: EntityKey?
}

/// Native keyboard focus and VoiceOver reading focus remain separate from
/// selection. The caller supplies whether this actual surface is visible.
@MainActor
public struct CrewEntityFocus: ViewModifier {
    private let key: EntityKey
    private let selected: EntityKey?
    private let coordinator: CrewFocus?
    private let active: Bool
    @FocusState private var keyboardFocus: Bool
    @AccessibilityFocusState private var readingFocus: Bool

    public init(key: EntityKey, selected: EntityKey?, coordinator: CrewFocus?, active: Bool) {
        self.key = key; self.selected = selected; self.coordinator = coordinator; self.active = active
    }

    public func body(content: Content) -> some View {
        content
            .id(key)
            .focused($keyboardFocus)
            .accessibilityFocused($readingFocus)
            .accessibilityAddTraits(selected == key ? .isSelected : [])
            .overlay(alignment: .topTrailing) {
                if selected == key { Image(systemName: "checkmark.circle.fill").foregroundStyle(CrewPalette.accent).accessibilityHidden(true) }
            }
            .onChange(of: coordinator?.request?.id) { _, _ in restore() }
            .onAppear { restore() }
    }

    private func restore() {
        guard active, coordinator?.request?.target == key else { return }
        let request = coordinator?.request?.id
        // One matched row only. Allow the native container to finish mounting;
        // a later scope/request cancels this callback before it moves focus.
        DispatchQueue.main.async {
            guard active, coordinator?.request?.id == request, coordinator?.request?.target == key else { return }
            keyboardFocus = true
            readingFocus = true
            coordinator?.consume(request)
        }
    }
}
