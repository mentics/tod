import AppKit

enum Direction: CaseIterable {
    case left, right, up, down

    var axis: Axis { self == .left || self == .right ? .horizontal : .vertical }
    var precedes: Bool { self == .left || self == .up }
}

enum Axis: String, Codable {
    case horizontal, vertical

    func coordinate(_ point: CGPoint) -> CGFloat {
        self == .horizontal ? point.x : point.y
    }
}

enum SplitBranch { case first, second }

struct SplitDivider {
    let axis: Axis
    let frame: CGRect
    let ratio: CGFloat
    let length: CGFloat
    let firstMinimum: CGFloat
    let secondMinimum: CGFloat

    func limiting(_ proposed: CGFloat) -> CGFloat {
        guard length > 0 else { return ratio }
        let lower = min(ratio, max(0.15, firstMinimum / length))
        let upper = max(ratio, min(0.85, 1 - secondMinimum / length))
        return min(upper, max(lower, proposed))
    }
}

struct LayoutGeometry {
    var slots: [UUID: CGRect] = [:]
    var dividers: [[SplitBranch]: SplitDivider] = [:]
}

struct Panel: Codable, Equatable {
    let title: String
    let kind: String
    let icon: String
    let rows: [String]

    static let catalog = [
        Panel(title: "My Queue", kind: "Collection", icon: "tray",
              rows: ["Review agent handoff", "Triage layout feedback", "Confirm release checklist", "Update workspace notes"]),
        Panel(title: "In Progress", kind: "Collection", icon: "circle.lefthalf.filled",
              rows: ["Prototype the new shell", "Review session recovery", "Prepare the next build"]),
        Panel(title: "Needs Review", kind: "Collection", icon: "checkmark.seal",
              rows: ["Keyboard shortcuts", "Window lifecycle", "Collection filter behavior"]),
        Panel(title: "Agent Activity", kind: "Panel", icon: "waveform.path",
              rows: ["Scout · Reviewing open tasks", "Atlas · Ready for input", "Quill · Finished summarizing"]),
        Panel(title: "Scratchpad", kind: "Panel", icon: "note.text", rows: [])
    ]
}

