//! Subject lifting: the subjects of an image (people, animals, objects) apart from its
//! background, for the editor's Remove background and the overlay's Copy subject.
//!
//! On macOS Vision's foreground request finds them (macOS 14, fully on the Mac, nothing leaves
//! it); elsewhere [`find_subjects`] fails with [`OcrError::NotImplemented`] until milestones 7
//! and 8 bring an engine. The masks are always computed from the image before anything was drawn
//! on it. A cut changes only the alpha of the image's own pixels and never their colour values,
//! so a Display P3 image stays Display P3.

use glimpsfy_capture::{Frame, PhysicalSize};

use crate::{CancelToken, OcrError, Result};

/// The subjects of one image: one soft mask per subject at the image's size, one byte per pixel
/// (0 outside, 255 inside). Subjects are numbered from 1 in Vision's order; 0 is the background.
#[derive(Clone, Debug, Default)]
pub struct Subjects {
    size: PhysicalSize,
    masks: Vec<Vec<u8>>,
}

/// Finds the subjects of `image`. Blocks for about half a second to two seconds; call it on a
/// worker thread. A cancelled token ends it with [`OcrError::Cancelled`]. An image without a
/// subject gives [`Subjects::count`] 0.
pub fn find_subjects(image: &Frame, cancel: &CancelToken) -> Result<Subjects> {
    #[cfg(target_os = "macos")]
    {
        let masks = crate::engines::vision::foreground(image, cancel)?;
        Subjects::new(image.size, masks)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (image, cancel);
        Err(OcrError::NotImplemented("subject lifting"))
    }
}

impl Subjects {
    /// Subjects of an image of `size` from their masks, each `width × height` bytes. The labels
    /// hold one byte per pixel, so at most 255 subjects are kept.
    pub fn new(size: PhysicalSize, mut masks: Vec<Vec<u8>>) -> Result<Subjects> {
        let pixels = size.width as usize * size.height as usize;
        if masks.iter().any(|m| m.len() != pixels) {
            return Err(OcrError::InvalidImage("a subject mask does not have the image's size".into()));
        }
        masks.truncate(255);
        Ok(Subjects { size, masks })
    }

    pub fn count(&self) -> usize {
        self.masks.len()
    }

    /// The size of the image the masks belong to.
    pub fn size(&self) -> PhysicalSize {
        self.size
    }

    /// One byte per pixel, row by row: the number of the subject that covers the pixel most, or 0
    /// where no subject covers half of it. The editor's page finds the count, the outline under
    /// the pointer and the chip's thumbnails in it.
    pub fn labels(&self) -> Vec<u8> {
        let pixels = self.size.width as usize * self.size.height as usize;
        let mut labels = vec![0u8; pixels];
        let mut best = vec![127u8; pixels];
        for (i, mask) in self.masks.iter().enumerate() {
            for ((label, top), &value) in labels.iter_mut().zip(best.iter_mut()).zip(mask) {
                if value > *top {
                    *top = value;
                    *label = i as u8 + 1;
                }
            }
        }
        labels
    }

    /// `frame` with only the subjects of `keep` left: each pixel's alpha is multiplied by the
    /// largest mask among them, and its colour values stay as they are. `frame` must be the image
    /// the masks were found in (or one drawn over it at its size). A number that is not a subject
    /// fails with [`OcrError::InvalidImage`], and so does an empty `keep`, since the last subject
    /// cannot be taken away.
    pub fn cut(&self, frame: &Frame, keep: &[u32]) -> Result<Frame> {
        if frame.size != self.size {
            return Err(OcrError::InvalidImage(format!(
                "the image is {}×{} pixels and its subjects were found in {}×{}",
                frame.size.width, frame.size.height, self.size.width, self.size.height
            )));
        }
        if keep.is_empty() || keep.iter().any(|&n| n == 0 || n as usize > self.count()) {
            return Err(OcrError::InvalidImage(format!("{keep:?} does not name the image's subjects")));
        }
        let kept: Vec<&[u8]> = keep.iter().map(|&n| self.masks[n as usize - 1].as_slice()).collect();
        let (width, height) = (self.size.width as usize, self.size.height as usize);
        let mut data = frame.data.clone();
        for y in 0..height {
            let row = &mut data[y * frame.bytes_per_row..y * frame.bytes_per_row + width * 4];
            for (x, px) in row.chunks_exact_mut(4).enumerate() {
                let at = y * width + x;
                let mask = kept.iter().map(|m| m[at]).max().unwrap_or(0) as u32;
                // Alpha is the fourth byte in both pixel formats.
                px[3] = ((px[3] as u32 * mask + 127) / 255) as u8;
            }
        }
        Ok(Frame::new(frame.size, frame.bytes_per_row, frame.format, frame.color.clone(), frame.scale, data)?)
    }

    /// Every subject's number, for a cut that keeps them all.
    pub fn all(&self) -> Vec<u32> {
        (1..=self.count() as u32).collect()
    }
}
