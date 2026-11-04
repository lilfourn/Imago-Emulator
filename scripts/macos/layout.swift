// Layout assertions for one window of the running app, checking
// docs/design-system.md: element frames from the app's layout probe
// (DS_EMULATOR_PROBE) and the window's buttons from its accessibility tree,
// pixels from a capture of the window. Each line reports what it measured,
// in points.
//
// usage: layout <pid> <window.png> <parts.tsv> [window title]
//        (capture with `screencapture -o -l`)
// Exit status: 0 all passed, 1 a check failed, 2 the capture doesn't match.
import AppKit
import ApplicationServices

let args = CommandLine.arguments
let pid = Int32(args[1])!
let image = NSBitmapImageRep(data: try! Data(contentsOf: URL(fileURLWithPath: args[2])))!
let title = args.count > 4 ? args[4] : nil
let app = AXUIElementCreateApplication(pid)

// The rules, in points.
/// Room between a window's edges and anything in it but the screens.
let edgeClearance = 16.0
/// How far two paddings may differ: icons and plates are drawn on whole
/// pixels, so they can be centred to within half of one at 2x. GPUI places
/// text by its advance, not its ink, a pixel left of centre at most, which
/// leaves its two sides a point apart.
let tight = 0.5, textTolerance = 1.0

// MARK: - Frames

var parts: [String: CGRect] = [:]
for line in (try! String(contentsOfFile: args[3], encoding: .utf8)).split(separator: "\n") {
    let field = line.split(separator: "\t")
    guard field.count == 5, let x = Double(field[1]), let y = Double(field[2]),
          let w = Double(field[3]), let h = Double(field[4]) else { continue }
    parts[String(field[0])] = CGRect(x: x, y: y, width: w, height: h)
}
/// The parts named `prefix…`, in name order.
func parts(_ prefix: String) -> [(String, CGRect)] {
    parts.filter { $0.key.hasPrefix(prefix) }.sorted { $0.key.localizedStandardCompare($1.key) == .orderedAscending }
        .map { (String($0.key.dropFirst(prefix.count)), $0.value) }
}

func attr(_ e: AXUIElement, _ name: String) -> AnyObject? {
    var v: AnyObject?
    return AXUIElementCopyAttributeValue(e, name as CFString, &v) == .success ? v : nil
}
func frame(_ e: AXUIElement) -> CGRect {
    var p = CGPoint.zero, s = CGSize.zero
    if let v = attr(e, kAXPositionAttribute) { AXValueGetValue(v as! AXValue, .cgPoint, &p) }
    if let v = attr(e, kAXSizeAttribute) { AXValueGetValue(v as! AXValue, .cgSize, &s) }
    return CGRect(origin: p, size: s)
}
// The window list can be briefly empty, e.g. right after a resize.
var found: AXUIElement?
for _ in 0..<30 {
    let windows = attr(app, kAXWindowsAttribute) as? [AXUIElement] ?? []
    found = windows.first { title == nil || ((attr($0, kAXTitleAttribute) as? String) ?? "").contains(title!) }
    if found != nil { break }
    usleep(100_000)
}
guard let window = found else { print("process \(pid) has no window \(title ?? "")"); exit(2) }
let win = frame(window)
let scale = (Double(image.pixelsWide) / win.width).rounded()
// A capture of another window, or of this one mid-resize, measures nothing.
guard abs(Double(image.pixelsWide) - win.width * scale) < 1,
      abs(Double(image.pixelsHigh) - win.height * scale) < 1 else {
    print("capture is \(image.pixelsWide)x\(image.pixelsHigh) px but the window is \(win.width)x\(win.height) pt; capture it again")
    exit(2)
}
let windowRect = CGRect(x: 0, y: 0, width: win.width, height: win.height)
/// The close button, from the accessibility tree: the traffic lights,
/// which the rest of the top bar lines up with. Absent when hidden.
let closeButton = (attr(window, kAXChildrenAttribute) as? [AXUIElement] ?? [])
    .first { (attr($0, kAXSubroleAttribute) as? String) == (kAXCloseButtonSubrole as String) }
    .map { frame($0).offsetBy(dx: -win.minX, dy: -win.minY) }
    .flatMap { $0.width > 0 ? $0 : nil }

// MARK: - Pixels

