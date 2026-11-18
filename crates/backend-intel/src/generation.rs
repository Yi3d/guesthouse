//! What one graphics architecture changes, picked by the IP version xe reports for the card.
//!
//! The adapter record names the platform and core families, which have to be right for the card,
//! so those come from here, along with what the card can't be asked for. The numbers are Intel's
//! open ones: the family and core enums from gmmlib's `igfxfmid.h`, and the rest from
//! compute-runtime's hardware tables for the platform. Both are MIT. The driver file names are the
//! ones Intel's INF gives these cards. The device ids are the Linux kernel's, from
//! `include/drm/intel/pciids.h`.
//!
//! A card is matched by its render IP first, so a card of a known generation works whatever its
//! device id. The device id settles it only where the IP doesn't: two platforms that share an IP,
//! and a platform from before GMD_ID, for which xe reports no IP at all. A card that matches nothing
//! is refused, as the guest would be told the wrong platform.
//!
//! What no card of a generation has confirmed yet is in its [`Generation::guesses`], which the
//! daemon logs at start.

use crate::protocol::guest_driver::{CardDrivers, XE2_HPG_DRIVERS, XE2_LPG_DRIVERS, XE3_LPG_DRIVERS};
use crate::xe_query::IpVersion;

/// One architecture.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Generation {
    pub name: &'static str,
    /// The render IPs' major and minor versions, as xe gives them from the card's GMD_ID. Empty for
    /// a platform from before GMD_ID.
    pub render_ips: &'static [(u16, u16)],
    /// The platform's PCI device ids, as the kernel lists them.
    pub devices: &'static [u16],
    /// Whether xe takes these cards by default. Otherwise i915 does, and xe only with its
    /// `force_probe` option naming the card.
    pub xe_by_default: bool,
    /// PRODUCT_FAMILY.
    pub product_family: u32,
    /// PCH_PRODUCT_FAMILY. The platform has no separate PCH to name.
    pub pch_family: u32,
    /// GFXCORE_FAMILY of the display and render cores.
    pub display_core: u32,
    pub render_core: u32,
    /// PLATFORM_TYPE.
    pub platform_type: u32,
    /// GTTYPE. Newer platforms leave it undefined.
    pub gt_type: u32,
    /// Hardware threads in each EU, for a card whose hwconfig doesn't say.
    pub threads_per_eu: u32,
    /// Whether the platform tiles as Xe2 and later do (FtrXe2PlusTiling).
    pub xe2_tiling: bool,
    /// The drivers Intel's INF names for these cards alone.
    pub drivers: CardDrivers,
    /// What in this profile no card has confirmed, for the log.
    pub guesses: &'static [&'static str],
}

/// GTTYPE_UNDEFINED, which every platform here gives.
const GT_TYPE_UNDEFINED: u32 = 18;
/// PCH_UNKNOWN and PLATFORM_NONE.
const NO_PCH: u32 = 0;
const NO_PLATFORM_TYPE: u32 = 0;

/// IGFX_XE2_LPG_CORE. gmmlib defines IGFX_XE2_HPG_CORE as the same number.
const XE2_CORE: u32 = 0x0C09;
/// IGFX_XE3_CORE.
const XE3_CORE: u32 = 0x1E00;

/// Hardware threads in each EU by compute-runtime's tables: eight up to Xe2, ten on Xe3.
const XE_THREADS_PER_EU: u32 = 8;
const XE3_THREADS_PER_EU: u32 = 10;

/// What is unconfirmed in the same way for every generation no card of has been recorded.
const GUESS_NO_RECORDING: &str = "no card of this generation has been recorded";
const GUESS_RECORD_LAYOUT: &str =
    "the record's layout and feature bits were read along Lunar Lake's path of the driver";

/// Lunar Lake's graphics, the Arc 130V and 140V: IGFX_LUNARLAKE on IGFX_XE2_LPG_CORE.
pub const XE2_LPG: Generation = Generation {
    name: "Xe2 LPG (Lunar Lake)",
    render_ips: &[(20, 4)],
    devices: &[0x6420, 0x64A0, 0x64B0],
    xe_by_default: true,
    product_family: 1275,
    pch_family: NO_PCH,
    display_core: XE2_CORE,
    render_core: XE2_CORE,
    platform_type: NO_PLATFORM_TYPE,
    gt_type: GT_TYPE_UNDEFINED,
    threads_per_eu: XE_THREADS_PER_EU,
    xe2_tiling: true,
    drivers: XE2_LPG_DRIVERS,
    guesses: &[GUESS_NO_RECORDING],
};

/// Battlemage's graphics, the Arc B570 and B580 on G21 (20.1) and the cards on G31 (20.2): IGFX_BMG
/// on IGFX_XE2_HPG_CORE, which is the same core number as Lunar Lake's.
pub const XE2_HPG: Generation = Generation {
    name: "Xe2 HPG (Battlemage)",
    render_ips: &[(20, 1), (20, 2)],
    devices: &[
        0xE202, 0xE209, 0xE20B, 0xE20C, 0xE20D, 0xE210, 0xE211, 0xE212, 0xE216, 0xE220, 0xE221, 0xE222,
        0xE223,
    ],
    xe_by_default: true,
    product_family: 1274,
    pch_family: NO_PCH,
    display_core: XE2_CORE,
    render_core: XE2_CORE,
    platform_type: NO_PLATFORM_TYPE,
    gt_type: GT_TYPE_UNDEFINED,
    threads_per_eu: XE_THREADS_PER_EU,
    xe2_tiling: true,
    drivers: XE2_HPG_DRIVERS,
    guesses: &["only G21, the B580, has been recorded: G31's IP 20.2 is compute-runtime's"],
};

