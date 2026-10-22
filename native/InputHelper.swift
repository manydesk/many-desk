// ManyDesk input helper.
// Reads newline-delimited JSON commands from stdin and injects them as
// macOS mouse / keyboard events through CoreGraphics.
// Coordinates are global display points (same space as Electron's screen API).
//
// Commands:
//   {"t":"move","x":100,"y":200}
//   {"t":"down","b":0,"x":100,"y":200,"c":1}   b: 0 left, 1 middle, 2 right; c: click count
//   {"t":"up","b":0,"x":100,"y":200,"c":1}
//   {"t":"scroll","dx":0,"dy":-40}               pixels, browser sign convention
//   {"t":"key","k":0,"d":true}                   k: mac virtual keycode, d: down?
//   {"t":"text","s":"héllo"}                     type text as is (keyboard-layout independent)
//   {"t":"block","on":true}                      privacy mode: drop the local keyboard/mouse
//                                                (only events posted here get through)
//   {"t":"release"}                              release every held key/button
//   {"t":"ping"}

import Foundation
import CoreGraphics
import ApplicationServices

let source = CGEventSource(stateID: .hidSystemState)
var pressedButtons = Set<Int>()
var pressedKeys = Set<UInt16>()
var flags: CGEventFlags = []
var lastPoint = CGPoint.zero
// Marks our own events, so the privacy-mode tap lets exactly those through.
let ownMark: Int64 = 0x4D44_4B21 // "MDK!"
var blockTap: CFMachPort?
var blockSource: CFRunLoopSource?

let modifierFlags: [UInt16: CGEventFlags] = [
    55: .maskCommand, 54: .maskCommand,
    56: .maskShift, 60: .maskShift,
    58: .maskAlternate, 61: .maskAlternate,
    59: .maskControl, 62: .maskControl,
    63: .maskSecondaryFn,
]

func reply(_ s: String) {
    FileHandle.standardOutput.write((s + "\n").data(using: .utf8)!)
}

func post(_ e: CGEvent?, modifiers: Bool = true) {
    guard let e = e else { return }
    if modifiers {
        e.flags = e.flags.union(flags)
    }
    e.setIntegerValueField(.eventSourceUserData, value: ownMark)
    e.post(tap: .cghidEventTap)
}

func typeText(_ s: String) {
    // CGEvent carries at most 20 UTF-16 units per event.
    var units = Array(s.utf16)
    while !units.isEmpty {
        let chunk = Array(units.prefix(20))
        units.removeFirst(chunk.count)
        for down in [true, false] {
            let e = CGEvent(keyboardEventSource: source, virtualKey: 0, keyDown: down)
            e?.flags = []
            chunk.withUnsafeBufferPointer { e?.keyboardSetUnicodeString(stringLength: chunk.count, unicodeString: $0.baseAddress) }
            post(e, modifiers: false)
        }
    }
}

let tapCallback: CGEventTapCallBack = { _, type, event, _ in
    if type == .tapDisabledByTimeout || type == .tapDisabledByUserInput {
        if let tap = blockTap { CGEvent.tapEnable(tap: tap, enable: true) }
        return Unmanaged.passUnretained(event)
    }
    return event.getIntegerValueField(.eventSourceUserData) == ownMark ? Unmanaged.passUnretained(event) : nil
}

func setBlock(_ on: Bool) {
    DispatchQueue.main.async {
        if on, blockTap == nil {
            let types: [CGEventType] = [.keyDown, .keyUp, .flagsChanged, .leftMouseDown, .leftMouseUp, .rightMouseDown, .rightMouseUp,
                                        .otherMouseDown, .otherMouseUp, .mouseMoved, .leftMouseDragged, .rightMouseDragged, .otherMouseDragged, .scrollWheel]
            let mask = types.reduce(CGEventMask(0)) { $0 | (CGEventMask(1) << $1.rawValue) }
            guard let tap = CGEvent.tapCreate(tap: .cghidEventTap, place: .headInsertEventTap, options: .defaultTap, eventsOfInterest: mask, callback: tapCallback, userInfo: nil) else {
                reply("{\"t\":\"block\",\"on\":false,\"e\":\"tap\"}")
                return
            }
            blockTap = tap
            blockSource = CFMachPortCreateRunLoopSource(kCFAllocatorDefault, tap, 0)
            CFRunLoopAddSource(CFRunLoopGetMain(), blockSource, .commonModes)
            CGEvent.tapEnable(tap: tap, enable: true)
        } else if !on, let tap = blockTap {
            CGEvent.tapEnable(tap: tap, enable: false)
            if let src = blockSource { CFRunLoopRemoveSource(CFRunLoopGetMain(), src, .commonModes) }
            blockTap = nil
            blockSource = nil
        }
        reply("{\"t\":\"block\",\"on\":\(blockTap != nil)}")
    }
}

