//! Exact framebuffer delta for scaled legacy BitmapUpdate sessions.
//!
//! ScreenCaptureKit reports dirty rectangles in *source-display* coordinates
//! even when it scales into a client-sized buffer. Reusing those rectangles is
//! incorrect (and can leave black or stale regions). Previously the legacy
//! path sent the entire scaled framebuffer on every animation tick.
//!
//! This module compares the actual *output* BGRA pixels against the last
//! enqueued framebuffer at tile granularity. It is deliberately independent of
//! ScreenCaptureKit / RDP so the geometry and framebuffer invariants can be
//! tested on any platform. Tiles are only an approximation of changed areas:
//! they contain every changed pixel and therefore cannot miss damage.

use std::collections::BTreeMap;

pub const TILE_SIDE: usize = 64;
/// Limit pathological checkerboard workloads and per-rectangle RDP overhead.
pub const MAX_DELTA_RECTS: usize = 96;

pub type Rect = (u16, u16, u16, u16);

/// Coalesce nearby output-space damage into fewer BitmapUpdates.
///
/// A legacy RDP client can present each BitmapUpdate separately, exposing
/// seams between different moments of a moving desktop. A bounding rectangle
/// covers all original damage, but can overdraw unchanged pixels. To avoid
/// regressing the successful low-latency tile path, permit at most 2x the
/// original dirty area for each group and never exceed the known-good maximum
/// bitmap update size. More distant regions remain independent.
///
/// This reduces paint boundaries; it does NOT promise atomic RDP presentation.
pub fn coalesce_bitmap_rects(rects: Vec<Rect>, max_pixels: u32) -> Vec<Rect> {
    // The common moving-dock case has ~3-4 rects. Avoid a quadratic search on
    // pathological checkerboards, which should be handled by the full fallback.
    const MAX_INPUT_RECTS: usize = 32;
    const MAX_OVERDRAW: u64 = 2;
    if rects.len() < 2 || rects.len() > MAX_INPUT_RECTS || max_pixels == 0 {
        return rects;
    }

    #[derive(Clone, Copy)]
    struct Group {
        rect: Rect,
        /// Area of the original, disjoint tile rectangles in this group.
        /// Tracking this across merges caps cumulative overdraw.
        dirty_area: u64,
    }

    fn area(rect: Rect) -> u64 {
        u64::from(rect.2) * u64::from(rect.3)
    }

    fn bounding(a: Rect, b: Rect) -> Option<Rect> {
        let left = u32::from(a.0.min(b.0));
        let top = u32::from(a.1.min(b.1));
        let right = (u32::from(a.0) + u32::from(a.2))
            .max(u32::from(b.0) + u32::from(b.2));
        let bottom = (u32::from(a.1) + u32::from(a.3))
            .max(u32::from(b.1) + u32::from(b.3));
        Some((
            u16::try_from(left).ok()?,
            u16::try_from(top).ok()?,
            u16::try_from(right - left).ok()?,
            u16::try_from(bottom - top).ok()?,
        ))
    }

    let mut groups: Vec<Group> = rects
        .into_iter()
        .map(|rect| Group {
            rect,
            dirty_area: area(rect),
        })
        .collect();
    loop {
        // Merge the least-expensive eligible pair first; this is small and
        // deterministic (bounded by 32 input rectangles). Prefer a pair with
        // minimum extra encoded pixels, then minimum output area.
        let mut best: Option<(u64, u64, usize, usize, Rect)> = None;
        for i in 0..groups.len() {
            for j in i + 1..groups.len() {
                let Some(merged) = bounding(groups[i].rect, groups[j].rect) else {
                    continue;
                };
                let merged_area = area(merged);
                let dirty_area = groups[i].dirty_area + groups[j].dirty_area;
                if merged_area > u64::from(max_pixels)
                    || merged_area > dirty_area * MAX_OVERDRAW
                {
                    continue;
                }
                let old_area = area(groups[i].rect) + area(groups[j].rect);
                let additional_pixels = merged_area.saturating_sub(old_area);
                let candidate = (additional_pixels, merged_area, i, j, merged);
                if best.is_none_or(|prev| candidate < prev) {
                    best = Some(candidate);
                }
            }
        }
        let Some((_, _, i, j, merged)) = best else {
            break;
        };
        let dirty_area = groups[i].dirty_area + groups[j].dirty_area;
        groups[i] = Group {
            rect: merged,
            dirty_area,
        };
        groups.remove(j);
    }
    // Preserve top-to-bottom / left-to-right presentation order across groups.
    groups.sort_by_key(|group| (group.rect.1, group.rect.0));
    groups.into_iter().map(|group| group.rect).collect()
}