/// Panther Lake's graphics (30.0 and 30.1) and Wildcat Lake's (30.3), which compute-runtime counts
/// as one platform: IGFX_PTL on IGFX_XE3_CORE.
pub const XE3_LPG: Generation = Generation {
    name: "Xe3 LPG (Panther Lake, Wildcat Lake)",
    render_ips: &[(30, 0), (30, 1), (30, 3)],
    devices: &[
        0xB080, 0xB081, 0xB082, 0xB083, 0xB084, 0xB085, 0xB086, 0xB087, 0xB08F, 0xB090, 0xB0A0, 0xB0B0,
        0xFD80, 0xFD81,
    ],
    xe_by_default: true,
    product_family: 1300,
    pch_family: NO_PCH,
    display_core: XE3_CORE,
    render_core: XE3_CORE,
    platform_type: NO_PLATFORM_TYPE,
    gt_type: GT_TYPE_UNDEFINED,
    threads_per_eu: XE3_THREADS_PER_EU,
    xe2_tiling: true,
    drivers: XE3_LPG_DRIVERS,
    guesses: &[GUESS_NO_RECORDING, GUESS_RECORD_LAYOUT],
};

const KNOWN: [Generation; 3] = [XE2_LPG, XE2_HPG, XE3_LPG];

/// Every generation this build has a profile for.
pub fn known() -> &'static [Generation] {
    &KNOWN
}

/// The profile for a card whose render IP is `ip` and whose PCI device id is `device`, if there is
/// one. The IP's revision doesn't matter.
pub fn for_card(ip: IpVersion, device: u16) -> Option<&'static Generation> {
    let ip = (ip.major, ip.minor);
    let has_ip = |generation: &&Generation| generation.render_ips.contains(&ip);
    // A platform from before GMD_ID is known by its id alone, as xe gives its IP as 0.0.
    let before_gmd_id = |generation: &&Generation| generation.render_ips.is_empty() && ip == (0, 0);
    let lists_device = |generation: &&Generation| generation.devices.contains(&device);
    let by_both = KNOWN
        .iter()
        .find(|generation| lists_device(generation) && (has_ip(generation) || before_gmd_id(generation)));
    if by_both.is_some() {
        return by_both;
    }
    let mut by_ip = KNOWN.iter().filter(has_ip);
    match (by_ip.next(), by_ip.next()) {
        (Some(only), None) => Some(only),
        _ => None,
    }
}

/// The generation that lists `device` as its own, whatever the card's IP, for the app, which can't
/// ask the card.
pub fn for_device(device: u16) -> Option<&'static Generation> {
    KNOWN.iter().find(|generation| generation.devices.contains(&device))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(major: u16, minor: u16) -> IpVersion {
        IpVersion { major, minor, revision: 0 }
    }

    #[test]
    fn lunar_lake_is_found_by_its_render_ip_at_any_revision() {
        for revision in [0, 4] {
            let ip = IpVersion { major: 20, minor: 4, revision };
            assert_eq!(for_card(ip, 0x64A0), Some(&XE2_LPG));
        }
    }

    #[test]
    fn battlemage_is_found_by_its_render_ip() {
        assert_eq!(for_card(ip(20, 1), 0xE20B), Some(&XE2_HPG));
        assert_eq!(for_card(ip(20, 2), 0xE223), Some(&XE2_HPG), "G31");
        assert_eq!(XE2_HPG.product_family, 1274);
    }

    #[test]
    fn panther_lake_and_wildcat_lake_are_one_profile_with_more_threads() {
        assert_eq!(for_card(ip(30, 0), 0xB080), Some(&XE3_LPG));
        assert_eq!(for_card(ip(30, 1), 0xB0A0), Some(&XE3_LPG));
        assert_eq!(for_card(ip(30, 3), 0xFD80), Some(&XE3_LPG), "Wildcat Lake");
        let numbers = (XE3_LPG.product_family, XE3_LPG.render_core, XE3_LPG.threads_per_eu);
        assert_eq!(numbers, (1300, 0x1E00, 10));
        assert_eq!(XE3_LPG.drivers, XE3_LPG_DRIVERS, "with no 32-bit OpenCL, as the INF gives none");
    }

    #[test]
    fn a_device_id_the_profile_lacks_is_taken_by_its_ip() {
        assert_eq!(for_card(ip(20, 4), 0x64FF), Some(&XE2_LPG));
        assert_eq!(for_card(ip(30, 0), 0xB0FF), Some(&XE3_LPG));
    }

    #[test]
    fn another_ip_has_no_profile() {
        // Nova Lake's Xe3 is another product family, and Intel's package has no driver for it.
        assert_eq!(for_card(ip(30, 4), 0xD740), None);
        assert_eq!(for_card(ip(35, 10), 0xD750), None);
        assert_eq!(for_card(ip(20, 3), 0xE20B), None);
        assert_eq!(for_card(IpVersion::default(), 0x64A0), None, "a kernel that gives no IP version");
    }

    #[test]
    fn the_app_knows_a_card_by_its_id_alone() {
        assert_eq!(for_device(0xE20B), Some(&XE2_HPG));
        assert_eq!(for_device(0xFD81), Some(&XE3_LPG));
        assert_eq!(for_device(0x46A6), None, "Alder Lake");
    }

    #[test]
    fn no_two_generations_list_the_same_device() {
        let mut seen = std::collections::BTreeSet::new();
        for device in KNOWN.iter().flat_map(|generation| generation.devices) {
            assert!(seen.insert(device), "{device:04x} twice");
        }
    }

    #[test]
    fn every_generation_says_what_is_unconfirmed() {
        for generation in KNOWN {
            assert!(!generation.guesses.is_empty(), "{}", generation.name);
        }
    }
}
