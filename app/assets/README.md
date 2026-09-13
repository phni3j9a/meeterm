# App image assets

## App icon

`app-icon.png` is an opaque 1024 × 1024 close-up of the same meerkat,
generated with ImageGen on 2026-09-13 using the alpha companion as reference.
The platform applies its own icon mask; the asset has no baked rounded corners.
Prompt: simplify the alert tan/ivory meerkat into a legible head-and-shoulders
app icon, dark brown outlines, calm paper-colored background, no text, terminal
symbol, shadow, or gradient. The 1254 px source was resized without changing
its artwork. Original generation remains outside the repository.

## Original mock illustration

`meerkat-companion.webp` is an unchanged copy of the existing r3/r5 mock asset
from `docs/mock/assets/meerkat-companion.webp`. Its ImageGen origin and prompt
are recorded in `docs/mock/assets/README.md`.

The original has a white RGB matte, not an alpha channel. It is kept for
provenance; the current native app uses the transparent v2 below.

An earlier ImageGen background extraction during the first app integration
changed the illustration and was rejected. It is separate from the accepted
2026-09-13 v2 below.

# Meerkat companion v2

`meerkat-companion-v2.png` was generated with the built-in ImageGen tool on
2026-09-13, using the existing mock companion as the edit target. It retains
the upright sand-and-ivory character and hand-drawn dark brown outline, with
real alpha transparency instead of the original white matte. The generated
1254 × 1254 PNG was reduced to 720 × 720 for the mobile app; alpha was retained.
The original remains in the local ImageGen output directory. No specific
image model was selectable in the tool call.

Prompt:

> Use case: background-extraction. Asset type: original meeterm mobile app meerkat illustration, refined hand-drawn editorial ink, for a warm ivory light interface. Input image 1 is the edit target. Preserve the exact slender upright meerkat character, pose, quiet friendly expression, tan sand fur, ivory belly, dark brown hand-drawn outlines, complete tail and feet. Remove the white background and white matte entirely. Deliver a real transparent PNG with alpha, clean antialiased edges, NO checkerboard pixels, NO paper rectangle, NO shadow, NO text, NO watermark. Keep the delicate drawn ground line directly under the feet only. Center the entire figure with 8 percent transparent padding, high quality crisp detail. This is a production bitmap cutout for native iOS and Android, so CSS blending will not be available.
