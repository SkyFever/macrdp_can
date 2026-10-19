# Relative pointer geometry in mirrored Retina sessions

A physical Mac display has a **point-coordinate** geometry for CoreGraphics
input, and may have a different **pixel-coordinate** geometry for capture.
The RDP client, meanwhile, sends pointer events in its negotiated desktop.

The prior input handler mapped absolute events through the inverse aspect-fit
rectangle but added relative events (`MouseEvent::RelMove`) directly to the Mac
cursor position, as if a client pixel were always a Mac point. This is incorrect
for downscaled / Retina mirroring and for letterboxed sessions. The error
accumulates as a visible offset between the client cursor and the Mac cursor.

## Change

- Absolute events retain their existing inverse aspect-fit mapping.
- Relative events now use the corresponding **inverse scale** of the displayed
  content rectangle, excluding black-bar offsets (a delta has no origin).
- Mouse position is still clamped to the destination display's last valid point;
  no input events are fabricated or buffered.
- The first absolute and first relative pointer event each emit a single
  `INFO ... RDP mouse geometry mode=...` diagnostic. For relative events the
  reported `scale_x` and `scale_y` are Mac points per RDP pixel.
- The capture, bitmap delta, batching, frame pacing, and display-resolution
  negotiation paths are unchanged. The observed 60-fps behavior is not
  promised to improve or deteriorate because of this input-only change.

For the reported case, a 3360x2100 Retina backing corresponds to 1680x1050
macOS display points, while the negotiated RDP canvas is 1968x2100. Aspect fit
produces content 1968x1230 centered vertically (435-px bars). Thus 1 relative
RDP pixel corresponds to 1680/1968 ~= 0.85366 Mac points on **both** axes,
not 1 Mac point. Absolute inputs already used this scale.

## Verification

In the Android Windows App, test both pointer/trackpad movement modes:

1. Move the cursor to the center of the mirrored display and then to the edge.
2. Click a small target at each corner and compare the displayed target with
   the cursor's actual hover/click position.
3. Test a sustained diagonal drag and reconnect, then repeat with `HIDPI=0`.
4. Observe `RDP mouse geometry mode=relative` in the log. If **only**
   `mode=absolute` appears, this particular fix does not explain a remaining
   offset; record the actual cursor position and inspect SCStream's content
   rectangle vs. the client-visible canvas.

This is an input-coordinate correction, not a fix for RDP cursor
prediction/cursor-shape presentation or client video frame skipping.

## Build and limitations

Run on the Mac: `cargo test --release coord_tests`, then `cargo build --release`.
The source-only checks in the patch authoring environment are not a substitute
for a macOS build, signed app packaging, or a real Android RDP input test.
