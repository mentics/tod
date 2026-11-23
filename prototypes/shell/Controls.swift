import AppKit

final class Button: NSButton {
    private let perform: () -> Void

    init(_ title: String, icon: String? = nil, perform: @escaping () -> Void) {
        self.perform = perform
        super.init(frame: .zero)
        self.title = title
        if let icon {
            image = NSImage(systemSymbolName: icon, accessibilityDescription: title)
            imagePosition = title.isEmpty ? .imageOnly : .imageLeading
        }
        bezelStyle = .rounded
        target = self
        action = #selector(invoke)
    }

    required init?(coder: NSCoder) { nil }

    @objc private func invoke() { perform() }
}

func label(_ text: String, size: CGFloat, weight: NSFont.Weight = .regular,
           color: NSColor = .labelColor) -> NSTextField {
    let field = NSTextField(labelWithString: text)
    field.font = .systemFont(ofSize: size, weight: weight)
    field.textColor = color
    field.lineBreakMode = .byTruncatingTail
    field.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)
    return field
}

func pin(_ view: NSView, to parent: NSView, inset: CGFloat = 0) {
    view.translatesAutoresizingMaskIntoConstraints = false
    parent.addSubview(view)
    NSLayoutConstraint.activate([
        view.topAnchor.constraint(equalTo: parent.topAnchor, constant: inset),
        view.bottomAnchor.constraint(equalTo: parent.bottomAnchor, constant: -inset),
        view.leadingAnchor.constraint(equalTo: parent.leadingAnchor, constant: inset),
        view.trailingAnchor.constraint(equalTo: parent.trailingAnchor, constant: -inset)
    ])
}

final class WorkArea: NSView {
    var layoutTree: Layout = .slot(UUID(), nil)
    var slots: [UUID: SlotView] = [:]
    private(set) var dividers: [[SplitBranch]: DividerView] = [:]
    var highlightedDivider: [SplitBranch]? {
        didSet {
            if let oldValue { dividers[oldValue]?.highlighted = false }
            if let highlightedDivider { dividers[highlightedDivider]?.highlighted = true }
        }
    }
    var onDividerChange: (([SplitBranch], CGFloat, Bool) -> Void)?
    override var isFlipped: Bool { true }

    override func layout() {
        super.layout()
        let geometry = layoutTree.geometry(in: bounds)
        for (id, frame) in geometry.slots {
            slots[id]?.frame = frame
        }
        for (path, divider) in dividers where geometry.dividers[path] == nil {
            divider.removeFromSuperview()
            dividers[path] = nil
        }
        for (path, handle) in geometry.dividers {
            let divider = dividers[path] ?? DividerView(path: path, handle: handle)
            if dividers[path] == nil {
                addSubview(divider)
                dividers[path] = divider
            }
            divider.update(handle)
            divider.highlighted = path == highlightedDivider
        }
    }

    func dragDivider(_ path: [SplitBranch], to ratio: CGFloat, finished: Bool) {
        onDividerChange?(path, ratio, finished)
    }
}

final class DividerView: NSView {
    private let path: [SplitBranch]
    private var handle: SplitDivider
    private var dragStart: (coordinate: CGFloat, ratio: CGFloat, length: CGFloat)?
    private var tracking: NSTrackingArea?
    private var hovered = false
    var highlighted = false {
        didSet { if highlighted != oldValue { needsDisplay = true } }
    }

    init(path: [SplitBranch], handle: SplitDivider) {
        self.path = path
        self.handle = handle
        super.init(frame: handle.frame)
        toolTip = "Drag to resize"
    }

    required init?(coder: NSCoder) { nil }

    func update(_ handle: SplitDivider) {
        self.handle = handle
        frame = handle.frame
        needsDisplay = true
    }

    override func updateTrackingAreas() {
        if let tracking { removeTrackingArea(tracking) }
        let area = NSTrackingArea(rect: .zero, options: [.activeInKeyWindow, .inVisibleRect, .mouseEnteredAndExited],
                                  owner: self)
        addTrackingArea(area)
        tracking = area
        super.updateTrackingAreas()
    }