/// One pixel, in points.
let step = 1 / scale
precondition(image.bitsPerSample == 8 && !image.bitmapFormat.contains(.alphaFirst), "expects 8-bit RGBA captures")
let bytes = image.bitmapData!
typealias RGB = (Int, Int, Int)
func pixel(_ x: Double, _ y: Double) -> RGB {
    let px = min(max(Int(x * scale), 0), image.pixelsWide - 1)
    let py = min(max(Int(y * scale), 0), image.pixelsHigh - 1)
    let i = py * image.bytesPerRow + px * image.samplesPerPixel
    return (Int(bytes[i]), Int(bytes[i + 1]), Int(bytes[i + 2]))
}
func close(_ a: RGB, _ b: RGB, _ tolerance: Int = 3) -> Bool {
    abs(a.0 - b.0) <= tolerance && abs(a.1 - b.1) <= tolerance && abs(a.2 - b.2) <= tolerance
}
/// The commonest colour in `rect`: what its ink sits on.
func background(_ rect: CGRect) -> RGB {
    var counts: [Int: (Int, RGB)] = [:]
    var y = rect.minY
    while y < rect.maxY {
        var x = rect.minX
        while x < rect.maxX {
            let p = pixel(x, y)
            counts[p.0 << 16 | p.1 << 8 | p.2, default: (0, p)].0 += 1
            x += step
        }
        y += step
    }
    return counts.values.max { $0.0 < $1.0 }?.1 ?? (0, 0, 0)
}
/// Bounds of the pixels in `rect` that differ from `on` (by default its
/// commonest colour): glyphs, icons, plates.
func ink(in rect: CGRect, on: RGB? = nil, tolerance: Int = 24) -> CGRect? {
    let rect = rect.intersection(windowRect)
    guard !rect.isEmpty else { return nil }
    let back = on ?? background(rect)
    var (minX, minY, maxX, maxY) = (Double.infinity, Double.infinity, -Double.infinity, -Double.infinity)
    var y = rect.minY
    while y < rect.maxY {
        var x = rect.minX
        while x < rect.maxX {
            if !close(pixel(x, y), back, tolerance) {
                (minX, maxX) = (min(minX, x), max(maxX, x + step))
                (minY, maxY) = (min(minY, y), max(maxY, y + step))
            }
            x += step
        }
        y += step
    }
    return minX.isFinite ? CGRect(x: minX, y: minY, width: maxX - minX, height: maxY - minY) : nil
}
/// The first glyph of the text inked in `rect`: its columns up to the first
/// gap. A label's first letter is a capital or a digit, so this spans its
/// cap height, from which text is centred: descenders and accents don't
/// count.
func firstGlyph(in rect: CGRect, on: RGB? = nil, tolerance: Int = 24) -> CGRect? {
    let back = on ?? background(rect)
    guard let all = ink(in: rect, on: back, tolerance: tolerance) else { return nil }
    var x = all.minX
    while x < all.maxX, ink(in: CGRect(x: x, y: all.minY, width: step, height: all.height), on: back, tolerance: tolerance) != nil {
        x += step
    }
    return ink(in: CGRect(x: all.minX, y: all.minY, width: x - all.minX, height: all.height), on: back, tolerance: tolerance)
}
/// The lines of text inked in `rect`, top to bottom, split at blank rows.
func lines(in rect: CGRect) -> [CGRect] {
    let back = background(rect)
    var found: [CGRect] = []
    var start: Double?
    var y = rect.minY
    while y <= rect.maxY {
        let row = y < rect.maxY ? ink(in: CGRect(x: rect.minX, y: y, width: rect.width, height: step), on: back) : nil
        if row != nil, start == nil { start = y }
        if row == nil, let top = start {
            if let line = ink(in: CGRect(x: rect.minX, y: top, width: rect.width, height: y - top), on: back) {
                found.append(line)
            }
            start = nil
        }
        y += step
    }
    return found
}
/// Bounds of the pixels in `rect` of `color`: a white knob, say.
func matching(in rect: CGRect, _ color: RGB, tolerance: Int = 24) -> CGRect? {
    var (minX, minY, maxX, maxY) = (Double.infinity, Double.infinity, -Double.infinity, -Double.infinity)
    var y = rect.minY
    while y < rect.maxY {
        var x = rect.minX
        while x < rect.maxX {
            if close(pixel(x, y), color, tolerance) {
                (minX, maxX) = (min(minX, x), max(maxX, x + step))
                (minY, maxY) = (min(minY, y), max(maxY, y + step))
            }
            x += step
        }
        y += step
    }
    return minX.isFinite ? CGRect(x: minX, y: minY, width: maxX - minX, height: maxY - minY) : nil
}
/// The plate under `point`: walk out from it until its colour ends.
func plate(at point: CGPoint, tolerance: Int = 6) -> CGRect {
    let color = pixel(point.x, point.y)
    func walk(_ dx: Double, _ dy: Double) -> Double {
        var (x, y) = (point.x, point.y)
        while close(pixel(x + dx, y + dy), color, tolerance), windowRect.insetBy(dx: step, dy: step).contains(CGPoint(x: x + dx, y: y + dy)) {
            x += dx; y += dy
        }
        return dx != 0 ? x : y
    }
    let (left, right, top, bottom) = (walk(-step, 0), walk(step, 0), walk(0, -step), walk(0, step))
    return CGRect(x: left, y: top, width: right - left + step, height: bottom - top + step)
}

