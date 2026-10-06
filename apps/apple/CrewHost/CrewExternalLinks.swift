import Foundation

/// Platform equivalent of web/src/text.tsx safeHref/linkPieces, with the
/// reviewed native absolute-host/userinfo restriction. This is OS presentation;
/// it does not establish gateway trust or perform network requests.
enum CrewExternalLinks {
    static func preview(_ candidate: String, detectedText: Bool = false) -> URL? {
        var exact = candidate
        if detectedText {
            let punctuation = Set(".,;:!?)\u{005D}\u{007D}")
            while let last = exact.last, punctuation.contains(last) { exact.removeLast() }
        }
        guard !exact.isEmpty,
              !exact.unicodeScalars.contains(where: { CharacterSet.whitespacesAndNewlines.contains($0) || CharacterSet.controlCharacters.contains($0) }),
              let url = URL(string: exact), let scheme = url.scheme?.lowercased(),
              scheme == "http" || scheme == "https",
              let host = url.host, !host.isEmpty,
              url.user == nil, url.password == nil else { return nil }
        return url
    }

    @discardableResult
    static func open(_ url: URL, explicitGesture: Bool, opener: (URL) -> Void) -> Bool {
        guard explicitGesture, let safe = preview(url.absoluteString) else { return false }
        opener(safe)
        return true
    }
}

struct CrewLinkPreview: Identifiable {
    let id = UUID()
    let url: URL
}