    override func resetCursorRects() {
        let cursor: NSCursor = handle.axis == .horizontal ? .resizeLeftRight : .resizeUpDown
        addCursorRect(bounds, cursor: cursor)
    }

    override func mouseEntered(with event: NSEvent) { hovered = true; needsDisplay = true }
    override func mouseExited(with event: NSEvent) { hovered = false; needsDisplay = true }

    override func draw(_ dirtyRect: NSRect) {
        let active = highlighted || hovered || dragStart != nil
        (active ? NSColor.controlAccentColor : NSColor.separatorColor).setFill()
        let thickness: CGFloat = active ? 3 : 1
        if handle.axis == .horizontal {
            NSRect(x: bounds.midX - thickness / 2, y: 4, width: thickness, height: max(0, bounds.height - 8)).fill()
        } else {
            NSRect(x: 4, y: bounds.midY - thickness / 2, width: max(0, bounds.width - 8), height: thickness).fill()
        }
    }

    override func mouseDown(with event: NSEvent) {
        guard let area = superview as? WorkArea, handle.length > 0 else { return }
        let coordinate = handle.axis.coordinate(area.convert(event.locationInWindow, from: nil))
        dragStart = (coordinate, handle.ratio, handle.length)
        needsDisplay = true
    }

    override func mouseDragged(with event: NSEvent) { drag(event, finished: false) }
    override func mouseUp(with event: NSEvent) { drag(event, finished: true) }

    private func drag(_ event: NSEvent, finished: Bool) {
        guard let area = superview as? WorkArea, let start = dragStart else { return }
        let coordinate = handle.axis.coordinate(area.convert(event.locationInWindow, from: nil))
        area.dragDivider(path, to: start.ratio + (coordinate - start.coordinate) / start.length, finished: finished)
        if finished {
            dragStart = nil
            hovered = bounds.contains(convert(event.locationInWindow, from: nil))
            needsDisplay = true
        }
    }
}

private final class ScrollDocument: NSView {
    override var isFlipped: Bool { true }
}

private final class PanelScrollView: NSScrollView {
    override func tile() {
        super.tile()
        guard let documentView, documentView.frame.width != contentSize.width else { return }
        documentView.setFrameSize(NSSize(width: contentSize.width, height: documentView.frame.height))
    }
}

final class SlotView: NSView {
    let id: UUID
    let panel: Panel?
    let detail: DetailRequest?
    private let highlight = CAShapeLayer()
    private let selection: NSStackView?
    private var selectedRow: Int?

    init(id: UUID, panel: Panel? = nil, detail: DetailRequest? = nil,
         setPanel: @escaping () -> Void = {}, showDetail: @escaping (DetailRequest) -> Void = { _ in },
         closeDetail: @escaping () -> Void = {}) {
        self.id = id
        self.panel = panel
        self.detail = detail
        selection = panel?.rows.isEmpty == false ? NSStackView() : nil
        super.init(frame: .zero)
        wantsLayer = true
        layer?.cornerRadius = 9
        layer?.masksToBounds = true
        highlight.fillColor = nil
        highlight.zPosition = 100
        layer?.addSublayer(highlight)
        setAccessibilityElement(true)
        setAccessibilityRole(.group)
        setAccessibilityLabel(detail?.title ?? panel?.title ?? "Empty slot")

        if let detail {
            buildDetail(detail, close: closeDetail)
        } else if let panel {
            buildPanel(panel, showDetail: showDetail)
        } else {
            let prompt = Button("Set Panel…", icon: "plus", perform: setPanel)
            prompt.translatesAutoresizingMaskIntoConstraints = false
            addSubview(prompt)
            NSLayoutConstraint.activate([
                prompt.centerXAnchor.constraint(equalTo: centerXAnchor),
                prompt.centerYAnchor.constraint(equalTo: centerYAnchor)
            ])
        }
    }

