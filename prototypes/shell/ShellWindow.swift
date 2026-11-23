import AppKit

final class ShellWindow: NSWindow {
    weak var shell: Shell?

    override func sendEvent(_ event: NSEvent) {
        if shell?.handle(event) != true {
            super.sendEvent(event)
            shell?.syncFocusWithResponder()
        }
    }
}

final class Shell: NSObject, NSWindowDelegate {
    private static let pushModifiers: NSEvent.ModifierFlags = [.command, .option, .shift]
    let window: ShellWindow
    private let app: ShellApp
    private let isMain: Bool
    private let root = NSView()
    private let workArea = WorkArea()
    private let layoutButton: Button
    private var layout: Layout
    private(set) var focused: UUID
    private var viewName: String?
    private var layoutUndo: SavedView?
    private var dividerStart: SavedView?
    private var keyboardPush: (key: UInt16, original: SavedView)?
    private var dividerHighlightTimer: Timer?
    private var detailTargets: [UUID: UUID]
    private var routePreview: UUID?
    private var details: [UUID: (source: UUID, request: DetailRequest)] = [:]
    private var floatingDetail: (source: UUID, view: SlotView)?
    private var cachedSlots: [UUID: [SlotView]] = [:]
    private var picker: Picker?
    private var cover: NSView?
    private weak var previousResponder: NSView?
    private var toast: NSVisualEffectView?
    private var toastTimer: Timer?

    init(app: ShellApp, initial: WindowState, isMain: Bool) {
        self.app = app
        self.isMain = isMain
        layout = initial.view.layout
        focused = initial.view.focused
        viewName = initial.namedView
        detailTargets = initial.view.detailTargets ?? [:]
        window = ShellWindow(contentRect: NSRect(x: 0, y: 0, width: 1020, height: 650),
                             styleMask: [.titled, .closable, .miniaturizable, .resizable],
                             backing: .buffered, defer: false)
        let edit = Button("", icon: "square.split.2x2") {}
        self.layoutButton = edit
        super.init()
        window.shell = self
        window.delegate = self
        window.isReleasedWhenClosed = false
        window.minSize = NSSize(width: 560, height: 390)
        window.contentView = root
        window.setFrameAutosaveName(isMain ? "TodShellMainWindow" : "")
        if !isMain || !window.setFrameUsingName("TodShellMainWindow") { window.center() }
        if !isMain { window.cascadeTopLeft(from: NSPoint(x: window.frame.minX + 28, y: window.frame.maxY - 28)) }
        buildWindow()
        render(focusSlot: true)
    }

    private func buildWindow() {
        root.wantsLayer = true
        let views = Button("Views", icon: "square.on.square") { [weak self] in self?.showViews() }
        let palette = iconButton("Command Palette", icon: "magnifyingglass") { [weak self] in self?.showCommands() }
        let settings = iconButton("Settings", icon: "gearshape") { [weak self] in self?.showSettings() }
        layoutButton.target = self
        layoutButton.action = #selector(showLayoutMenu)
        configureIcon(layoutButton, title: "Layout Menu")

        let header = NSStackView(views: [views, NSView(), palette, layoutButton, settings])
        header.alignment = .centerY
        header.spacing = 8
        header.edgeInsets = NSEdgeInsets(top: 5, left: 2, bottom: 5, right: 2)
        header.heightAnchor.constraint(equalToConstant: 34).isActive = true

        workArea.onDividerChange = { [weak self] path, ratio, finished in
            self?.dragDivider(path, to: ratio, finished: finished)
        }
        let stack = NSStackView(views: [header, workArea])
        stack.orientation = .vertical
        stack.alignment = .leading
        stack.spacing = 5
        pin(stack, to: root, inset: 12)
        for child in stack.arrangedSubviews {
            child.widthAnchor.constraint(equalTo: stack.widthAnchor).isActive = true
        }
    }

    private func iconButton(_ title: String, icon: String, perform: @escaping () -> Void) -> Button {
        let button = Button("", icon: icon, perform: perform)
        configureIcon(button, title: title)
        return button
    }

    private func configureIcon(_ button: Button, title: String) {
        button.isBordered = false
        button.contentTintColor = .labelColor
        button.image?.isTemplate = true
        button.setAccessibilityLabel(title)
        button.toolTip = title
        button.widthAnchor.constraint(equalToConstant: 33).isActive = true
    }

