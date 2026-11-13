# rune-dock

EchoDock's copy of [Rune](https://github.com/anomalyco/rune), a GPU-rendered Rust UI framework. It's taken from Rune's `dev` branch at `2d4ceb9` and trimmed to the crates, vendored dependencies and theme that apps build against, without the demos, site and examples.

## Changes from Rune

- **Overlays choose their layer-shell layer.** `OverlayOptions::wayland_layer` takes `Background`, `Bottom`, `Top` or `Overlay`, and `Overlay` stays the default. A dock sits on `Top`, so fullscreen windows and the lock screen cover it.
- **Overlays target an output by connector name.** `OverlayOptions::output_name` puts the surface on that output (for example `eDP-1`), giving one surface per monitor.
- **Overlays can reserve space.** `OverlayOptions::exclusive_zone` sets the layer surface's exclusive zone.
- **Liquid Glass lighting** (`rune-fx` lens):
  - The glass reads as a thick slab: the page darkens slightly toward the rim.
  - Specular light sits on the curved arcs facing the light and reflects the brightness of the surroundings, instead of drawing a uniform outline.
  - A soft caustic glow appears just inside the lit edge.

## License

MIT, © Anomaly (see `LICENSE`).