// MARK: - Checks

var failures = 0
func check(_ ok: Bool, _ what: String, _ detail: @autoclosure () -> String = "") {
    if !ok { failures += 1 }
    print("\(ok ? "PASS" : "FAIL")  \(what)\(ok ? "" : ": \(detail())")")
}
func f(_ v: Double) -> String { String(format: "%.1f", v) }
/// `a` and `b` match, within `tolerance`.
func even(_ what: String, _ a: Double, _ b: Double, _ names: (String, String), _ tolerance: Double = tight) {
    check(abs(a - b) <= tolerance, "\(what) (\(names.0) \(f(a)), \(names.1) \(f(b)))", "off by \(f(abs(a - b)))")
}
/// `content` sits centred in `box`, across and down.
func centered(_ what: String, _ box: CGRect, _ content: CGRect, across: Double? = tight, down: Double? = tight) {
    if let across {
        even("\(what): left and right padding match", content.minX - box.minX, box.maxX - content.maxX, ("left", "right"), across)
    }
    if let down {
        even("\(what): top and bottom padding match", content.minY - box.minY, box.maxY - content.maxY, ("top", "bottom"), down)
    }
}
/// The frame of a part, or nothing if it isn't drawn in this state.
func part(_ name: String) -> CGRect? { parts[name] }
/// The library's games, by their place in it.
let gameTiles = parts("tile:").filter { Int($0.0) != nil }

print("window \(Int(win.width))x\(Int(win.height)) pt, capture at \(f(scale))x, \(parts.count) parts")

// 1. Nothing but the screens and the traffic lights comes within 16pt of
//    the window's sides. A side's commonest colour on a row is its
//    background there.
// A running game shows the speed readout in these checks' settings.
let screensShown = parts.isEmpty || part("speed") != nil || part("sheet") != nil
if !screensShown {
    var touching: [Double] = []
    var y = 40.0
    while y < win.height - edgeClearance {
        let sides = [CGRect(x: 1, y: y, width: edgeClearance - 1, height: step),
                     CGRect(x: win.width - edgeClearance, y: y, width: edgeClearance - 1, height: step)]
        if sides.contains(where: { ink(in: $0) != nil }) { touching.append(y) }
        y += 1
    }
    check(touching.isEmpty, "nothing comes within \(Int(edgeClearance))pt of the window's sides",
          "\(touching.count) rows do, first at y \(f(touching.first ?? 0))")
}

// 2. Buttons: their glyph or label is centred in them. An icon is centred
//    by its ink; a label across by its ink and down by its first letter's.
for (name, box) in parts("button:") {
    // A button scrolled out of the window's view, or of the grid's, shows
    // nothing.
    if box.maxY > win.height || box.minY < 0 { continue }
    if let grid = part("grid"), box.minY >= grid.minY, !grid.insetBy(dx: -1, dy: -1).contains(box) { continue }
    let back = background(box)
    guard let glyphs = ink(in: box.insetBy(dx: 1, dy: 1), on: back) else {
        check(false, "\(name) button shows something"); continue
    }
    // A label is wider than it is tall; an icon is about square.
    let label = glyphs.width > glyphs.height * 1.6
    if label, let letter = firstGlyph(in: box.insetBy(dx: 1, dy: 1), on: back) {
        centered("\(name) button label", box, CGRect(x: glyphs.minX, y: letter.minY, width: glyphs.width, height: letter.height),
                 across: textTolerance, down: textTolerance)
    } else {
        centered("\(name) button icon", box, glyphs)
    }
}