    private func render(focusSlot: Bool) {
        let ids = layout.slots
        if !ids.contains(focused) { focused = ids[0] }
        for (id, oldView) in workArea.slots where !ids.contains(id) {
            oldView.removeFromSuperview()
            workArea.slots[id] = nil
        }
        for id in ids {
            let panel = layout.panel(at: id)
            let detail = details[id]?.request
            if let existing = workArea.slots[id], existing.panel == panel, existing.detail == detail { continue }
            workArea.slots[id]?.removeFromSuperview()
            let view: SlotView
            if let detail {
                view = SlotView(id: id, panel: panel, detail: detail, closeDetail: {
                    [weak self] in self?.closeDetail(in: id)
                })
            } else if let cached = cachedSlots[id]?.first(where: { $0.panel == panel }) {
                view = cached
            } else {
                view = SlotView(id: id, panel: panel, setPanel: { [weak self] in self?.showPanels(for: id) },
                                showDetail: { [weak self] in self?.present($0, from: id) })
                cachedSlots[id, default: []].append(view)
            }
            workArea.addSubview(view)
            workArea.slots[id] = view
        }
        workArea.layoutTree = layout
        workArea.needsLayout = true
        window.title = viewName.map { "\($0) — Tod Shell" } ?? "Tod Shell"
        if focusSlot, let slot = workArea.slots[focused] {
            window.makeFirstResponder(slot)
        }
        updateFocus()
        cachedSlots = cachedSlots.compactMapValues { views in
            guard let id = views.first?.id, ids.contains(id),
                  let assigned = views.first(where: { $0.panel == layout.panel(at: id) }) else { return nil }
            return [assigned]
        }
        persist()
    }

    private func persist() {
        if isMain && !app.testing {
            app.write(WindowState(view: snapshot, namedView: viewName), key: "latestWindow")
        }
    }

    private var snapshot: SavedView { SavedView(layout: layout, focused: focused, detailTargets: detailTargets) }

    private func focus(_ id: UUID) {
        guard let slot = workArea.slots[id] else { return }
        focused = id
        window.makeFirstResponder(slot)
        updateFocus()
        persist()
    }

    private func updateFocus() {
        root.effectiveAppearance.performAsCurrentDrawingAppearance {
            root.layer?.backgroundColor = NSColor.windowBackgroundColor.cgColor
        }
        for (id, slot) in workArea.slots {
            slot.showFocus(selected: id == focused && floatingDetail == nil, active: window.isKeyWindow,
                           previewing: id == routePreview)
        }
        floatingDetail?.view.showFocus(selected: true, active: window.isKeyWindow)
    }

    func windowDidBecomeKey(_ notification: Notification) { updateFocus() }
    func windowDidResignKey(_ notification: Notification) {
        finishKeyboardPush()
        updateFocus()
    }
    func windowWillClose(_ notification: Notification) {
        finishKeyboardPush()
        dividerHighlightTimer?.invalidate()
        toastTimer?.invalidate()
        app.close(self)
    }

    func syncFocusWithResponder() {
        guard picker == nil, floatingDetail == nil else { return }
        var responder = window.firstResponder as? NSView
        if let editor = responder as? NSTextView, editor.isFieldEditor {
            responder = editor.delegate as? NSView
        }
        if let slot = slotContaining(responder), slot.id != focused {
            focused = slot.id
            updateFocus()
            persist()
        }
    }

    private func slotContaining(_ startingView: NSView?) -> SlotView? {
        var view = startingView
        while let current = view {
            if let slot = current as? SlotView { return slot }
            view = current.superview
        }
        return nil
    }