indirect enum Layout: Codable, Equatable {
    case slot(UUID, Panel?)
    case split(Axis, CGFloat, Layout, Layout)

    var slots: [UUID] {
        switch self {
        case let .slot(id, _): return [id]
        case let .split(_, _, first, second): return first.slots + second.slots
        }
    }

    func panel(at id: UUID) -> Panel? {
        switch self {
        case let .slot(slot, panel): return slot == id ? panel : nil
        case let .split(_, _, first, second): return first.panel(at: id) ?? second.panel(at: id)
        }
    }

    func replacing(_ id: UUID, with change: (Layout) -> Layout) -> Layout {
        switch self {
        case let .slot(slot, _): return slot == id ? change(self) : self
        case let .split(axis, ratio, first, second):
            return .split(axis, ratio, first.replacing(id, with: change), second.replacing(id, with: change))
        }
    }

    func removing(_ id: UUID) -> Layout? {
        switch self {
        case let .slot(slot, _): return slot == id ? nil : self
        case let .split(axis, ratio, first, second):
            guard let newFirst = first.removing(id) else { return second }
            guard let newSecond = second.removing(id) else { return first }
            return .split(axis, ratio, newFirst, newSecond)
        }
    }

    func dividerPath(for id: UUID, edge direction: Direction) -> [SplitBranch]? {
        guard case let .split(axis, _, first, second) = self else { return nil }
        let inFirst = first.slots.contains(id)
        if let child = (inFirst ? first : second).dividerPath(for: id, edge: direction) {
            return [inFirst ? .first : .second] + child
        }
        guard axis == direction.axis, inFirst != direction.precedes else { return nil }
        return []
    }

    static func clampedRatio(_ ratio: CGFloat) -> CGFloat { min(0.85, max(0.15, ratio)) }

    func resizingDivider(at path: ArraySlice<SplitBranch>, to ratio: CGFloat) -> Layout {
        guard case let .split(axis, oldRatio, first, second) = self else { return self }
        switch path.first {
        case nil: return .split(axis, Self.clampedRatio(ratio), first, second)
        case .first: return .split(axis, oldRatio, first.resizingDivider(at: path.dropFirst(), to: ratio), second)
        case .second: return .split(axis, oldRatio, first, second.resizingDivider(at: path.dropFirst(), to: ratio))
        }
    }

    func geometry(in bounds: CGRect) -> LayoutGeometry {
        var geometry = LayoutGeometry()
        measure(in: bounds, path: [], into: &geometry)
        return geometry
    }

    private func minimumExtent(on target: Axis) -> CGFloat {
        switch self {
        case .slot: return target == .horizontal ? 96 : 72
        case let .split(axis, ratio, first, second):
            let a = first.minimumExtent(on: target)
            let b = second.minimumExtent(on: target)
            return axis == target ? 8 + max(a / ratio, b / (1 - ratio)) : max(a, b)
        }
    }

    private func measure(in bounds: CGRect, path: [SplitBranch], into geometry: inout LayoutGeometry) {
        switch self {
        case let .slot(id, _): geometry.slots[id] = bounds
        case let .split(axis, ratio, first, second):
            let total = axis == .horizontal ? bounds.width : bounds.height
            let gap = min(8, total)
            let available = total - gap
            let firstSize = (available * ratio).rounded()
            let secondSize = max(0, available - firstSize)
            let a: CGRect
            let b: CGRect
            let handle: CGRect
            if axis == .horizontal {
                a = CGRect(x: bounds.minX, y: bounds.minY, width: firstSize, height: bounds.height)
                b = CGRect(x: bounds.minX + firstSize + gap, y: bounds.minY, width: secondSize, height: bounds.height)
                handle = CGRect(x: a.maxX, y: bounds.minY, width: gap, height: bounds.height)
            } else {
                a = CGRect(x: bounds.minX, y: bounds.minY, width: bounds.width, height: firstSize)
                b = CGRect(x: bounds.minX, y: bounds.minY + firstSize + gap, width: bounds.width, height: secondSize)
                handle = CGRect(x: bounds.minX, y: a.maxY, width: bounds.width, height: gap)
            }
            geometry.dividers[path] = SplitDivider(axis: axis, frame: handle, ratio: ratio, length: available,
                                                   firstMinimum: first.minimumExtent(on: axis),
                                                   secondMinimum: second.minimumExtent(on: axis))
            first.measure(in: a, path: path + [.first], into: &geometry)
            second.measure(in: b, path: path + [.second], into: &geometry)
        }
    }

    func neighbor(of id: UUID, toward direction: Direction, in bounds: CGRect) -> UUID? {
        let rectangles = geometry(in: bounds).slots
        guard let source = rectangles[id] else { return nil }
        let horizontal = direction.axis == .horizontal
        let sourceStart = horizontal ? source.minY : source.minX
        let sourceEnd = horizontal ? source.maxY : source.maxX
        let sourceEdge = switch direction {
        case .left: source.minX
        case .right: source.maxX
        case .up: source.minY
        case .down: source.maxY
        }
        let candidates = slots.compactMap { candidate -> (UUID, CGFloat, CGFloat, CGFloat)? in
            guard candidate != id, let rect = rectangles[candidate] else { return nil }
            let gap = switch direction {
            case .left: sourceEdge - rect.maxX
            case .right: rect.minX - sourceEdge
            case .up: sourceEdge - rect.maxY
            case .down: rect.minY - sourceEdge
            }
            guard gap >= 0 else { return nil }
            let start = horizontal ? rect.minY : rect.minX
            let end = horizontal ? rect.maxY : rect.maxX
            let offset = abs((start + end) - (sourceStart + sourceEnd))
            let offAxis = max(0, max(sourceStart, start) - min(sourceEnd, end))
            return (candidate, offAxis, gap, offset)
        }
        return candidates.min { ($0.1, $0.2, $0.3) < ($1.1, $1.2, $1.3) }?.0
    }

    static var starter: Layout {
        .split(.horizontal, 0.58, .slot(UUID(), Panel.catalog[0]),
               .split(.vertical, 0.52, .slot(UUID(), Panel.catalog[1]), .slot(UUID(), Panel.catalog[3])))
    }
}

struct SavedView: Codable {
    let layout: Layout
    let focused: UUID
    var detailTargets: [UUID: UUID]? = nil
}

struct DetailRequest: Equatable {
    let title: String
    let collection: String
    let summary: String
}

struct WindowState: Codable {
    let view: SavedView
    let namedView: String?
}
