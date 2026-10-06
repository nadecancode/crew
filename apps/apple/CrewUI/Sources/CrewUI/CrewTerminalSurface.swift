import CrewBindings
import Foundation
import SwiftTerm
import SwiftUI
#if os(macOS)
import AppKit
#elseif os(iOS)
import UIKit
#endif

/// UI attachment identity using the complete generated ClientScope,
/// including all optional incarnation and attempt facts.
/// An attachment ID alone cannot address this renderer or its callbacks.
public struct CrewTerminalIdentity: Hashable {
    public let scope: ClientScope
    public let attachmentId: UInt64

    public init(scope: ClientScope, attachmentId: UInt64) {
        self.scope = scope
        self.attachmentId = attachmentId
    }
}

/// One native VT renderer per immutable scoped attachment. The view exists
/// before mounting, so early output does not need a separate byte replay queue.
/// Input availability comes from the real shared service; this is not a grant
/// or connection implementation. Closed instances cannot be re-enabled.
@MainActor
public final class CrewTerminalRenderer: NSObject, TerminalViewDelegate {
    public let identity: CrewTerminalIdentity
    public private(set) var endReason: TerminalEnd?
    public var isEnded: Bool { ended }
    let view: SwiftTerm.TerminalView
    private var inputAvailable = false
    private var ended = false
    private var input: ((CrewTerminalIdentity, Data) -> Void)?
    private var resize: ((CrewTerminalIdentity, Int, Int) -> Void)?
    private var openLink: ((String) -> Void)?

    public init(
        identity: CrewTerminalIdentity,
        input: @escaping (CrewTerminalIdentity, Data) -> Void,
        resize: @escaping (CrewTerminalIdentity, Int, Int) -> Void,
        openLink: @escaping (String) -> Void
    ) {
        self.identity = identity
        self.input = input
        self.resize = resize
        self.openLink = openLink
        let options = TerminalOptions(
            cursorStyle: .steadyBlock,
            scrollback: 2_000,
            enableSixelReported: false,
            kittyImageCacheLimitBytes: 4 * 1_024 * 1_024
        )
        #if os(macOS)
        view = SwiftTerm.TerminalView(
            frame: .zero,
            font: NSFont.monospacedSystemFont(ofSize: NSFont.systemFontSize, weight: .regular),
            options: options
        )
        #else
        view = SwiftTerm.TerminalView(
            frame: .zero,
            font: UIFont.monospacedSystemFont(ofSize: UIFont.preferredFont(forTextStyle: .body).pointSize, weight: .regular),
            options: options
        )
        #endif
        super.init()
        view.terminalDelegate = self
        #if os(macOS)
        view.nativeBackgroundColor = NSColor(srgbRed: 16.0 / 255, green: 16.0 / 255, blue: 16.0 / 255, alpha: 1)
        view.nativeForegroundColor = NSColor(srgbRed: 237.0 / 255, green: 237.0 / 255, blue: 237.0 / 255, alpha: 1)
        view.caretViewTracksFocus = true
        #else
        view.nativeBackgroundColor = UIColor(red: 16.0 / 255, green: 16.0 / 255, blue: 16.0 / 255, alpha: 1)
        view.nativeForegroundColor = UIColor(red: 237.0 / 255, green: 237.0 / 255, blue: 237.0 / 255, alpha: 1)
        // The mobile software keyboard belongs to the explicit prompt draft.
        // This input view supports focused hardware keys without a software
        // Return that writes directly to the PTY.
        view.inputView = UIView(frame: .zero)
        view.inputAccessoryView = nil
        #endif
    }

    public var hasInputFocus: Bool {
        #if os(macOS)
        return view.window?.firstResponder === view
        #else
        return view.isFirstResponder
        #endif
    }

    public func setInputAvailable(_ available: Bool) {
        let wasAvailable = inputAvailable
        inputAvailable = available && !ended
        if !inputAvailable { releaseInputFocus() }
        if inputAvailable && !wasAvailable && view.bounds.width > 0 && view.bounds.height > 0 {
            let terminal = view.getTerminal()
            resize?(identity, terminal.cols, terminal.rows)
        }
    }

