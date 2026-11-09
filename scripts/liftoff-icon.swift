import AppKit
let out = CommandLine.arguments[1]
try? FileManager.default.createDirectory(atPath: out, withIntermediateDirectories: true)
func draw(_ px: Int) -> NSBitmapImageRep {
  let rep = NSBitmapImageRep(bitmapDataPlanes: nil, pixelsWide: px, pixelsHigh: px, bitsPerSample: 8, samplesPerPixel: 4, hasAlpha: true, isPlanar: false, colorSpaceName: .deviceRGB, bytesPerRow: 0, bitsPerPixel: 0)!
  NSGraphicsContext.saveGraphicsState()
  NSGraphicsContext.current = NSGraphicsContext(bitmapImageRep: rep)
  let s = CGFloat(px), m = s * 0.1, b = s - 2 * m
  let body = NSBezierPath(roundedRect: NSRect(x: m, y: m, width: b, height: b), xRadius: b * 0.225, yRadius: b * 0.225)
  NSGradient(colors: [NSColor(red: 0.98, green: 0.36, blue: 0.55, alpha: 1), NSColor(red: 0.45, green: 0.30, blue: 0.95, alpha: 1), NSColor(red: 0.12, green: 0.55, blue: 1.0, alpha: 1)])!.draw(in: body, angle: -60)
  for (i, a) in [0.45, 0.7, 1.0].enumerated() {
    let y = m + b * (0.26 + CGFloat(i) * 0.17), w = b * 0.46, h = b * 0.17
    let p = NSBezierPath()
    p.move(to: NSPoint(x: s / 2 - w / 2, y: y))
    p.line(to: NSPoint(x: s / 2, y: y + h))
    p.line(to: NSPoint(x: s / 2 + w / 2, y: y))
    p.lineWidth = b * 0.075; p.lineCapStyle = .round; p.lineJoinStyle = .round
    NSColor(white: 1, alpha: a).setStroke(); p.stroke()
  }
  NSGraphicsContext.restoreGraphicsState()
  return rep
}
for (pt, sc) in [(16,1),(16,2),(32,1),(32,2),(128,1),(128,2),(256,1),(256,2),(512,1),(512,2)] {
  let n = sc == 1 ? "icon_\(pt)x\(pt).png" : "icon_\(pt)x\(pt)@2x.png"
  try! draw(pt * sc).representation(using: .png, properties: [:])!.write(to: URL(fileURLWithPath: "\(out)/\(n)"))
}