// 3. The top bar lines up with the traffic lights: its title and icons
//    share their midline, and its last icon ends as far from the right edge
//    as the close button starts from the left, mirroring them.
if let close = closeButton, let lights = ink(in: close.insetBy(dx: -2, dy: -2)) {
    let midline = lights.midY
    if let bar = part("header.title"), let letter = firstGlyph(in: bar) {
        even("top bar title sits on the traffic lights' midline", letter.midY, midline, ("title", "lights"), textTolerance)
    }
    // The tabs between the library and the store: each label centred on
    // its pill, and on the traffic lights' midline.
    for (name, tab) in parts("tab:") {
        let back = background(tab)
        // In from the pill's round ends, whose corners are the track's
        // colour, and clear of its anti-aliased top and bottom edges. Only
        // strong ink counts: the title bar's edge effect shades an inactive
        // window's chosen pill from top to bottom.
        let inside = tab.insetBy(dx: tab.height / 2, dy: 2)
        guard let glyphs = ink(in: inside, on: back, tolerance: 80),
              let letter = firstGlyph(in: inside, on: back, tolerance: 80) else {
            check(false, "\(name) tab shows its name"); continue
        }
        even("\(name) tab's name is centred across it", glyphs.midX, tab.midX, ("name", "tab"), textTolerance)
        even("\(name) tab's name sits on the traffic lights' midline", letter.midY, midline, ("name", "lights"), textTolerance)
    }
    let icons = parts("button:").filter { $0.1.maxY <= 44 }.compactMap { name, box in ink(in: box.insetBy(dx: 1, dy: 1)).map { (name, $0) } }
    for (name, glyph) in icons {
        even("\(name) icon sits on the traffic lights' midline", glyph.midY, midline, ("icon", "lights"))
    }
    if let last = icons.max(by: { $0.1.maxX < $1.1.maxX }) {
        even("top bar's ends mirror each other", win.width - last.1.maxX, lights.minX, ("\(last.0) to the right edge", "close button to the left"))
    }
    let buttons = parts("button:").filter { $0.1.maxY <= 44 }
    if let first = buttons.first {
        for (name, box) in buttons.dropFirst() {
            check(abs(box.width - first.1.width) <= tight && abs(box.height - first.1.height) <= tight,
                  "\(name) button is the size of \(first.0)", "\(f(box.width))x\(f(box.height)) against \(f(first.1.width))x\(f(first.1.height))")
        }
    }
} else if let bar = part("header.title"), let letter = firstGlyph(in: bar) {
    // Without the traffic lights, the bar's midline is its own.
    even("top bar title is centred in the bar", letter.midY, bar.midY, ("title", "bar"), textTolerance)
}
// Clear of the traffic lights, which end 80pt in.
if title == "Settings", let bar = part("header.title"),
   let glyphs = ink(in: CGRect(x: 90, y: bar.minY, width: win.width - 180, height: bar.height)) {
    even("Settings title is centred across the window", glyphs.midX, win.width / 2, ("title", "window"), textTolerance)
}

// 4. The search field: its icon is as far from its left edge as from its
//    top and bottom, and the icon and the placeholder share a midline, the
//    field's own. (What is typed may start with a descender.)
if let field = part("field:search") {
    let plateRect = field
    if let icon = part("field:search.icon").flatMap({ ink(in: $0) }) {
        even("search icon is inset as far from the side as from the top", icon.minX - plateRect.minX,
             (plateRect.height - icon.height) / 2, ("side", "top and bottom"))
        even("search icon is centred down the field", icon.midY, plateRect.midY, ("icon", "field"))
        if let text = part("field:search.placeholder"), let letter = firstGlyph(in: text) {
            even("search placeholder sits on the icon's midline", letter.midY, icon.midY, ("text", "icon"), textTolerance)
        }
    }
    // The field spans the content: it starts and ends where the grid does,
    // once the games fill a row.
    if let first = part("tile:0") {
        even("search field starts where the games do", plateRect.minX, first.minX, ("field", "games"))
        let row = gameTiles.map { $0.1 }.filter { abs($0.minY - first.minY) < 1 }
        if gameTiles.count > row.count, let last = row.max(by: { $0.maxX < $1.maxX }) {
            even("search field ends where the games do", plateRect.maxX, last.maxX, ("field", "games"))
        }
    }
}

