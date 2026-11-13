//! Liquid Glass. The compositor draws a rune-fx lens in the shape of the dock first, refracting
//! and frosting a capture of the screen behind it, then draws Rune's UI (tint, rim, icons) over
//! it. The capture sits in a surface-sized texture at the place it was taken from, so the glass
//! bends the right part of the screen wherever the dock is while it slides.

use std::sync::{Arc, Mutex};

use rune::compose::{Compositor, DeviceKey, Frame};
use rune_fx::*;

use crate::dock::Dock;

#[derive(Default)]
struct Shot {
    /// Where it was taken, in surface points.
    band: [f32; 4],
    width: u32,
    height: u32,
    rgba: Vec<u8>,
    generation: u64,
}

/// The latest capture behind one dock, shared by the capture thread and the compositor.
#[derive(Clone, Default)]
pub struct Backdrop(Arc<Mutex<Shot>>);

impl Backdrop {
    pub fn set(&self, band: [f32; 4], width: u32, height: u32, rgba: Vec<u8>) {
        let mut s = self.0.lock().unwrap();
        s.band = band;
        s.width = width;
        s.height = height;
        s.rgba = rgba;
        s.generation += 1;
    }

    pub fn ready(&self) -> bool {
        self.0.lock().unwrap().generation > 0
    }
}
