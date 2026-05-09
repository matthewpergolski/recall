// swift-tools-version: 6.0

import PackageDescription

let package = Package(
    name: "RecallCapture",
    platforms: [
        .macOS(.v14)
    ],
    products: [
        .executable(name: "recall-capture", targets: ["RecallCapture"])
    ],
    targets: [
        .executableTarget(name: "RecallCapture")
    ]
)
