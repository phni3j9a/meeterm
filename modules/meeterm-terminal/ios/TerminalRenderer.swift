import CoreGraphics
import CoreText
import Foundation
import Metal
import MetalKit
import UIKit

private let terminalShaderSource = """
#include <metal_stdlib>
using namespace metal;

struct TerminalRasterData {
  float4 position [[position]];
  float2 textureCoordinate;
};

vertex TerminalRasterData terminal_vertex(uint vertexId [[vertex_id]]) {
  constexpr float2 positions[] = {
    float2(-1.0, -1.0),
    float2( 1.0, -1.0),
    float2(-1.0,  1.0),
    float2( 1.0,  1.0)
  };
  // Core Graphics rasterizes into bottom-up bitmap rows, while the fullscreen
  // Metal quad's upper vertices must sample the first visible terminal row.
  // Flip only the texture Y axis so glyphs remain upright on screen.
  constexpr float2 coordinates[] = {
    float2(0.0, 1.0),
    float2(1.0, 1.0),
    float2(0.0, 0.0),
    float2(1.0, 0.0)
  };

  TerminalRasterData output;
  output.position = float4(positions[vertexId], 0.0, 1.0);
  output.textureCoordinate = coordinates[vertexId];
  return output;
}

fragment half4 terminal_fragment(
  TerminalRasterData input [[stage_in]],
  texture2d<half> terminalTexture [[texture(0)]]) {
  constexpr sampler terminalSampler(
    address::clamp_to_edge,
    min_filter::nearest,
    mag_filter::nearest
  );
  return terminalTexture.sample(terminalSampler, input.textureCoordinate);
}
"""

enum TerminalRendererError: Error {
  case shaderLibrary
  case shaderFunction
  case renderPipeline
  case commandQueue
}

protocol TerminalFrameRendering: AnyObject {
  func attachTerminal(_ handle: UInt64)
  func setPreedit(_ value: String)
  func requestFrame()
}

/// A demand-driven Metal renderer. Rust owns the terminal grid; this object
/// pulls a native snapshot, rasterizes terminal cells with CoreText, uploads
/// the resulting native texture, and submits it directly to the GPU.
final class TerminalRenderer: NSObject, MTKViewDelegate, TerminalFrameRendering {
  static let cellWidthPoints: CGFloat = 9
  static let cellHeightPoints: CGFloat = 20
  fileprivate static let fontSizePoints: CGFloat = 15

  private let device: MTLDevice
  private let commandQueue: MTLCommandQueue
  private let pipeline: MTLRenderPipelineState
  private var terminalHandle: UInt64 = 0
  private var preedit = ""
  private var emittedFirstFrameMarker = false
  weak var view: MTKView?

  init(device: MTLDevice, colorPixelFormat: MTLPixelFormat) throws {
    self.device = device
    guard let commandQueue = device.makeCommandQueue() else {
      throw TerminalRendererError.commandQueue
    }
    self.commandQueue = commandQueue

    guard let library = try? device.makeLibrary(source: terminalShaderSource, options: nil) else {
      throw TerminalRendererError.shaderLibrary
    }
    guard let vertex = library.makeFunction(name: "terminal_vertex"),
          let fragment = library.makeFunction(name: "terminal_fragment") else {
      throw TerminalRendererError.shaderFunction
    }

    let descriptor = MTLRenderPipelineDescriptor()
    descriptor.label = "meeterm terminal texture pipeline"
    descriptor.vertexFunction = vertex
    descriptor.fragmentFunction = fragment
    descriptor.colorAttachments[0].pixelFormat = colorPixelFormat
    guard let pipeline = try? device.makeRenderPipelineState(descriptor: descriptor) else {
      throw TerminalRendererError.renderPipeline
    }
    self.pipeline = pipeline
    super.init()
  }

  func attachTerminal(_ handle: UInt64) {
    terminalHandle = handle
    requestFrame()
  }

  func setPreedit(_ value: String) {
    guard value != preedit else {
      return
    }
    preedit = value
    requestFrame()
  }

  func requestFrame() {
    view?.setNeedsDisplay()
  }

  func mtkView(_ view: MTKView, drawableSizeWillChange size: CGSize) {
    guard size.width > 0, size.height > 0 else {
      return
    }
    view.setNeedsDisplay()
  }

