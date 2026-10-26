// swift-tools-version: 6.0
import PackageDescription

interface BoardProfile {
    func cpuFlags() -> [String]
    func flashSizeKb(_ multiplier: Int) -> Int
}

struct Pico2WBoard: BoardProfile {
    let baseKb: Int

    func cpuFlags() -> [String] {
        ["-mcpu=cortex-m33", "-mfloat-abi=hard"]
    }

    func flashSizeKb(_ multiplier: Int) -> Int {
        self.baseKb * multiplier
    }
}

let board = Pico2WBoard(baseKb: 2048)

// `Shizuku` conforms to the built-in `Package` protocol/interface!
struct Shizuku: Package {
    let cxxLanguageStandard = .cxx23
    let platforms = [
        .rp2350(board: .pico2W, floatABI: .hard),
        .esp32c5,
        .macOS(.v15),
    ]
    let products = [
        .firmware(name: "shizuku_uf2", format: .uf2, targets: ["shizuku"]),
    ]
    let dependencies = [
        .package(url: "https://github.com/raspberrypi/pico-sdk", exact: "2.2.0"),
        .package(url: "https://gitlab.com/libeigen/eigen", from: "3.4.0"),
    ]
    let targets = [
        .target(
            name: "ShizukuCore",
            path: "firmware/core",
            dependencies: [
                .product(name: "Eigen", package: "eigen"),
                .product(name: "pico_stdlib", package: "pico-sdk", condition: .when(platforms: [.rp2350])),
            ],
            cxxSettings: [
                .unsafeFlags(["-fno-exceptions", "-fno-rtti", "-O0", "-O2"].filter { $0 != "-O0" }),
                .unsafeFlags(board.cpuFlags(), .when(platforms: [.rp2350])),
                .define("SHIZUKU_HOST_SIM", .when(platforms: [.macOS, .linux])),
                .headerSearchPath("include"),
            ] + ["PICO_RP2350", "PICO_CYW43_SUPPORTED"].map {
                .define($0, to: "1", .when(platforms: [.rp2350]))
            }
        ),
        .executableTarget(
            name: "shizuku",
            path: "firmware",
            sources: ["main.cpp"],
            dependencies: [
                "ShizukuCore",
                .product(
                    name: "pico_cyw43_arch_lwip_threadsafe_background",
                    package: "pico-sdk",
                    condition: .when(platforms: [.rp2350])
                ),
            ],
            linkerSettings: [
                .gcSections(.when(platforms: [.rp2350, .esp32c5])),
                .outputFormat(.uf2, family: .rp2350ArmS, .when(platforms: [.rp2350])),
                .linkedLibrary("m"),
            ]
        ),
        .testTarget(
            name: "ShizukuCoreTests",
            dependencies: ["ShizukuCore"]
        ),
    ]
}
