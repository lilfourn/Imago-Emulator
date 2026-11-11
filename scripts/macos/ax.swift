// Drives one process without taking over the screen: its menus and window
// through the accessibility tree, and keys posted to that process alone, so
// it never moves the cursor, changes focus or types into another app.
// GPUI 0.2 exposes no element tree, only the window and the menu bar; the
// app's layout probe (DS_EMULATOR_PROBE) reports element frames instead.
//
// usage: ax <pid> tree                 print roles, names, actions, frames
//        ax <pid> press <name> [n]     press the nth menu item or control whose
//                                      name contains <name> ("=" prefix: exact)
//        ax <pid> key <key>            post a key to the process: escape,
//                                      return, left, right, up, down or a letter
//        ax <pid> size <w> <h> [title] resize the window with <title> in its
//        ax <pid> place [title]        title (default: the first) / move it clear
//        ax <pid> winid [title]        of the cursor / its number, for
//                                      `screencapture -l`
import ApplicationServices
import CoreGraphics
import Foundation

let args = CommandLine.arguments
let pid = Int32(args[1])!
let app = AXUIElementCreateApplication(pid)

func attr(_ e: AXUIElement, _ name: String) -> AnyObject? {
    var v: AnyObject?
    return AXUIElementCopyAttributeValue(e, name as CFString, &v) == .success ? v : nil
}
func str(_ e: AXUIElement, _ name: String) -> String? {
    guard let v = attr(e, name) else { return nil }
    if let s = v as? String { return s.isEmpty ? nil : s }
    if let n = v as? NSNumber { return n.stringValue }
    return nil
}
func actions(_ e: AXUIElement) -> [String] {
    var names: CFArray?
    return AXUIElementCopyActionNames(e, &names) == .success ? (names as? [String] ?? []) : []
}
func rect(_ e: AXUIElement) -> CGRect {
    var p = CGPoint.zero, s = CGSize.zero
    if let v = attr(e, kAXPositionAttribute) { AXValueGetValue(v as! AXValue, .cgPoint, &p) }
    if let v = attr(e, kAXSizeAttribute) { AXValueGetValue(v as! AXValue, .cgSize, &s) }
    return CGRect(origin: p, size: s)
}
func name(_ e: AXUIElement) -> String {
    [kAXTitleAttribute, kAXDescriptionAttribute, kAXValueAttribute].compactMap { str(e, $0) }
        .joined(separator: " | ")
}
func walk(_ e: AXUIElement, _ depth: Int, _ out: inout [(AXUIElement, Int)]) {
    out.append((e, depth))
    for child in attr(e, kAXChildrenAttribute) as? [AXUIElement] ?? [] { walk(child, depth + 1, &out) }
}
func nodes() -> [(AXUIElement, Int)] {
    var out: [(AXUIElement, Int)] = []
    walk(app, 0, &out)
    return out
}
func matches(_ needle: String, _ e: AXUIElement) -> Bool {
    needle.hasPrefix("=") ? name(e) == String(needle.dropFirst()) : name(e).contains(needle)
}
/// The window whose title contains `title`, or the first.
func window(_ title: String?) -> AXUIElement {
    let windows = attr(app, kAXWindowsAttribute) as? [AXUIElement] ?? []
    guard let window = windows.first(where: { title == nil || (str($0, kAXTitleAttribute) ?? "").contains(title!) })
    else { print("process \(pid) has no window \(title ?? "")"); exit(2) }
    return window
}