  func draw(in view: MTKView) {
    autoreleasepool {
      guard terminalHandle != 0,
            let snapshotData = MeetermCore.snapshot(terminalId: terminalHandle),
            let snapshot = TerminalSnapshotParser.parse(snapshotData),
            let drawable = view.currentDrawable,
            let renderPass = view.currentRenderPassDescriptor,
            let texture = makeTexture(for: snapshot, in: view),
            let commandBuffer = commandQueue.makeCommandBuffer(),
            let encoder = commandBuffer.makeRenderCommandEncoder(descriptor: renderPass) else {
        return
      }

      commandBuffer.label = "meeterm terminal frame"
      encoder.label = "meeterm terminal frame encoder"
      encoder.setRenderPipelineState(pipeline)
      encoder.setFragmentTexture(texture, index: 0)
      encoder.drawPrimitives(type: .triangleStrip, vertexStart: 0, vertexCount: 4)
      encoder.endEncoding()
      commandBuffer.present(drawable)
      commandBuffer.commit()

      if !emittedFirstFrameMarker {
        emittedFirstFrameMarker = true
        NSLog("MEETERM_SMOKE_FIRST_FRAME_METAL")
      }
    }
  }

  private func makeTexture(for snapshot: TerminalSnapshot, in view: MTKView) -> MTLTexture? {
    let width = Int(view.drawableSize.width.rounded(.down))
    let height = Int(view.drawableSize.height.rounded(.down))
    guard width > 0, height > 0 else {
      return nil
    }

    let scale = view.bounds.width > 0
      ? CGFloat(view.drawableSize.width) / view.bounds.width
      : view.contentScaleFactor
    guard let pixels = TerminalRasterizer.rasterize(
      snapshot: snapshot,
      preedit: preedit,
      width: width,
      height: height,
      scale: max(1, scale)
    ) else {
      return nil
    }

    let descriptor = MTLTextureDescriptor.texture2DDescriptor(
      pixelFormat: .bgra8Unorm,
      width: width,
      height: height,
      mipmapped: false
    )
    descriptor.storageMode = .shared
    descriptor.usage = [.shaderRead]
    guard let texture = device.makeTexture(descriptor: descriptor) else {
      return nil
    }
    texture.label = "meeterm terminal raster"

    pixels.withUnsafeBufferPointer { buffer in
      guard let baseAddress = buffer.baseAddress else {
        return
      }
      texture.replace(
        region: MTLRegionMake2D(0, 0, width, height),
        mipmapLevel: 0,
        withBytes: baseAddress,
        bytesPerRow: width * 4
      )
    }
    return texture
  }
}

#if targetEnvironment(simulator)
/// GitHub's standard Intel macOS runner does not guarantee a Metal device.
/// Keep the simulator smoke path native by drawing the same Rust snapshot and
/// CoreText raster into a UIView when Metal is unavailable.
final class TerminalSoftwareView: UIView, TerminalFrameRendering {
  private var terminalHandle: UInt64 = 0
  private var preedit = ""
  private var emittedFirstFrameMarker = false

  override init(frame: CGRect) {
    super.init(frame: frame)
    isOpaque = true
    contentMode = .redraw
    NSLog("MEETERM_RENDERER_SOFTWARE_FALLBACK")
  }

  @available(*, unavailable)
  required init?(coder: NSCoder) {
    fatalError("init(coder:) has not been implemented")
  }

  func attachTerminal(_ handle: UInt64) {
    terminalHandle = handle
    requestFrame()
  }

  func setPreedit(_ value: String) {
    guard value != preedit else {
      return
    }
    preedit = value
    requestFrame()
  }

  func requestFrame() {
    setNeedsDisplay()
  }

  override func draw(_ rect: CGRect) {
    autoreleasepool {
      let scale = max(1, window?.screen.scale ?? contentScaleFactor)
      let width = Int((bounds.width * scale).rounded(.up))
      let height = Int((bounds.height * scale).rounded(.up))
      guard terminalHandle != 0,
            width > 0,
            height > 0,
            let snapshotData = MeetermCore.snapshot(terminalId: terminalHandle),
            let snapshot = TerminalSnapshotParser.parse(snapshotData),
            let pixels = TerminalRasterizer.rasterize(
              snapshot: snapshot,
              preedit: preedit,
              width: width,
              height: height,
              scale: scale
            ),
            let image = TerminalRasterizer.makeImage(
              pixels: pixels,
              width: width,
              height: height
            ),
            let context = UIGraphicsGetCurrentContext() else {
        return
      }

      context.saveGState()
      context.interpolationQuality = .none
      context.translateBy(x: 0, y: bounds.height)
      context.scaleBy(x: 1, y: -1)
      context.draw(image, in: bounds)
      context.restoreGState()

      if !emittedFirstFrameMarker {
        emittedFirstFrameMarker = true
        NSLog("MEETERM_SMOKE_FIRST_FRAME_SOFTWARE")
      }
    }
  }
}
#endif

