//! Resolution policy shared by the capture and input paths.
//!
//! Mirroring a physical Retina panel and attaching a virtual display are
//! distinct operations. A physical mirror may sample the panel's backing
//! pixels, then *fit those pixels within the negotiated RDP desktop*. The
//! virtual display, conversely, re-modes itself to the negotiated desktop;
//! it has no physical Retina backing size to sample. Neither mode may emit
//! a BitmapUpdate outside the desktop size accepted by the client.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DisplayPolicy {
    MirrorLogical,
    MirrorRetina,
    Virtual,
}

impl DisplayPolicy {
    pub fn from_flags(virtual_display: bool, hidpi: bool) -> Self {
        if virtual_display {
            Self::Virtual // HiDPI is not a virtual-display mode setting.
        } else if hidpi {
            Self::MirrorRetina
        } else {
            Self::MirrorLogical
        }
    }

    pub fn is_mirror_retina(self) -> bool {
        matches!(self, Self::MirrorRetina)
    }

    // Backing-pixel dimensions determine source quality, not the negotiated
    // RDP canvas limits. A tall portrait client's blank bars may extend past
    // the source height without requiring any source upscaling.

    pub fn source_dimensions(self, logical: (u16, u16), backing: (u16, u16)) -> (u16, u16) {
        if self.is_mirror_retina() {
            backing
        } else {
            logical
        }
    }
}

/// The rectangle occupied by a source image within an aspect-preserving
/// output canvas. Floating-point sub-pixel edges intentionally mirror the
/// macOS SCStream `preservesAspectRatio` transform and the input-side inverse.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ContentRect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

pub fn aspect_fit(source: (f64, f64), output: (f64, f64)) -> ContentRect {
    let (sw, sh) = (source.0.max(1.0), source.1.max(1.0));
    let (ow, oh) = (output.0.max(1.0), output.1.max(1.0));
    let scale = (ow / sw).min(oh / sh);
    let width = (sw * scale).min(ow);
    let height = (sh * scale).min(oh);
    ContentRect {
        x: (ow - width) / 2.0,
        y: (oh - height) / 2.0,
        width,
        height,
    }
}

/// A render/capture decision for a *negotiated* RDP desktop.
/// The output size is never enlarged here: the acceptor must first agree with
/// the client on any higher resolution. This avoids the Android `1080x1196`
/// vs `3360x2100` corruption seen when the old HiDPI path forced full backing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CapturePlan {
    pub full_frame_damage: bool,
    pub letterbox: bool,
}