switch args[2] {
case "tree":
    for (e, depth) in nodes() {
        var parts = [str(e, kAXRoleAttribute) ?? "?"]
        let n = name(e)
        if !n.isEmpty { parts.append("\"\(n.prefix(100))\"") }
        if str(e, kAXEnabledAttribute) == "0" { parts.append("disabled") }
        let a = actions(e).filter { $0 != "AXShowMenu" && $0 != "AXScrollToVisible" }
        if !a.isEmpty { parts.append("[\(a.joined(separator: ","))]") }
        let r = rect(e)
        parts.append("@\(Int(r.minX)),\(Int(r.minY)) \(Int(r.width))x\(Int(r.height))")
        print(String(repeating: "  ", count: depth) + parts.joined(separator: " "))
    }
case "press":
    // The app's windows and its own menus, not the Apple menu, whose
    // System Settings… a search for "Settings…" would find.
    var roots = attr(app, kAXWindowsAttribute) as? [AXUIElement] ?? []
    if let bar = attr(app, kAXMenuBarAttribute) {
        roots += (attr(bar as! AXUIElement, kAXChildrenAttribute) as? [AXUIElement] ?? []).dropFirst()
    }
    var found: [(AXUIElement, Int)] = []
    for root in roots { walk(root, 0, &found) }
    let pressable = found.map { $0.0 }.filter { matches(args[3], $0) && actions($0).contains(kAXPressAction) }
    let nth = args.count > 4 ? Int(args[4])! : 0
    guard nth < pressable.count else { print("nothing pressable matches \(args[3])"); exit(2) }
    let result = AXUIElementPerformAction(pressable[nth], kAXPressAction as CFString)
    print("pressed \"\(name(pressable[nth]).prefix(80))\"", result == .success ? "" : "(error \(result.rawValue))")
case "key":
    // Virtual key codes from HIToolbox's Events.h (ANSI layout).
    let codes: [String: CGKeyCode] = [
        "escape": 53, "return": 36, "space": 49, "left": 123, "right": 124, "down": 125, "up": 126,
        "a": 0, "s": 1, "d": 2, "f": 3, "h": 4, "g": 5, "z": 6, "x": 7, "c": 8, "v": 9, "b": 11, "q": 12,
        "w": 13, "e": 14, "r": 15, "y": 16, "t": 17, "o": 31, "u": 32, "i": 34, "p": 35, "l": 37, "j": 38,
        "k": 40, "n": 45, "m": 46, ",": 43,
    ]
    // A key may follow modifiers, as in cmd-shift-g: menus can't be pressed
    // once the app has sat behind other windows, but their shortcuts work.
    var parts = args[3].split(separator: "-").map(String.init)
    let key = parts.removeLast()
    let modifiers: [String: CGEventFlags] = ["cmd": .maskCommand, "shift": .maskShift, "alt": .maskAlternate, "ctrl": .maskControl]
    var flags: CGEventFlags = []
    for part in parts {
        guard let flag = modifiers[part] else { print("unknown modifier \(part)"); exit(1) }
        flags.insert(flag)
    }
    guard let code = codes[key] else { print("unknown key \(key)"); exit(1) }
    // A private source and only the flags asked for, so a modifier the
    // user holds can't turn W into ⌘W.
    let source = CGEventSource(stateID: .privateState)
    for down in [true, false] {
        let event = CGEvent(keyboardEventSource: source, virtualKey: code, keyDown: down)!
        event.flags = flags
        event.postToPid(pid)
        usleep(30_000)
    }
    print("posted \(args[3])")
case "size":
    var size = CGSize(width: Double(args[3])!, height: Double(args[4])!)
    let target = window(args.count > 5 ? args[5] : nil)
    let result = AXUIElementSetAttributeValue(target, kAXSizeAttribute as CFString, AXValueCreate(.cgSize, &size)!)
    print(result == .success ? "window \(rect(target))" : "error \(result.rawValue)")
case "place":
    // Moves the window to the half of the main screen the cursor isn't on,
    // so the user's cursor doesn't hover anything in it.
    let target = window(args.count > 3 ? args[3] : nil)
    let screen = CGDisplayBounds(CGMainDisplayID())
    let cursor = CGEvent(source: nil)!.location
    let size = rect(target).size
    var origin = CGPoint(x: cursor.x < screen.midX ? screen.maxX - size.width - 40 : screen.minX + 40, y: screen.minY + 60)
    let result = AXUIElementSetAttributeValue(target, kAXPositionAttribute as CFString, AXValueCreate(.cgPoint, &origin)!)
    print(result == .success ? "window \(rect(target))" : "error \(result.rawValue)")
case "winid":
    let target = rect(window(args.count > 3 ? args[3] : nil))
    let windows = CGWindowListCopyWindowInfo([.optionAll], kCGNullWindowID) as! [[String: Any]]
    let match = windows.first { info in
        guard info[kCGWindowOwnerPID as String] as? Int32 == pid, info[kCGWindowLayer as String] as? Int == 0,
              let b = info[kCGWindowBounds as String] as? [String: Double] else { return false }
        return abs(b["X"]! - target.minX) <= 1 && abs(b["Y"]! - target.minY) <= 1
            && abs(b["Width"]! - target.width) <= 1 && abs(b["Height"]! - target.height) <= 1
    }
    guard let id = match?[kCGWindowNumber as String] as? Int else { print("no window matches \(target)"); exit(2) }
    print(id)
default:
    print("unknown command \(args[2])")
    exit(1)
}
