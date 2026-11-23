import AppKit

enum SlotNavigationShortcut: String, CaseIterable {
    case commandOption, control

    var title: String {
        switch self {
        case .commandOption: return "Command + Option + arrow"
        case .control: return "Control + arrow"
        }
    }

    var modifiers: NSEvent.ModifierFlags {
        switch self {
        case .commandOption: return [.command, .option]
        case .control: return .control
        }
    }
}

final class ShellApp: NSObject, NSApplicationDelegate {
    static let starterName = "Sample Workspace"

    let testing = CommandLine.arguments.contains("--smoke-test")
    private let defaults = UserDefaults(suiteName: "tod.interactive-shell-prototype")!
    private(set) var windows: [Shell] = []
    private(set) var views: [String: SavedView] = [:]
    var slotNavigationShortcut: SlotNavigationShortcut = .commandOption {
        didSet {
            if !testing { defaults.set(slotNavigationShortcut.rawValue, forKey: "slotNavigationShortcut") }
        }
    }

    func applicationDidFinishLaunching(_ notification: Notification) {
        configureMenu()
        let starter = Layout.starter
        let example = SavedView(layout: starter, focused: starter.slots[0])
        if !testing {
            views = read("savedViews") ?? [:]
            if let name = defaults.string(forKey: "slotNavigationShortcut"),
               let shortcut = SlotNavigationShortcut(rawValue: name) {
                slotNavigationShortcut = shortcut
            }
        }
        views[Self.starterName] = example
        let previous: WindowState? = testing ? nil : read("latestWindow")
        let primary = open(initial: previous ?? WindowState(view: example, namedView: Self.starterName), isMain: true)
        NSApp.activate(ignoringOtherApps: true)
        if testing {
            DispatchQueue.main.async {
                primary.runSmokeTest()
                print("Shell smoke test passed")
                NSApp.terminate(nil)
            }
        }
    }

    @discardableResult
    func open(panel: Panel? = nil, initial: WindowState? = nil, isMain: Bool = false) -> Shell {
        let id = UUID()
        let empty = SavedView(layout: .slot(id, panel), focused: id)
        let shell = Shell(app: self, initial: initial ?? WindowState(view: empty, namedView: nil), isMain: isMain)
        windows.append(shell)
        shell.window.makeKeyAndOrderFront(nil)
        return shell
    }

    func close(_ shell: Shell) { windows.removeAll { $0 === shell } }

    func applicationShouldTerminateAfterLastWindowClosed(_ sender: NSApplication) -> Bool { true }

    func save(_ view: SavedView, named name: String) {
        views[name] = view
        if !testing { write(views.filter { $0.key != Self.starterName }, key: "savedViews") }
    }

    func write<T: Encodable>(_ value: T, key: String) {
        if let data = try? JSONEncoder().encode(value) {
            defaults.set(data, forKey: key)
        }
    }

    private func read<T: Decodable>(_ key: String) -> T? {
        guard let data = defaults.data(forKey: key) else { return nil }
        return try? JSONDecoder().decode(T.self, from: data)
    }

    private func configureMenu() {
        let bar = NSMenu()
        let appItem = bar.addItem(withTitle: "Tod Shell", action: nil, keyEquivalent: "")
        appItem.submenu = NSMenu(title: "Tod Shell")
        appItem.submenu?.addItem(withTitle: "Quit Tod Shell", action: #selector(NSApplication.terminate(_:)), keyEquivalent: "q")

        let editItem = bar.addItem(withTitle: "Edit", action: nil, keyEquivalent: "")
        let edit = NSMenu(title: "Edit")
        for (title, action, shortcut) in [("Cut", "cut:", "x"), ("Copy", "copy:", "c"),
                                           ("Paste", "paste:", "v"), ("Select All", "selectAll:", "a")] {
            edit.addItem(withTitle: title, action: NSSelectorFromString(action), keyEquivalent: shortcut)
        }
        editItem.submenu = edit

        let windowItem = bar.addItem(withTitle: "Window", action: nil, keyEquivalent: "")
        let menu = NSMenu(title: "Window")
        let newWindow = menu.addItem(withTitle: "New Window", action: #selector(newWindow), keyEquivalent: "")
        newWindow.target = self
        menu.addItem(withTitle: "Close", action: #selector(NSWindow.performClose(_:)), keyEquivalent: "w")
        windowItem.submenu = menu
        NSApp.mainMenu = bar
        NSApp.windowsMenu = menu
    }

    @objc private func newWindow() { open(isMain: windows.isEmpty) }
}

let application = NSApplication.shared
let delegate = ShellApp()
application.setActivationPolicy(.regular)
application.delegate = delegate
application.run()
