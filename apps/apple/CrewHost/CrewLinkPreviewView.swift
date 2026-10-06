import CrewUI
import SwiftUI

struct CrewLinkPreviewView: View {
    let preview: CrewLinkPreview
    @Environment(\.openURL) private var openURL
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        NavigationStack {
            ScrollView {
                VStack(alignment: .leading, spacing: 24) {
                    Text("Open outside Crew").font(.title2.bold())
                    Text(verbatim: preview.url.absoluteString)
                        .font(.body.monospaced()).textSelection(.enabled)
                        .fixedSize(horizontal: false, vertical: true)
                    Text("Review the complete address. Opening uses this device's browser and its existing website sessions.")
                        .font(.callout)
                    Button("Open this URL") {
                        CrewExternalLinks.open(preview.url, explicitGesture: true) { url in openURL(url) }
                    }
                    .buttonStyle(.borderedProminent)
                    Button("Copy URL") { CrewClipboard.copy(preview.url.absoluteString) }
                }
                .padding(24).frame(maxWidth: 640, alignment: .leading)
            }
            .navigationTitle("Review link")
            .toolbar {
                ToolbarItem(placement: .cancellationAction) { Button("Done") { dismiss() }.keyboardShortcut(.cancelAction) }
            }
        }
        #if os(macOS)
        .frame(minWidth: 360, idealWidth: 560, minHeight: 320)
        #endif
    }
}
