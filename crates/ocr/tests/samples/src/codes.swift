import CoreImage
import AppKit
let args = CommandLine.arguments
let kind = args[1]; let payload = args[2]; let out = args[3]
let filter = CIFilter(name: kind == "qr" ? "CIQRCodeGenerator" : "CICode128BarcodeGenerator")!
filter.setValue(payload.data(using: .ascii), forKey: "inputMessage")
if kind == "qr" { filter.setValue("M", forKey: "inputCorrectionLevel") }
let img = filter.outputImage!.transformed(by: CGAffineTransform(scaleX: 8, y: 8))
let ctx = CIContext()
let cg = ctx.createCGImage(img, from: img.extent)!
let rep = NSBitmapImageRep(cgImage: cg)
try! rep.representation(using: .png, properties: [:])!.write(to: URL(fileURLWithPath: out))
