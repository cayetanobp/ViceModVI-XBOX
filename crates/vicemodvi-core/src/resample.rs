//! Separable image resampling with alpha-weighted color, following the
//! ImageMagick resize filters that the C# build used.

use rayon::prelude::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Filter {
    /// Area average; used to generate mip levels.
    Box,
    /// Linear interpolation that widens when downsampling.
    Triangle,
}

impl Filter {
    fn support(self) -> f64 {
        match self {
            Filter::Box => 0.5,
            Filter::Triangle => 1.0,
        }
    }

    fn weight(self, x: f64) -> f64 {
        match self {
            Filter::Box => 1.0,
            Filter::Triangle => (1.0 - x.abs()).max(0.0),
        }
    }
}

/// 8-bit BGRA pixels.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Image {
    pub width: usize,
    pub height: usize,
    pub bgra: Vec<u8>,
}

struct Contribution {
    start: usize,
    weights: Vec<f32>,
}

fn contributions(source: usize, target: usize, filter: Filter) -> Vec<Contribution> {
    let factor = target as f64 / source as f64;
    let mut scale = (1.0 / factor).max(1.0);
    let mut support = scale * filter.support();
    if support < 0.5 {
        support = 0.5;
        scale = 1.0;
    }
    let inverse = 1.0 / scale;
    (0..target)
        .map(|x| {
            let center = (x as f64 + 0.5) / factor;
            let start = ((center - support + 0.5).floor().max(0.0) as usize).min(source - 1);
            let stop = ((center + support + 0.5).floor() as usize).clamp(start + 1, source);
            let mut weights: Vec<f64> = (start..stop).map(|j| filter.weight(inverse * (j as f64 - center + 0.5))).collect();
            let density: f64 = weights.iter().sum();
            if density > 0.0 {
                weights.iter_mut().for_each(|w| *w /= density);
            } else {
                // Degenerate kernel: fall back to the nearest source pixel.
                weights.iter_mut().for_each(|w| *w = 0.0);
                let nearest = ((center.floor() as usize).clamp(start, stop - 1)) - start;
                weights[nearest] = 1.0;
            }
            Contribution { start, weights: weights.into_iter().map(|w| w as f32).collect() }
        })
        .collect()
}

/// Resizes to `width` × `height`, ignoring the aspect ratio.
pub fn resize(image: &Image, width: usize, height: usize, filter: Filter) -> Image {
    let (sw, sh) = (image.width, image.height);
    if sw == width && sh == height {
        return image.clone();
    }
    // Premultiplied floating-point pixels: B, G, R scaled by alpha, then A.
    let source: Vec<[f32; 4]> = image
        .bgra
        .chunks_exact(4)
        .map(|p| {
            let a = p[3] as f32 / 255.0;
            [p[0] as f32 * a, p[1] as f32 * a, p[2] as f32 * a, a]
        })
        .collect();
    let columns = contributions(sw, width, filter);
    let mut horizontal = vec![[0f32; 4]; width * sh];
    horizontal.par_chunks_mut(width).enumerate().for_each(|(y, row)| {
        let line = &source[y * sw..(y + 1) * sw];
        for (x, c) in columns.iter().enumerate() {
            let mut sum = [0f32; 4];
            for (k, &w) in c.weights.iter().enumerate() {
                let p = line[c.start + k];
                for i in 0..4 {
                    sum[i] += w * p[i];
                }
            }
            row[x] = sum;
        }
    });
    let rows = contributions(sh, height, filter);
    let mut bgra = vec![0u8; width * height * 4];
    bgra.par_chunks_mut(width * 4).enumerate().for_each(|(y, out)| {
        let c = &rows[y];
        for x in 0..width {
            let mut sum = [0f32; 4];
            for (k, &w) in c.weights.iter().enumerate() {
                let p = horizontal[(c.start + k) * width + x];
                for i in 0..4 {
                    sum[i] += w * p[i];
                }
            }
            let alpha = sum[3];
            let pixel = &mut out[x * 4..x * 4 + 4];
            for i in 0..3 {
                let value = if alpha > 1e-6 { sum[i] / alpha } else { 0.0 };
                pixel[i] = (value + 0.5).floor().clamp(0.0, 255.0) as u8;
            }
            pixel[3] = (alpha * 255.0 + 0.5).floor().clamp(0.0, 255.0) as u8;
        }
    });
    Image { width, height, bgra }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn box_halves_by_averaging() {
        let image = Image { width: 2, height: 2, bgra: vec![0, 0, 0, 255, 255, 255, 255, 255, 0, 0, 0, 255, 255, 255, 255, 255] };
        let half = resize(&image, 1, 1, Filter::Box);
        assert_eq!(half.bgra, vec![128, 128, 128, 255]);
    }

    #[test]
    fn transparent_pixels_do_not_darken_color() {
        let image = Image { width: 2, height: 1, bgra: vec![0, 0, 255, 255, 0, 0, 0, 0] };
        let half = resize(&image, 1, 1, Filter::Box);
        assert_eq!(half.bgra, vec![0, 0, 255, 128]);
    }

    #[test]
    fn upscale_and_odd_sizes_keep_dimensions() {
        let image = Image { width: 3, height: 5, bgra: vec![200; 3 * 5 * 4] };
        for (w, h) in [(7, 2), (1, 1), (3, 5), (16, 16)] {
            let out = resize(&image, w, h, Filter::Triangle);
            assert_eq!(out.bgra.len(), w * h * 4);
            assert!(out.bgra.iter().all(|&v| v == 200));
        }
    }
}