// 5. The games: the grid's sides mirror each other once they fill a row,
//    each game's picture sits as far in from its tile's sides as from its
//    top, and its title starts where the picture does and fits its box.
let tiles = gameTiles
if let first = tiles.first?.1 {
    let row = tiles.map { $0.1 }.filter { abs($0.minY - first.minY) < 1 }
    if tiles.count > row.count, let last = row.max(by: { $0.maxX < $1.maxX }) {
        even("the games' sides mirror each other", first.minX, win.width - last.maxX, ("left", "right"))
    }
}
for (index, tile) in tiles.prefix(6) {
    guard let art = part("tile.art:\(index)") else { continue }
    even("game \(index)'s picture is inset evenly", art.minX - tile.minX, tile.maxX - art.maxX, ("left", "right"))
    even("game \(index)'s picture is as far from the sides as the top", art.minX - tile.minX, art.minY - tile.minY, ("side", "top"))
    // A title under an open game menu shows only in part.
    if let titleBox = part("tile.title:\(index)"), !(part("menu")?.intersects(titleBox) ?? false),
       let line = lines(in: titleBox).first {
        even("game \(index)'s title starts where its picture does", line.minX, art.minX, ("title", "picture"), textTolerance)
        check(line.maxX < titleBox.maxX - 0.5,
              "game \(index)'s title fits its box", "ink to \(f(line.maxX)), box to \(f(titleBox.maxX))")
    }
}

// The game last played: as far in as everything else, its words as far
// in from its side as from its foot.
if let hero = part("hero") {
    even("the last-played game's sides mirror each other", hero.minX, win.width - hero.maxX, ("left", "right"))
    even("the last-played game is inset as far as the search field", hero.minX, edgeClearance, ("banner", "inset"))
    if let resume = part("button:resume"), let title = part("hero.title"), let letter = firstGlyph(in: title) {
        even("the last-played game's words are as far in as its button is up", letter.minX - hero.minX, hero.maxY - resume.maxY, ("side", "foot"), textTolerance)
    }
}

// 6. An empty library: its title and each line of its hint are centred.
for name in ["empty.title", "empty.hint"] {
    guard let box = part(name) else { continue }
    for (number, line) in lines(in: box).enumerated() {
        even("\(name) line \(number + 1) is centred", line.midX, win.width / 2, ("line", "window"), textTolerance)
    }
}

