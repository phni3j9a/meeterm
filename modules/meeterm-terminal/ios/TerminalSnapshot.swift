import Foundation

private let snapshotHeaderSize = 28
private let snapshotCellMetadataSize = 28
private let snapshotVersion: UInt16 = 1

let terminalFlagInverse: UInt16 = 1 << 0
let terminalFlagBold: UInt16 = 1 << 1
let terminalFlagUnderline: UInt16 = 1 << 3
let terminalFlagHidden: UInt16 = 1 << 8
let terminalFlagDoubleUnderline: UInt16 = 1 << 11
let terminalFlagUndercurl: UInt16 = 1 << 12
let terminalFlagDottedUnderline: UInt16 = 1 << 13
let terminalFlagDashedUnderline: UInt16 = 1 << 14

struct TerminalColor {
  let red: UInt8
  let green: UInt8
  let blue: UInt8
  let alpha: UInt8
}

struct TerminalCell {
  let row: Int
  let column: Int
  let width: Int
  let flags: UInt16
  let foreground: TerminalColor
  let background: TerminalColor
  let base: String
  let combining: String

  var text: String {
    base + combining
  }
}

struct TerminalSnapshot {
  let columns: Int
  let rows: Int
  let cursorRow: Int?
  let cursorColumn: Int?
  let cells: [TerminalCell]
}

/// Defensive decoder for Rust's native-only, little-endian MTRM snapshot.
enum TerminalSnapshotParser {
  static func parse(_ data: Data) -> TerminalSnapshot? {
    var reader = SnapshotReader(data)
    guard reader.count >= snapshotHeaderSize,
          reader.readBytes(count: 4) == [0x4d, 0x54, 0x52, 0x4d],
          reader.readUInt16() == snapshotVersion,
          let encodedHeaderSize = reader.readUInt16(),
          encodedHeaderSize >= UInt16(snapshotHeaderSize),
          Int(encodedHeaderSize) <= reader.count,
          let encodedColumns = reader.readUInt32(),
          let encodedRows = reader.readUInt32(),
          let encodedCursorRow = reader.readUInt32(),
          let encodedCursorColumn = reader.readUInt32(),
          let encodedCellCount = reader.readUInt32() else {
      return nil
    }

    let columns = Int(encodedColumns)
    let rows = Int(encodedRows)
    let cellCount = Int(encodedCellCount)
    guard columns >= 2, rows >= 1,
          columns <= 4096, rows <= 4096,
          cellCount <= columns * rows,
          reader.seek(to: Int(encodedHeaderSize)) else {
      return nil
    }

    var cells: [TerminalCell] = []
    cells.reserveCapacity(cellCount)
    for _ in 0..<cellCount {
      guard reader.remaining >= snapshotCellMetadataSize,
            let encodedRow = reader.readUInt32(),
            let encodedColumn = reader.readUInt32(),
            let encodedWidth = reader.readUInt8(),
            reader.readUInt8() != nil,
            let flags = reader.readUInt16(),
            let foreground = reader.readColor(),
            let background = reader.readColor(),
            let encodedBaseLength = reader.readUInt32(),
            let encodedCombiningLength = reader.readUInt32() else {
        return nil
      }

      let row = Int(encodedRow)
      let column = Int(encodedColumn)
      let width = Int(encodedWidth)
      let baseLength = Int(encodedBaseLength)
      let combiningLength = Int(encodedCombiningLength)
      guard row >= 0, row < rows,
            column >= 0, column < columns,
            width == 1 || width == 2,
            column + width <= columns,
            baseLength <= reader.remaining,
            combiningLength <= reader.remaining - baseLength,
            let baseBytes = reader.readBytes(count: baseLength),
            let combiningBytes = reader.readBytes(count: combiningLength),
            let base = String(bytes: baseBytes, encoding: .utf8),
            let combining = String(bytes: combiningBytes, encoding: .utf8) else {
        return nil
      }

      cells.append(
        TerminalCell(
          row: row,
          column: column,
          width: width,
          flags: flags,
          foreground: foreground,
          background: background,
          base: base,
          combining: combining
        )
      )
    }

    guard reader.remaining == 0 else {
      return nil
    }

    let cursorIsHidden = encodedCursorRow == UInt32.max
    guard cursorIsHidden == (encodedCursorColumn == UInt32.max) else {
      return nil
    }
    let cursorRow = cursorIsHidden ? nil : Int(encodedCursorRow)
    let cursorColumn = cursorIsHidden ? nil : Int(encodedCursorColumn)
    if let cursorRow, let cursorColumn,
       !(0..<rows).contains(cursorRow) || !(0..<columns).contains(cursorColumn) {
      return nil
    }

    return TerminalSnapshot(
      columns: columns,
      rows: rows,
      cursorRow: cursorRow,
      cursorColumn: cursorColumn,
      cells: cells
    )
  }
}

private struct SnapshotReader {
  private let bytes: [UInt8]
  private(set) var offset = 0

  init(_ data: Data) {
    bytes = Array(data)
  }

  var count: Int {
    bytes.count
  }

  var remaining: Int {
    bytes.count - offset
  }

  mutating func seek(to newOffset: Int) -> Bool {
    guard newOffset >= offset, newOffset <= bytes.count else {
      return false
    }
    offset = newOffset
    return true
  }

  mutating func readUInt8() -> UInt8? {
    guard offset < bytes.count else {
      return nil
    }
    defer { offset += 1 }
    return bytes[offset]
  }

  mutating func readUInt16() -> UInt16? {
    guard let low = readUInt8(), let high = readUInt8() else {
      return nil
    }
    return UInt16(low) | (UInt16(high) << 8)
  }

  mutating func readUInt32() -> UInt32? {
    guard let byte0 = readUInt8(),
          let byte1 = readUInt8(),
          let byte2 = readUInt8(),
          let byte3 = readUInt8() else {
      return nil
    }
    return UInt32(byte0)
      | (UInt32(byte1) << 8)
      | (UInt32(byte2) << 16)
      | (UInt32(byte3) << 24)
  }

  mutating func readColor() -> TerminalColor? {
    guard let red = readUInt8(),
          let green = readUInt8(),
          let blue = readUInt8(),
          let alpha = readUInt8() else {
      return nil
    }
    return TerminalColor(red: red, green: green, blue: blue, alpha: alpha)
  }

  mutating func readBytes(count: Int) -> [UInt8]? {
    guard count >= 0, count <= remaining else {
      return nil
    }
    defer { offset += count }
    return Array(bytes[offset..<(offset + count)])
  }
}