    func handle(_ event: NSEvent) -> Bool {
        let modifiers = event.modifierFlags.intersection([.command, .control, .shift, .option])
        if let push = keyboardPush {
            if event.type == .keyUp, event.keyCode == push.key {
                finishKeyboardPush()
                return true
            }
            if (event.type == .flagsChanged && modifiers != Self.pushModifiers)
                || (event.type == .keyDown && (!event.isARepeat || event.keyCode != push.key))
                || event.type == .leftMouseDown {
                finishKeyboardPush()
            }
        }
        if event.type == .leftMouseDown, let floatingDetail,
           !floatingDetail.view.bounds.contains(floatingDetail.view.convert(event.locationInWindow, from: nil)) {
            closeFloatingDetail()
            return true
        }
        if event.type == .leftMouseDown, picker == nil, floatingDetail == nil {
            let point = root.convert(event.locationInWindow, from: nil)
            if let slot = slotContaining(root.hitTest(point)), slot.id != focused {
                focus(slot.id)
            }
        }
        guard event.type == .keyDown else { return false }
        if let picker {
            switch event.keyCode {
            case 53: hidePicker()
            case 36, 76: picker.accept()
            case 125 where modifiers.isEmpty: picker.navigate(1)
            case 126 where modifiers.isEmpty: picker.navigate(-1)
            case 48: picker.navigate(modifiers.contains(.shift) ? -1 : 1)
            default: return false
            }
            return true
        }
        if floatingDetail != nil {
            switch event.keyCode {
            case 53: closeFloatingDetail()
            case 36, 76: closeFloatingDetail()
            case 48: break
            default: return false
            }
            return true
        }
        if modifiers == Self.pushModifiers, let direction = arrow(event.keyCode) {
            pushBoundary(direction, key: event.keyCode)
            return true
        }
        if modifiers == app.slotNavigationShortcut.modifiers, let direction = arrow(event.keyCode) {
            if let next = layout.neighbor(of: focused, toward: direction, in: workArea.bounds) { focus(next) }
            return true
        }
        if modifiers == .command {
            switch event.charactersIgnoringModifiers {
            case "k": showCommands()
            case "p": showPanels(for: focused)
            case "n": app.open()
            default: return false
            }
            return true
        }
        if modifiers.isEmpty, let slot = workArea.slots[focused],
           responderIsInSlot(slot) {
            switch event.keyCode {
            case 53 where details[focused] != nil: closeDetail(in: focused); return true
            case 125: return slot.selectRelativeRow(1)
            case 126: return slot.selectRelativeRow(-1)
            case 36, 76:
                if let request = slot.detailRequest() { present(request, from: focused); return true }
            default: break
            }
        }
        return false
    }

    private func responderIsInSlot(_ slot: SlotView) -> Bool {
        slotContaining(window.firstResponder as? NSView) === slot
    }

    private func arrow(_ key: UInt16) -> Direction? {
        switch key {
        case 123: return .left
        case 124: return .right
        case 125: return .down
        case 126: return .up
        default: return nil
        }
    }