    public func focusInput() {
        guard inputAvailable, !ended else { return }
        #if os(macOS)
        view.window?.makeFirstResponder(view)
        #else
        _ = view.becomeFirstResponder()
        #endif
    }

    public func releaseInputFocus() {
        #if os(macOS)
        if hasInputFocus { view.window?.makeFirstResponder(nil) }
        #else
        if hasInputFocus { _ = view.resignFirstResponder() }
        #endif
    }

    public func receive(_ chunk: TerminalChunk) {
        guard identity.scope == chunk.scope, identity.attachmentId == chunk.attachmentId, !ended else { return }
        // Disable input before feeding an ending chunk, including any emulator
        // response generated while its final output is interpreted.
        if let reason = chunk.ended { endReason = reason; end() }
        if !chunk.data.isEmpty { view.feed(byteArray: Array(chunk.data)[...]) }
    }

    public func end() {
        ended = true
        setInputAvailable(false)
        input = nil
        resize = nil
        openLink = nil
    }

    public func updateFont() {
        #if os(macOS)
        view.font = NSFont.monospacedSystemFont(ofSize: NSFont.systemFontSize, weight: .regular)
        #else
        view.font = UIFont.monospacedSystemFont(ofSize: UIFont.preferredFont(forTextStyle: .body).pointSize, weight: .regular)
        #endif
    }

    nonisolated public func sizeChanged(source: SwiftTerm.TerminalView, newCols: Int, newRows: Int) {
        MainActor.assumeIsolated {
            guard source === view, inputAvailable, !ended, newCols > 0, newRows > 0 else { return }
            // Shared driver validates/clamps the protocol dimensions. Preserve
            // measured values rather than truncating a smaller integer.
            resize?(identity, newCols, newRows)
        }
    }

    nonisolated public func send(source: SwiftTerm.TerminalView, data: ArraySlice<UInt8>) {
        // The pinned view requires input callbacks on the UI thread. Do not
        // schedule a Task that could retain and later replay terminal bytes.
        MainActor.assumeIsolated {
            guard source === view, inputAvailable, !ended, hasInputFocus else { return }
            input?(identity, Data(data))
        }
    }

    nonisolated public func requestOpenLink(source: SwiftTerm.TerminalView, link: String, params: [String: String]) {
        MainActor.assumeIsolated {
            guard source === view, !ended else { return }
            // Explicit user activation. The host applies the shared safe-link
            // policy before invoking the OS opener.
            openLink?(link)
        }
    }

    nonisolated public func setTerminalTitle(source: SwiftTerm.TerminalView, title: String) {}
    nonisolated public func hostCurrentDirectoryUpdate(source: SwiftTerm.TerminalView, directory: String?) {}
    nonisolated public func scrolled(source: SwiftTerm.TerminalView, position: Double) {}
    nonisolated public func bell(source: SwiftTerm.TerminalView) {}
    nonisolated public func clipboardCopy(source: SwiftTerm.TerminalView, content: Data) {}
    nonisolated public func clipboardRead(source: SwiftTerm.TerminalView) -> Data? { nil }
    nonisolated public func iTermContent(source: SwiftTerm.TerminalView, content: ArraySlice<UInt8>) {}
    nonisolated public func rangeChanged(source: SwiftTerm.TerminalView, startY: Int, endY: Int) {}
}

#if os(macOS)
public struct CrewTerminalSurface: NSViewRepresentable {
    private let renderer: CrewTerminalRenderer

    public init(renderer: CrewTerminalRenderer) { self.renderer = renderer }

    public func makeNSView(context: Context) -> SwiftTerm.TerminalView { renderer.view }
    public func updateNSView(_ view: SwiftTerm.TerminalView, context: Context) {}
}
#elseif os(iOS)
public struct CrewTerminalSurface: UIViewRepresentable {
    private let renderer: CrewTerminalRenderer
    @Environment(\.dynamicTypeSize) private var dynamicTypeSize

    public init(renderer: CrewTerminalRenderer) { self.renderer = renderer }

    public func makeUIView(context: Context) -> SwiftTerm.TerminalView { renderer.view }
    public func updateUIView(_ view: SwiftTerm.TerminalView, context: Context) {
        _ = dynamicTypeSize
        renderer.updateFont()
    }
}
#endif
