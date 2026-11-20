// Draws Tern's app icon, 1024 pixels square, into the PNG file named on
// the command line: a dark rounded square with a prompt on it.
// Usage: swift scripts/icon.swift <out.png> [--next]
import AppKit

let size: CGFloat = 1024
let args = CommandLine.arguments
let isNext = args.count == 3 && args[2] == "--next"
guard args.count == 2 || isNext else {
    FileHandle.standardError.write("usage: icon.swift <out.png> [--next]\n".data(using: .utf8)!)
    exit(2)
}
let out = URL(fileURLWithPath: args[1])

guard
    let colorSpace = CGColorSpace(name: CGColorSpace.sRGB),
    let ctx = CGContext(
        data: nil,
        width: Int(size),
        height: Int(size),
        bitsPerComponent: 8,
        bytesPerRow: 0,
        space: colorSpace,
        bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue
    )
else { exit(1) }
NSGraphicsContext.saveGraphicsState()
NSGraphicsContext.current = NSGraphicsContext(cgContext: ctx, flipped: false)

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

// The tile: charcoal for stable, warm brown for Next.
let top = isNext
    ? NSColor(srgbRed: 0.25, green: 0.17, blue: 0.12, alpha: 1)
    : NSColor(srgbRed: 0.17, green: 0.19, blue: 0.22, alpha: 1)
let bottom = isNext
    ? NSColor(srgbRed: 0.12, green: 0.08, blue: 0.06, alpha: 1)
    : NSColor(srgbRed: 0.08, green: 0.09, blue: 0.11, alpha: 1)
NSGradient(starting: top, ending: bottom)?.draw(in: shape, angle: -90)

// A hairline edge, so the tile holds on a dark Dock.
NSColor(white: 1, alpha: 0.10).setStroke()
let edge = NSBezierPath(roundedRect: tile.insetBy(dx: 2, dy: 2), xRadius: radius - 2, yRadius: radius - 2)
edge.lineWidth = 4
edge.stroke()

// The prompt: a blue or amber chevron and a white cursor block.
let accent = isNext
    ? NSColor(srgbRed: 1, green: 0.68, blue: 0.32, alpha: 1)
    : NSColor(srgbRed: 0.38, green: 0.69, blue: 0.94, alpha: 1)
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
guard let image = ctx.makeImage() else { exit(1) }
let rep = NSBitmapImageRep(cgImage: image)
guard let png = rep.representation(using: .png, properties: [:]) else { exit(1) }
do {
    try png.write(to: out)
} catch {
    FileHandle.standardError.write("cannot write \(out.path): \(error)\n".data(using: .utf8)!)
    exit(1)
}
