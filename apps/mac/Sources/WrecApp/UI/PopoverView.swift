// The whole recorder in one 320pt panel. Structured so observation stays
// narrow: transport, config, and status are separate child views, each
// reading only the model properties it renders.

import AppKit
import SwiftUI

struct PopoverView: View {
    @Bindable var model: RecorderModel

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            HeaderRow(model: model)

            if !model.screenPermission.isGranted {
                PermissionBanner(model: model)
            }

            ConfigSection(model: model)
            TransportSection(model: model)

            if let toast = model.toast {
                Divider()
                Text(toast)
                    .font(.pixel(10))
                    .foregroundStyle(.secondary)
                    .lineLimit(2)
                    .padding(.horizontal, 14)
                    .padding(.vertical, 8)
            }
        }
        .frame(width: 320)
        .task {
            await model.refreshMicPermission()
            // Screen Recording can be granted directly in System Settings;
            // without a re-check the "access needed" banner never clears.
            await model.refreshScreenPermission(requestIfNeeded: false)
            await model.refreshTargets()
        }
    }
}

// MARK: - Header

private struct HeaderRow: View {
    let model: RecorderModel

    var body: some View {
        HStack(spacing: 8) {
            Text("WREC")
                .font(.pixel(13))
                .foregroundStyle(model.phase.isActiveSession ? Theme.red : Color.primary)
            Spacer()
            SettingsLink {
                Image(systemName: "gearshape")
                    .imageScale(.medium)
            }
            .buttonStyle(.borderless)
            Button {
                model.quit()
            } label: {
                Image(systemName: "power")
                    .imageScale(.medium)
            }
            .buttonStyle(.borderless)
            .help("Quit wrec")
        }
        .padding(.horizontal, 14)
        .padding(.vertical, 10)
    }
}

// MARK: - Permission banner

private struct PermissionBanner: View {
    let model: RecorderModel

    var body: some View {
        HStack(spacing: 8) {
            Image(systemName: "exclamationmark.triangle.fill")
                .foregroundStyle(.orange)
            Text("Screen Recording access needed")
                .font(.system(size: 12))
            Spacer()
            Button("Grant") {
                Task { await model.refreshScreenPermission(requestIfNeeded: true) }
            }
            .controlSize(.small)
        }
        .padding(.horizontal, 14)
        .padding(.vertical, 8)
    }
}

// MARK: - Transport

private struct TransportSection: View {
    let model: RecorderModel

    var body: some View {
        VStack(spacing: 8) {
            HStack(spacing: 8) {
                if model.phase.isActiveSession {
                    PauseButton(model: model)
                }
                RecordButton(model: model)
            }
            if model.showNerdLogs, model.phase.isActiveSession, !model.metricsText.isEmpty {
                Text(model.metricsText)
                    .font(.pixel(11))
                    .foregroundStyle(.secondary)
                    .monospacedDigit()
            }
        }
        .padding(.horizontal, 14)
        .padding(.vertical, 12)
    }
}

private struct RecordButton: View {
    let model: RecorderModel

    var body: some View {
        Button(action: model.toggleRecording) {
            HStack(spacing: 8) {
                Image(systemName: model.phase.isActiveSession ? "stop.fill" : "record.circle.fill")
                    .font(.system(size: 13, weight: .bold))
                Text(label)
                    .font(.pixel(13))
            }
            .frame(maxWidth: .infinity)
            .frame(height: 40)
            .foregroundStyle(.white)
            .background(Theme.red, in: RoundedRectangle(cornerRadius: 8))
        }
        .buttonStyle(.plain)
        .disabled(disabled)
        .opacity(disabled ? 0.45 : 1)
    }

    private var label: String {
        switch model.phase {
        case .starting: "STARTING"
        case .stopping: "STOPPING"
        default: model.phase.isActiveSession ? "STOP" : "REC"
        }
    }

    private var disabled: Bool {
        if model.phase.isActiveSession {
            return model.phase == .starting || model.phase == .stopping
        }
        return !model.canRecord
    }
}

private struct PauseButton: View {
    let model: RecorderModel

    var body: some View {
        Button(action: model.togglePause) {
            Image(systemName: model.phase == .paused ? "play.fill" : "pause.fill")
                .font(.system(size: 13, weight: .bold))
                .frame(width: 48, height: 40)
                .background(.quaternary, in: RoundedRectangle(cornerRadius: 8))
        }
        .buttonStyle(.plain)
        .disabled(model.phase == .pausing || model.phase == .resuming || model.phase == .stopping)
    }
}

// MARK: - Config

private struct ConfigSection: View {
    @Bindable var model: RecorderModel

    private var locked: Bool {
        model.phase.isActiveSession || model.phase.isBusy
    }

