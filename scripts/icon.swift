// Draws Tern's app icon, 1024 pixels square, into the PNG file named on
// the command line: a dark rounded square with a prompt on it.
// Usage: swift scripts/icon.swift <out.png>
import AppKit

let size: CGFloat = 1024
guard CommandLine.arguments.count == 2 else {
    FileHandle.standardError.write("usage: icon.swift <out.png>\n".data(using: .utf8)!)
    exit(2)
}
let out = URL(fileURLWithPath: CommandLine.arguments[1])

guard let rep = NSBitmapImageRep(
    bitmapDataPlanes: nil, pixelsWide: Int(size), pixelsHigh: Int(size),
    bitsPerSample: 8, samplesPerPixel: 4, hasAlpha: true, isPlanar: false,
    colorSpaceName: .deviceRGB, bytesPerRow: 0, bitsPerPixel: 0)
else { exit(1) }
NSGraphicsContext.saveGraphicsState()
NSGraphicsContext.current = NSGraphicsContext(bitmapImageRep: rep)

// The shape macOS icons use: a rounded square inset from the canvas.
let inset: CGFloat = 100
let tile = NSRect(x: inset, y: inset, width: size - 2 * inset, height: size - 2 * inset)
let radius: CGFloat = 185
let shape = NSBezierPath(roundedRect: tile, xRadius: radius, yRadius: radius)

// A soft shadow under the tile.
NSGraphicsContext.saveGraphicsState()
let shadow = NSShadow()
shadow.shadowColor = NSColor(white: 0, alpha: 0.35)
shadow.shadowOffset = NSSize(width: 0, height: -12)
shadow.shadowBlurRadius = 28
shadow.set()
NSColor(srgbRed: 0.09, green: 0.10, blue: 0.12, alpha: 1).setFill()
shape.fill()
NSGraphicsContext.restoreGraphicsState()

// The tile: charcoal, a little lighter at the top.
NSGradient(
    starting: NSColor(srgbRed: 0.17, green: 0.19, blue: 0.22, alpha: 1),
    ending: NSColor(srgbRed: 0.08, green: 0.09, blue: 0.11, alpha: 1)
)?.draw(in: shape, angle: -90)

// A hairline edge, so the tile holds on a dark Dock.
NSColor(white: 1, alpha: 0.10).setStroke()
let edge = NSBezierPath(roundedRect: tile.insetBy(dx: 2, dy: 2), xRadius: radius - 2, yRadius: radius - 2)
edge.lineWidth = 4
edge.stroke()

// The prompt: a chevron and a cursor block, in the terminal's blue.
let accent = NSColor(srgbRed: 0.38, green: 0.69, blue: 0.94, alpha: 1)
let chevron = NSBezierPath()
chevron.move(to: NSPoint(x: 300, y: 640))
chevron.line(to: NSPoint(x: 470, y: 512))
chevron.line(to: NSPoint(x: 300, y: 384))
chevron.lineWidth = 64
chevron.lineCapStyle = .round
chevron.lineJoinStyle = .round
accent.setStroke()
chevron.stroke()
NSColor(srgbRed: 0.86, green: 0.87, blue: 0.89, alpha: 1).setFill()
NSBezierPath(roundedRect: NSRect(x: 540, y: 352, width: 200, height: 64), xRadius: 14, yRadius: 14).fill()

NSGraphicsContext.restoreGraphicsState()
guard let png = rep.representation(using: .png, properties: [:]) else { exit(1) }
do {
    try png.write(to: out)
} catch {
    FileHandle.standardError.write("cannot write \(out.path): \(error)\n".data(using: .utf8)!)
    exit(1)
}