#[derive(Debug, PartialEq, Eq)]
pub enum Delta {
    /// New dimensions, a new connection, or too much change to benefit from
    /// individual rectangles. The caller must enqueue a whole framebuffer.
    Full,
    /// Pixel-perfect match; no BitmapUpdate is needed.
    Unchanged,
    /// Rectangles cover *all* pixels changed since the last enqueued frame.
    Partial(Vec<Rect>),
}

/// Per-connection baseline. Stored tightly packed rather than in SCK's possibly
/// padded stride; updates to unchanged tiles do not allocate or copy pixels.
#[derive(Default)]
pub struct FrameDelta {
    width: u16,
    height: u16,
    previous: Vec<u8>,
}

impl FrameDelta {
    /// Update the baseline and return the rectangles to send, in display
    /// coordinates. `frame` must be BGRA with `stride >= width * 4`.
    ///
    /// Important: use this ONLY when the caller preserves all enqueued deltas
    /// in order. A reconnect must construct a fresh `FrameDelta`, and dropping
    /// a not-yet-sent rectangle requires another full seed. A baseline is not
    /// a record of client ACKs.
    pub fn update(&mut self, frame: &[u8], stride: usize, width: u16, height: u16) -> Delta {
        let (w, h) = (usize::from(width), usize::from(height));
        let row_bytes = w * 4;
        if w == 0 || h == 0 || stride < row_bytes {
            return Delta::Full;
        }
        let Some(min_bytes) = (h - 1)
            .checked_mul(stride)
            .and_then(|n| n.checked_add(row_bytes))
        else {
            return Delta::Full;
        };
        if frame.len() < min_bytes {
            // Invalid input: do not index outside the source. SCK should
            // always provide a complete locked CVPixelBuffer.
            return Delta::Full;
        }

        if self.width != width || self.height != height || self.previous.len() != row_bytes * h {
            self.width = width;
            self.height = height;
            self.previous.resize(row_bytes * h, 0);
            self.copy_full(frame, stride);
            return Delta::Full;
        }

        let cols = w.div_ceil(TILE_SIDE);
        let rows = h.div_ceil(TILE_SIDE);
        let mut changed = vec![false; cols * rows];
        let mut changed_tiles = 0usize;
        for ty in 0..rows {
            let y0 = ty * TILE_SIDE;
            let y1 = (y0 + TILE_SIDE).min(h);
            for tx in 0..cols {
                let x0 = tx * TILE_SIDE;
                let xbytes = x0 * 4;
                let nbytes = ((x0 + TILE_SIDE).min(w) - x0) * 4;
                let differs = (y0..y1).any(|y| {
                    let src_at = y * stride + xbytes;
                    let old_at = y * row_bytes + xbytes;
                    frame[src_at..src_at + nbytes]
                        != self.previous[old_at..old_at + nbytes]
                });
                if differs {
                    changed[ty * cols + tx] = true;
                    changed_tiles += 1;
                    // Advance the baseline only for tiles that really changed.
                    // This also keeps pad bytes outside the comparison.
                    for y in y0..y1 {
                        let src_at = y * stride + xbytes;
                        let old_at = y * row_bytes + xbytes;
                        self.previous[old_at..old_at + nbytes]
                            .copy_from_slice(&frame[src_at..src_at + nbytes]);
                    }
                }
            }
        }
        if changed_tiles == 0 {
            return Delta::Unchanged;
        }
        // Large animations / videos benefit more from the original full-frame
        // path; too many independent bitmap rectangles increase PDU overhead.
        if changed_tiles * 2 > cols * rows {
            return Delta::Full;
        }

        // Coalesce horizontal tile runs, then extend runs vertically when their
        // X extents match. The result is lossless (overestimates dirty regions
        // but never omits a changed pixel), with ≤ MAX_DELTA_RECTS updates.
        let mut rects: Vec<Rect> = Vec::new();
        let mut active: BTreeMap<(usize, usize), usize> = BTreeMap::new();
        for ty in 0..rows {
            let y = ty * TILE_SIDE;
            let band_h = (h - y).min(TILE_SIDE);
            let mut next = BTreeMap::new();
            let mut tx = 0;
            while tx < cols {
                if !changed[ty * cols + tx] {
                    tx += 1;
                    continue;
                }
                let start = tx;
                while tx < cols && changed[ty * cols + tx] {
                    tx += 1;
                }
                let end = tx;
                let index = if let Some(index) = active.remove(&(start, end)) {
                    rects[index].3 += band_h as u16;
                    index
                } else {
                    let x = start * TILE_SIDE;
                    let right = (end * TILE_SIDE).min(w);
                    rects.push((x as u16, y as u16, (right - x) as u16, band_h as u16));
                    rects.len() - 1
                };
                next.insert((start, end), index);
                if rects.len() > MAX_DELTA_RECTS {
                    return Delta::Full;
                }
            }
            active = next;
        }
        Delta::Partial(rects)
    }