    required init?(coder: NSCoder) { nil }
    override var acceptsFirstResponder: Bool { true }

    override func layout() {
        super.layout()
        CATransaction.begin()
        CATransaction.setDisableActions(true)
        highlight.path = CGPath(roundedRect: bounds.insetBy(dx: 2, dy: 2), cornerWidth: 8, cornerHeight: 8, transform: nil)
        CATransaction.commit()
    }

    func showFocus(selected: Bool, active: Bool, previewing: Bool = false) {
        effectiveAppearance.performAsCurrentDrawingAppearance {
            layer?.backgroundColor = NSColor.controlBackgroundColor.cgColor
            CATransaction.begin()
            CATransaction.setDisableActions(true)
            highlight.strokeColor = (previewing && active ? NSColor.systemOrange :
                selected && active ? NSColor.systemBlue :
                selected ? NSColor.tertiaryLabelColor : NSColor.separatorColor).cgColor
            highlight.lineWidth = (selected || previewing) && active ? 4 : selected ? 2 : 1
            CATransaction.commit()
        }
    }

    private func buildPanel(_ panel: Panel, showDetail: @escaping (DetailRequest) -> Void) {
        let title = label(panel.title, size: 19, weight: .semibold)
        let kind = label(panel.kind, size: 11, weight: .medium, color: .secondaryLabelColor)
        let symbol = NSImageView(image: NSImage(systemSymbolName: panel.icon, accessibilityDescription: nil) ?? NSImage())
        symbol.symbolConfiguration = NSImage.SymbolConfiguration(pointSize: 17, weight: .medium)
        symbol.contentTintColor = .secondaryLabelColor

        let heading = NSStackView(views: [symbol, title, NSView(), kind])
        if panel.kind == "Collection" {
            let details = Button("", icon: "doc.text.magnifyingglass") { [weak self] in
                if let request = self?.detailRequest() { showDetail(request) }
            }
            details.toolTip = "View Details (Return)"
            details.setAccessibilityLabel("View Details")
            details.isBordered = false
            details.contentTintColor = .labelColor
            heading.addArrangedSubview(details)
        }
        heading.alignment = .centerY
        heading.spacing = 9
        heading.translatesAutoresizingMaskIntoConstraints = false
        addSubview(heading)

        let content: NSView
        if let selection {
            selection.orientation = .vertical
            selection.alignment = .leading
            selection.spacing = 5
            selection.translatesAutoresizingMaskIntoConstraints = false
            for (index, row) in panel.rows.enumerated() {
                let button = Button(row, icon: panel.kind == "Collection" ? "circle" : "point.3.connected.trianglepath.dotted") {
                    [weak self] in self?.selectRow(index)
                }
                button.alignment = .left
                button.imageHugsTitle = true
                button.font = .systemFont(ofSize: 13)
                button.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)
                selection.addArrangedSubview(button)
                button.widthAnchor.constraint(equalTo: selection.widthAnchor).isActive = true
                button.heightAnchor.constraint(equalToConstant: 30).isActive = true
            }
            let scroll = PanelScrollView()
            scroll.drawsBackground = false
            scroll.hasVerticalScroller = true
            let height = CGFloat(panel.rows.count) * 35
            let document = ScrollDocument(frame: NSRect(x: 0, y: 0, width: 100, height: height))
            scroll.documentView = document
            document.addSubview(selection)
            NSLayoutConstraint.activate([
                selection.topAnchor.constraint(equalTo: document.topAnchor),
                selection.leadingAnchor.constraint(equalTo: document.leadingAnchor),
                selection.trailingAnchor.constraint(equalTo: document.trailingAnchor)
            ])
            content = scroll
        } else {
            let notes = NSTextField()
            notes.placeholderString = "Type a note…"
            notes.setAccessibilityLabel("Scratchpad notes")
            let holder = NSView()
            notes.translatesAutoresizingMaskIntoConstraints = false
            holder.addSubview(notes)
            NSLayoutConstraint.activate([
                notes.topAnchor.constraint(equalTo: holder.topAnchor),
                notes.leadingAnchor.constraint(equalTo: holder.leadingAnchor),
                notes.trailingAnchor.constraint(equalTo: holder.trailingAnchor)
            ])
            content = holder
        }
        content.translatesAutoresizingMaskIntoConstraints = false
        addSubview(content)
        let contentBottom = content.bottomAnchor.constraint(equalTo: bottomAnchor, constant: -10)
        contentBottom.priority = .defaultHigh
        NSLayoutConstraint.activate([
            heading.topAnchor.constraint(equalTo: topAnchor, constant: 15),
            heading.leadingAnchor.constraint(equalTo: leadingAnchor, constant: 16),
            heading.trailingAnchor.constraint(equalTo: trailingAnchor, constant: -16),
            content.topAnchor.constraint(equalTo: heading.bottomAnchor, constant: 12),
            content.leadingAnchor.constraint(equalTo: heading.leadingAnchor),
            content.trailingAnchor.constraint(equalTo: heading.trailingAnchor),
            contentBottom
        ])
    }

    private func selectRow(_ index: Int) {
        guard let rows = selection?.arrangedSubviews, let button = rows[index] as? Button else { return }
        if let selectedRow { (rows[selectedRow] as? Button)?.bezelColor = nil }
        window?.makeFirstResponder(button)
        button.bezelColor = .controlAccentColor
        selectedRow = index
        selection?.scrollToVisible(button.frame)
    }

    func selectRelativeRow(_ delta: Int) -> Bool {
        guard panel?.kind == "Collection", let rows = selection?.arrangedSubviews, !rows.isEmpty else { return false }
        let index = selectedRow.map { min(rows.count - 1, max(0, $0 + delta)) } ?? 0
        selectRow(index)
        return true
    }

    func detailRequest() -> DetailRequest? {
        guard let panel, panel.kind == "Collection", !panel.rows.isEmpty else { return nil }
        let selected = selectedRow ?? 0
        if selectedRow == nil { selectRow(selected) }
        let item = panel.rows[selected]
        let summary = switch item {
        case "Review agent handoff": "Check the latest agent notes and confirm who owns the remaining work."
        case "Triage layout feedback": "Group the feedback by keyboard navigation, slot editing, and window behavior."
        case "Prototype the new shell": "Try the keyboard flow from collection selection through opening a detail."
        default: "This item is ready to review in \(panel.title)."
        }
        return DetailRequest(title: item, collection: panel.title, summary: summary)
    }

    private func buildDetail(_ detail: DetailRequest, close: @escaping () -> Void) {
        let heading = label("Item Details", size: 12, weight: .semibold, color: .secondaryLabelColor)
        let close = Button("Close", perform: close)
        close.toolTip = "Close Details (Escape)"
        let header = NSStackView(views: [heading, NSView(), close])
        header.alignment = .centerY
        let title = label(detail.title, size: 20, weight: .semibold)
        let source = label(detail.collection, size: 12, color: .secondaryLabelColor)
        let summary = NSTextField(wrappingLabelWithString: detail.summary)
        summary.font = .systemFont(ofSize: 14)
        summary.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)
        let content = NSStackView(views: [header, title, source, summary])
        content.orientation = .vertical
        content.alignment = .leading
        content.spacing = 10
        content.translatesAutoresizingMaskIntoConstraints = false
        addSubview(content)
        NSLayoutConstraint.activate([
            content.topAnchor.constraint(equalTo: topAnchor, constant: 15),
            content.leadingAnchor.constraint(equalTo: leadingAnchor, constant: 16),
            content.trailingAnchor.constraint(equalTo: trailingAnchor, constant: -16)
        ])
        for child in content.arrangedSubviews {
            child.widthAnchor.constraint(equalTo: content.widthAnchor).isActive = true
        }
    }
}