    @objc private func showLayoutMenu() {
        let menu = NSMenu()
        for choice in layoutCommands() {
            let item = NSMenuItem(title: choice.title, action: #selector(runLayoutMenuItem(_:)), keyEquivalent: "")
            item.target = self
            item.representedObject = choice
            menu.addItem(item)
        }
        menu.popUp(positioning: nil, at: NSPoint(x: layoutButton.bounds.minX, y: layoutButton.bounds.minY - 3),
                   in: layoutButton)
    }

    @objc private func runLayoutMenuItem(_ item: NSMenuItem) {
        (item.representedObject as? Choice)?.perform()
    }

    private func layoutCommands() -> [Choice] {
        var commands = [
            Choice(title: "Split Right") { [weak self] in self?.split(.horizontal) },
            Choice(title: "Split Below") { [weak self] in self?.split(.vertical) },
            Choice(title: "Resize Slot…") { [weak self] in self?.showResize() },
            Choice(title: "Remove Slot") { [weak self] in self?.remove() },
            Choice(title: "Set Detail Target…") { [weak self] in self?.showDetailTargets() }
        ]
        if layoutUndo != nil {
            commands.append(Choice(title: "Undo Layout Change") { [weak self] in self?.undoLayoutChange() })
        }
        return commands
    }

    private func undoLayoutChange() {
        guard let previous = layoutUndo else { return }
        layoutUndo = nil
        layout = previous.layout
        focused = previous.focused
        detailTargets = previous.detailTargets ?? [:]
        pruneDetails()
        render(focusSlot: true)
        notify("Layout change undone")
    }

    private func dragDivider(_ path: [SplitBranch], to ratio: CGFloat, finished: Bool) {
        if let handle = layout.geometry(in: workArea.bounds).dividers[path] {
            let previous = snapshot
            if resizeDivider(path, to: handle.limiting(ratio)), dividerStart == nil {
                dividerStart = previous
            }
        }
        if finished, let original = dividerStart {
            dividerStart = nil
            if layout != original.layout {
                layoutUndo = original
                persist()
            }
        }
    }

    @discardableResult
    private func resizeDivider(_ path: [SplitBranch], to ratio: CGFloat) -> Bool {
        let resized = layout.resizingDivider(at: path[...], to: ratio)
        guard resized != layout else { return false }
        layout = resized
        workArea.layoutTree = resized
        workArea.needsLayout = true
        workArea.layoutSubtreeIfNeeded()
        return true
    }

    private func pushBoundary(_ direction: Direction, key: UInt16) {
        guard let path = layout.dividerPath(for: focused, edge: direction),
              let divider = layout.geometry(in: workArea.bounds).dividers[path], divider.length > 0 else { return }
        dividerHighlightTimer?.invalidate()
        workArea.highlightedDivider = path
        if keyboardPush == nil { keyboardPush = (key, snapshot) }
        let step: CGFloat = direction.precedes ? -12 : 12
        resizeDivider(path, to: divider.limiting(divider.ratio + step / divider.length))
    }

    private func finishKeyboardPush() {
        guard let push = keyboardPush else { return }
        keyboardPush = nil
        if layout != push.original.layout {
            layoutUndo = push.original
            persist()
        }
        dividerHighlightTimer = Timer.scheduledTimer(withTimeInterval: 0.22, repeats: false) { [weak self] _ in
            self?.workArea.highlightedDivider = nil
        }
    }

    private func split(_ axis: Axis) {
        layoutUndo = snapshot
        let new = UUID()
        layout = layout.replacing(focused) { .split(axis, 0.5, $0, .slot(new, nil)) }
        focused = new
        render(focusSlot: true)
    }

    private func remove() {
        guard let remaining = layout.removing(focused) else {
            notify("A window needs at least one slot")
            return
        }
        let next = Direction.allCases.compactMap { layout.neighbor(of: focused, toward: $0, in: workArea.bounds) }.first
        layoutUndo = snapshot
        layout = remaining
        detailTargets = detailTargets.filter { $0.key != focused && $0.value != focused }
        pruneDetails()
        focused = next ?? layout.slots[0]
        render(focusSlot: true)
    }

    private func showResize() {
        let directions: [(Direction, String)] = [(.left, "Left"), (.right, "Right"), (.up, "Top"), (.down, "Bottom")]
        let geometry = layout.geometry(in: workArea.bounds)
        let choices = [true, false].flatMap { growing in
            directions.compactMap { (direction, title) -> Choice? in
                guard let path = layout.dividerPath(for: focused, edge: direction),
                      let divider = geometry.dividers[path] else { return nil }
                let delta: CGFloat = direction.precedes == growing ? -0.08 : 0.08
                let ratio = divider.limiting(divider.ratio + delta)
                guard ratio != divider.ratio else { return nil }
                let verb = growing ? "Grow" : "Shrink"
                return Choice(title: "\(verb) at \(title) Edge") { [weak self] in
                    guard let self else { return }
                    layoutUndo = snapshot
                    resizeDivider(path, to: ratio)
                    persist()
                }
            }
        }
        guard !choices.isEmpty else { notify("Split a slot before resizing"); return }
        showPicker(title: "Resize Slot", choices: choices)
    }

    private func showPanels(for id: UUID) {
        showPicker(title: "Set Panel · \(layout.panel(at: id)?.title ?? "Empty slot")",
                   choices: Panel.catalog.map { panel in
            Choice(title: panel.title, detail: panel.kind) { [weak self] in self?.assign(panel, to: id) }
        })
    }

    private func assign(_ panel: Panel, to id: UUID) {
        layoutUndo = snapshot
        details[id] = nil
        layout = layout.replacing(id) { _ in .slot(id, panel) }
        focused = id
        render(focusSlot: true)
    }

    private func showDetailTargets() {
        let source = focused
        let current = detailTargets[source]
        let from = layout.panel(at: source)?.title ?? "Empty Slot"
        let floating = Choice(title: "Floating Panel", detail: current == nil ? "Current" : "") {
            [weak self] in self?.setDetailTarget(nil, for: source)
        }
        let destinations = layout.slots.enumerated().compactMap { (index, id) -> Choice? in
            guard id != source else { return nil }
            let panelName = layout.panel(at: id)?.title ?? "Empty Slot"
            return Choice(title: "Slot \(index + 1) · \(panelName)", detail: current == id ? "Current" : "",
                          targetSlot: id) {
                [weak self] in self?.setDetailTarget(id, for: source)
            }
        }
        showPicker(title: "Set Detail Target · \(from)", choices: [floating] + destinations, onSelection: {
            [weak self] selection in
            self?.routePreview = selection?.targetSlot
            self?.updateFocus()
        })
    }

    private func setDetailTarget(_ target: UUID?, for source: UUID) {
        guard detailTargets[source] != target else { return }
        layoutUndo = snapshot
        detailTargets[source] = target
        persist()
    }

    private func present(_ request: DetailRequest, from source: UUID) {
        guard layout.slots.contains(source) else { return }
        if let target = detailTargets[source], layout.slots.contains(target) {
            details[target] = (source, request)
            focused = target
            render(focusSlot: true)
        } else {
            let backdrop = makeBackdrop()
            let view = SlotView(id: source, detail: request, closeDetail: { [weak self] in
                self?.closeFloatingDetail()
            })
            view.translatesAutoresizingMaskIntoConstraints = false
            backdrop.addSubview(view)
            NSLayoutConstraint.activate([
                view.centerXAnchor.constraint(equalTo: backdrop.centerXAnchor),
                view.topAnchor.constraint(equalTo: backdrop.topAnchor, constant: 64),
                view.widthAnchor.constraint(equalToConstant: 460),
                view.heightAnchor.constraint(equalToConstant: 200)
            ])
            cover = backdrop
            floatingDetail = (source, view)
            window.makeFirstResponder(view)
            updateFocus()
        }
    }

    private func closeDetail(in target: UUID) {
        guard let presentation = details.removeValue(forKey: target) else { return }
        let hadFocus = focused == target
        if hadFocus, layout.slots.contains(presentation.source) { focused = presentation.source }
        render(focusSlot: hadFocus)
    }

    private func closeFloatingDetail() {
        guard let floatingDetail else { return }
        self.floatingDetail = nil
        cover?.removeFromSuperview()
        cover = nil
        focus(floatingDetail.source)
    }

    private func pruneDetails() {
        let slots = Set(layout.slots)
        details = details.filter { slots.contains($0.key) && slots.contains($0.value.source) }
    }

    private func popOut() {
        guard let panel = layout.panel(at: focused) else { return }
        app.open(panel: panel)
    }

    private func showCommands() {
        let target = focused
        var commands: [Choice] = [
            Choice(title: "Set Panel…", detail: "⌘P") { [weak self] in self?.showPanels(for: target) },
            Choice(title: "New Window", detail: "⌘N") { [weak self] in self?.app.open() }
        ]
        if details[target] != nil {
            commands.append(Choice(title: "Close Details", detail: "Esc") { [weak self] in self?.closeDetail(in: target) })
        } else if layout.panel(at: target) != nil {
            commands.append(Choice(title: "Pop Out Panel") { [weak self] in self?.popOut() })
        }
        commands += layoutCommands()
        commands += [
            Choice(title: "Switch View…") { [weak self] in self?.showViews() },
            Choice(title: "Save View") { [weak self] in self?.save() },
            Choice(title: "Save View As…") { [weak self] in self?.showSave() },
            Choice(title: "Settings") { [weak self] in self?.showSettings() }
        ]
        showPicker(title: "Command Palette", choices: commands)
    }

    private func showViews() {
        let choices = app.views.keys.sorted { $0.localizedStandardCompare($1) == .orderedAscending }.map { name in
            Choice(title: name) { [weak self] in self?.loadView(named: name) }
        }
        showPicker(title: "Switch View", choices: choices)
    }

    private func loadView(named name: String) {
        guard let saved = app.views[name] else { return }
        layoutUndo = nil
        viewName = name
        layout = saved.layout
        focused = saved.focused
        detailTargets = saved.detailTargets ?? [:]
        details.removeAll()
        render(focusSlot: true)
        notify("Opened \(name)")
    }

    private func save() {
        guard let name = viewName, name != ShellApp.starterName else { showSave(); return }
        showPicker(title: "Save changes to \(name)?", choices: [
            Choice(title: "Save View") { [weak self] in self?.saveView(named: name) },
            Choice(title: "Keep Editing") {}
        ])
    }

    private func showSave() {
        showPicker(title: "Save View As", choices: [], name: "", submitName: { [weak self] name in
            guard let self else { return nil }
            guard !name.isEmpty else { return "Enter a view name" }
            guard !app.views.keys.contains(where: { $0.caseInsensitiveCompare(name) == .orderedSame }) else {
                return "That view name is already in use"
            }
            hidePicker()
            saveView(named: name)
            return nil
        })
    }

    private func saveView(named name: String) {
        app.save(snapshot, named: name)
        viewName = name
        render(focusSlot: false)
        notify("Saved \(name)")
    }

    private func showSettings() {
        let choices = SlotNavigationShortcut.allCases.map { shortcut in
            Choice(title: shortcut.title, detail: app.slotNavigationShortcut == shortcut ? "Current" : "") {
                [weak self] in
                self?.app.slotNavigationShortcut = shortcut
                self?.notify("Slot navigation uses \(shortcut.title)")
            }
        }
        showPicker(title: "Slot Navigation Shortcut", choices: choices)
    }

    private func showPicker(title: String, choices: [Choice], name: String? = nil,
                            onSelection: ((Choice?) -> Void)? = nil,
                            submitName: ((String) -> String?)? = nil) {
        hidePicker()
        let responder = window.firstResponder as? NSView
        if let editor = responder as? NSTextView, editor.isFieldEditor {
            previousResponder = editor.delegate as? NSView
        } else {
            previousResponder = responder
        }
        let cover = makeBackdrop()
        let picker = Picker(title: title, choices: choices, name: name, onSelection: onSelection, submitName: submitName) {
            [weak self] in self?.hidePicker()
        }
        picker.translatesAutoresizingMaskIntoConstraints = false
        cover.addSubview(picker)
        NSLayoutConstraint.activate([
            picker.topAnchor.constraint(equalTo: cover.topAnchor, constant: 47),
            picker.centerXAnchor.constraint(equalTo: cover.centerXAnchor),
            picker.widthAnchor.constraint(equalToConstant: 470)
        ])
        self.cover = cover
        self.picker = picker
        window.makeFirstResponder(picker.input)
    }

    private func makeBackdrop() -> NSView {
        let view = NSView()
        view.wantsLayer = true
        view.layer?.backgroundColor = NSColor.black.withAlphaComponent(0.23).cgColor
        pin(view, to: root)
        return view
    }

    private func hidePicker() {
        guard picker != nil else { return }
        let restore = previousResponder
        previousResponder = nil
        window.makeFirstResponder(nil)
        cover?.removeFromSuperview()
        cover = nil
        picker = nil
        if routePreview != nil {
            routePreview = nil
            updateFocus()
        }
        if let restore, restore.window === window, window.makeFirstResponder(restore) { return }
        if let slot = workArea.slots[focused] { window.makeFirstResponder(slot) }
    }

    private func notify(_ message: String) {
        toastTimer?.invalidate()
        toast?.removeFromSuperview()
        let view = NSVisualEffectView()
        view.material = .hudWindow
        view.state = .active
        view.wantsLayer = true
        view.layer?.cornerRadius = 7
        view.layer?.masksToBounds = true
        view.translatesAutoresizingMaskIntoConstraints = false
        let field = label(message, size: 13, weight: .medium)
        view.setAccessibilityElement(true)
        view.setAccessibilityLabel(message)
        root.addSubview(view)
        pin(field, to: view, inset: 9)
        NSLayoutConstraint.activate([
            view.centerXAnchor.constraint(equalTo: root.centerXAnchor),
            view.bottomAnchor.constraint(equalTo: root.bottomAnchor, constant: -23)
        ])
        toast = view
        NSAccessibility.post(element: view, notification: .announcementRequested, userInfo: [
            .announcement: message, .priority: NSAccessibilityPriorityLevel.medium.rawValue
        ])
        toastTimer = Timer.scheduledTimer(withTimeInterval: 2.5, repeats: false) { [weak view] _ in
            view?.removeFromSuperview()
        }
    }
}

extension Shell {
    func runSmokeTest() {
        func key(_ code: UInt16, _ characters: String = "", modifiers: NSEvent.ModifierFlags = [],
                 type: NSEvent.EventType = .keyDown, repeating: Bool = false) {
            let event = NSEvent.keyEvent(with: type, location: .zero, modifierFlags: modifiers,
                                         timestamp: 0, windowNumber: window.windowNumber, context: nil,
                                         characters: characters, charactersIgnoringModifiers: characters,
                                         isARepeat: repeating, keyCode: code)!
            window.sendEvent(event)
        }

        func type(_ string: String) {
            guard let picker else { preconditionFailure("Expected an open picker") }
            for character in string { key(0, String(character)) }
            precondition(picker.input.stringValue == string, "Text must reach the picker")
        }

        func command(_ title: String) {
            key(40, "k", modifiers: .command)
            type(title)
            key(36, "\r")
        }

        func mouse(_ type: NSEvent.EventType, at point: NSPoint) {
            let event = NSEvent.mouseEvent(with: type, location: point, modifierFlags: [], timestamp: 0,
                                           windowNumber: window.windowNumber, context: nil, eventNumber: 0,
                                           clickCount: 1, pressure: 1)!
            window.sendEvent(event)
        }

        root.layoutSubtreeIfNeeded()
        let starter = snapshot
        let source = focused
        let target = layout.slots[1]
        let originalTargetView = workArea.slots[target]
        let geometry = layout.geometry(in: workArea.bounds)

        for path: [SplitBranch] in [[], [.second]] {
            let handle = geometry.dividers[path]!
            let start = workArea.convert(NSPoint(x: handle.frame.midX, y: handle.frame.midY), to: nil)
            let finish = handle.axis == .horizontal ? NSPoint(x: start.x + 40, y: start.y)
                : NSPoint(x: start.x, y: start.y - 40)
            mouse(.leftMouseDown, at: start)
            mouse(.leftMouseDragged, at: finish)
            let live = layout.geometry(in: workArea.bounds).dividers[path]!
            precondition(live.ratio > handle.ratio, "Divider should resize live on either axis")
            precondition(workArea.dividers[path]?.frame == live.frame)
            mouse(.leftMouseUp, at: finish)
            precondition(focused == source)
            command("Undo Layout Change")
            precondition(layout == starter.layout, "A drag should be one undoable operation")
            root.layoutSubtreeIfNeeded()
        }

        let push = Self.pushModifiers
        key(123, modifiers: push)
        key(123, modifiers: push, type: .keyUp)
        precondition(layout == starter.layout && keyboardPush == nil && layoutUndo == nil)

        key(124, modifiers: push)
        let tapped = layout.geometry(in: workArea.bounds)
        precondition(tapped.dividers[[]]!.frame.minX == geometry.dividers[[]]!.frame.minX + 12)
        precondition(tapped.slots[target]!.minX == tapped.slots[layout.slots[2]]!.minX)
        precondition(workArea.dividers[[]]?.highlighted == true && focused == source)
        for _ in 0..<150 { key(124, modifiers: push, repeating: true) }
        let limit = layout.geometry(in: workArea.bounds)
        precondition(limit.slots[target]!.width >= 96)
        precondition(limit.slots[target]!.width < tapped.slots[target]!.width)
        let atLimit = layout
        key(124, modifiers: push, repeating: true)
        precondition(layout == atLimit)
        key(124, modifiers: push, type: .keyUp)
        precondition(keyboardPush == nil)
        command("Undo Layout Change")
        precondition(layout == starter.layout)

        focus(target)
        key(125, modifiers: push)
        key(125, modifiers: push, type: .keyUp)
        let down = layout.geometry(in: workArea.bounds)
        precondition(down.dividers[[.second]]!.frame.minY == geometry.dividers[[.second]]!.frame.minY + 12)
        precondition(down.dividers[[]]!.frame == geometry.dividers[[]]!.frame)
        focus(layout.slots[2])
        key(126, modifiers: push)
        key(126, modifiers: push, type: .keyUp)
        precondition(layout.geometry(in: workArea.bounds).dividers[[.second]]!.frame == geometry.dividers[[.second]]!.frame)
        key(123, modifiers: push)
        key(123, modifiers: push, type: .keyUp)
        precondition(layout.geometry(in: workArea.bounds).dividers[[]]!.frame.minX == geometry.dividers[[]]!.frame.minX - 12)
        command("Undo Layout Change")
        focus(source)

        key(124, modifiers: .option)
        precondition(focused == source, "Option-arrow is reserved for navigation history")
        key(124, modifiers: [.control, .command])
        precondition(focused == source, "Control-Command-arrow is reserved for Spaces")
        key(124, modifiers: [.command, .option])
        precondition(focused == target, "Default Command-Option-arrow should move right")
        key(125, modifiers: [.command, .option])
        precondition(focused == layout.slots[2], "Default Command-Option-arrow should move down")
        app.slotNavigationShortcut = .control
        key(123, modifiers: .control)
        precondition(focused == source, "Control-arrow should work when the OS delivers it")
        app.slotNavigationShortcut = .commandOption

        key(125)
        key(125)
        key(36, "\r")
        precondition(floatingDetail?.view.detail?.title == "Triage layout feedback")
        precondition(app.windows.count == 1, "Fallback detail must stay in the current window")
        key(53)
        precondition(floatingDetail == nil && focused == source)

        command("Set Detail Target")
        type("Slot 2")
        precondition(routePreview == target)
        key(36, "\r")
        precondition(routePreview == nil && detailTargets[source] == target)
        key(36, "\r")
        precondition(focused == target && workArea.slots[target]?.detail?.title == "Triage layout feedback")
        precondition(layout.panel(at: target) == Panel.catalog[1])
        key(53)
        precondition(focused == source && workArea.slots[target] === originalTargetView)

        command("Set Detail Target")
        type("Floating Panel")
        key(36, "\r")
        precondition(detailTargets[source] == nil)
        command("Undo Layout Change")
        precondition(detailTargets[source] == target)

        key(124, modifiers: [.command, .option])
        precondition(focused == target)
        command("Remove Slot")
        precondition(detailTargets[source] == nil)
        command("Undo Layout Change")
        precondition(detailTargets[source] == target && layout == starter.layout)
        command("Split Below")
        precondition(layout.slots.count == 4 && layout.panel(at: focused) == nil)
        let empty = focused
        let beforeResize = layout
        command("Resize Slot")
        type("Grow at Top Edge")
        key(36, "\r")
        precondition(layout != beforeResize, "Expected keyboard resize to change the split ratio")
        key(35, "p", modifiers: .command)
        type("Needs Review")
        key(36, "\r")
        precondition(layout.panel(at: empty) == Panel.catalog[2])
        command("Remove Slot")
        precondition(layout.slots.count == 3)

        command("Split Right")
        key(35, "p", modifiers: .command)
        type("Scratchpad")
        key(36, "\r")
        precondition(layout.slots.count == 4)
        let saved = snapshot
        command("Save View As")
        type("Review")
        key(36, "\r")
        precondition(app.views["Review"]?.layout == saved.layout)
        let encoded = try! JSONEncoder().encode(app.views)
        let loaded = try! JSONDecoder().decode([String: SavedView].self, from: encoded)
        precondition(loaded["Review"]?.focused == focused)
        precondition(loaded["Review"]?.detailTargets?[source] == target)
        command("Pop Out Panel")
        guard let poppedOut = app.windows.last, poppedOut !== self else { preconditionFailure("Expected a new window") }
        precondition(poppedOut.window !== window && poppedOut.layout.slots.count == 1)
        precondition(poppedOut.window.isKeyWindow && !window.isKeyWindow)
        precondition(poppedOut.layout.panel(at: poppedOut.focused) == Panel.catalog[4])
        precondition(layout.panel(at: focused) == Panel.catalog[4])
        poppedOut.split(.horizontal)
        precondition(poppedOut.layout.slots.count == 2 && layout == saved.layout)
        poppedOut.loadView(named: "Review")
        precondition(poppedOut.layout == saved.layout && poppedOut.detailTargets[source] == target)
        poppedOut.window.close()
        precondition(app.windows.count == 1)

        if let index = CommandLine.arguments.firstIndex(of: "--capture"), CommandLine.arguments.count > index + 1 {
            window.makeKeyAndOrderFront(nil)
            loadView(named: ShellApp.starterName)
            toast?.removeFromSuperview()
            if CommandLine.arguments.contains("--detail-preview") {
                key(36, "\r")
            }
            root.layoutSubtreeIfNeeded()
            root.displayIfNeeded()
            let bitmap = root.bitmapImageRepForCachingDisplay(in: root.bounds)!
            root.cacheDisplay(in: root.bounds, to: bitmap)
            let url = URL(fileURLWithPath: CommandLine.arguments[index + 1])
            try! bitmap.representation(using: .png, properties: [:])!.write(to: url)
        }
    }
}