private enum TerminalRasterizer {
  private static let background = TerminalColor(red: 9, green: 11, blue: 15, alpha: 255)
  private static let preeditColor = TerminalColor(red: 255, green: 201, blue: 92, alpha: 255)
  private static let cursorColor = TerminalColor(red: 197, green: 212, blue: 236, alpha: 255)
  private static let underlineMask = terminalFlagUnderline
    | terminalFlagDoubleUnderline
    | terminalFlagUndercurl
    | terminalFlagDottedUnderline
    | terminalFlagDashedUnderline

  static func rasterize(
    snapshot: TerminalSnapshot,
    preedit: String,
    width: Int,
    height: Int,
    scale: CGFloat
  ) -> [UInt8]? {
    let byteCount = width.multipliedReportingOverflow(by: height)
    guard !byteCount.overflow else {
      return nil
    }
    let rgbaCount = byteCount.partialValue.multipliedReportingOverflow(by: 4)
    guard !rgbaCount.overflow else {
      return nil
    }

    var pixels = [UInt8](repeating: 0, count: rgbaCount.partialValue)
    let rendered = pixels.withUnsafeMutableBufferPointer { buffer -> Bool in
      guard let baseAddress = buffer.baseAddress,
            let context = CGContext(
              data: UnsafeMutableRawPointer(baseAddress),
              width: width,
              height: height,
              bitsPerComponent: 8,
              bytesPerRow: width * 4,
              space: CGColorSpaceCreateDeviceRGB(),
              bitmapInfo: CGImageAlphaInfo.premultipliedFirst.rawValue
                | CGBitmapInfo.byteOrder32Little.rawValue
            ) else {
        return false
      }

      context.setShouldAntialias(true)
      context.setAllowsAntialiasing(true)
      context.setFillColor(background.cgColor)
      context.fill(CGRect(x: 0, y: 0, width: CGFloat(width), height: CGFloat(height)))

      let cellWidth = max(1, Int((TerminalRenderer.cellWidthPoints * scale).rounded()))
      let cellHeight = max(1, Int((TerminalRenderer.cellHeightPoints * scale).rounded()))
      let fontSize = TerminalRenderer.fontSizePoints * scale
      let regularFont = UIFont.monospacedSystemFont(ofSize: fontSize, weight: .regular)
      let boldFont = UIFont.monospacedSystemFont(ofSize: fontSize, weight: .bold)

      for cell in snapshot.cells {
        let rect = cellRect(
          row: cell.row,
          column: cell.column,
          cellsWide: cell.width,
          cellWidth: cellWidth,
          cellHeight: cellHeight,
          canvasHeight: height
        )
        guard rect.maxX > 0, rect.minX < CGFloat(width),
              rect.maxY > 0, rect.minY < CGFloat(height) else {
          continue
        }

        let inverted = cell.flags & terminalFlagInverse != 0
        let foreground = inverted ? cell.background : cell.foreground
        let background = inverted ? cell.foreground : cell.background
        context.setFillColor(background.cgColor)
        context.fill(rect)

        if cell.flags & terminalFlagHidden == 0, cell.text != " " {
          let font = cell.flags & terminalFlagBold != 0 ? boldFont : regularFont
          _ = drawText(
            cell.text,
            in: rect,
            font: font,
            color: foreground,
            context: context
          )
        }

        if cell.flags & underlineMask != 0 {
          drawUnderline(in: rect, color: foreground, scale: scale, context: context)
        }
      }

      if let cursorRow = snapshot.cursorRow,
         let cursorColumn = snapshot.cursorColumn {
        let cursorCellRect = cellRect(
          row: cursorRow,
          column: cursorColumn,
          cellsWide: 1,
          cellWidth: cellWidth,
          cellHeight: cellHeight,
          canvasHeight: height
        )
        let cursorRect = cursorCellRect.insetBy(dx: max(1, scale), dy: max(1, scale))
        context.setStrokeColor(cursorColor.cgColor)
        context.setLineWidth(max(1, scale))
        context.stroke(cursorRect)

        if !preedit.isEmpty {
          let preeditRect = CGRect(
            x: CGFloat(cursorColumn * cellWidth),
            y: cursorCellRect.minY,
            width: max(0, CGFloat(width - cursorColumn * cellWidth)),
            height: CGFloat(cellHeight)
          )
          let preeditWidth = drawText(
            preedit,
            in: preeditRect,
            font: regularFont,
            color: preeditColor,
            context: context
          )
          if preeditWidth > 0 {
            drawUnderline(
              in: CGRect(
                x: preeditRect.minX,
                y: preeditRect.minY,
                width: preeditWidth,
                height: preeditRect.height
              ),
              color: preeditColor,
              scale: scale,
              context: context
            )
          }
        }
      }
      return true
    }

    return rendered ? pixels : nil
  }