    fn copy_full(&mut self, frame: &[u8], stride: usize) {
        let row_bytes = usize::from(self.width) * 4;
        for (row, dst) in self.previous.chunks_exact_mut(row_bytes).enumerate() {
            let start = row * stride;
            dst.copy_from_slice(&frame[start..start + row_bytes]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn filled(w: usize, h: usize, stride: usize) -> Vec<u8> {
        assert!(stride >= w * 4);
        vec![0; h * stride]
    }

    fn paint(frame: &mut [u8], stride: usize, x: usize, y: usize) {
        frame[y * stride + x * 4..][..4].copy_from_slice(&[17, 29, 37, 255]);
    }

    fn apply(dst: &mut [u8], src: &[u8], stride: usize, w: usize, h: usize, delta: &Delta) {
        let rects = match delta {
            Delta::Full => vec![(0, 0, w as u16, h as u16)],
            Delta::Unchanged => Vec::new(),
            Delta::Partial(rects) => rects.clone(),
        };
        for (x, y, rw, rh) in rects {
            for ry in usize::from(y)..usize::from(y + rh) {
                let start = ry * stride + usize::from(x) * 4;
                let len = usize::from(rw) * 4;
                dst[start..start + len].copy_from_slice(&src[start..start + len]);
            }
        }
    }

    #[test]
    fn initial_seed_then_unchanged_and_single_pixel() {
        let (w, h, stride) = (129, 130, 129 * 4 + 16);
        let mut f = filled(w, h, stride);
        let mut prev = f.clone();
        let mut d = FrameDelta::default();
        assert_eq!(d.update(&f, stride, w as u16, h as u16), Delta::Full);
        assert_eq!(d.update(&f, stride, w as u16, h as u16), Delta::Unchanged);
        paint(&mut f, stride, 128, 129);
        let delta = d.update(&f, stride, w as u16, h as u16);
        assert_eq!(delta, Delta::Partial(vec![(128, 128, 1, 2)]));
        apply(&mut prev, &f, stride, w, h, &delta);
        assert_eq!(prev, f);
        assert_eq!(d.update(&f, stride, w as u16, h as u16), Delta::Unchanged);
    }

    #[test]
    fn adjacent_tiles_merge_both_directions() {
        let (w, h, stride) = (256, 256, 1024);
        let mut f = filled(w, h, stride);
        let mut d = FrameDelta::default();
        d.update(&f, stride, 256, 256);
        for &(x, y) in &[(1, 1), (65, 1), (1, 65), (65, 65)] {
            paint(&mut f, stride, x, y);
        }
        assert_eq!(d.update(&f, stride, 256, 256), Delta::Partial(vec![(0, 0, 128, 128)]));
    }

    #[test]
    fn many_tiles_fall_back_to_full_and_update_baseline() {
        let (w, h, stride) = (192, 128, 768);
        let mut f = filled(w, h, stride);
        let mut d = FrameDelta::default();
        d.update(&f, stride, 192, 128);
        for &(x, y) in &[(0, 0), (65, 0), (129, 0), (0, 65)] {
            paint(&mut f, stride, x, y);
        }
        assert_eq!(d.update(&f, stride, 192, 128), Delta::Full);
        assert_eq!(d.update(&f, stride, 192, 128), Delta::Unchanged);
    }

    #[test]
    fn randomized_padded_frames_reconstruct_exactly() {
        let (w, h, stride) = (321, 257, 321 * 4 + 12);
        let mut f = filled(w, h, stride);
        let mut reconstructed = f.clone();
        let mut d = FrameDelta::default();
        let mut rng: u32 = 0x6e5e_edcd;
        for _ in 0..80 {
            for _ in 0..7 {
                rng ^= rng << 13;
                rng ^= rng >> 17;
                rng ^= rng << 5;
                let x = rng as usize % w;
                let y = (rng >> 16) as usize % h;
                paint(&mut f, stride, x, y);
            }
            let delta = d.update(&f, stride, w as u16, h as u16);
            apply(&mut reconstructed, &f, stride, w, h, &delta);
            assert_eq!(reconstructed, f);
        }
    }


    #[test]
    fn batching_merges_nearby_tiles_but_not_distant_regions() {
        let near = vec![(0, 128, 128, 64), (192, 128, 64, 64), (0, 192, 256, 64)];
        assert_eq!(
            coalesce_bitmap_rects(near, 1280 * 720),
            vec![(0, 128, 256, 128)]
        );
        let far = vec![(0, 0, 64, 64), (1728, 960, 64, 64)];
        assert_eq!(coalesce_bitmap_rects(far.clone(), 1280 * 720), far);
    }

    #[test]
    fn batching_obeys_single_update_area_limit() {
        let split = vec![(0, 0, 960, 512), (0, 512, 960, 512)];
        assert_eq!(coalesce_bitmap_rects(split.clone(), 1280 * 720), split);
        assert_eq!(
            coalesce_bitmap_rects(split, 1024 * 1024),
            vec![(0, 0, 960, 1024)]
        );
    }

    #[test]
    fn batching_preserves_exact_reconstruction_for_moving_damage() {
        let (w, h, stride) = (384usize, 320usize, 384 * 4 + 12);
        let mut f = filled(w, h, stride);
        let mut result = f.clone();
        let mut delta = FrameDelta::default();
        assert_eq!(delta.update(&f, stride, w as u16, h as u16), Delta::Full);
        for step in 0..24usize {
            // Simulate displaced dock icons with non-aligned, sparse motion.
            for dy in 0..20 {
                let x = (step * 7 + dy * 3) % 320;
                let y = 180 + ((step * 13 + dy * 9) % 120);
                paint(&mut f, stride, x, y);
            }
            let original = delta.update(&f, stride, w as u16, h as u16);
            let batched = match original {
                Delta::Partial(rects) => {
                    Delta::Partial(coalesce_bitmap_rects(rects, 1280 * 720))
                }
                other => other,
            };
            apply(&mut result, &f, stride, w, h, &batched);
            assert_eq!(result, f, "frame {step}: batching must preserve every pixel");
        }
    }

    #[test]
    fn resize_always_seeds_new_dimensions() {
        let mut d = FrameDelta::default();
        assert_eq!(d.update(&[0; 64], 16, 4, 4), Delta::Full);
        assert_eq!(d.update(&[0; 64], 16, 4, 4), Delta::Unchanged);
        assert_eq!(d.update(&[0; 96], 24, 6, 4), Delta::Full);
    }
}