struct Choice {
    let title: String
    var detail = ""
    var targetSlot: UUID? = nil
    let perform: () -> Void
}

final class Picker: NSVisualEffectView, NSTextFieldDelegate {
    let input: NSTextField
    private let titleField: NSTextField
    private let results = NSStackView()
    private let allChoices: [Choice]
    private let submitName: ((String) -> String?)?
    private let onSelection: ((Choice?) -> Void)?
    private var choices: [Choice] = []
    private var selected = 0
    private var resultButtons: [Button] = []
    private var close: () -> Void = {}

    init(title: String, choices: [Choice], name: String? = nil, onSelection: ((Choice?) -> Void)? = nil,
         submitName: ((String) -> String?)? = nil, close: @escaping () -> Void) {
        self.allChoices = choices
        self.submitName = submitName
        self.onSelection = onSelection
        self.close = close
        input = NSTextField()
        titleField = label(title, size: 15, weight: .semibold)
        super.init(frame: .zero)
        material = .popover
        state = .active
        wantsLayer = true
        layer?.cornerRadius = 12
        layer?.masksToBounds = true
        input.placeholderString = submitName == nil ? "Search…" : "View name"
        input.stringValue = name ?? ""
        input.delegate = self
        input.setAccessibilityLabel(input.placeholderString ?? title)

        results.orientation = .vertical
        results.alignment = .leading
        results.spacing = 3

        let escape = Button("Cancel", perform: close)
        let heading = NSStackView(views: [titleField, NSView(), escape])
        heading.alignment = .centerY
        let stack = NSStackView(views: [heading, input, results])
        stack.orientation = .vertical
        stack.alignment = .leading
        stack.spacing = 9
        pin(stack, to: self, inset: 14)
        for child in stack.arrangedSubviews {
            child.widthAnchor.constraint(equalTo: stack.widthAnchor).isActive = true
        }
        refresh()
    }

