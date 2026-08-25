//! The per-architecture build slices an Apple export cross-builds.
//!
//! One Apple platform is more than one machine: iOS is a device *and* a
//! simulator, and each slice is its own triple, its own SDK, and its own
//! static archives. The export builds every slice of every platform it
//! addresses and hands the linker one `OTHER_LDFLAGS` block per slice,
//! conditioned on the SDK Xcode happens to be building — which is what makes
//! one target open in Xcode and run on both.

use crate::apple::ApplePlatform;

/// One architecture slice of one platform.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArchSlice {
    /// The slice's name in artifact paths, e.g. `ios-sim`.
    pub label: &'static str,
    /// The `arch-os-abi` triple the Kira backend emits for.
    pub arch: &'static str,
    pub os: &'static str,
    pub abi: &'static str,
    /// The SDK `xcrun --sdk <name>` resolves; also Xcode's `$PLATFORM_NAME`.
    pub apple_sdk: &'static str,
    /// The `OTHER_LDFLAGS[sdk=…]` condition this slice's flags sit behind.
    ///
    /// macOS has exactly one SDK, so its block is unconditional.
    pub sdk_condition: Option<&'static str>,
}

impl ArchSlice {
    /// The normalized `arch-vendor-os-abi` spelling rustc and clang take.
    pub fn normalized_triple(&self) -> String {
        match self.os {
            "macos" => format!("{}-apple-darwin", self.arch),
            "ios" | "tvos" | "visionos" => match self.abi {
                "sim" => format!("{}-apple-{}-sim", self.arch, self.os),
                _ => format!("{}-apple-{}", self.arch, self.os),
            },
            os => format!("{}-unknown-{os}-{}", self.arch, self.abi),
        }
    }

    /// The architecture as Xcode's `ARCHS`/`VALID_ARCHS` spell it.
    ///
    /// Kira names the machine `aarch64` everywhere a triple is written; Xcode
    /// never learned that spelling.
    pub fn xcode_arch(&self) -> &'static str {
        match self.arch {
            "aarch64" => "arm64",
            other => other,
        }
    }

    /// The Mach-O `LC_BUILD_VERSION` platform this slice's objects must name.
    ///
    /// These are the loader's fixed codes (`mach-o/loader.h`), not anyone's
    /// spelling: a static archive whose members stamp one platform is refused
    /// by a link aimed at another, which is exactly what an archive reused
    /// across Apple families needs corrected.
    pub fn platform_code(&self) -> u32 {
        match (self.os, self.abi) {
            ("macos", _) => 1,
            ("ios", "sim") => 7,
            ("tvos", "sim") => 8,
            ("visionos", "sim") => 11,
            ("ios", _) => 2,
            ("tvos", _) => 3,
            ("visionos", _) => 10,
            _ => 1,
        }
    }
}

/// This machine's architecture as a triple's arch component.
pub fn host_arch() -> &'static str {
    std::env::consts::ARCH
}

/// Every slice `platform` exports, in link order.
///
/// The macOS slice follows the machine running the export — a Mac export runs
/// on the Mac that built it — while the device-and-simulator platforms are
/// always arm64, which is the only architecture those targets ship in.
pub fn slices_for(platform: ApplePlatform) -> Vec<ArchSlice> {
    match platform {
        ApplePlatform::Macos => vec![ArchSlice {
            label: "macos",
            arch: host_arch(),
            os: "macos",
            abi: "none",
            apple_sdk: "macosx",
            sdk_condition: None,
        }],
        ApplePlatform::Ios => vec![
            ArchSlice {
                label: "ios-device",
                arch: "aarch64",
                os: "ios",
                abi: "none",
                apple_sdk: "iphoneos",
                sdk_condition: Some("iphoneos*"),
            },
            ArchSlice {
                label: "ios-sim",
                arch: "aarch64",
                os: "ios",
                abi: "sim",
                apple_sdk: "iphonesimulator",
                sdk_condition: Some("iphonesimulator*"),
            },
        ],
        ApplePlatform::Tvos => vec![
            ArchSlice {
                label: "tvos-device",
                arch: "aarch64",
                os: "tvos",
                abi: "none",
                apple_sdk: "appletvos",
                sdk_condition: Some("appletvos*"),
            },
            ArchSlice {
                label: "tvos-sim",
                arch: "aarch64",
                os: "tvos",
                abi: "sim",
                apple_sdk: "appletvsimulator",
                sdk_condition: Some("appletvsimulator*"),
            },
        ],
        ApplePlatform::Visionos => vec![
            ArchSlice {
                label: "visionos-device",
                arch: "aarch64",
                os: "visionos",
                abi: "none",
                apple_sdk: "xros",
                sdk_condition: Some("xros*"),
            },
            ArchSlice {
                label: "visionos-sim",
                arch: "aarch64",
                os: "visionos",
                abi: "sim",
                apple_sdk: "xrsimulator",
                sdk_condition: Some("xrsimulator*"),
            },
        ],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn macos_follows_the_host_and_ios_has_two_slices() {
        let macos = slices_for(ApplePlatform::Macos);
        assert_eq!(macos.len(), 1);
        assert_eq!(macos[0].arch, host_arch());
        assert_eq!(macos[0].sdk_condition, None);

        let ios = slices_for(ApplePlatform::Ios);
        assert_eq!(
            ios.iter().map(|slice| slice.label).collect::<Vec<_>>(),
            vec!["ios-device", "ios-sim"]
        );
    }

    #[test]
    fn simulator_slices_normalize_to_their_own_spelling() {
        let ios = slices_for(ApplePlatform::Ios);
        assert_eq!(ios[0].normalized_triple(), "aarch64-apple-ios");
        assert_eq!(ios[1].normalized_triple(), "aarch64-apple-ios-sim");
        assert_eq!(ios[0].xcode_arch(), "arm64");
        assert!(macos_xcode_arch_matches_the_host());

        let vision = slices_for(ApplePlatform::Visionos);
        assert_eq!(vision[1].normalized_triple(), "aarch64-apple-visionos-sim");
    }

    fn macos_xcode_arch_matches_the_host() -> bool {
        let macos = slices_for(ApplePlatform::Macos);
        let expected = if macos[0].arch == "aarch64" {
            "arm64"
        } else {
            macos[0].arch
        };
        macos[0].xcode_arch() == expected
    }

    #[test]
    fn every_platform_has_at_least_one_slice_and_an_sdk() {
        for platform in [
            ApplePlatform::Macos,
            ApplePlatform::Ios,
            ApplePlatform::Tvos,
            ApplePlatform::Visionos,
        ] {
            let slices = slices_for(platform);
            assert!(!slices.is_empty());
            for slice in slices {
                assert!(!slice.apple_sdk.is_empty());
            }
        }
    }

    #[test]
    fn mach_o_platform_codes_follow_the_loader_table() {
        assert_eq!(slices_for(ApplePlatform::Macos)[0].platform_code(), 1);
        let ios = slices_for(ApplePlatform::Ios);
        assert_eq!(ios[0].platform_code(), 2);
        assert_eq!(ios[1].platform_code(), 7);
        assert_eq!(slices_for(ApplePlatform::Tvos)[1].platform_code(), 8);
        assert_eq!(slices_for(ApplePlatform::Visionos)[0].platform_code(), 10);
        assert_eq!(slices_for(ApplePlatform::Visionos)[1].platform_code(), 11);
    }
}
