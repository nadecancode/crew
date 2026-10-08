# Raster metadata fixtures

The four 2×2 images encode the same constant RGB value (0, 128, 255).
They were generated from an uncompressed PPM using the local ffmpeg encoder;
no model, network image source or copyrighted picture is involved. GIF uses
`-loop -1` so it has no animation-loop extension. Tests inspect all four
complete files and every truncated prefix, then build clearly synthetic
animation/size/checksum negatives from them.

The metadata reader does not decode compressed pixels or claim to repair
corrupted image data. It validates container bounds, static-image metadata,
dimensions and byte limits before exposing an immutable raster asset.