    var body: some View {
        Grid(alignment: .leading, horizontalSpacing: 10, verticalSpacing: 8) {
            GridRow {
                FieldLabel("SOURCE")
                Segmented(
                    options: CaptureSourceKind.allCases.map(\.label),
                    selection: sourceBinding
                )
            }
            GridRow {
                FieldLabel("TARGET")
                PopUp(options: model.visibleTargets.map(\.name), selection: targetBinding)
                    .frame(maxWidth: .infinity)
            }
            GridRow {
                FieldLabel("FORMAT")
                enumPicker(\.codec, Codec.allCases) { $0.label }
            }
            GridRow {
                FieldLabel("PRESET")
                enumPicker(\.quality, Quality.allCases) { $0.label }
            }
            GridRow {
                FieldLabel("SIZE")
                enumPicker(
                    \.resolution, Resolution.allCases.filter(model.settings.resolutionAllowed)
                ) { $0.label }
            }
            GridRow {
                FieldLabel("FPS")
                enumPicker(\.fps, FrameRate.allCases.filter(model.settings.fpsAllowed)) {
                    $0.label
                }
            }
            GridRow {
                FieldLabel("AUDIO")
                HStack(spacing: 6) {
                    toggle(\.includeSystemAudio)
                    Spacer(minLength: 0)
                    FieldLabel("MICROPHONE", width: nil)
                    micToggle
                }
            }
        }
        .disabled(locked)
        .controlSize(.small)
        .padding(.horizontal, 14)
        .padding(.vertical, 12)
    }

    private var sourceBinding: Binding<Int> {
        Binding(
            get: { CaptureSourceKind.allCases.firstIndex(of: model.settings.source) ?? 0 },
            set: { index in
                let kinds = CaptureSourceKind.allCases
                guard kinds.indices.contains(index) else { return }
                model.update { $0.source = kinds[index] }
            }
        )
    }

    private var targetBinding: Binding<Int> {
        Binding(
            get: {
                model.visibleTargets.firstIndex { $0.key == model.selectedTarget?.key } ?? 0
            },
            set: { index in
                guard model.visibleTargets.indices.contains(index) else { return }
                model.select(targetKey: model.visibleTargets[index].key)
            }
        )
    }

    private func enumPicker<Value: Hashable>(
        _ keyPath: WritableKeyPath<RecorderSettings, Value>,
        _ options: [Value],
        label: @escaping (Value) -> String
    ) -> some View {
        PopUp(
            options: options.map(label),
            selection: Binding(
                get: { options.firstIndex(of: model.settings[keyPath: keyPath]) ?? 0 },
                set: { index in
                    guard options.indices.contains(index) else { return }
                    model.update { $0[keyPath: keyPath] = options[index] }
                }
            )
        )
        .frame(maxWidth: .infinity)
    }

    /// Shows on only when mic permission is actually granted. Toggling on
    /// without permission triggers the system prompt (or System Settings if
    /// previously denied) and only sticks if access was granted.
    private var micToggle: some View {
        Toggle(
            "",
            isOn: Binding(
                get: { model.settings.includeMicrophone && model.micPermission.isGranted },
                set: { value in
                    guard value else {
                        model.update { $0.includeMicrophone = false }
                        return
                    }
                    Task {
                        await model.requestMicPermission()
                        if model.micPermission.isGranted {
                            model.update { $0.includeMicrophone = true }
                        }
                    }
                }
            )
        )
        .toggleStyle(.switch)
        .labelsHidden()
    }

    private func toggle(_ keyPath: WritableKeyPath<RecorderSettings, Bool>) -> some View {
        Toggle(
            "",
            isOn: Binding(
                get: { model.settings[keyPath: keyPath] },
                set: { value in model.update { $0[keyPath: keyPath] = value } }
            )
        )
        .toggleStyle(.switch)
        .labelsHidden()
    }
}

/// `NSSegmentedControl` bridged for the same reason as `PopUp`: SwiftUI's
/// segmented picker hugs its labels and centers in the column instead of
/// adopting the column width.
private struct Segmented: NSViewRepresentable {
    let options: [String]
    @Binding var selection: Int
    @Environment(\.isEnabled) private var isEnabled
    @Environment(\.controlSize) private var controlSize

    func makeNSView(context: Context) -> NSSegmentedControl {
        let control = NSSegmentedControl(
            labels: options,
            trackingMode: .selectOne,
            target: context.coordinator,
            action: #selector(Coordinator.changed(_:))
        )
        control.segmentDistribution = .fillEqually
        control.setContentHuggingPriority(.defaultLow, for: .horizontal)
        control.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)
        return control
    }

    func updateNSView(_ control: NSSegmentedControl, context: Context) {
        context.coordinator.selection = $selection
        if control.segmentCount == options.count {
            for (index, label) in options.enumerated() {
                control.setLabel(label, forSegment: index)
            }
        }
        if options.indices.contains(selection) {
            control.selectedSegment = selection
        }
        control.controlSize = controlSize.nsControlSize
        control.font = .systemFont(ofSize: NSFont.systemFontSize(for: controlSize.nsControlSize))
        control.isEnabled = isEnabled
    }

    func makeCoordinator() -> Coordinator { Coordinator($selection) }

    final class Coordinator: NSObject {
        var selection: Binding<Int>

        init(_ selection: Binding<Int>) { self.selection = selection }

        // AppKit only fires control actions on the main thread.
        @MainActor @objc func changed(_ sender: NSSegmentedControl) {
            selection.wrappedValue = sender.selectedSegment
        }
    }
}