  #if targetEnvironment(simulator)
  static func makeImage(pixels: [UInt8], width: Int, height: Int) -> CGImage? {
    let rowBytes = width.multipliedReportingOverflow(by: 4)
    let expectedBytes = rowBytes.partialValue.multipliedReportingOverflow(by: height)
    guard width > 0,
          height > 0,
          !rowBytes.overflow,
          !expectedBytes.overflow,
          pixels.count == expectedBytes.partialValue,
          let provider = CGDataProvider(data: Data(pixels) as CFData) else {
      return nil
    }

    let bitmapInfo = CGBitmapInfo.byteOrder32Little.union(
      CGBitmapInfo(rawValue: CGImageAlphaInfo.premultipliedFirst.rawValue)
    )
    return CGImage(
      width: width,
      height: height,
      bitsPerComponent: 8,
      bitsPerPixel: 32,
      bytesPerRow: rowBytes.partialValue,
      space: CGColorSpaceCreateDeviceRGB(),
      bitmapInfo: bitmapInfo,
      provider: provider,
      decode: nil,
      shouldInterpolate: false,
      intent: .defaultIntent
    )
  }
  #endif

  private static func cellRect(
    row: Int,
    column: Int,
    cellsWide: Int,
    cellWidth: Int,
    cellHeight: Int,
    canvasHeight: Int
  ) -> CGRect {
    CGRect(
      x: CGFloat(column * cellWidth),
      y: CGFloat(canvasHeight - (row + 1) * cellHeight),
      width: CGFloat(cellsWide * cellWidth),
      height: CGFloat(cellHeight)
    )
  }

  @discardableResult
  private static func drawText(
    _ text: String,
    in rect: CGRect,
    font: UIFont,
    color: TerminalColor,
    context: CGContext
  ) -> CGFloat {
    let attributed = NSAttributedString(
      string: text,
      attributes: [
        .font: font,
        .foregroundColor: UIColor(cgColor: color.cgColor)
      ]
    )
    let line = CTLineCreateWithAttributedString(attributed)
    let baseline = rect.minY + max(0, (rect.height - font.lineHeight) / 2) - font.descender

    context.saveGState()
    context.clip(to: rect)
    context.textMatrix = .identity
    context.textPosition = CGPoint(x: rect.minX, y: baseline)
    CTLineDraw(line, context)
    context.restoreGState()
    let measuredWidth = CGFloat(CTLineGetTypographicBounds(line, nil, nil, nil)).rounded(.up)
    return min(rect.width, measuredWidth)
  }

  private static func drawUnderline(
    in rect: CGRect,
    color: TerminalColor,
    scale: CGFloat,
    context: CGContext
  ) {
    let thickness = max(1, scale)
    context.setFillColor(color.cgColor)
    context.fill(
      CGRect(
        x: rect.minX,
        y: rect.minY + max(thickness, 2 * scale),
        width: rect.width,
        height: thickness
      )
    )
  }
}

private extension TerminalColor {
  var cgColor: CGColor {
    CGColor(
      red: CGFloat(red) / 255,
      green: CGFloat(green) / 255,
      blue: CGFloat(blue) / 255,
      alpha: CGFloat(alpha) / 255
    )
  }
}