func buttonInfo(_ b: Int) -> (CGMouseButton, CGEventType, CGEventType, CGEventType) {
    switch b {
    case 2: return (.right, .rightMouseDown, .rightMouseUp, .rightMouseDragged)
    case 1: return (.center, .otherMouseDown, .otherMouseUp, .otherMouseDragged)
    default: return (.left, .leftMouseDown, .leftMouseUp, .leftMouseDragged)
    }
}

func point(_ m: [String: Any]) -> CGPoint {
    if let x = m["x"] as? Double, let y = m["y"] as? Double {
        lastPoint = CGPoint(x: x, y: y)
    }
    return lastPoint
}

func handle(_ m: [String: Any]) {
    guard let t = m["t"] as? String else { return }
    switch t {
    case "move":
        let p = point(m)
        if let b = pressedButtons.min() {
            let (btn, _, _, drag) = buttonInfo(b)
            post(CGEvent(mouseEventSource: source, mouseType: drag, mouseCursorPosition: p, mouseButton: btn))
        } else {
            post(CGEvent(mouseEventSource: source, mouseType: .mouseMoved, mouseCursorPosition: p, mouseButton: .left))
        }
    case "down", "up":
        let p = point(m)
        let b = m["b"] as? Int ?? 0
        let (btn, downT, upT, _) = buttonInfo(b)
        let isDown = t == "down"
        if isDown { pressedButtons.insert(b) } else { pressedButtons.remove(b) }
        let e = CGEvent(mouseEventSource: source, mouseType: isDown ? downT : upT, mouseCursorPosition: p, mouseButton: btn)
        e?.setIntegerValueField(.mouseEventClickState, value: Int64(max(1, m["c"] as? Int ?? 1)))
        post(e)
    case "scroll":
        let dx = Int32(-(m["dx"] as? Double ?? 0))
        let dy = Int32(-(m["dy"] as? Double ?? 0))
        post(CGEvent(scrollWheelEvent2Source: source, units: .pixel, wheelCount: 2, wheel1: dy, wheel2: dx, wheel3: 0))
    case "key":
        guard let k = m["k"] as? Int else { return }
        let code = UInt16(k)
        let down = m["d"] as? Bool ?? true
        if let f = modifierFlags[code] {
            if down { flags.insert(f) } else { flags.remove(f) }
            let e = CGEvent(keyboardEventSource: source, virtualKey: code, keyDown: down)
            e?.type = .flagsChanged
            e?.flags = flags
            post(e, modifiers: false)
        } else {
            post(CGEvent(keyboardEventSource: source, virtualKey: code, keyDown: down))
        }
        if down { pressedKeys.insert(code) } else { pressedKeys.remove(code) }
    case "release":
        for code in pressedKeys {
            post(CGEvent(keyboardEventSource: source, virtualKey: code, keyDown: false))
        }
        pressedKeys.removeAll()
        flags = []
        for b in pressedButtons {
            let (btn, _, upT, _) = buttonInfo(b)
            post(CGEvent(mouseEventSource: source, mouseType: upT, mouseCursorPosition: lastPoint, mouseButton: btn))
        }
        pressedButtons.removeAll()
    case "text":
        if let str = m["s"] as? String { typeText(str) }
    case "block":
        setBlock(m["on"] as? Bool ?? false)
    case "ping":
        reply("{\"t\":\"pong\",\"trusted\":\(AXIsProcessTrusted())}")
    default:
        break
    }
}

setvbuf(stdout, nil, _IOLBF, 0)
reply("{\"t\":\"ready\",\"trusted\":\(AXIsProcessTrusted())}")

// Commands come in on a reader thread; the main run loop serves the privacy-mode tap.
// End of input (ManyDesk quit or the session ended) lifts the block with the process.
Thread {
    while let line = readLine(strippingNewline: true) {
        guard let data = line.data(using: .utf8),
              let obj = try? JSONSerialization.jsonObject(with: data) as? [String: Any] else { continue }
        handle(obj)
    }
    exit(0)
}.start()
CFRunLoopRun()