/// `NSPopUpButton` bridged by hand because SwiftUI's menu `Picker` sizes its
/// bezel to the widest menu item and ignores proposed widths — the config
/// rows need every popup to adopt the column width so their edges align.
private struct PopUp: NSViewRepresentable {
    let options: [String]
    @Binding var selection: Int
    @Environment(\.isEnabled) private var isEnabled
    @Environment(\.controlSize) private var controlSize

    func makeNSView(context: Context) -> TruncatingPopUp {
        let button = TruncatingPopUp(frame: .zero, pullsDown: false)
        button.target = context.coordinator
        button.action = #selector(Coordinator.changed(_:))
        button.setContentHuggingPriority(.defaultLow, for: .horizontal)
        button.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)
        return button
    }

    func updateNSView(_ button: TruncatingPopUp, context: Context) {
        context.coordinator.selection = $selection
        if button.itemTitles != options {
            button.removeAllItems()
            let menu = NSMenu()
            for title in options {
                menu.addItem(NSMenuItem(title: title, action: nil, keyEquivalent: ""))
            }
            button.menu = menu
        }
        if options.indices.contains(selection) {
            button.selectItem(at: selection)
        }
        button.controlSize = controlSize.nsControlSize
        button.font = .menuFont(ofSize: NSFont.systemFontSize(for: controlSize.nsControlSize))
        button.displayTitle = options.indices.contains(selection) ? options[selection] : ""
        button.isEnabled = isEnabled
    }

    // Without this, SwiftUI sizes the button to its intrinsic (title) width
    // and centers it in the proposed frame, so each popup's left edge drifts
    // with its selected title instead of sitting on the column edge.
    func sizeThatFits(
        _ proposal: ProposedViewSize, nsView: TruncatingPopUp, context: Context
    ) -> NSSize? {
        NSSize(
            width: proposal.width ?? nsView.intrinsicContentSize.width,
            height: nsView.intrinsicContentSize.height
        )
    }

    func makeCoordinator() -> Coordinator { Coordinator($selection) }

    final class Coordinator: NSObject {
        var selection: Binding<Int>

        init(_ selection: Binding<Int>) { self.selection = selection }

        // AppKit only fires control actions on the main thread.
        @MainActor @objc func changed(_ sender: NSPopUpButton) {
            selection.wrappedValue = sender.indexOfSelectedItem
        }
    }
}

/// When the selected title overflows the bezel, AppKit shaves the title's
/// leading inset, so a long TARGET name starts left of every other popup's
/// title. The cell is no longer consulted for layout, so instead the button
/// displays a detached menu item whose title is pre-truncated to fit — the
/// dropdown menu keeps the full names.
final class TruncatingPopUp: NSPopUpButton {
    var displayTitle: String = "" {
        didSet {
            if displayTitle != oldValue { needsLayout = true }
        }
    }

    override func layout() {
        super.layout()
        updateDisplayItem()
    }

    private func updateDisplayItem() {
        guard let cell = cell as? NSPopUpButtonCell else { return }
        // Leading inset plus chevron area; found empirically, and erring
        // large only truncates a touch early — it can't break alignment.
        let available = bounds.width - 40
        var title = displayTitle
        if available > 0, width(of: title) > available {
            while !title.isEmpty, width(of: title + "…") > available {
                title.removeLast()
            }
            title = title.trimmingCharacters(in: .whitespaces) + "…"
        }
        guard cell.menuItem?.title != title || cell.usesItemFromMenu else { return }
        cell.usesItemFromMenu = false
        cell.menuItem = NSMenuItem(title: title, action: nil, keyEquivalent: "")
        needsDisplay = true
    }

    private func width(of title: String) -> CGFloat {
        let font = self.font ?? NSFont.menuFont(ofSize: 0)
        return (title as NSString).size(withAttributes: [.font: font]).width
    }
}

extension ControlSize {
    fileprivate var nsControlSize: NSControl.ControlSize {
        switch self {
        case .mini: .mini
        case .small: .small
        case .large, .extraLarge: .large
        default: .regular
        }
    }
}

private struct FieldLabel: View {
    let text: String
    let width: CGFloat?

    init(_ text: String, width: CGFloat? = 58) {
        self.text = text
        self.width = width
    }

    var body: some View {
        Text(text)
            .font(.pixel(10))
            .foregroundStyle(.secondary)
            .fixedSize()
            .gridColumnAlignment(.leading)
            .frame(width: width, alignment: .leading)
    }
}
