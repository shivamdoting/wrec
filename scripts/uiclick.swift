// Tiny CGEvent driver for UI testing. Compile once:
//   xcrun swiftc -O -o /tmp/uiclick scripts/uiclick.swift
// Usage (coordinates are CG/global, top-left origin):
//   uiclick click <x> <y>
//   uiclick move <x> <y>
//   uiclick key <keycode>        (e.g. 53 = esc)
import CoreGraphics
import Foundation

let args = CommandLine.arguments
guard args.count >= 2 else {
    FileHandle.standardError.write(Data("usage: uiclick click|move|key ...\n".utf8))
    exit(64)
}

func post(_ event: CGEvent?) {
    event?.post(tap: .cghidEventTap)
    usleep(60_000)
}

switch args[1] {
case "click", "move":
    guard args.count == 4, let x = Double(args[2]), let y = Double(args[3]) else { exit(64) }
    let point = CGPoint(x: x, y: y)
    post(CGEvent(mouseEventSource: nil, mouseType: .mouseMoved, mouseCursorPosition: point, mouseButton: .left))
    if args[1] == "click" {
        post(CGEvent(mouseEventSource: nil, mouseType: .leftMouseDown, mouseCursorPosition: point, mouseButton: .left))
        post(CGEvent(mouseEventSource: nil, mouseType: .leftMouseUp, mouseCursorPosition: point, mouseButton: .left))
    }
case "key":
    guard args.count == 3, let code = UInt16(args[2]) else { exit(64) }
    post(CGEvent(keyboardEventSource: nil, virtualKey: code, keyDown: true))
    post(CGEvent(keyboardEventSource: nil, virtualKey: code, keyDown: false))
default:
    exit(64)
}
