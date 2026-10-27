# JPEG pixel references

The PPM files are independent libjpeg-turbo 3.1.4.1 decodes. Tests compare
every RGB byte, including reduced-IDCT output, without a tolerance.
`baseline_420_16x16.jpg` is the existing baseline input.

`progressive_8x8.jpg` is a synthetic 8×8 RGB image with pixels
`(x * 29, y * 31, (x * 19 + y * 13) % 256)`, encoded with
`cjpeg -quality 90 -sample 1x1 -progressive`. Its varying pixels distinguish
reduced IDCT from full-resolution nearest-neighbor resizing.

The references were produced with:

```sh
djpeg baseline_420_16x16.jpg > baseline_420_16x16.ppm
djpeg -scale 1/4 baseline_420_16x16.jpg > baseline_420_16x16.r2.ppm
djpeg progressive_8x8.jpg > progressive_8x8.ppm
djpeg -scale 1/2 progressive_8x8.jpg > progressive_8x8.r1.ppm
```

Do not update references merely to make a changed decoder pass.