pub fn capture_plan(
    policy: DisplayPolicy,
    logical: (u16, u16),
    backing: (u16, u16),
    negotiated: (u16, u16),
    auto_size: bool,
    stretch: bool,
) -> CapturePlan {
    // SCK's dirty rectangles refer to the source when the output is scaled.
    // Preserve the previous exemption for either 1:1 logical or backing sizes.
    let full_frame_damage = negotiated != logical && negotiated != backing;
    // A virtual display changes its own mode and is never letterboxed. When
    // mirroring, letterbox only if a client-sized output actually leaves bars.
    let letterbox = !matches!(policy, DisplayPolicy::Virtual)
        && auto_size
        && !stretch
        && {
            let source = policy.source_dimensions(logical, backing);
            let content = aspect_fit(
                (f64::from(source.0), f64::from(source.1)),
                (f64::from(negotiated.0), f64::from(negotiated.1)),
            );
            content.x > 0.5 || content.y > 0.5
        };
    CapturePlan {
        full_frame_damage,
        letterbox,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn real_and_virtual_retina_modes_are_separate() {
        assert_eq!(DisplayPolicy::from_flags(false, false), DisplayPolicy::MirrorLogical);
        assert_eq!(DisplayPolicy::from_flags(false, true), DisplayPolicy::MirrorRetina);
        assert_eq!(DisplayPolicy::from_flags(true, false), DisplayPolicy::Virtual);
        assert_eq!(DisplayPolicy::from_flags(true, true), DisplayPolicy::Virtual);
        assert_eq!(DisplayPolicy::MirrorRetina.source_dimensions((1680, 1050), (3360, 2100)), (3360, 2100));
        assert_eq!(DisplayPolicy::Virtual.source_dimensions((1680, 1050), (3360, 2100)), (1680, 1050));
    }

    #[test]
    fn retina_mirror_letterboxes_portrait_android_request_without_oversize() {
        let mode = DisplayPolicy::MirrorRetina;
        let plan = capture_plan(mode, (1680, 1050), (3360, 2100), (1080, 1196), true, false);
        assert_eq!(plan, CapturePlan { full_frame_damage: true, letterbox: true });
        let rect = aspect_fit((3360.0, 2100.0), (1080.0, 1196.0));
        assert_eq!(rect.width, 1080.0);
        assert!((rect.height - 675.0).abs() < 0.001);
        assert!((rect.y - 260.5).abs() < 0.001);
        assert!(rect.x >= 0.0 && rect.y >= 0.0);
        assert!(rect.x + rect.width <= 1080.0 && rect.y + rect.height <= 1196.0);
    }

    #[test]
    fn tall_portrait_client_keeps_its_full_canvas_beyond_backing_height() {
        let source = (3360.0, 2100.0);
        let negotiated = (1968.0, 2184.0);
        let rect = aspect_fit(source, negotiated);
        assert_eq!(rect.x, 0.0);
        assert!((rect.width - 1968.0).abs() < 1e-6);
        assert!((rect.height - 1230.0).abs() < 1e-6);
        assert!((rect.y - 477.0).abs() < 1e-6);
        assert!(rect.y + rect.height <= negotiated.1);
        assert_eq!(
            capture_plan(
                DisplayPolicy::MirrorRetina,
                (1680, 1050),
                (3360, 2100),
                (1968, 2184),
                true,
                false,
            ),
            CapturePlan { full_frame_damage: true, letterbox: true },
        );
    }

    #[test]
    fn retina_backing_fits_as_native_without_extra_scaling() {
        let plan = capture_plan(
            DisplayPolicy::MirrorRetina,
            (1680, 1050),
            (3360, 2100),
            (3360, 2100),
            true,
            false,
        );
        assert_eq!(plan, CapturePlan { full_frame_damage: false, letterbox: false });
    }

    #[test]
    fn retina_source_downscales_into_smaller_matching_aspect() {
        // The RDP output never grows past what the client requested.
        let p = DisplayPolicy::MirrorRetina;
        let plan = capture_plan(p, (1680, 1050), (3360, 2100), (2560, 1600), true, false);
        assert_eq!(plan, CapturePlan { full_frame_damage: true, letterbox: false });
        let rect = aspect_fit((3360.0, 2100.0), (2560.0, 1600.0));
        assert_eq!((rect.x, rect.y, rect.width, rect.height), (0.0, 0.0, 2560.0, 1600.0));
    }

    #[test]
    fn retina_letterboxes_a_high_resolution_narrow_canvas() {
        let p = DisplayPolicy::MirrorRetina;
        let plan = capture_plan(p, (1680, 1050), (3360, 2100), (2400, 2000), true, false);
        assert_eq!(plan, CapturePlan { full_frame_damage: true, letterbox: true });
        let rect = aspect_fit((3360.0, 2100.0), (2400.0, 2000.0));
        assert_eq!(rect.width, 2400.0);
        assert_eq!(rect.height, 1500.0);
        assert_eq!(rect.y, 250.0);
    }

    #[test]
    fn physical_logical_mirror_retains_old_sizing_and_stretch_opt_out() {
        let p = DisplayPolicy::MirrorLogical;
        assert_eq!(capture_plan(p, (1680, 1050), (3360, 2100), (1920, 1080), true, false),
            CapturePlan { full_frame_damage: true, letterbox: true });
        assert_eq!(capture_plan(p, (1680, 1050), (3360, 2100), (1920, 1080), true, true),
            CapturePlan { full_frame_damage: true, letterbox: false });
        assert_eq!(capture_plan(p, (1680, 1050), (3360, 2100), (1920, 1080), false, false),
            CapturePlan { full_frame_damage: true, letterbox: false });
    }

    #[test]
    fn virtual_display_is_1_to_1_even_with_hidpi_flag() {
        let p = DisplayPolicy::from_flags(true, true);
        assert_eq!(capture_plan(p, (1080, 1196), (1080, 1196), (1080, 1196), true, false),
            CapturePlan { full_frame_damage: false, letterbox: false });
    }

    #[test]
    fn aspect_fit_always_fits_output_without_stretch() {
        for (sw, sh, dw, dh) in [
            (3360.0, 2100.0, 1080.0, 1196.0),
            (3360.0, 2100.0, 1920.0, 1080.0),
            (3360.0, 2100.0, 3360.0, 2100.0),
            (1920.0, 1080.0, 3440.0, 1440.0),
            (1680.0, 1050.0, 400.0, 800.0),
        ] {
            let r = aspect_fit((sw, sh), (dw, dh));
            assert!(r.x >= 0.0 && r.y >= 0.0);
            assert!(r.x + r.width <= dw + 1e-9);
            assert!(r.y + r.height <= dh + 1e-9);
            assert!((r.width / r.height - sw / sh).abs() < 1e-9);
        }
    }
}