    required init?(coder: NSCoder) { nil }

    func controlTextDidChange(_ notification: Notification) {
        if submitName == nil {
            selected = 0
            refresh()
        }
    }

    func navigate(_ delta: Int) {
        guard !choices.isEmpty else { return }
        selected = min(choices.count - 1, max(0, selected + delta))
        styleSelection()
    }

    func accept() {
        if let submitName {
            if let error = submitName(input.stringValue.trimmingCharacters(in: .whitespacesAndNewlines)) {
                titleField.stringValue = error
            }
        } else if choices.indices.contains(selected) {
            let choice = choices[selected]
            close()
            choice.perform()
        }
    }

    private func refresh() {
        results.arrangedSubviews.forEach { $0.removeFromSuperview() }
        resultButtons.removeAll()
        if submitName != nil {
            let save = Button("Save") { [weak self] in self?.accept() }
            results.addArrangedSubview(save)
            return
        }
        let terms = input.stringValue.split(whereSeparator: \.isWhitespace)
        choices = Array(allChoices.filter { choice in
            terms.allSatisfy { (choice.title + " " + choice.detail).localizedCaseInsensitiveContains($0) }
        }.prefix(9))
        if choices.isEmpty {
            results.addArrangedSubview(label("No matches", size: 13, color: .secondaryLabelColor))
        }
        for (index, choice) in choices.enumerated() {
            let text = choice.detail.isEmpty ? choice.title : "\(choice.title)   ·   \(choice.detail)"
            let row = Button(text) { [weak self] in
                self?.selected = index
                self?.accept()
            }
            row.alignment = .left
            row.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)
            results.addArrangedSubview(row)
            row.widthAnchor.constraint(equalTo: results.widthAnchor).isActive = true
            resultButtons.append(row)
        }
        styleSelection()
    }

    private func styleSelection() {
        for (index, button) in resultButtons.enumerated() {
            button.bezelColor = index == selected ? .controlAccentColor : nil
        }
        if resultButtons.indices.contains(selected) {
            input.setAccessibilityHelp("Selected result: \(resultButtons[selected].title)")
        }
        onSelection?(choices.indices.contains(selected) ? choices[selected] : nil)
    }
}