// 7. The game menu's sheet: centred, as far off the window's foot as its
//    sides, with its content inset evenly, its header on one midline, and
//    its tiles alike.
if let sheet = part("sheet") {
    even("sheet is centred", sheet.minX, win.width - sheet.maxX, ("left", "right"))
    // Once the sheet is as wide as it gets, its sides grow past its foot.
    if sheet.width < 420 - tight {
        even("sheet sits as far off the foot as the sides", win.height - sheet.maxY, sheet.minX, ("foot", "sides"))
    }
    // What is scrolled out of the sheet's view doesn't count.
    let content = (parts("tile:").map { $0.1 } + parts("row:").map { $0.1 } + parts("button:").map { $0.1 }
        + [part("menu.header"), part("controls")].compactMap { $0 })
        .filter { sheet.contains($0) }
    if let first = content.first {
        let all = content.dropFirst().reduce(first) { $0.union($1) }
        even("sheet's content is inset evenly", all.minX - sheet.minX, sheet.maxX - all.maxX, ("left", "right"))
        even("sheet's content ends as far from the foot as the sides", sheet.maxY - all.maxY, all.minX - sheet.minX, ("foot", "sides"))
    }
    if let titleBox = part("menu.title"), let letter = firstGlyph(in: titleBox) {
        if let back = part("button:back"), let glyph = ink(in: back.insetBy(dx: 1, dy: 1)) {
            even("back button and title share a midline", glyph.midY, letter.midY, ("back", "title"), textTolerance)
        } else if let left = parts("tile:").map({ $0.1.minX }).min(), let glyphs = ink(in: titleBox) {
            even("title starts where the content does", glyphs.minX, left, ("title", "content"), textTolerance)
        }
    }
    let menuTiles = parts("tile:").filter { !$0.0.contains(".") && Int($0.0) == nil }
    var iconSizes: [(String, CGRect)] = []
    for (name, tile) in menuTiles {
        guard let iconBox = part("tile.icon:\(name)"), let labelBox = part("tile.label:\(name)"),
              let icon = ink(in: iconBox), let label = ink(in: labelBox), let letter = firstGlyph(in: labelBox) else { continue }
        iconSizes.append((name, iconBox))
        even("\(name) tile's icon is centred across it", icon.midX, tile.midX, ("icon", "tile"))
        even("\(name) tile's label is centred across it", label.midX, tile.midX, ("label", "tile"), textTolerance)
        let group = CGRect(x: tile.minX, y: icon.minY, width: tile.width, height: letter.maxY - icon.minY)
        centered("\(name) tile's icon and label", tile, group, across: nil, down: textTolerance)
    }
    if let first = iconSizes.first {
        for (name, box) in iconSizes.dropFirst() {
            check(abs(box.width - first.1.width) <= tight && abs(box.height - first.1.height) <= tight,
                  "\(name) tile's icon is the size of \(first.0)'s",
                  "\(f(box.width))x\(f(box.height)) against \(f(first.1.width))x\(f(first.1.height))")
        }
    }
    if let first = menuTiles.first?.1 {
        for (name, tile) in menuTiles.dropFirst() {
            check(abs(tile.width - first.width) <= tight && abs(tile.height - first.height) <= tight,
                  "\(name) tile is the size of the first", "\(f(tile.width))x\(f(tile.height))")
        }
    }
    // A cheat's row: its name starts as far in as its switch ends, and the
    // switch's knob sits evenly in its track.
    for (name, row) in parts("row:cheat.") {
        guard let toggle = part("switch:\(name)"),
              let text = ink(in: CGRect(x: row.minX, y: row.minY, width: toggle.minX - row.minX, height: row.height)) else { continue }
        even("cheat \(name)'s ends mirror each other", text.minX - row.minX, row.maxX - toggle.maxX, ("name", "switch"))
        if let knob = matching(in: toggle, (255, 255, 255)) {
            let side = min(knob.minX - toggle.minX, toggle.maxX - knob.maxX)
            even("cheat \(name)'s knob sits evenly in its track", knob.minY - toggle.minY, toggle.maxY - knob.maxY, ("top", "bottom"))
            even("cheat \(name)'s knob is as far from the end as the edges", side, knob.minY - toggle.minY, ("end", "top"))
        }
    }
    let pair = [part("button:cancel"), part("button:erase")].compactMap { $0 }
    if pair.count == 2 {
        check(abs(pair[0].width - pair[1].width) <= tight && abs(pair[0].height - pair[1].height) <= tight,
              "Cancel and Erase & Restart are one size", "\(f(pair[0].width))x\(f(pair[0].height)) and \(f(pair[1].width))x\(f(pair[1].height))")
    }
}

// 8. Settings: names end on one line and share their control's midline;
//    each choice's label is centred on it; the chosen one's plate sits
//    evenly in its track; headings start where the controls do; and the
//    form is centred in the window.
let names = parts("name:")
if !names.isEmpty {
    let ends = names.compactMap { name, box in ink(in: box).map { (name, $0.maxX) } }
    if let first = ends.first {
        for (name, end) in ends.dropFirst() {
            even("\(name) ends where \(first.0) does", end, first.1, (name, first.0))
        }
    }
    var formLeft = Double.infinity, formRight = -Double.infinity
    for (name, box) in names {
        guard let track = part("track:\(name)") else { continue }
        let outer = track
        if let glyphs = ink(in: box), let letter = firstGlyph(in: box) {
            even("\(name)'s name sits on its control's midline", letter.midY, outer.midY, ("name", "control"), textTolerance)
            formLeft = min(formLeft, glyphs.minX)
        }
        formRight = max(formRight, outer.maxX)
        for (segmentName, segment) in parts("segment:\(name).") {
            let back = background(segment)
            guard let glyphs = ink(in: segment.insetBy(dx: 1, dy: 1), on: back),
                  let letter = firstGlyph(in: segment.insetBy(dx: 1, dy: 1), on: back) else { continue }
            centered("\(name) choice \(segmentName)", segment,
                     CGRect(x: glyphs.minX, y: letter.minY, width: glyphs.width, height: letter.height),
                     across: textTolerance, down: textTolerance)
            if !close(back, background(track), 12) {
                // Inside the track's 1pt edge.
                let inner = track.insetBy(dx: 1, dy: 1)
                even("\(name)'s chosen plate sits evenly in its track", segment.minY - inner.minY, inner.maxY - segment.maxY, ("top", "bottom"))
            }
        }
    }
    for (name, box) in parts("heading:") {
        guard let glyphs = ink(in: box), let firstTrack = parts("track:").first?.1 else { continue }
        even("\(name) heading starts where the controls do", glyphs.minX, firstTrack.minX, ("heading", "controls"), textTolerance)
    }
    if formLeft.isFinite {
        even("the form is centred in the window", formLeft, win.width - formRight, ("left", "right"), 2)
    }
}

