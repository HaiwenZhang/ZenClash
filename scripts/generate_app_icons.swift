import AppKit
import Foundation

enum IconGenerationError: Error, CustomStringConvertible {
    case invalidSource(URL)
    case bitmapCreationFailed(Int)
    case pngEncodingFailed(Int)

    var description: String {
        switch self {
        case .invalidSource(let url):
            return "Cannot load SVG source at \(url.path)"
        case .bitmapCreationFailed(let size):
            return "Cannot create a \(size)x\(size) bitmap"
        case .pngEncodingFailed(let size):
            return "Cannot encode the \(size)x\(size) PNG"
        }
    }
}

let fileManager = FileManager.default
let scriptURL = URL(fileURLWithPath: #filePath).standardizedFileURL
let projectRoot = scriptURL.deletingLastPathComponent().deletingLastPathComponent()
let sourceURL = projectRoot.appendingPathComponent("platforms/branding/zenclash-app-icon.svg")
let macOSDirectory = projectRoot.appendingPathComponent("platforms/macos")
let windowsDirectory = projectRoot.appendingPathComponent("platforms/windows")
let sourceImage = NSImage(contentsOf: sourceURL)

guard let sourceImage else {
    throw IconGenerationError.invalidSource(sourceURL)
}

func pngData(size: Int) throws -> Data {
    guard let bitmap = NSBitmapImageRep(
        bitmapDataPlanes: nil,
        pixelsWide: size,
        pixelsHigh: size,
        bitsPerSample: 8,
        samplesPerPixel: 4,
        hasAlpha: true,
        isPlanar: false,
        colorSpaceName: .deviceRGB,
        bytesPerRow: 0,
        bitsPerPixel: 0
    ) else {
        throw IconGenerationError.bitmapCreationFailed(size)
    }

    bitmap.size = NSSize(width: size, height: size)
    guard let context = NSGraphicsContext(bitmapImageRep: bitmap) else {
        throw IconGenerationError.bitmapCreationFailed(size)
    }

    NSGraphicsContext.saveGraphicsState()
    NSGraphicsContext.current = context
    context.imageInterpolation = .high
    NSColor.clear.setFill()
    NSRect(x: 0, y: 0, width: size, height: size).fill()
    sourceImage.draw(
        in: NSRect(x: 0, y: 0, width: size, height: size),
        from: .zero,
        operation: .copy,
        fraction: 1
    )
    context.flushGraphics()
    NSGraphicsContext.restoreGraphicsState()

    guard let data = bitmap.representation(using: .png, properties: [:]) else {
        throw IconGenerationError.pngEncodingFailed(size)
    }
    return data
}

func appendLittleEndian<T: FixedWidthInteger>(_ value: T, to data: inout Data) {
    var littleEndian = value.littleEndian
    withUnsafeBytes(of: &littleEndian) { data.append(contentsOf: $0) }
}

func appendBigEndian<T: FixedWidthInteger>(_ value: T, to data: inout Data) {
    var bigEndian = value.bigEndian
    withUnsafeBytes(of: &bigEndian) { data.append(contentsOf: $0) }
}

try fileManager.createDirectory(at: macOSDirectory, withIntermediateDirectories: true)
try fileManager.createDirectory(at: windowsDirectory, withIntermediateDirectories: true)

let macOSPNG = macOSDirectory.appendingPathComponent("ZenClash.png")
try pngData(size: 1024).write(to: macOSPNG, options: .atomic)

let icnsEntries: [(String, Int)] = [
    ("icp4", 16),
    ("icp5", 32),
    ("icp6", 64),
    ("ic07", 128),
    ("ic08", 256),
    ("ic09", 512),
    ("ic10", 1024),
]

var icnsPayload = Data()
for (type, size) in icnsEntries {
    let png = try pngData(size: size)
    icnsPayload.append(type.data(using: .ascii)!)
    appendBigEndian(UInt32(png.count + 8), to: &icnsPayload)
    icnsPayload.append(png)
}

var icns = Data("icns".utf8)
appendBigEndian(UInt32(icnsPayload.count + 8), to: &icns)
icns.append(icnsPayload)
try icns.write(
    to: macOSDirectory.appendingPathComponent("ZenClash.icns"),
    options: .atomic
)

let windowsPNG = try pngData(size: 256)
var ico = Data()
appendLittleEndian(UInt16(0), to: &ico)
appendLittleEndian(UInt16(1), to: &ico)
appendLittleEndian(UInt16(1), to: &ico)
ico.append(contentsOf: [0, 0, 0, 0])
appendLittleEndian(UInt16(1), to: &ico)
appendLittleEndian(UInt16(32), to: &ico)
appendLittleEndian(UInt32(windowsPNG.count), to: &ico)
appendLittleEndian(UInt32(22), to: &ico)
ico.append(windowsPNG)
try ico.write(to: windowsDirectory.appendingPathComponent("ZenClash.ico"), options: .atomic)

print("Generated ZenClash PNG, ICNS, and ICO assets.")