// 9. The key guide, under the game menu's tiles: each key's letter
//    centred on its cap, and each row's keys ending where it does, so a
//    column's keys end on one line. What is scrolled out of the sheet's
//    view doesn't count.
if part("controls") != nil, let sheet = part("sheet") {
    let shown = sheet.insetBy(dx: 17, dy: 17)
    // In from the rounded corners, which show the sheet.
    let caps = parts("controls.cap:").filter { shown.contains($0.1) }.compactMap { name, cap -> (String, CGRect, CGRect)? in
        let inside = cap.insetBy(dx: 3, dy: 3)
        return ink(in: inside, on: background(inside)).map { (name, inside, $0) }
    }
    // Letters are centred down by their capitals: as tall as most keys'
    // ink, which a Q's tail or the p of Space runs under.
    let heights = Dictionary(grouping: caps.map { ($0.2.height * 2).rounded() / 2 }, by: { $0 })
    let capitals = heights.max { $0.value.count < $1.value.count }?.key ?? 0
    for (name, inside, glyphs) in caps {
        let letters = CGRect(x: glyphs.minX, y: glyphs.minY, width: glyphs.width, height: min(glyphs.height, capitals))
        centered("\(name) key", inside, letters, across: textTolerance, down: textTolerance)
    }
    for (name, row) in parts("controls.row:") where shown.contains(row) {
        if let last = parts("controls.cap:\(name).").map({ $0.1 }).max(by: { $0.maxX < $1.maxX }) {
            even("\(name)'s keys end where its row does", last.maxX, row.maxX, ("keys", "row"))
        }
    }
}

// 10. Notices and readouts: their words sit evenly inside their plates,
//     the same distance in from the window's sides, on one line. Under the
//     game menu, the sheet covers the readouts.
for name in ["notice", "speed", "rec"] {
    guard let box = part(name), part("sheet") == nil || name == "notice", let glyphs = ink(in: box) else { continue }
    let letter = firstGlyph(in: box) ?? glyphs
    centered("\(name)", box, CGRect(x: glyphs.minX, y: letter.minY, width: glyphs.width, height: letter.height),
             across: tight, down: textTolerance)
}

if let notice = part("notice"), let speed = part("speed"), part("sheet") == nil {
    even("notice and speed readout are as far from their sides", notice.minX, win.width - speed.maxX, ("notice", "readout"))
    even("notice and speed readout share a foot", notice.maxY, speed.maxY, ("notice", "readout"))
}

// 11. A game's menu: inside the window, as far off its foot as its sides;
//     each action's icon centred down its row and on one line with the
//     others, and the actions' names starting on one line.
if let menu = part("menu") {
    check(win.height - menu.maxY >= edgeClearance - tight, "game menu stays \(Int(edgeClearance))pt off the window's foot",
          "\(f(win.height - menu.maxY))pt off")
    var icons: [(String, CGRect)] = [], names: [(String, CGRect)] = []
    for (name, row) in parts("menu.action:") {
        let back = background(row)
        guard let icon = ink(in: CGRect(x: row.minX, y: row.minY, width: row.height, height: row.height).insetBy(dx: 1, dy: 1), on: back),
              let letter = firstGlyph(in: CGRect(x: row.minX + row.height, y: row.minY, width: row.width / 2, height: row.height), on: back) else {
            check(false, "menu \(name) shows its icon and name"); continue
        }
        even("menu \(name) icon is centred down its row", icon.midY, row.midY, ("icon", "row"))
        icons.append((name, icon))
        names.append((name, letter))
    }
    if let first = icons.first {
        for (name, icon) in icons.dropFirst() {
            even("menu \(name) icon is on one line with \(first.0)'s", icon.midX, first.1.midX, (name, first.0))
        }
    }
    if let first = names.first {
        for (name, letter) in names.dropFirst() {
            even("menu \(name) name starts where \(first.0)'s does", letter.minX, first.1.minX, (name, first.0), textTolerance)
        }
    }
}

print(failures == 0 ? "all checks passed" : "\(failures) checks failed")
exit(failures == 0 ? 0 : 1)
